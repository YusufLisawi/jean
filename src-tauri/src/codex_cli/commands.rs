//! Tauri commands for Codex CLI management

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use super::config::{ensure_cli_dir, get_cli_binary_path, resolve_cli_binary};
use crate::http_server::EmitExt;
use crate::platform::silent_command;

/// GitHub API URL for Codex CLI releases
const CODEX_RELEASES_API: &str = "https://api.github.com/repos/openai/codex/releases";

/// Extract version number from a tag like "v0.104.0" or "vrust-v0.104.0"
fn extract_version_from_tag(tag: &str) -> String {
    // Try to find a semver pattern (digits.digits.digits)
    for part in tag.split('v') {
        let trimmed = part.trim_end_matches('-');
        if trimmed
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
            && trimmed.contains('.')
        {
            return trimmed.to_string();
        }
    }
    tag.to_string()
}

/// Status of the Codex CLI installation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexCliStatus {
    pub installed: bool,
    pub version: Option<String>,
    pub path: Option<String>,
}

/// Auth status of the Codex CLI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexAuthStatus {
    pub authenticated: bool,
    pub error: Option<String>,
}

/// Information about a Codex CLI release
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexReleaseInfo {
    pub version: String,
    pub tag_name: String,
    pub published_at: String,
    pub prerelease: bool,
}

/// Progress event for CLI installation
#[derive(Debug, Clone, Serialize)]
pub struct CodexInstallProgress {
    pub stage: String,
    pub message: String,
    pub percent: u8,
}

/// GitHub API release response structure
#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    published_at: String,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

fn emit_progress(app: &AppHandle, stage: &str, message: &str, percent: u8) {
    let _ = app.emit_all(
        "codex-cli:install-progress",
        &CodexInstallProgress {
            stage: stage.to_string(),
            message: message.to_string(),
            percent,
        },
    );
}

fn build_github_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("Jean-App/1.0")
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))
}

/// Check if Codex CLI is installed and get its status
#[tauri::command]
pub async fn check_codex_cli_installed(app: AppHandle) -> Result<CodexCliStatus, String> {
    log::trace!("Checking Codex CLI installation status");

    let binary_path = resolve_cli_binary(&app);

    if !binary_path.exists() {
        log::trace!("Codex CLI not found at {:?}", binary_path);
        return Ok(CodexCliStatus {
            installed: false,
            version: None,
            path: None,
        });
    }

    // Get version
    let version = match silent_command(&binary_path).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if version_str.is_empty() {
                None
            } else {
                // codex --version might return "codex 0.104.0" or just "0.104.0"
                let version = version_str
                    .split_whitespace()
                    .last()
                    .map(|s| s.trim_start_matches('v').to_string())
                    .unwrap_or(version_str);
                Some(version)
            }
        }
        _ => None,
    };

    Ok(CodexCliStatus {
        installed: true,
        version,
        path: Some(binary_path.to_string_lossy().to_string()),
    })
}

/// Check if Codex CLI is authenticated
#[tauri::command]
pub async fn check_codex_cli_auth(app: AppHandle) -> Result<CodexAuthStatus, String> {
    log::trace!("Checking Codex CLI authentication status");

    let binary_path = resolve_cli_binary(&app);

    if !binary_path.exists() {
        return Ok(CodexAuthStatus {
            authenticated: false,
            error: Some("Codex CLI not installed".to_string()),
        });
    }

    // Run `codex login status` to check authentication
    let output = silent_command(&binary_path)
        .args(["login", "status"])
        .output()
        .map_err(|e| format!("Failed to execute Codex CLI: {e}"))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        log::trace!("Codex CLI auth check output: {stdout}");
        Ok(CodexAuthStatus {
            authenticated: true,
            error: None,
        })
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        log::trace!("Codex CLI auth check failed: {stderr}");
        Ok(CodexAuthStatus {
            authenticated: false,
            error: if stderr.is_empty() {
                Some("Not authenticated".to_string())
            } else {
                Some(stderr)
            },
        })
    }
}

/// Get available Codex CLI versions from GitHub releases
#[tauri::command]
pub async fn get_available_codex_versions() -> Result<Vec<CodexReleaseInfo>, String> {
    log::trace!("Fetching available Codex CLI versions from GitHub API");

    let client = build_github_client()?;

    // Fetch enough releases to find stable ones buried behind prereleases
    let response = client
        .get(format!("{CODEX_RELEASES_API}?per_page=100"))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch releases: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("GitHub API returned status: {}", response.status()));
    }

    let releases: Vec<GitHubRelease> = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse GitHub API response: {e}"))?;

    let versions: Vec<CodexReleaseInfo> = releases
        .into_iter()
        .filter(|r| !r.prerelease && !r.assets.is_empty())
        .take(5)
        .map(|r| CodexReleaseInfo {
            version: extract_version_from_tag(&r.tag_name),
            tag_name: r.tag_name,
            published_at: r.published_at,
            prerelease: r.prerelease,
        })
        .collect();

    log::trace!("Found {} Codex CLI versions", versions.len());
    Ok(versions)
}

/// Get the Codex target triple for the current platform
fn get_codex_target() -> Result<&'static str, String> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Ok("aarch64-apple-darwin");
    }

    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return Ok("x86_64-apple-darwin");
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Ok("x86_64-unknown-linux-gnu");
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        return Ok("aarch64-unknown-linux-gnu");
    }

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Ok("x86_64-pc-windows-msvc");
    }

    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        return Ok("aarch64-pc-windows-msvc");
    }

    #[allow(unreachable_code)]
    Err("Unsupported platform".to_string())
}

/// Fetch the latest Codex CLI version from GitHub API
async fn fetch_latest_codex_version() -> Result<String, String> {
    log::trace!("Fetching latest Codex CLI version");

    let client = build_github_client()?;
    let response = client
        .get(format!("{CODEX_RELEASES_API}/latest"))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch latest release: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "Failed to fetch latest release: HTTP {}",
            response.status()
        ));
    }

    let release: GitHubRelease = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse release info: {e}"))?;

    let version = extract_version_from_tag(&release.tag_name);
    log::trace!("Latest Codex CLI version: {version}");
    Ok(version)
}

/// Find the download URL for a specific asset by searching recent releases
async fn find_asset_url(version: &str, asset_name: &str) -> Result<String, String> {
    let client = build_github_client()?;
    let response = client
        .get(CODEX_RELEASES_API)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch releases: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("GitHub API returned status: {}", response.status()));
    }

    let releases: Vec<GitHubRelease> = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse releases: {e}"))?;

    for release in &releases {
        let release_version = extract_version_from_tag(&release.tag_name);
        if release_version == version {
            for asset in &release.assets {
                if asset.name == asset_name {
                    return Ok(asset.browser_download_url.clone());
                }
            }
            return Err(format!(
                "Asset {asset_name} not found in release {}",
                release.tag_name
            ));
        }
    }

    Err(format!("Release for version {version} not found"))
}

/// Install Codex CLI by downloading from GitHub releases
#[tauri::command]
pub async fn install_codex_cli(app: AppHandle, version: Option<String>) -> Result<(), String> {
    log::trace!("Installing Codex CLI, version: {:?}", version);

    let _cli_dir = ensure_cli_dir(&app)?;
    let binary_path = get_cli_binary_path(&app)?;

    // Emit progress: starting
    emit_progress(&app, "starting", "Preparing installation...", 0);

    // Determine version
    let version = match version {
        Some(v) => v,
        None => fetch_latest_codex_version().await?,
    };

    let target = get_codex_target()?;
    log::trace!("Installing version {version} for target {target}");

    // Build asset name to search for in release assets
    #[cfg(target_os = "windows")]
    let (asset_name, is_zip) = (format!("codex-{target}.exe.zip"), true);
    #[cfg(not(target_os = "windows"))]
    let (asset_name, is_zip) = (format!("codex-{target}.tar.gz"), false);

    // Find the download URL from the release assets
    let download_url = find_asset_url(&version, &asset_name).await?;
    log::trace!("Downloading from: {download_url}");

    // Emit progress: downloading
    emit_progress(&app, "downloading", "Downloading Codex CLI...", 20);

    let client = reqwest::Client::builder()
        .user_agent("Jean-App/1.0")
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let response = client
        .get(&download_url)
        .send()
        .await
        .map_err(|e| format!("Failed to download Codex CLI: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "Failed to download Codex CLI: HTTP {}",
            response.status()
        ));
    }

    let archive_content = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read archive content: {e}"))?;

    log::trace!("Downloaded {} bytes", archive_content.len());

    // Emit progress: extracting
    emit_progress(&app, "extracting", "Extracting archive...", 45);

    // On Windows, a running codex.exe holds a file lock that prevents overwriting.
    // Rename the old binary out of the way before extracting the new one.
    #[cfg(windows)]
    if binary_path.exists() {
        let old_path = binary_path.with_extension("exe.old");
        let _ = std::fs::remove_file(&old_path); // Clean up previous .old if any
        if let Err(e) = std::fs::rename(&binary_path, &old_path) {
            log::warn!("Could not rename existing binary (may be unlocked): {e}");
            // Try removing directly as a fallback
            if let Err(e2) = std::fs::remove_file(&binary_path) {
                return Err(format!(
                    "Cannot replace existing Codex CLI binary — it may be in use by another process. \
                     Please close any running Codex sessions and try again. (rename: {e}, remove: {e2})"
                ));
            }
        }
    }

    if is_zip {
        extract_zip_binary(&archive_content, &binary_path, target)?;
    } else {
        extract_tar_gz_binary(&archive_content, &binary_path, target)?;
    }

    // Emit progress: installing
    emit_progress(&app, "installing", "Installing Codex CLI...", 65);

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&binary_path)
            .map_err(|e| format!("Failed to get binary metadata: {e}"))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&binary_path, perms)
            .map_err(|e| format!("Failed to set binary permissions: {e}"))?;
    }

    // Remove macOS quarantine attribute
    #[cfg(target_os = "macos")]
    {
        let _ = silent_command("xattr")
            .args(["-d", "com.apple.quarantine"])
            .arg(&binary_path)
            .output();
    }

    // Emit progress: verifying
    emit_progress(&app, "verifying", "Verifying installation...", 80);

    // Verify the binary works
    let version_output = silent_command(&binary_path)
        .arg("--version")
        .output()
        .map_err(|e| format!("Failed to verify Codex CLI: {e}"))?;

    if !version_output.status.success() {
        let stderr = String::from_utf8_lossy(&version_output.stderr);
        let stdout = String::from_utf8_lossy(&version_output.stdout);
        let output = if !stderr.is_empty() {
            stderr.to_string()
        } else if !stdout.is_empty() {
            stdout.to_string()
        } else {
            format!("exit code {}", version_output.status)
        };
        return Err(format!("Codex CLI verification failed: {output}"));
    }

    // Clean up stale .old binary from Windows rename-on-reinstall
    #[cfg(windows)]
    {
        let old_path = binary_path.with_extension("exe.old");
        let _ = std::fs::remove_file(&old_path);
    }

    // Emit progress: complete
    emit_progress(&app, "complete", "Installation complete!", 100);

    log::trace!("Codex CLI installed successfully at {:?}", binary_path);
    Ok(())
}

/// Extract the codex binary from a tar.gz archive
fn extract_tar_gz_binary(
    archive_content: &[u8],
    binary_path: &std::path::Path,
    target: &str,
) -> Result<(), String> {
    use flate2::read::GzDecoder;
    use std::io::{Cursor, Read};
    use tar::Archive;

    let cursor = Cursor::new(archive_content);
    let decoder = GzDecoder::new(cursor);
    let mut archive = Archive::new(decoder);

    // Match only the main codex binary (e.g. "codex-aarch64-apple-darwin"),
    // not helper binaries like codex-command-runner or codex-windows-sandbox-setup.
    let expected_name = format!("codex-{target}");

    for entry in archive
        .entries()
        .map_err(|e| format!("Failed to read tar entries: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("Failed to read tar entry: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("Failed to get entry path: {e}"))?;

        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name == expected_name {
                let mut content = Vec::new();
                entry
                    .read_to_end(&mut content)
                    .map_err(|e| format!("Failed to read binary from archive: {e}"))?;

                crate::platform::write_binary_file(binary_path, &content)
                    .map_err(|e| format!("Failed to write binary: {e}"))?;

                return Ok(());
            }
        }
    }

    Err(format!(
        "Codex binary '{expected_name}' not found in tar.gz archive"
    ))
}

/// Extract the codex binary from a zip archive (Windows)
///
/// The Windows zip may contain helper binaries (codex-command-runner.exe,
/// codex-windows-sandbox-setup.exe) bundled for WinGet. We must extract only
/// the main codex binary matching the expected target name.
fn extract_zip_binary(
    archive_content: &[u8],
    binary_path: &std::path::Path,
    target: &str,
) -> Result<(), String> {
    use std::io::{Cursor, Read};

    let cursor = Cursor::new(archive_content);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("Failed to open zip archive: {e}"))?;

    let expected_name = format!("codex-{target}.exe");

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read zip entry: {e}"))?;

        if let Some(name) = file.enclosed_name().and_then(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        }) {
            if name == expected_name {
                let mut content = Vec::new();
                file.read_to_end(&mut content)
                    .map_err(|e| format!("Failed to read binary from archive: {e}"))?;

                crate::platform::write_binary_file(binary_path, &content)
                    .map_err(|e| format!("Failed to write binary: {e}"))?;

                return Ok(());
            }
        }
    }

    Err(format!(
        "Codex binary '{expected_name}' not found in zip archive"
    ))
}
