use once_cell::sync::Lazy;
use serde_json::Value;
use std::io::Read as _;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::AppHandle;
use tokio::sync::Mutex;

use super::types::*;
use super::{backend, config, proxy_server};

#[derive(Debug, Clone, serde::Serialize)]
pub struct AiProxyInstallProgress {
    pub stage: String,
    pub message: String,
    pub percent: u8,
}

const GITHUB_RELEASES_URL: &str =
    "https://api.github.com/repos/router-for-me/CLIProxyAPIPlus/releases/latest";

/// Holds the real proxy server handle whose `shutdown_tx` keeps the server alive.
/// Dropping this sender triggers graceful shutdown.
static PROXY_HANDLE: Lazy<Mutex<Option<proxy_server::ProxyServerHandle>>> =
    Lazy::new(|| Mutex::new(None));
/// Baseline snapshot used to emulate "reset" semantics for backend management usage.
static MANAGEMENT_USAGE_BASELINE: Lazy<Mutex<Option<UsageStats>>> = Lazy::new(|| Mutex::new(None));

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Quick health check against a local HTTP port.
fn port_is_healthy(port: u16) -> bool {
    reqwest::blocking::Client::new()
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .timeout(Duration::from_millis(800))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Save preferences to disk without the full validation/migration pipeline
/// (which lives in the private `save_preferences` tauri command in lib.rs).
fn save_preferences_sync(app: &AppHandle, prefs: &crate::AppPreferences) -> Result<(), String> {
    let path = crate::get_preferences_path(app)?;
    let json = serde_json::to_string_pretty(prefs)
        .map_err(|e| format!("Failed to serialize preferences: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("Failed to write preferences: {e}"))?;
    Ok(())
}

fn normalize_percent(value: f64) -> Option<f64> {
    if !value.is_finite() {
        return None;
    }

    let normalized = if value <= 1.0 { value * 100.0 } else { value };
    Some(normalized.clamp(0.0, 100.0))
}

fn normalize_usage_window(used: Option<f64>, left: Option<f64>) -> (Option<f64>, Option<f64>) {
    let used = used.and_then(normalize_percent);
    let left = left.and_then(normalize_percent);

    match (used, left) {
        (Some(used), Some(left)) => (Some(used), Some(left)),
        (Some(used), None) => (Some(used), Some((100.0 - used).clamp(0.0, 100.0))),
        (None, Some(left)) => (Some((100.0 - left).clamp(0.0, 100.0)), Some(left)),
        (None, None) => (None, None),
    }
}

fn parse_provider_model_id(raw: &str) -> Option<(String, String)> {
    let (provider, model) = raw.split_once('/')?;
    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some((provider.to_string(), model.to_string()))
}

fn pick_str(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    })
}

fn parse_proxy_model_entry(entry: &Value) -> Option<ProxyModelInfo> {
    if let Some(raw) = entry.as_str() {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if let Some((provider, model)) = parse_provider_model_id(raw) {
            return Some(ProxyModelInfo {
                id: format!("{provider}/{model}"),
                provider,
                model,
            });
        }
        return Some(ProxyModelInfo {
            id: raw.to_string(),
            provider: "unknown".to_string(),
            model: raw.to_string(),
        });
    }

    let id_candidate = pick_str(entry, &["id", "model", "name"]);
    let provider_candidate = pick_str(
        entry,
        &[
            "provider",
            "provider_id",
            "providerID",
            "owned_by",
            "ownedBy",
        ],
    );
    let mut model_candidate = pick_str(entry, &["model", "model_id", "modelID"]);

    let provider_from_id = id_candidate
        .as_deref()
        .and_then(parse_provider_model_id)
        .map(|(provider, model)| {
            if model_candidate.is_none() {
                model_candidate = Some(model.clone());
            }
            provider
        });

    if model_candidate.is_none() {
        model_candidate = id_candidate.clone();
    }

    let model = model_candidate?;
    let provider = provider_candidate
        .or(provider_from_id)
        .unwrap_or_else(|| "unknown".to_string());
    let id = if provider == "unknown" {
        model.clone()
    } else {
        format!("{provider}/{model}")
    };

    Some(ProxyModelInfo {
        provider,
        model,
        id,
    })
}

fn parse_proxy_models_payload(payload: &Value) -> Vec<ProxyModelInfo> {
    let entries = payload
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| payload.get("models").and_then(Value::as_array))
        .or_else(|| payload.as_array())
        .cloned()
        .unwrap_or_default();

    let mut models: Vec<ProxyModelInfo> =
        entries.iter().filter_map(parse_proxy_model_entry).collect();

    models.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then(a.model.cmp(&b.model))
            .then(a.id.cmp(&b.id))
    });
    models.dedup_by(|a, b| a.provider == b.provider && a.model == b.model);
    models
}

fn management_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::HeaderName::from_static("x-management-key"),
        reqwest::header::HeaderValue::from_static(config::MANAGEMENT_KEY),
    );
    headers
}

fn read_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64().or_else(|| n.as_i64().and_then(|v| u64::try_from(v).ok())),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
}

fn infer_provider_from_model(model: &str) -> String {
    if let Some((provider, _)) = parse_provider_model_id(model) {
        return provider;
    }

    let lower = model.to_ascii_lowercase();
    if lower.starts_with("claude") {
        "claude".to_string()
    } else if lower.starts_with("gpt")
        || lower.starts_with("o1")
        || lower.starts_with("o3")
        || lower.starts_with("o4")
        || lower.starts_with("oai")
    {
        "openai".to_string()
    } else if lower.starts_with("gemini") {
        "gemini".to_string()
    } else if lower.starts_with("qwen") {
        "qwen".to_string()
    } else if lower.starts_with("kiro") {
        "kiro".to_string()
    } else {
        "unknown".to_string()
    }
}

fn parse_management_auth_files_payload(payload: &Value) -> std::collections::HashMap<String, String> {
    let mut mapping = std::collections::HashMap::new();
    let files = payload
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for file in files {
        let Some(auth_index) = pick_str(&file, &["auth_index", "authIndex"]) else {
            continue;
        };
        let account = pick_str(&file, &["email", "account", "label", "name", "id"])
            .unwrap_or_else(|| auth_index.clone());
        mapping.insert(auth_index, account);
    }

    mapping
}

fn parse_management_usage_payload(
    payload: &Value,
    account_by_index: &std::collections::HashMap<String, String>,
) -> Option<UsageStats> {
    let usage = payload.get("usage")?;
    let apis = usage.get("apis").and_then(Value::as_object)?;

    let mut per_model = std::collections::HashMap::<String, u64>::new();
    let mut per_provider = std::collections::HashMap::<String, u64>::new();
    let mut per_account = std::collections::HashMap::<String, u64>::new();
    let mut by_request_map =
        std::collections::HashMap::<(String, String, String), u64>::new();
    let mut derived_total = 0u64;

    for api in apis.values() {
        let Some(models) = api.get("models").and_then(Value::as_object) else {
            continue;
        };

        for (raw_model, model_snapshot) in models {
            let (provider, model) = if let Some((provider, model)) = parse_provider_model_id(raw_model)
            {
                (provider, model)
            } else {
                (infer_provider_from_model(raw_model), raw_model.to_string())
            };

            if let Some(details) = model_snapshot.get("details").and_then(Value::as_array) {
                if !details.is_empty() {
                    for detail in details {
                        let auth_index = pick_str(detail, &["auth_index", "authIndex"]);
                        let account = auth_index
                            .as_ref()
                            .and_then(|idx| account_by_index.get(idx))
                            .cloned()
                            .or_else(|| auth_index.map(|idx| format!("auth:{idx}")))
                            .unwrap_or_else(|| "unknown".to_string());

                        *per_model.entry(model.clone()).or_default() += 1;
                        *per_provider.entry(provider.clone()).or_default() += 1;
                        *per_account.entry(account.clone()).or_default() += 1;
                        *by_request_map
                            .entry((provider.clone(), model.clone(), account))
                            .or_default() += 1;
                        derived_total += 1;
                    }
                    continue;
                }
            }

            let requests = model_snapshot
                .get("total_requests")
                .and_then(read_u64)
                .unwrap_or(0);
            if requests == 0 {
                continue;
            }

            *per_model.entry(model.clone()).or_default() += requests;
            *per_provider.entry(provider.clone()).or_default() += requests;
            *per_account.entry("unknown".to_string()).or_default() += requests;
            *by_request_map
                .entry((provider.clone(), model, "unknown".to_string()))
                .or_default() += requests;
            derived_total += requests;
        }
    }

    let mut by_request = by_request_map
        .into_iter()
        .map(|((provider, model, account), requests)| UsageRequestSummary {
            provider,
            model,
            account,
            requests,
        })
        .collect::<Vec<_>>();
    by_request.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then(a.model.cmp(&b.model))
            .then(a.account.cmp(&b.account))
    });

    let total_requests = usage
        .get("total_requests")
        .and_then(read_u64)
        .filter(|v| *v > 0)
        .unwrap_or(derived_total);

    Some(UsageStats {
        total_requests,
        per_model,
        per_provider,
        per_account,
        by_request,
    })
}

fn subtract_count_maps(
    current: &std::collections::HashMap<String, u64>,
    baseline: &std::collections::HashMap<String, u64>,
) -> std::collections::HashMap<String, u64> {
    let mut out = std::collections::HashMap::new();
    for (key, value) in current {
        let base = baseline.get(key).copied().unwrap_or(0);
        let diff = value.saturating_sub(base);
        if diff > 0 {
            out.insert(key.clone(), diff);
        }
    }
    out
}

fn subtract_usage_stats(current: UsageStats, baseline: &UsageStats) -> UsageStats {
    let mut baseline_rows = std::collections::HashMap::<(String, String, String), u64>::new();
    for row in &baseline.by_request {
        *baseline_rows
            .entry((row.provider.clone(), row.model.clone(), row.account.clone()))
            .or_default() += row.requests;
    }

    let mut by_request = Vec::new();
    for row in current.by_request {
        let key = (row.provider.clone(), row.model.clone(), row.account.clone());
        let base = baseline_rows.get(&key).copied().unwrap_or(0);
        let diff = row.requests.saturating_sub(base);
        if diff > 0 {
            by_request.push(UsageRequestSummary {
                provider: row.provider,
                model: row.model,
                account: row.account,
                requests: diff,
            });
        }
    }
    by_request.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then(a.model.cmp(&b.model))
            .then(a.account.cmp(&b.account))
    });

    UsageStats {
        total_requests: current.total_requests.saturating_sub(baseline.total_requests),
        per_model: subtract_count_maps(&current.per_model, &baseline.per_model),
        per_provider: subtract_count_maps(&current.per_provider, &baseline.per_provider),
        per_account: subtract_count_maps(&current.per_account, &baseline.per_account),
        by_request,
    }
}

fn collect_backend_ports(status: &ProxyStatus, prefs: Option<&crate::AppPreferences>) -> Vec<u16> {
    let mut ports = Vec::new();
    if let Some(port) = status.backend_port {
        ports.push(port);
    }
    if let Some(prefs) = prefs {
        ports.push(prefs.ai_proxy_backend_port);
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}

async fn fetch_backend_management_usage(
    client: &reqwest::Client,
    backend_port: u16,
) -> Result<UsageStats, String> {
    let base = format!("http://127.0.0.1:{backend_port}/v0/management");
    let headers = management_headers();

    let auth_resp = client
        .get(format!("{base}/auth-files"))
        .headers(headers.clone())
        .send()
        .await
        .map_err(|e| format!("{base}/auth-files: {e}"))?;
    if !auth_resp.status().is_success() {
        return Err(format!(
            "{base}/auth-files: HTTP {}",
            auth_resp.status()
        ));
    }
    let auth_payload: Value = auth_resp
        .json()
        .await
        .map_err(|e| format!("{base}/auth-files: invalid JSON ({e})"))?;
    let account_by_index = parse_management_auth_files_payload(&auth_payload);

    let usage_resp = client
        .get(format!("{base}/usage"))
        .headers(headers)
        .send()
        .await
        .map_err(|e| format!("{base}/usage: {e}"))?;
    if !usage_resp.status().is_success() {
        return Err(format!("{base}/usage: HTTP {}", usage_resp.status()));
    }
    let usage_payload: Value = usage_resp
        .json()
        .await
        .map_err(|e| format!("{base}/usage: invalid JSON ({e})"))?;

    parse_management_usage_payload(&usage_payload, &account_by_index)
        .ok_or_else(|| format!("{base}/usage: missing usage payload"))
}

// ---------------------------------------------------------------------------
// Backend installation
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn check_ai_proxy_backend_installed(app: AppHandle) -> Result<BackendStatus, String> {
    let path = config::get_backend_binary_path(&app)?;
    let exists = path.exists();
    Ok(BackendStatus {
        installed: exists,
        path: if exists {
            Some(path.to_string_lossy().into_owned())
        } else {
            None
        },
    })
}

#[tauri::command]
pub async fn install_ai_proxy_backend(app: AppHandle) -> Result<(), String> {
    let target_dir = config::ensure_backend_dir(&app)?;
    let binary_path = config::get_backend_binary_path(&app)?;

    // Determine platform asset suffix
    let (os, ext) = if cfg!(target_os = "macos") {
        ("darwin", "tar.gz")
    } else if cfg!(target_os = "windows") {
        ("windows", "zip")
    } else {
        ("linux", "tar.gz")
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    };
    let asset_suffix = format!("{os}_{arch}.{ext}");
    emit_progress(&app, "starting", "Preparing installation...", 0);

    // Fetch latest release info from GitHub API
    let client = reqwest::Client::builder()
        .user_agent("jean-desktop")
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;
    emit_progress(&app, "fetching", "Fetching latest release info...", 10);

    let release: serde_json::Value = client
        .get(GITHUB_RELEASES_URL)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch latest release: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse release JSON: {e}"))?;

    // Find the matching asset download URL
    let assets = release["assets"].as_array().ok_or("No assets in release")?;

    let download_url = assets
        .iter()
        .find_map(|asset| {
            let name = asset["name"].as_str()?;
            if name.ends_with(&asset_suffix) {
                asset["browser_download_url"]
                    .as_str()
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| format!("No release asset found for {asset_suffix}"))?;
    emit_progress(&app, "downloading", "Downloading AI proxy backend...", 25);

    log::info!("Downloading CLIProxyAPIPlus from: {download_url}");

    // Download the archive
    let response = client
        .get(&download_url)
        .send()
        .await
        .map_err(|e| format!("Failed to download: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("Download failed: HTTP {}", response.status()));
    }

    let archive_bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read download: {e}"))?;
    emit_progress(&app, "extracting", "Extracting binary from archive...", 60);

    log::info!("Downloaded {} bytes", archive_bytes.len());

    // Extract binary from archive
    if ext == "tar.gz" {
        let decoder = flate2::read::GzDecoder::new(&archive_bytes[..]);
        let mut archive = tar::Archive::new(decoder);
        let mut found = false;

        for entry in archive
            .entries()
            .map_err(|e| format!("Failed to read tar: {e}"))?
        {
            let mut entry = entry.map_err(|e| format!("Failed to read tar entry: {e}"))?;
            let path = entry
                .path()
                .map_err(|e| format!("Failed to read entry path: {e}"))?
                .to_path_buf();

            if let Some(name) = path.file_name() {
                if name == "cli-proxy-api-plus" {
                    entry
                        .unpack(&binary_path)
                        .map_err(|e| format!("Failed to extract binary: {e}"))?;
                    found = true;
                    break;
                }
            }
        }

        if !found {
            return Err("Binary 'cli-proxy-api-plus' not found in archive".into());
        }
    } else {
        // zip (Windows)
        let cursor = std::io::Cursor::new(&archive_bytes[..]);
        let mut zip =
            zip::ZipArchive::new(cursor).map_err(|e| format!("Failed to read zip: {e}"))?;

        let mut found = false;
        for i in 0..zip.len() {
            let mut file = zip
                .by_index(i)
                .map_err(|e| format!("Failed to read zip entry: {e}"))?;

            if file.name().contains("cli-proxy-api-plus") && !file.name().ends_with('/') {
                let mut contents = Vec::new();
                file.read_to_end(&mut contents)
                    .map_err(|e| format!("Failed to read zip entry: {e}"))?;
                std::fs::write(&binary_path, &contents)
                    .map_err(|e| format!("Failed to write binary: {e}"))?;
                found = true;
                break;
            }
        }

        if !found {
            return Err("Binary 'cli-proxy-api-plus' not found in zip".into());
        }
    }

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&binary_path)
            .map_err(|e| format!("Failed to get binary metadata: {e}"))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&binary_path, perms)
            .map_err(|e| format!("Failed to set permissions: {e}"))?;
    }
    emit_progress(&app, "installing", "Setting up binary...", 85);

    // Also extract config.example.yaml if available
    let config_example_path = target_dir.join("config.example.yaml");
    if !config_example_path.exists() {
        if ext == "tar.gz" {
            let decoder = flate2::read::GzDecoder::new(&archive_bytes[..]);
            let mut archive = tar::Archive::new(decoder);
            for entry in archive.entries().into_iter().flatten() {
                if let Ok(mut entry) = entry {
                    if let Ok(path) = entry.path() {
                        if path
                            .file_name()
                            .map(|n| n == "config.example.yaml")
                            .unwrap_or(false)
                        {
                            let _ = entry.unpack(&config_example_path);
                            break;
                        }
                    }
                }
            }
        }
    }

    log::info!("CLIProxyAPIPlus installed to {}", binary_path.display());
    emit_progress(
        &app,
        "complete",
        "AI proxy backend installed successfully!",
        100,
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Proxy lifecycle
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn start_ai_proxy(app: AppHandle) -> Result<ProxyStatus, String> {
    let prefs = crate::load_preferences_sync(&app)?;

    let backend_port = prefs.ai_proxy_backend_port;
    let proxy_port = prefs.ai_proxy_port;
    let strategy = prefs.ai_proxy_rotation_strategy.clone();
    let groups = prefs.ai_proxy_model_groups.clone();

    // 1. Start the backend (blocking I/O with internal health-check loop).
    let app_ref = app.clone();
    let actual_backend_port = tokio::task::spawn_blocking(move || {
        backend::start_backend(&app_ref, backend_port, &strategy)
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))??;

    // 2. Start the proxy layer in front of the backend.
    let handle = proxy_server::start_proxy(proxy_port, actual_backend_port, groups).await?;
    let bound_port = handle.port;

    // 3. Register for status queries and keep the real handle alive.
    proxy_server::store_handle(bound_port, actual_backend_port).await;
    *PROXY_HANDLE.lock().await = Some(handle);
    *MANAGEMENT_USAGE_BASELINE.lock().await = None;

    Ok(ProxyStatus {
        running: true,
        proxy_port: Some(bound_port),
        backend_port: Some(actual_backend_port),
        backend_installed: true,
    })
}

#[tauri::command]
pub async fn stop_ai_proxy() -> Result<(), String> {
    // Send shutdown to the real proxy handle (triggers graceful shutdown).
    if let Some(handle) = PROXY_HANDLE.lock().await.take() {
        let _ = handle.shutdown_tx.send(());
    }
    // Also clear the status-tracking handle.
    proxy_server::stop_proxy();

    tokio::task::spawn_blocking(backend::stop_backend)
        .await
        .map_err(|e| format!("Task join error: {e}"))??;
    *MANAGEMENT_USAGE_BASELINE.lock().await = None;

    Ok(())
}

#[tauri::command]
pub async fn get_ai_proxy_status(app: AppHandle) -> Result<ProxyStatus, String> {
    let installed = config::get_backend_binary_path(&app)
        .map(|p| p.exists())
        .unwrap_or(false);

    // Get proxy state from the stored handle (authoritative for port info).
    let mut status = proxy_server::get_status().await;
    status.backend_installed = installed;

    // If the proxy thinks it's running, verify backend is actually healthy.
    if status.running {
        if let Some(bp) = status.backend_port {
            let healthy = tokio::task::spawn_blocking(move || port_is_healthy(bp))
                .await
                .unwrap_or(false);
            if !healthy {
                status.running = false;
                status.backend_port = None;
            }
        }
    }

    Ok(status)
}

// ---------------------------------------------------------------------------
// Auth / login
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn ai_proxy_login(
    app: AppHandle,
    provider: String,
    qwen_email: Option<String>,
) -> Result<(), String> {
    backend::trigger_login(&app, &provider, qwen_email)
}

#[tauri::command]
pub async fn get_ai_proxy_active_logins() -> Result<Vec<String>, String> {
    Ok(backend::get_active_logins())
}

#[tauri::command]
pub async fn get_ai_proxy_models(app: AppHandle) -> Result<Vec<ProxyModelInfo>, String> {
    let status = proxy_server::get_status().await;
    let prefs = crate::load_preferences_sync(&app).ok();

    let mut ports: Vec<u16> = Vec::new();
    if let Some(port) = status.proxy_port {
        ports.push(port);
    }
    if let Some(port) = status.backend_port {
        ports.push(port);
    }
    if let Some(prefs) = prefs {
        ports.push(prefs.ai_proxy_port);
        ports.push(prefs.ai_proxy_backend_port);
    }
    ports.sort_unstable();
    ports.dedup();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let mut last_error: Option<String> = None;
    for port in ports {
        let url = format!("http://127.0.0.1:{port}/v1/models");
        let response = match client.get(&url).send().await {
            Ok(resp) => resp,
            Err(e) => {
                last_error = Some(format!("{url}: {e}"));
                continue;
            }
        };

        if !response.status().is_success() {
            last_error = Some(format!("{url}: HTTP {}", response.status()));
            continue;
        }

        let payload: Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                last_error = Some(format!("{url}: invalid JSON ({e})"));
                continue;
            }
        };

        return Ok(parse_proxy_models_payload(&payload));
    }

    Err(format!(
        "Could not fetch /v1/models from local AI proxy/backend{}",
        last_error.map(|e| format!(" ({e})")).unwrap_or_default()
    ))
}

// ---------------------------------------------------------------------------
// Account management
// ---------------------------------------------------------------------------

/// Parse an RFC3339/ISO8601 timestamp into epoch seconds (UTC).
///
/// Handles `2025-03-10T12:34:56Z`, `2025-03-10T12:34:56.123Z`,
/// and `2025-03-10T12:34:56+05:30`.
fn parse_rfc3339_epoch(s: &str) -> Option<i64> {
    let b = s.trim().as_bytes();
    if b.len() < 19 {
        return None;
    }

    let int = |from: usize, to: usize| -> Option<i64> {
        std::str::from_utf8(b.get(from..to)?).ok()?.parse().ok()
    };

    let year = int(0, 4)?;
    let month = int(5, 7)?;
    let day = int(8, 10)?;
    let hour = int(11, 13)?;
    let min = int(14, 16)?;
    let sec = int(17, 19)?;

    const DAYS_BEFORE: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let m = (month - 1) as usize;
    if m >= 12 {
        return None;
    }

    let leap_years = (year - 1) / 4 - (year - 1) / 100 + (year - 1) / 400 - 477;
    let is_leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let leap_adj = if month > 2 && is_leap { 1 } else { 0 };
    let days = 365 * (year - 1970) + leap_years + DAYS_BEFORE[m] + (day - 1) + leap_adj;
    let epoch = days * 86400 + hour * 3600 + min * 60 + sec;

    // Parse timezone offset from the portion after seconds.
    let rest = std::str::from_utf8(&b[19..]).unwrap_or("");
    // Skip optional fractional seconds (e.g., ".123456").
    let tz = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());

    let offset = if tz.is_empty() || tz.starts_with('Z') || tz.starts_with('z') {
        0i64
    } else if tz.len() >= 5 && (tz.starts_with('+') || tz.starts_with('-')) {
        let sign: i64 = if tz.starts_with('-') { -1 } else { 1 };
        let h: i64 = tz.get(1..3)?.parse().ok()?;
        let m: i64 = tz.get(4..6)?.parse().ok()?;
        sign * (h * 3600 + m * 60)
    } else {
        0
    };

    Some(epoch - offset)
}

/// Returns true if the RFC3339 timestamp is in the past. False on parse failure.
fn is_expired_rfc3339(s: &str) -> bool {
    let Some(epoch) = parse_rfc3339_epoch(s) else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    epoch < now
}

/// Scan a directory for `*.json` account files and return parsed accounts.
fn scan_account_dir(dir: &PathBuf, enabled: bool) -> Vec<ProviderAccount> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .map(|ext| ext == "json")
                .unwrap_or(false)
        })
        .filter_map(|entry| {
            let path = entry.path();
            let account_id = path.file_stem()?.to_string_lossy().into_owned();
            let content = std::fs::read_to_string(&path).ok()?;
            let json: serde_json::Value = serde_json::from_str(&content).ok()?;

            let provider = json
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let display_name = json
                .get("email")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| json.get("login").and_then(|v| v.as_str()))
                .unwrap_or(&account_id)
                .to_string();

            let expired = json
                .get("expired")
                .and_then(|v| v.as_str())
                .map(is_expired_rfc3339)
                .unwrap_or(false);

            Some(ProviderAccount {
                provider,
                account_id,
                display_name,
                enabled,
                expired,
            })
        })
        .collect()
}

#[tauri::command]
pub async fn get_ai_proxy_accounts() -> Result<Vec<ProviderAccount>, String> {
    tokio::task::spawn_blocking(|| {
        let auth_dir = config::get_auth_dir()?;
        let disabled_dir = auth_dir.join(".disabled");
        let auto_disabled_dir = disabled_dir.join(".auto");

        let mut accounts = scan_account_dir(&auth_dir, true);
        accounts.extend(scan_account_dir(&disabled_dir, false));
        accounts.extend(scan_account_dir(&auto_disabled_dir, false));

        accounts.sort_by(|a, b| {
            a.provider
                .cmp(&b.provider)
                .then(a.display_name.cmp(&b.display_name))
        });

        Ok(accounts)
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

#[tauri::command]
pub async fn disable_ai_proxy_account(account_id: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let auth_dir = config::get_auth_dir()?;
        let src = auth_dir.join(format!("{account_id}.json"));
        if !src.exists() {
            return Err(format!("Account file not found: {account_id}"));
        }

        let disabled_dir = auth_dir.join(".disabled");
        std::fs::create_dir_all(&disabled_dir)
            .map_err(|e| format!("Failed to create .disabled directory: {e}"))?;

        let dst = disabled_dir.join(format!("{account_id}.json"));
        std::fs::rename(&src, &dst)
            .map_err(|e| format!("Failed to disable account {account_id}: {e}"))
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

#[tauri::command]
pub async fn enable_ai_proxy_account(account_id: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let auth_dir = config::get_auth_dir()?;
        let filename = format!("{account_id}.json");

        let disabled_dir = auth_dir.join(".disabled");
        let auto_dir = disabled_dir.join(".auto");

        let src = if disabled_dir.join(&filename).exists() {
            disabled_dir.join(&filename)
        } else if auto_dir.join(&filename).exists() {
            auto_dir.join(&filename)
        } else {
            return Err(format!("Disabled account file not found: {account_id}"));
        };

        let dst = auth_dir.join(&filename);
        std::fs::rename(&src, &dst)
            .map_err(|e| format!("Failed to enable account {account_id}: {e}"))
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

#[tauri::command]
pub async fn delete_ai_proxy_account(account_id: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let auth_dir = config::get_auth_dir()?;
        let filename = format!("{account_id}.json");
        let disabled_dir = auth_dir.join(".disabled");
        let auto_dir = disabled_dir.join(".auto");

        let mut deleted_any = false;
        for path in [
            auth_dir.join(&filename),
            disabled_dir.join(&filename),
            auto_dir.join(&filename),
        ] {
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("Failed to delete account {account_id}: {e}"))?;
                deleted_any = true;
            }
        }

        if !deleted_any {
            return Err(format!("Account file not found: {account_id}"));
        }

        Ok(())
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

// ---------------------------------------------------------------------------
// Z.AI API key (no OAuth — user pastes key directly)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn save_zai_api_key(api_key: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let auth_dir = config::get_auth_dir()?;
        std::fs::create_dir_all(&auth_dir)
            .map_err(|e| format!("Failed to create auth directory: {e}"))?;

        // Generate a short unique filename: zai-<8-char-uuid>.json
        let uuid_short = &uuid::Uuid::new_v4().to_string()[..8];
        let filename = format!("zai-{uuid_short}.json");

        // Mask key for display: first 8 chars...last 4 chars
        let display = if api_key.len() > 12 {
            format!("{}...{}", &api_key[..8], &api_key[api_key.len() - 4..])
        } else {
            api_key.clone()
        };

        let json = serde_json::json!({
            "type": "zai",
            "email": display,
            "api_key": api_key,
        });

        let path = auth_dir.join(&filename);
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json).unwrap_or_default(),
        )
        .map_err(|e| format!("Failed to write Z.AI key file: {e}"))?;

        // Set restrictive permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)
                .map_err(|e| format!("Failed to get file metadata: {e}"))?
                .permissions();
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(&path, perms);
        }

        Ok(())
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

// ---------------------------------------------------------------------------
// Open auth folder
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn open_ai_proxy_auth_folder() -> Result<(), String> {
    let auth_dir = config::get_auth_dir()?;
    std::fs::create_dir_all(&auth_dir)
        .map_err(|e| format!("Failed to create auth directory: {e}"))?;

    let path_str = auth_dir
        .to_str()
        .ok_or_else(|| "Auth dir path contains invalid UTF-8".to_string())?
        .to_string();

    tokio::task::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open")
                .arg(&path_str)
                .spawn()
                .map_err(|e| format!("Failed to open folder: {e}"))?;
        }
        #[cfg(target_os = "windows")]
        {
            std::process::Command::new("explorer")
                .arg(&path_str)
                .spawn()
                .map_err(|e| format!("Failed to open folder: {e}"))?;
        }
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("xdg-open")
                .arg(&path_str)
                .spawn()
                .map_err(|e| format!("Failed to open folder: {e}"))?;
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

// ---------------------------------------------------------------------------
// Per-account usage tracking
// ---------------------------------------------------------------------------

/// Read the full account JSON from an account file.
fn read_account_json(auth_dir: &PathBuf, account_id: &str) -> Option<serde_json::Value> {
    let path = auth_dir.join(format!("{account_id}.json"));
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Fetch Claude usage from the Anthropic API.
async fn fetch_claude_usage(account_id: &str, token: &str) -> AccountUsage {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .timeout(Duration::from_secs(10))
        .send()
        .await;

    let resp = match resp {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let status = r.status().as_u16();
            return AccountUsage {
                account_id: account_id.to_string(),
                provider: "claude".to_string(),
                primary_used_percent: None,
                primary_left_percent: None,
                primary_reset_seconds: None,
                secondary_used_percent: None,
                secondary_left_percent: None,
                secondary_reset_seconds: None,
                plan_type: None,
                status: if status == 401 || status == 403 {
                    "invalid_credentials".to_string()
                } else {
                    "error".to_string()
                },
            };
        }
        Err(_) => {
            return AccountUsage {
                account_id: account_id.to_string(),
                provider: "claude".to_string(),
                primary_used_percent: None,
                primary_left_percent: None,
                primary_reset_seconds: None,
                secondary_used_percent: None,
                secondary_left_percent: None,
                secondary_reset_seconds: None,
                plan_type: None,
                status: "error".to_string(),
            };
        }
    };

    let json: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => {
            return AccountUsage {
                account_id: account_id.to_string(),
                provider: "claude".to_string(),
                primary_used_percent: None,
                primary_left_percent: None,
                primary_reset_seconds: None,
                secondary_used_percent: None,
                secondary_left_percent: None,
                secondary_reset_seconds: None,
                plan_type: None,
                status: "error".to_string(),
            };
        }
    };

    // Parse Claude usage response.
    // Claude OAuth usage API returns:
    //   { "five_hour": { "utilization": 35.0, "resets_at": "..." },
    //     "seven_day": { "utilization": 14.0, "resets_at": "..." },
    //     "extra_usage": { "is_enabled": true, "used_credits": 10.5, "monthly_limit": 100.0 } }
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let primary_used_raw = json
        .pointer("/five_hour/utilization")
        .and_then(|v| v.as_f64());
    let primary_resets_at = json
        .pointer("/five_hour/resets_at")
        .and_then(|v| v.as_str())
        .and_then(|s| parse_rfc3339_epoch(s))
        .map(|epoch| (epoch - now_epoch).max(0));

    let secondary_used_raw = json
        .pointer("/seven_day/utilization")
        .and_then(|v| v.as_f64());
    let secondary_resets_at = json
        .pointer("/seven_day/resets_at")
        .and_then(|v| v.as_str())
        .and_then(|s| parse_rfc3339_epoch(s))
        .map(|epoch| (epoch - now_epoch).max(0));

    // utilization is already 0-100, no left_percent needed
    let (primary_used, primary_left) = normalize_usage_window(primary_used_raw, None);
    let (secondary_used, secondary_left) =
        normalize_usage_window(secondary_used_raw, None);

    log::debug!(
        "claude usage [{account_id}] raw: five_hour.utilization={primary_used_raw:?} seven_day.utilization={secondary_used_raw:?} → normalized: primary_used={primary_used:?} secondary_used={secondary_used:?}"
    );

    let plan_type = json
        .get("plan_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    AccountUsage {
        account_id: account_id.to_string(),
        provider: "claude".to_string(),
        primary_used_percent: primary_used,
        primary_left_percent: primary_left,
        primary_reset_seconds: primary_resets_at,
        secondary_used_percent: secondary_used,
        secondary_left_percent: secondary_left,
        secondary_reset_seconds: secondary_resets_at,
        plan_type,
        status: "loaded".to_string(),
    }
}

/// Fetch Codex/ChatGPT usage.
async fn fetch_codex_usage(account_id: &str, token: &str) -> AccountUsage {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://chatgpt.com/backend-api/wham/usage")
        .header("Authorization", format!("Bearer {token}"))
        .timeout(Duration::from_secs(10))
        .send()
        .await;

    let resp = match resp {
        Ok(r) if r.status().is_success() => r,
        _ => {
            return AccountUsage {
                account_id: account_id.to_string(),
                provider: "codex".to_string(),
                primary_used_percent: None,
                primary_left_percent: None,
                primary_reset_seconds: None,
                secondary_used_percent: None,
                secondary_left_percent: None,
                secondary_reset_seconds: None,
                plan_type: None,
                status: "error".to_string(),
            };
        }
    };

    let json: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => {
            return AccountUsage {
                account_id: account_id.to_string(),
                provider: "codex".to_string(),
                primary_used_percent: None,
                primary_left_percent: None,
                primary_reset_seconds: None,
                secondary_used_percent: None,
                secondary_left_percent: None,
                secondary_reset_seconds: None,
                plan_type: None,
                status: "error".to_string(),
            };
        }
    };

    // Try nested format first, then array format
    let (primary_used_raw, primary_left_raw, primary_reset) =
        if let Some(pw) = json.pointer("/rate_limit/primary_window") {
            (
                pw.get("used_percent").and_then(|v| v.as_f64()),
                pw.get("left_percent")
                    .and_then(|v| v.as_f64())
                    .or_else(|| pw.get("remaining_percent").and_then(|v| v.as_f64())),
                pw.get("reset_after_seconds").and_then(|v| v.as_i64()),
            )
        } else if let Some(limits) = json.get("rate_limits").and_then(|v| v.as_array()) {
            let pw = limits
                .iter()
                .find(|l| l.get("window_type").and_then(|v| v.as_str()) == Some("primary"));
            (
                pw.and_then(|l| l.get("used_percent").and_then(|v| v.as_f64())),
                pw.and_then(|l| {
                    l.get("left_percent")
                        .and_then(|v| v.as_f64())
                        .or_else(|| l.get("remaining_percent").and_then(|v| v.as_f64()))
                }),
                pw.and_then(|l| l.get("reset_after_seconds").and_then(|v| v.as_i64())),
            )
        } else {
            (None, None, None)
        };

    let (secondary_used_raw, secondary_left_raw, secondary_reset) =
        if let Some(sw) = json.pointer("/rate_limit/secondary_window") {
            (
                sw.get("used_percent").and_then(|v| v.as_f64()),
                sw.get("left_percent")
                    .and_then(|v| v.as_f64())
                    .or_else(|| sw.get("remaining_percent").and_then(|v| v.as_f64())),
                sw.get("reset_after_seconds").and_then(|v| v.as_i64()),
            )
        } else if let Some(limits) = json.get("rate_limits").and_then(|v| v.as_array()) {
            let sw = limits
                .iter()
                .find(|l| l.get("window_type").and_then(|v| v.as_str()) == Some("secondary"));
            (
                sw.and_then(|l| l.get("used_percent").and_then(|v| v.as_f64())),
                sw.and_then(|l| {
                    l.get("left_percent")
                        .and_then(|v| v.as_f64())
                        .or_else(|| l.get("remaining_percent").and_then(|v| v.as_f64()))
                }),
                sw.and_then(|l| l.get("reset_after_seconds").and_then(|v| v.as_i64())),
            )
        } else {
            (None, None, None)
        };
    let (primary_used, primary_left) = normalize_usage_window(primary_used_raw, primary_left_raw);
    let (secondary_used, secondary_left) =
        normalize_usage_window(secondary_used_raw, secondary_left_raw);

    log::debug!(
        "codex usage [{account_id}] raw: primary_used={primary_used_raw:?} primary_left={primary_left_raw:?} secondary_used={secondary_used_raw:?} secondary_left={secondary_left_raw:?} → normalized: primary_used={primary_used:?} secondary_used={secondary_used:?}"
    );

    let plan_type = json
        .get("plan_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    AccountUsage {
        account_id: account_id.to_string(),
        provider: "codex".to_string(),
        primary_used_percent: primary_used,
        primary_left_percent: primary_left,
        primary_reset_seconds: primary_reset,
        secondary_used_percent: secondary_used,
        secondary_left_percent: secondary_left,
        secondary_reset_seconds: secondary_reset,
        plan_type,
        status: "loaded".to_string(),
    }
}

/// Known paths to the Gemini CLI oauth2.js file containing client credentials.
const GEMINI_OAUTH2_JS_PATHS: &[&str] = &[
    ".bun/install/global/node_modules/@google/gemini-cli-core/dist/src/code_assist/oauth2.js",
    ".npm-global/lib/node_modules/@google/gemini-cli-core/dist/src/code_assist/oauth2.js",
    ".nvm/versions/node/current/lib/node_modules/@google/gemini-cli-core/dist/src/code_assist/oauth2.js",
];

/// Parse OAUTH_CLIENT_ID and OAUTH_CLIENT_SECRET from gemini-cli-core oauth2.js.
fn parse_gemini_client_creds(text: &str) -> Option<(String, String)> {
    let id_re = regex::Regex::new(r#"OAUTH_CLIENT_ID\s*=\s*['"]([^'"]+)['"]"#).ok()?;
    let secret_re = regex::Regex::new(r#"OAUTH_CLIENT_SECRET\s*=\s*['"]([^'"]+)['"]"#).ok()?;
    let id = id_re.captures(text)?.get(1)?.as_str().to_string();
    let secret = secret_re.captures(text)?.get(1)?.as_str().to_string();
    Some((id, secret))
}

/// Try to load Gemini OAuth client creds from known installation paths.
fn load_gemini_client_creds() -> Option<(String, String)> {
    let home = dirs::home_dir()?;
    for rel_path in GEMINI_OAUTH2_JS_PATHS {
        let path = home.join(rel_path);
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Some(creds) = parse_gemini_client_creds(&content) {
                return Some(creds);
            }
        }
    }
    None
}

/// Refresh a Gemini OAuth token using the refresh_token grant.
async fn refresh_gemini_token(
    client: &reqwest::Client,
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> Option<String> {
    let resp = client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let json: Value = resp.json().await.ok()?;
    json.get("access_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Recursively collect quota buckets with `remainingFraction` from Gemini quota response.
fn collect_gemini_quota_buckets(value: &Value, out: &mut Vec<(String, f64, Option<String>)>) {
    match value {
        Value::Array(arr) => {
            for item in arr {
                collect_gemini_quota_buckets(item, out);
            }
        }
        Value::Object(map) => {
            if let Some(remaining) = map.get("remainingFraction").and_then(|v| v.as_f64()) {
                let model_id = map
                    .get("modelId")
                    .or_else(|| map.get("model_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let reset_time = map
                    .get("resetTime")
                    .or_else(|| map.get("reset_time"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                out.push((model_id, remaining, reset_time));
            }
            for val in map.values() {
                collect_gemini_quota_buckets(val, out);
            }
        }
        _ => {}
    }
}

/// Fetch Gemini usage from the Google Cloud Code API.
async fn fetch_gemini_usage(account_id: &str, account_json: Value) -> AccountUsage {
    let make_error = |status: &str| AccountUsage {
        account_id: account_id.to_string(),
        provider: "gemini".to_string(),
        primary_used_percent: None,
        primary_left_percent: None,
        primary_reset_seconds: None,
        secondary_used_percent: None,
        secondary_left_percent: None,
        secondary_reset_seconds: None,
        plan_type: None,
        status: status.to_string(),
    };

    // Get access token, try refresh if we have refresh_token
    let mut access_token = account_json
        .get("access_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let refresh_token = account_json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let client = reqwest::Client::new();

    // Try token refresh if we have client creds and refresh token
    if let Some(ref rt) = refresh_token {
        if let Some((client_id, client_secret)) = load_gemini_client_creds() {
            if let Some(new_token) =
                refresh_gemini_token(&client, rt, &client_id, &client_secret).await
            {
                access_token = Some(new_token);
            }
        }
    }

    let Some(token) = access_token else {
        return make_error("invalid_credentials");
    };

    // Fetch quota
    let resp = client
        .post("https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota")
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .body("{}")
        .timeout(Duration::from_secs(10))
        .send()
        .await;

    let resp = match resp {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let status = r.status().as_u16();
            return make_error(if status == 401 || status == 403 {
                "invalid_credentials"
            } else {
                "error"
            });
        }
        Err(_) => return make_error("error"),
    };

    let json: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return make_error("error"),
    };

    // Collect all quota buckets recursively
    let mut buckets = Vec::new();
    collect_gemini_quota_buckets(&json, &mut buckets);

    if buckets.is_empty() {
        log::debug!("gemini usage [{account_id}] no quota buckets found in response");
        return make_error("error");
    }

    // Separate Pro and Flash buckets
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let mut pro_best: Option<(f64, Option<i64>)> = None;
    let mut flash_best: Option<(f64, Option<i64>)> = None;

    for (model_id, remaining, reset_time) in &buckets {
        let lower = model_id.to_lowercase();
        let used = ((1.0 - remaining) * 100.0).clamp(0.0, 100.0);
        let reset_secs = reset_time
            .as_deref()
            .and_then(parse_rfc3339_epoch)
            .map(|epoch| (epoch - now_epoch).max(0));

        if lower.contains("gemini") && lower.contains("pro") {
            if pro_best.is_none() || used > pro_best.unwrap().0 {
                pro_best = Some((used, reset_secs));
            }
        } else if lower.contains("gemini") && lower.contains("flash") {
            if flash_best.is_none() || used > flash_best.unwrap().0 {
                flash_best = Some((used, reset_secs));
            }
        }
    }

    let (primary_used, primary_left) = normalize_usage_window(pro_best.map(|(u, _)| u), None);
    let primary_reset = pro_best.and_then(|(_, r)| r);

    let (secondary_used, secondary_left) =
        normalize_usage_window(flash_best.map(|(u, _)| u), None);
    let secondary_reset = flash_best.and_then(|(_, r)| r);

    log::debug!(
        "gemini usage [{account_id}] buckets={} pro={pro_best:?} flash={flash_best:?}",
        buckets.len()
    );

    AccountUsage {
        account_id: account_id.to_string(),
        provider: "gemini".to_string(),
        primary_used_percent: primary_used,
        primary_left_percent: primary_left,
        primary_reset_seconds: primary_reset,
        secondary_used_percent: secondary_used,
        secondary_left_percent: secondary_left,
        secondary_reset_seconds: secondary_reset,
        plan_type: None,
        status: "loaded".to_string(),
    }
}

/// Fetch GitHub Copilot usage from the GitHub API.
async fn fetch_copilot_usage(account_id: &str, token: &str) -> AccountUsage {
    let make_error = |status: &str| AccountUsage {
        account_id: account_id.to_string(),
        provider: "github-copilot".to_string(),
        primary_used_percent: None,
        primary_left_percent: None,
        primary_reset_seconds: None,
        secondary_used_percent: None,
        secondary_left_percent: None,
        secondary_reset_seconds: None,
        plan_type: None,
        status: status.to_string(),
    };

    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.github.com/copilot_internal/user")
        .header("Authorization", format!("token {token}"))
        .header("Accept", "application/json")
        .header("Editor-Version", "vscode/1.96.2")
        .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("X-Github-Api-Version", "2025-04-01")
        .timeout(Duration::from_secs(10))
        .send()
        .await;

    let resp = match resp {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let status = r.status().as_u16();
            return make_error(if status == 401 || status == 403 {
                "invalid_credentials"
            } else {
                "error"
            });
        }
        Err(_) => return make_error("error"),
    };

    let json: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return make_error("error"),
    };

    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let plan_type = json
        .get("copilot_plan")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Paid tier: quota_snapshots.premium_interactions.percent_remaining
    let snapshots = json.get("quota_snapshots");
    let premium = snapshots.and_then(|s| s.get("premium_interactions"));
    let premium_remaining = premium.and_then(|p| p.get("percent_remaining").and_then(|v| v.as_f64()));
    let chat_snapshot = snapshots.and_then(|s| s.get("chat"));
    let chat_remaining =
        chat_snapshot.and_then(|c| c.get("percent_remaining").and_then(|v| v.as_f64()));

    let reset_date_str = json.get("quota_reset_date").and_then(|v| v.as_str());
    let reset_secs = reset_date_str
        .and_then(parse_rfc3339_epoch)
        .map(|epoch| (epoch - now_epoch).max(0));

    // Free tier: limited_user_quotas
    let limited = json.get("limited_user_quotas");
    let monthly = json.get("monthly_quotas");
    let free_reset_str = json.get("limited_user_reset_date").and_then(|v| v.as_str());
    let free_reset_secs = free_reset_str
        .and_then(parse_rfc3339_epoch)
        .map(|epoch| (epoch - now_epoch).max(0));

    let (primary_used, primary_left, primary_reset);
    let (secondary_used, secondary_left, secondary_reset);

    if premium_remaining.is_some() || chat_remaining.is_some() {
        // Paid tier
        let premium_used = premium_remaining.map(|r| (100.0 - r).clamp(0.0, 100.0));
        let chat_used = chat_remaining.map(|r| (100.0 - r).clamp(0.0, 100.0));

        let (pu, pl) = normalize_usage_window(premium_used, None);
        primary_used = pu;
        primary_left = pl;
        primary_reset = reset_secs;

        let (su, sl) = normalize_usage_window(chat_used, None);
        secondary_used = su;
        secondary_left = sl;
        secondary_reset = reset_secs;
    } else if let (Some(lq), Some(mq)) = (limited, monthly) {
        // Free tier
        let chat_remaining_count = lq.get("chat").and_then(|v| v.as_f64());
        let chat_total = mq.get("chat").and_then(|v| v.as_f64());
        let completions_remaining = lq.get("completions").and_then(|v| v.as_f64());
        let completions_total = mq.get("completions").and_then(|v| v.as_f64());

        let chat_pct = match (chat_remaining_count, chat_total) {
            (Some(rem), Some(tot)) if tot > 0.0 => {
                Some(((tot - rem) / tot * 100.0).clamp(0.0, 100.0))
            }
            _ => None,
        };
        let completions_pct = match (completions_remaining, completions_total) {
            (Some(rem), Some(tot)) if tot > 0.0 => {
                Some(((tot - rem) / tot * 100.0).clamp(0.0, 100.0))
            }
            _ => None,
        };

        let (pu, pl) = normalize_usage_window(chat_pct, None);
        primary_used = pu;
        primary_left = pl;
        primary_reset = free_reset_secs;

        let (su, sl) = normalize_usage_window(completions_pct, None);
        secondary_used = su;
        secondary_left = sl;
        secondary_reset = free_reset_secs;
    } else {
        primary_used = None;
        primary_left = None;
        primary_reset = None;
        secondary_used = None;
        secondary_left = None;
        secondary_reset = None;
    }

    log::debug!(
        "copilot usage [{account_id}] premium_remaining={premium_remaining:?} chat_remaining={chat_remaining:?} → primary_used={primary_used:?} secondary_used={secondary_used:?}"
    );

    AccountUsage {
        account_id: account_id.to_string(),
        provider: "github-copilot".to_string(),
        primary_used_percent: primary_used,
        primary_left_percent: primary_left,
        primary_reset_seconds: primary_reset,
        secondary_used_percent: secondary_used,
        secondary_left_percent: secondary_left,
        secondary_reset_seconds: secondary_reset,
        plan_type,
        status: "loaded".to_string(),
    }
}

#[tauri::command]
pub async fn get_ai_proxy_account_usage() -> Result<Vec<AccountUsage>, String> {
    let auth_dir = config::get_auth_dir()?;

    // Get all active accounts
    let accounts = tokio::task::spawn_blocking({
        let auth_dir = auth_dir.clone();
        move || scan_account_dir(&auth_dir, true)
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?;

    // Fetch usage for all supported providers in parallel
    let mut futures = Vec::new();
    for acc in &accounts {
        let account_json = read_account_json(&auth_dir, &acc.account_id);
        let Some(account_json) = account_json else { continue };
        let token = account_json
            .get("access_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        match acc.provider.as_str() {
            "claude" => {
                let Some(token) = token else { continue };
                let id = acc.account_id.clone();
                futures.push(tokio::spawn(async move {
                    fetch_claude_usage(&id, &token).await
                }));
            }
            "codex" => {
                let Some(token) = token else { continue };
                let id = acc.account_id.clone();
                futures.push(tokio::spawn(
                    async move { fetch_codex_usage(&id, &token).await },
                ));
            }
            "gemini" => {
                let id = acc.account_id.clone();
                let json = account_json;
                futures.push(tokio::spawn(async move {
                    fetch_gemini_usage(&id, json).await
                }));
            }
            "github-copilot" => {
                let Some(token) = token else { continue };
                let id = acc.account_id.clone();
                futures.push(tokio::spawn(async move {
                    fetch_copilot_usage(&id, &token).await
                }));
            }
            _ => {}
        }
    }

    let mut results = Vec::new();
    for handle in futures {
        if let Ok(usage) = handle.await {
            results.push(usage);
        }
    }

    Ok(results)
}

// ---------------------------------------------------------------------------
// Model groups
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn update_ai_proxy_model_groups(
    app: AppHandle,
    groups: Vec<ProxyModelGroup>,
) -> Result<(), String> {
    let mut prefs = crate::load_preferences_sync(&app)?;
    prefs.ai_proxy_model_groups = groups.clone();
    save_preferences_sync(&app, &prefs)?;

    // Hot-reload into the running proxy (no-op if not running).
    proxy_server::update_model_groups(groups).await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Usage stats
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_ai_proxy_usage(app: AppHandle) -> Result<UsageStats, String> {
    let status = proxy_server::get_status().await;
    let prefs = crate::load_preferences_sync(&app).ok();
    let backend_ports = collect_backend_ports(&status, prefs.as_ref());

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let mut last_mgmt_error: Option<String> = None;
    for port in backend_ports {
        match fetch_backend_management_usage(&client, port).await {
            Ok(stats) => {
                let baseline = MANAGEMENT_USAGE_BASELINE.lock().await.clone();
                return Ok(if let Some(base) = baseline {
                    subtract_usage_stats(stats, &base)
                } else {
                    stats
                });
            }
            Err(e) => last_mgmt_error = Some(e),
        }
    }

    if let Some(stats) = proxy_server::get_usage_stats().await {
        return Ok(stats);
    }

    Err(format!(
        "Proxy usage unavailable{}",
        last_mgmt_error
            .map(|e| format!(" ({e})"))
            .unwrap_or_default()
    ))
}

#[tauri::command]
pub async fn reset_ai_proxy_usage(app: AppHandle) -> Result<(), String> {
    let status = proxy_server::get_status().await;
    let prefs = crate::load_preferences_sync(&app).ok();
    let backend_ports = collect_backend_ports(&status, prefs.as_ref());

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let mut baseline: Option<UsageStats> = None;
    for port in backend_ports {
        if let Ok(stats) = fetch_backend_management_usage(&client, port).await {
            baseline = Some(stats);
            break;
        }
    }
    *MANAGEMENT_USAGE_BASELINE.lock().await = baseline;

    proxy_server::reset_usage().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers (install progress)
// ---------------------------------------------------------------------------

fn emit_progress(app: &AppHandle, stage: &str, message: &str, percent: u8) {
    use crate::http_server::EmitExt;
    let progress = AiProxyInstallProgress {
        stage: stage.to_string(),
        message: message.to_string(),
        percent,
    };
    if let Err(e) = app.emit_all("ai-proxy:install-progress", &progress) {
        log::warn!("Failed to emit ai-proxy install progress: {e}");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_rfc3339_utc() {
        let epoch = parse_rfc3339_epoch("2025-03-10T12:00:00Z").unwrap();
        // 2025-03-10 12:00:00 UTC = known epoch
        assert!(epoch > 1_700_000_000);
        assert!(epoch < 1_800_000_000);
    }

    #[test]
    fn parse_rfc3339_with_offset() {
        let utc = parse_rfc3339_epoch("2025-03-10T12:00:00Z").unwrap();
        let plus5 = parse_rfc3339_epoch("2025-03-10T17:00:00+05:00").unwrap();
        assert_eq!(utc, plus5);
    }

    #[test]
    fn parse_rfc3339_with_fractional() {
        let a = parse_rfc3339_epoch("2025-03-10T12:00:00Z").unwrap();
        let b = parse_rfc3339_epoch("2025-03-10T12:00:00.123Z").unwrap();
        assert_eq!(a, b); // Fractional seconds ignored (integer precision)
    }

    #[test]
    fn expired_past_date() {
        assert!(is_expired_rfc3339("2020-01-01T00:00:00Z"));
    }

    #[test]
    fn not_expired_future_date() {
        assert!(!is_expired_rfc3339("2099-01-01T00:00:00Z"));
    }

    #[test]
    fn invalid_input_not_expired() {
        assert!(!is_expired_rfc3339("not-a-date"));
        assert!(!is_expired_rfc3339(""));
    }

    #[test]
    fn normalize_usage_window_from_fractional_used() {
        let (used, left) = normalize_usage_window(Some(0.25), None);
        assert_eq!(used, Some(25.0));
        assert_eq!(left, Some(75.0));
    }

    #[test]
    fn normalize_usage_window_from_percent_used() {
        let (used, left) = normalize_usage_window(Some(82.0), None);
        assert_eq!(used, Some(82.0));
        assert_eq!(left, Some(18.0));
    }

    #[test]
    fn normalize_usage_window_from_left_only() {
        let (used, left) = normalize_usage_window(None, Some(15.0));
        assert_eq!(used, Some(85.0));
        assert_eq!(left, Some(15.0));
    }

    #[test]
    fn parse_proxy_models_best_effort_and_deterministic() {
        let payload = json!({
            "data": [
                { "id": "openai/gpt-4o-mini" },
                { "id": "claude-sonnet-4", "owned_by": "claude" },
                { "id": "gpt-4o-mini", "provider": "openai" },
                { "id": "zeta-model" }
            ]
        });

        let parsed = parse_proxy_models_payload(&payload);
        let pairs: Vec<(String, String)> = parsed
            .iter()
            .map(|m| (m.provider.clone(), m.model.clone()))
            .collect();

        assert_eq!(
            pairs,
            vec![
                ("claude".to_string(), "claude-sonnet-4".to_string()),
                ("openai".to_string(), "gpt-4o-mini".to_string()),
                ("unknown".to_string(), "zeta-model".to_string()),
            ]
        );
    }

    #[test]
    fn parse_management_auth_files_maps_auth_index_to_email() {
        let payload = json!({
            "files": [
                {"auth_index": "abc123", "email": "a@example.com"},
                {"auth_index": "def456", "name": "fallback-name"}
            ]
        });

        let map = parse_management_auth_files_payload(&payload);
        assert_eq!(map.get("abc123"), Some(&"a@example.com".to_string()));
        assert_eq!(map.get("def456"), Some(&"fallback-name".to_string()));
    }

    #[test]
    fn parse_management_usage_aggregates_provider_model_account() {
        let usage_payload = json!({
            "usage": {
                "total_requests": 3,
                "apis": {
                    "key-1": {
                        "models": {
                            "openai/gpt-5-mini": {
                                "total_requests": 2,
                                "details": [
                                    {"auth_index": "a1"},
                                    {"auth_index": "a1"}
                                ]
                            },
                            "claude-sonnet-4": {
                                "total_requests": 1,
                                "details": [
                                    {"auth_index": "a2"}
                                ]
                            }
                        }
                    }
                }
            }
        });

        let mut account_map = std::collections::HashMap::new();
        account_map.insert("a1".to_string(), "openai@example.com".to_string());
        account_map.insert("a2".to_string(), "claude@example.com".to_string());

        let stats = parse_management_usage_payload(&usage_payload, &account_map).unwrap();
        assert_eq!(stats.total_requests, 3);
        assert_eq!(stats.per_provider.get("openai"), Some(&2));
        assert_eq!(stats.per_provider.get("claude"), Some(&1));
        assert_eq!(stats.per_model.get("gpt-5-mini"), Some(&2));
        assert_eq!(stats.per_model.get("claude-sonnet-4"), Some(&1));
        assert_eq!(stats.per_account.get("openai@example.com"), Some(&2));
        assert_eq!(stats.per_account.get("claude@example.com"), Some(&1));

        let rows: Vec<(String, String, String, u64)> = stats
            .by_request
            .iter()
            .map(|r| {
                (
                    r.provider.clone(),
                    r.model.clone(),
                    r.account.clone(),
                    r.requests,
                )
            })
            .collect();

        assert_eq!(
            rows,
            vec![
                (
                    "claude".to_string(),
                    "claude-sonnet-4".to_string(),
                    "claude@example.com".to_string(),
                    1,
                ),
                (
                    "openai".to_string(),
                    "gpt-5-mini".to_string(),
                    "openai@example.com".to_string(),
                    2,
                ),
            ]
        );
    }
}
