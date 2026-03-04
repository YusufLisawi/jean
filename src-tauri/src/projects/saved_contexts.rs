use serde::{Deserialize, Serialize};
use tauri::Manager;
use uuid::Uuid;

/// Attached saved context info returned to frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachedSavedContext {
    pub slug: String,
    pub name: Option<String>,
    pub size: u64,
    pub created_at: u64,
}

/// Strip nested "{uuid}-context-" prefixes from an attached-context slug.
fn strip_nested_attached_prefixes(slug: &str) -> String {
    let mut current = slug.trim().to_string();

    loop {
        let Some((prefix, rest)) = current.split_once("-context-") else {
            break;
        };
        if Uuid::parse_str(prefix).is_ok() {
            current = rest.to_string();
            continue;
        }
        break;
    }

    current
}

/// Sanitize a slug for use in filenames.
fn sanitize_slug_component(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Extract slug from "{session_id}-context-{slug}.md" if the filename matches.
fn extract_attached_slug_from_filename(filename: &str) -> Option<String> {
    if !filename.ends_with(".md") {
        return None;
    }
    let name_without_ext = &filename[..filename.len() - 3];
    let (prefix, slug) = name_without_ext.split_once("-context-")?;
    if Uuid::parse_str(prefix).is_ok() {
        Some(slug.to_string())
    } else {
        None
    }
}

/// Attach a saved context to a session by copying it to the session-specific location.
///
/// Storage location: `app-data/session-context/{session_id}-context-{slug}.md`
#[tauri::command]
pub async fn attach_saved_context(
    app: tauri::AppHandle,
    session_id: String,
    source_path: String,
    slug: String,
) -> Result<AttachedSavedContext, String> {
    log::trace!("Attaching saved context '{slug}' for session {session_id}");

    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data directory: {e}"))?;

    let saved_contexts_dir = app_data_dir.join("session-context");
    std::fs::create_dir_all(&saved_contexts_dir)
        .map_err(|e| format!("Failed to create session-context directory: {e}"))?;

    // Read source file
    let source = std::path::Path::new(&source_path);
    if !source.exists() {
        return Err(format!("Source context file not found: {source_path}"));
    }

    // Normalize slug to avoid nested malformed names like
    // "{session_a}-context-{session_b}-context-{slug}.md".
    let source_slug = source
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(extract_attached_slug_from_filename)
        .unwrap_or_default();
    let slug_input = if slug.trim().is_empty() {
        source_slug
    } else {
        slug
    };
    let normalized_slug = sanitize_slug_component(&strip_nested_attached_prefixes(&slug_input));
    if normalized_slug.is_empty() {
        return Err("Invalid context slug".to_string());
    }

    let content = std::fs::read_to_string(source)
        .map_err(|e| format!("Failed to read source context file: {e}"))?;

    // Extract name from content (first line if it starts with # )
    let name = content
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("# "))
        .map(|s| s.to_string());

    // Destination file: {session_id}-context-{slug}.md
    let dest_file = saved_contexts_dir.join(format!("{session_id}-context-{normalized_slug}.md"));

    // Write content to destination
    std::fs::write(&dest_file, &content)
        .map_err(|e| format!("Failed to write attached context file: {e}"))?;

    // Get file metadata for size and created_at
    let metadata =
        std::fs::metadata(&dest_file).map_err(|e| format!("Failed to get file metadata: {e}"))?;

    let size = metadata.len();
    let created_at = metadata
        .created()
        .or_else(|_| metadata.modified())
        .map_err(|e| format!("Failed to get file time: {e}"))?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("Failed to convert time: {e}"))?
        .as_secs();

    log::trace!("Attached saved context '{normalized_slug}' for session {session_id}");

    Ok(AttachedSavedContext {
        slug: normalized_slug,
        name,
        size,
        created_at,
    })
}

/// Remove an attached saved context from a session.
#[tauri::command]
pub async fn remove_saved_context(
    app: tauri::AppHandle,
    session_id: String,
    slug: String,
) -> Result<(), String> {
    log::trace!("Removing saved context '{slug}' from session {session_id}");

    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data directory: {e}"))?;

    let context_file = app_data_dir
        .join("session-context")
        .join(format!("{session_id}-context-{slug}.md"));

    if context_file.exists() {
        std::fs::remove_file(&context_file)
            .map_err(|e| format!("Failed to remove saved context file: {e}"))?;
        log::trace!("Removed saved context '{slug}' from session {session_id}");
    }

    Ok(())
}

/// List all attached saved contexts for a session.
#[tauri::command]
pub async fn list_attached_saved_contexts(
    app: tauri::AppHandle,
    session_id: String,
) -> Result<Vec<AttachedSavedContext>, String> {
    log::trace!("Listing attached saved contexts for session {session_id}");

    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data directory: {e}"))?;

    let saved_contexts_dir = app_data_dir.join("session-context");

    if !saved_contexts_dir.exists() {
        return Ok(vec![]);
    }

    let mut contexts = Vec::new();
    let prefix = format!("{session_id}-context-");

    if let Ok(entries) = std::fs::read_dir(&saved_contexts_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();

            // Match files like "{session_id}-context-{slug}.md"
            if file_name.starts_with(&prefix) && file_name.ends_with(".md") {
                // Extract slug from filename
                let slug = file_name[prefix.len()..file_name.len() - 3].to_string();

                // Read file to extract name from first line
                let name = if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    content
                        .lines()
                        .next()
                        .and_then(|line| line.strip_prefix("# "))
                        .map(|s| s.to_string())
                } else {
                    None
                };

                // Get file metadata
                if let Ok(metadata) = std::fs::metadata(entry.path()) {
                    let size = metadata.len();
                    let created_at = metadata
                        .created()
                        .or_else(|_| metadata.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);

                    contexts.push(AttachedSavedContext {
                        slug,
                        name,
                        size,
                        created_at,
                    });
                }
            }
        }
    }

    // Sort by created_at (newest first)
    contexts.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    log::trace!("Found {} attached saved contexts", contexts.len());
    Ok(contexts)
}

/// Get the content of an attached saved context file.
#[tauri::command]
pub async fn get_saved_context_content(
    app: tauri::AppHandle,
    session_id: String,
    slug: String,
) -> Result<String, String> {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data directory: {e}"))?;

    let context_file = app_data_dir
        .join("session-context")
        .join(format!("{session_id}-context-{slug}.md"));

    if !context_file.exists() {
        return Err(format!("Saved context file not found for slug '{slug}'"));
    }

    std::fs::read_to_string(&context_file)
        .map_err(|e| format!("Failed to read saved context file: {e}"))
}

/// Delete all saved context files for a session.
///
/// Called during session deletion to clean up context files.
pub fn cleanup_saved_contexts_for_session(
    app: &tauri::AppHandle,
    session_id: &str,
) -> Result<(), String> {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data directory: {e}"))?;

    let saved_contexts_dir = app_data_dir.join("session-context");
    if !saved_contexts_dir.exists() {
        return Ok(());
    }

    let prefix = format!("{session_id}-context-");
    if let Ok(entries) = std::fs::read_dir(&saved_contexts_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name.starts_with(&prefix) && file_name.ends_with(".md") {
                if let Err(e) = std::fs::remove_file(entry.path()) {
                    log::warn!("Failed to remove saved context file {file_name}: {e}");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_nested_attached_prefixes() {
        assert_eq!(
            strip_nested_attached_prefixes(
                "424e58f4-2ffb-4e5a-a99f-a43504d325ac-context-oauth-ux-improvements"
            ),
            "oauth-ux-improvements"
        );
        assert_eq!(
            strip_nested_attached_prefixes(
                "424e58f4-2ffb-4e5a-a99f-a43504d325ac-context-2fcaeb1b-2958-43c8-a8fc-eb58c5a5cb20-context-vibeproxy-jean-integration"
            ),
            "vibeproxy-jean-integration"
        );
        assert_eq!(
            strip_nested_attached_prefixes("vibeproxy-jean-integration"),
            "vibeproxy-jean-integration"
        );
    }

    #[test]
    fn test_extract_attached_slug_from_filename() {
        assert_eq!(
            extract_attached_slug_from_filename(
                "149c4d16-6a54-4735-afcc-15be517adbd5-context-oauth-ux-improvements.md"
            ),
            Some("oauth-ux-improvements".to_string())
        );
        assert_eq!(
            extract_attached_slug_from_filename(
                "vibeproxy-jean-integration-1772621222-vibeproxy-jean-integration.md"
            ),
            None
        );
    }

    #[test]
    fn test_sanitize_slug_component() {
        assert_eq!(
            sanitize_slug_component("OAuth UX Improvements!!!"),
            "oauth-ux-improvements"
        );
    }
}
