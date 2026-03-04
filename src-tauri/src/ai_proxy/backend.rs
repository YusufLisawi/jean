use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

use crate::ai_proxy::config::{get_backend_binary_path, write_config_yaml};
use crate::ai_proxy::types::{login_flag_for_provider, LoginOutputEvent, LoginStatusEvent};
use crate::platform::silent_command;

/// Cached AppHandle so stop/shutdown paths can access app data dir without param changes.
static APP_HANDLE: once_cell::sync::OnceCell<AppHandle> = once_cell::sync::OnceCell::new();

struct BackendProcess {
    child: Child,
    port: u16,
}

static BACKEND_PROCESS: Lazy<Mutex<Option<BackendProcess>>> = Lazy::new(|| Mutex::new(None));

/// Tracks providers with an active OAuth login process (provider → child PID).
static ACTIVE_LOGINS: Lazy<Mutex<HashMap<String, u32>>> = Lazy::new(|| Mutex::new(HashMap::new()));

// ---------------------------------------------------------------------------
// Health check
// ---------------------------------------------------------------------------

fn is_healthy(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/v1/models");
    reqwest::blocking::Client::new()
        .get(url)
        .timeout(Duration::from_millis(1200))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn wait_until_healthy(port: u16, attempts: u32) -> bool {
    for _ in 0..attempts {
        if is_healthy(port) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

// ---------------------------------------------------------------------------
// PID file for crash-recovery cleanup
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct BackendPidRecord {
    jean_pid: u32,
    server_pid: u32,
    port: u16,
}

fn pid_file_path() -> Option<PathBuf> {
    APP_HANDLE
        .get()
        .and_then(|app| app.path().app_data_dir().ok())
        .map(|d| d.join("ai-proxy-backend.pid"))
}

fn write_pid_file(server_pid: u32, port: u16) {
    let Some(path) = pid_file_path() else { return };
    let record = BackendPidRecord {
        jean_pid: std::process::id(),
        server_pid,
        port,
    };
    if let Ok(json) = serde_json::to_string(&record) {
        let _ = fs::write(&path, json);
    }
}

fn remove_pid_file() {
    if let Some(path) = pid_file_path() {
        let _ = fs::remove_file(path);
    }
}

// ---------------------------------------------------------------------------
// Orphan cleanup (called once at app startup)
// ---------------------------------------------------------------------------

/// Kill an orphaned AI proxy backend left behind by a previous Jean crash.
/// Call once at app startup, before any `start_backend()`.
pub fn cleanup_orphaned_backend(app: &AppHandle) {
    // Seed the OnceCell early so pid_file_path() works.
    let _ = APP_HANDLE.set(app.clone());

    let path = match app.path().app_data_dir() {
        Ok(d) => d.join("ai-proxy-backend.pid"),
        Err(_) => return,
    };

    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return, // No PID file -> nothing to clean up
    };

    let record: BackendPidRecord = match serde_json::from_str(&content) {
        Ok(r) => r,
        Err(_) => {
            let _ = fs::remove_file(&path);
            return;
        }
    };

    // If the Jean instance that spawned the backend is still alive, leave it alone.
    if crate::platform::is_process_alive(record.jean_pid) {
        log::debug!(
            "[AI PROXY CLEANUP] PID file exists but Jean PID {} is still alive — another instance owns the backend",
            record.jean_pid
        );
        return;
    }

    // Jean is dead. Check if the backend is still running AND healthy on our port
    // (health check guards against PID recycling — an unrelated process won't respond).
    if crate::platform::is_process_alive(record.server_pid) && is_healthy(record.port) {
        log::info!(
            "[AI PROXY CLEANUP] Killing orphaned backend (PID {}) from crashed Jean (PID {})",
            record.server_pid,
            record.jean_pid
        );
        let _ = crate::platform::kill_process_tree(record.server_pid);
        std::thread::sleep(Duration::from_millis(300));
        // Verify kill succeeded
        if is_healthy(record.port) {
            log::warn!(
                "[AI PROXY CLEANUP] Backend still healthy after tree kill, trying direct kill"
            );
            let _ = crate::platform::kill_process(record.server_pid);
        }
    } else {
        log::debug!(
            "[AI PROXY CLEANUP] Stale PID file (backend PID {} not alive or not healthy), cleaning up",
            record.server_pid
        );
    }

    let _ = fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// Start / Stop
// ---------------------------------------------------------------------------

/// Start the CLIProxyAPIPlus backend on the given port with the given strategy.
///
/// Returns the port the backend is listening on.
pub fn start_backend(app: &AppHandle, port: u16, strategy: &str) -> Result<u16, String> {
    let _ = APP_HANDLE.set(app.clone());

    // If backend is already running + healthy, return early.
    {
        let mut guard = BACKEND_PROCESS
            .lock()
            .map_err(|e| format!("Backend lock error: {e}"))?;

        if let Some(proc) = guard.as_mut() {
            match proc.child.try_wait() {
                Ok(None) if wait_until_healthy(proc.port, 5) => {
                    return Ok(proc.port);
                }
                _ => {
                    *guard = None;
                }
            }
        }
    }

    let binary_path = get_backend_binary_path(app)?;
    if !binary_path.exists() {
        return Err(format!(
            "AI proxy backend binary not found at {}",
            binary_path.display()
        ));
    }

    // Write config.yaml for the backend to read.
    let auth_dir = crate::ai_proxy::config::get_auth_dir()?;
    write_config_yaml(port, strategy)?;

    let config_path = auth_dir.join("config.yaml");

    let mut cmd = silent_command(&binary_path);
    cmd.arg("--config")
        .arg(&config_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to start AI proxy backend: {e}"))?;

    let server_pid = child.id();

    {
        let mut guard = BACKEND_PROCESS
            .lock()
            .map_err(|e| format!("Backend lock error: {e}"))?;
        *guard = Some(BackendProcess { child, port });
    }

    write_pid_file(server_pid, port);

    if !wait_until_healthy(port, 50) {
        return Err("AI proxy backend started but did not become healthy in time".to_string());
    }

    Ok(port)
}

fn stop_backend_inner() -> Result<bool, String> {
    let mut guard = BACKEND_PROCESS
        .lock()
        .map_err(|e| format!("Backend lock error: {e}"))?;

    let Some(proc) = guard.as_mut() else {
        return Ok(false);
    };

    let pid = proc.child.id();
    let _ = crate::platform::kill_process_tree(pid);
    // Fallback direct child kill in case tree-kill is unsupported/fails.
    let _ = proc.child.kill();
    let _ = proc.child.wait();
    *guard = None;
    remove_pid_file();
    Ok(true)
}

/// Stop the managed backend process.
pub fn stop_backend() -> Result<bool, String> {
    stop_backend_inner()
}

/// Shutdown the backend (called at app exit).
pub fn shutdown_backend() -> Result<bool, String> {
    stop_backend_inner()
}

// ---------------------------------------------------------------------------
// OAuth login trigger
// ---------------------------------------------------------------------------

/// Returns providers currently in an active OAuth login flow.
pub fn get_active_logins() -> Vec<String> {
    ACTIVE_LOGINS
        .lock()
        .map(|guard| guard.keys().cloned().collect())
        .unwrap_or_default()
}

/// Manage the login process lifecycle: provider-specific stdin, timeout, and
/// event emission when the process completes.
fn handle_login_process(
    app: AppHandle,
    provider: String,
    qwen_email: Option<String>,
    mut child: Child,
) {
    // Phase 0: Spawn stdout/stderr readers to capture device codes and other output
    let emit_output = |app: &AppHandle, provider: &str, line: &str| {
        let _ = app.emit(
            "ai-proxy:login-output",
            &LoginOutputEvent {
                provider: provider.to_string(),
                line: line.to_string(),
            },
        );
    };

    if let Some(stdout) = child.stdout.take() {
        let app_r = app.clone();
        let prov_r = provider.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    emit_output(&app_r, &prov_r, trimmed);
                }
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let app_r = app.clone();
        let prov_r = provider.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    emit_output(&app_r, &prov_r, trimmed);
                }
            }
        });
    }

    // Phase 1: Provider-specific stdin handling
    match provider.as_str() {
        "gemini" => {
            std::thread::sleep(Duration::from_secs(3));
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(b"\n");
            }
        }
        "codex" => {
            std::thread::sleep(Duration::from_secs(12));
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(b"\n");
            }
        }
        "qwen" => {
            // Qwen flow requires submitting the account email after OAuth.
            if let Some(email) = qwen_email {
                std::thread::sleep(Duration::from_secs(10));
                if let Some(ref mut stdin) = child.stdin {
                    let _ = stdin.write_all(format!("{email}\n").as_bytes());
                }
            }
        }
        _ => {}
    }

    // Phase 2: Wait for process exit with 5-minute timeout
    let child_id = child.id();
    let timed_out = Arc::new(AtomicBool::new(false));
    let timed_out_watchdog = timed_out.clone();
    let finished = Arc::new(AtomicBool::new(false));
    let finished_watchdog = finished.clone();

    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(300));
        if finished_watchdog.load(Ordering::SeqCst) {
            return;
        }
        timed_out_watchdog.store(true, Ordering::SeqCst);
        let _ = crate::platform::kill_process(child_id);
    });

    let exit_status = child.wait();
    finished.store(true, Ordering::SeqCst);

    // Phase 3: Cleanup and notify
    if let Ok(mut guard) = ACTIVE_LOGINS.lock() {
        guard.remove(&provider);
    }

    let status = if timed_out.load(Ordering::SeqCst) {
        "timed_out"
    } else if exit_status.as_ref().map(|s| s.success()).unwrap_or(false) {
        "completed"
    } else {
        "failed"
    };

    if status == "failed" {
        let detail = match exit_status {
            Ok(code) => format!("Login process exited unsuccessfully: {code}"),
            Err(e) => format!("Failed waiting for login process exit: {e}"),
        };
        emit_output(&app, &provider, &detail);
    }

    let event = LoginStatusEvent {
        provider: provider.clone(),
        status: status.to_string(),
    };
    let _ = app.emit("ai-proxy:login-status", &event);
}

/// Trigger OAuth login for a provider by spawning the backend binary with the
/// provider-specific login flag. Uses `Command::new` (not `silent_command`)
/// because this opens a browser.
pub fn trigger_login(
    app: &AppHandle,
    provider: &str,
    qwen_email: Option<String>,
) -> Result<(), String> {
    let flag =
        login_flag_for_provider(provider).ok_or_else(|| format!("Unknown provider: {provider}"))?;

    if flag.is_empty() {
        return Err(format!("{provider} uses API keys, not OAuth"));
    }

    // Prevent double-login for the same provider
    {
        let guard = ACTIVE_LOGINS
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?;
        if guard.contains_key(provider) {
            return Err(format!("Login already in progress for {provider}"));
        }
    }

    let binary_path = get_backend_binary_path(app)?;
    if !binary_path.exists() {
        return Err(format!(
            "AI proxy backend binary not found at {}",
            binary_path.display()
        ));
    }

    let qwen_email = qwen_email
        .map(|email| email.trim().to_string())
        .filter(|email| !email.is_empty());

    if provider == "qwen" && qwen_email.is_none() {
        return Err("Qwen login requires an email address".to_string());
    }

    // Always refresh config.yaml before login in case external tools modified it.
    let prefs = crate::load_preferences_sync(app)?;
    let config_path = write_config_yaml(
        prefs.ai_proxy_backend_port,
        &prefs.ai_proxy_rotation_strategy,
    )?;

    let needs_stdin = matches!(provider, "gemini" | "codex" | "qwen");

    let mut cmd = std::process::Command::new(&binary_path);
    cmd.arg("--config").arg(&config_path).arg(flag);

    if needs_stdin {
        cmd.stdin(Stdio::piped());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to trigger login for {provider}: {e}"))?;

    // Track the active login
    {
        let mut guard = ACTIVE_LOGINS
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?;
        guard.insert(provider.to_string(), child.id());
    }

    // Emit "started" event
    let _ = app.emit(
        "ai-proxy:login-status",
        &LoginStatusEvent {
            provider: provider.to_string(),
            status: "started".to_string(),
        },
    );

    // Background thread manages stdin, timeout, and completion notification
    let provider_owned = provider.to_string();
    let qwen_email_owned = qwen_email;
    let app_clone = app.clone();
    std::thread::spawn(move || {
        handle_login_process(app_clone, provider_owned, qwen_email_owned, child);
    });

    Ok(())
}
