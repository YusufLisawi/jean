//! Binary path resolution and YAML config generation for the AI proxy backend.

use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// Directory name for the backend binary inside app data.
const BACKEND_DIR_NAME: &str = "cli-proxy-api";

/// Backend binary name.
#[cfg(windows)]
const BACKEND_BINARY_NAME: &str = "cli-proxy-api-plus.exe";
#[cfg(not(windows))]
const BACKEND_BINARY_NAME: &str = "cli-proxy-api-plus";

/// Auth directory name under the user's home.
const AUTH_DIR_NAME: &str = ".cli-proxy-api";
/// Shared management key for localhost-only usage introspection endpoints.
pub const MANAGEMENT_KEY: &str = "jean-local-management-key";

// ---------------------------------------------------------------------------
// App-managed backend paths
// ---------------------------------------------------------------------------

/// Get the directory where the backend binary is installed.
///
/// Returns: `<app_data_dir>/cli-proxy-api/`
pub fn get_backend_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data directory: {e}"))?;
    Ok(app_data_dir.join(BACKEND_DIR_NAME))
}

/// Get the full path to the backend binary.
///
/// Returns: `<app_data_dir>/cli-proxy-api/cli-proxy-api`
pub fn get_backend_binary_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(get_backend_dir(app)?.join(BACKEND_BINARY_NAME))
}

/// Resolve backend binary path (app-managed location only).
pub fn resolve_backend_binary(app: &AppHandle) -> PathBuf {
    get_backend_binary_path(app)
        .unwrap_or_else(|_| PathBuf::from(BACKEND_DIR_NAME).join(BACKEND_BINARY_NAME))
}

/// Ensure the backend directory exists, creating it if necessary.
pub fn ensure_backend_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = get_backend_dir(app)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create backend directory: {e}"))?;
    Ok(dir)
}

// ---------------------------------------------------------------------------
// Auth directory (~/.cli-proxy-api/)
// ---------------------------------------------------------------------------

/// Get the auth directory where CLIProxyAPIPlus stores tokens.
///
/// Returns: `~/.cli-proxy-api/`
pub fn get_auth_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "Failed to resolve home directory".to_string())?;
    Ok(home.join(AUTH_DIR_NAME))
}

// ---------------------------------------------------------------------------
// Config YAML generation
// ---------------------------------------------------------------------------

/// Format the config YAML that CLIProxyAPIPlus expects.
///
/// String formatting is intentionally used over serde_yaml — the config is
/// only a few lines and this avoids an extra crate dependency.
pub fn format_config_yaml(port: u16, auth_dir: &str, strategy: &str) -> String {
    format!(
        "host: '127.0.0.1'\nport: {port}\nauth-dir: '{auth_dir}'\nrouting:\n  strategy: '{strategy}'\nremote-management:\n  secret-key: '{MANAGEMENT_KEY}'\n"
    )
}

/// Write the config YAML to the auth directory.
pub fn write_config_yaml(backend_port: u16, rotation_strategy: &str) -> Result<PathBuf, String> {
    let auth_dir = get_auth_dir()?;
    std::fs::create_dir_all(&auth_dir)
        .map_err(|e| format!("Failed to create auth directory: {e}"))?;

    let auth_dir_str = auth_dir
        .to_str()
        .ok_or_else(|| "Auth directory path contains invalid UTF-8".to_string())?;

    let yaml = format_config_yaml(backend_port, auth_dir_str, rotation_strategy);
    let config_path = auth_dir.join("config.yaml");

    std::fs::write(&config_path, yaml).map_err(|e| format!("Failed to write config.yaml: {e}"))?;

    Ok(config_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_path_has_expected_shape() {
        let resolved = PathBuf::from(BACKEND_DIR_NAME).join(BACKEND_BINARY_NAME);
        assert!(resolved.ends_with(BACKEND_BINARY_NAME));
        assert!(resolved.to_string_lossy().contains(BACKEND_DIR_NAME));
    }

    #[test]
    fn config_yaml_format() {
        let yaml = format_config_yaml(8080, "/home/user/.cli-proxy-api", "round_robin");
        assert!(yaml.contains("port: 8080"));
        assert!(yaml.contains("auth-dir: '/home/user/.cli-proxy-api'"));
        assert!(yaml.contains("strategy: 'round_robin'"));
        assert!(yaml.contains("host: '127.0.0.1'"));
        assert!(yaml.contains("remote-management:"));
        assert!(yaml.contains("secret-key:"));
    }
}
