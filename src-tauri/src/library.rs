use std::path::MAIN_SEPARATOR;

use serde::Serialize;
use sqlx::FromRow;
use tauri::State;

use crate::db::Db;

/// A root folder the user added to their library.
#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct LibraryFolder {
    pub id: i64,
    pub path: String,
    /// Unix seconds.
    pub added_at: i64,
}

/// Row shape used for the containment checks; `path_key` never leaves Rust.
#[derive(FromRow)]
struct FolderKey {
    path: String,
    path_key: String,
}

/// A canonical path to an existing directory, plus its comparison key.
///
/// Parse, don't validate: everything downstream (the scanner, next task) relies
/// on paths being canonical, so the only way to build one is through `parse`.
struct LibraryPath {
    path: String,
    key: String,
}

impl LibraryPath {
    /// Canonicalizes `raw` and derives its comparison key.
    ///
    /// This touches the filesystem, so callers must run it off the async
    /// runtime -- on a disconnected network share it can block for seconds.
    fn parse(raw: &str) -> Result<Self, String> {
        // dunce, not std::fs::canonicalize: on Windows the std version returns
        // extended-length `\\?\D:\Music` paths, which look wrong in the UI and
        // break some downstream APIs.
        let canonical = dunce::canonicalize(raw)
            .map_err(|_| format!("Could not open \"{raw}\". Is the drive connected?"))?;

        if !canonical.is_dir() {
            return Err(format!("\"{raw}\" is not a folder."));
        }

        let path = canonical
            .to_str()
            .ok_or("That folder's path is not valid UTF-8.")?
            .to_string();

        Ok(Self {
            key: comparison_key(&path),
            path,
        })
    }
}

/// Builds the key used for uniqueness and containment tests.
///
/// Windows filesystems are case-insensitive, so `D:\Music` and `d:\music` are
/// one folder and must collapse to one key. Linux and macOS-with-a-case-
/// sensitive-volume treat them as two, so the path is left alone there.
#[cfg(windows)]
fn comparison_key(path: &str) -> String {
    path.to_lowercase()
}

#[cfg(not(windows))]
fn comparison_key(path: &str) -> String {
    path.to_string()
}

/// True when `child` sits inside `parent` (and is not `parent` itself).
///
/// The separator check is what stops `D:\Music` from swallowing
/// `D:\Musicology`, which a plain `starts_with` would wrongly match.
fn is_inside(child: &str, parent: &str) -> bool {
    let Some(rest) = child.strip_prefix(parent) else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    parent.ends_with(MAIN_SEPARATOR) || rest.starts_with(MAIN_SEPARATOR)
}

/// Adds a folder the user picked in the frontend's native dialog.
///
/// Rejects duplicates and any folder that overlaps one already in the library:
/// overlapping roots would make the next task's scanner walk the same files
/// twice.
#[tauri::command]
pub async fn add_library_folder(
    app: tauri::AppHandle,
    path: String,
    db: State<'_, Db>,
) -> Result<LibraryFolder, String> {
    let candidate = tauri::async_runtime::spawn_blocking(move || LibraryPath::parse(&path))
        .await
        .map_err(|e| e.to_string())??;

    // Downloaded YouTube audio lives in the app's own folder. Scanning it would
    // have the scanner try to claim those files as separate local tracks, so
    // adding it -- or anything containing it -- is refused up front.
    if let Ok(downloads) = crate::download::downloads_dir(&app) {
        if let Some(downloads_key) = downloads.to_str().map(comparison_key) {
            if candidate.key == downloads_key || is_inside(&downloads_key, &candidate.key) {
                return Err(
                    "That folder holds the app's own downloads. Pick a different one."
                        .to_string(),
                );
            }
        }
    }

    let existing: Vec<FolderKey> = sqlx::query_as("SELECT path, path_key FROM library_folders")
        .fetch_all(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    for folder in &existing {
        if folder.path_key == candidate.key {
            return Err("That folder is already in your library.".to_string());
        }
        if is_inside(&candidate.key, &folder.path_key) {
            return Err(format!("Already covered by \"{}\".", folder.path));
        }
        if is_inside(&folder.path_key, &candidate.key) {
            return Err(format!("\"{}\" is already inside this folder.", folder.path));
        }
    }

    sqlx::query_as(
        "INSERT INTO library_folders (path, path_key) VALUES (?, ?) \
         RETURNING id, path, added_at",
    )
    .bind(&candidate.path)
    .bind(&candidate.key)
    .fetch_one(&db.pool)
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_library_folders(db: State<'_, Db>) -> Result<Vec<LibraryFolder>, String> {
    sqlx::query_as("SELECT id, path, added_at FROM library_folders ORDER BY added_at, id")
        .fetch_all(&db.pool)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn remove_library_folder(id: i64, db: State<'_, Db>) -> Result<(), String> {
    let mut tx = db.pool.begin().await.map_err(|e| e.to_string())?;

    // The foreign key is ON DELETE SET NULL, which detaches the tracks but says
    // nothing about their availability -- mark them explicitly so the UI stops
    // offering them as playable. Deleting them instead would take every
    // playlist entry and the play history with it.
    sqlx::query("UPDATE tracks SET state = 'missing' WHERE folder_id = ? AND source = 'local'")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    sqlx::query("DELETE FROM library_folders WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    tx.commit().await.map_err(|e| e.to_string())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Joins segments with the platform separator so these stay true on every OS.
    fn p(segments: &[&str]) -> String {
        segments.join(&MAIN_SEPARATOR.to_string())
    }

    #[test]
    fn a_folder_is_not_inside_itself() {
        assert!(!is_inside(&p(&["music"]), &p(&["music"])));
    }

    #[test]
    fn a_subfolder_is_inside_its_parent() {
        assert!(is_inside(&p(&["music", "rock"]), &p(&["music"])));
    }

    #[test]
    fn a_sibling_with_a_shared_prefix_is_not_inside() {
        assert!(!is_inside(&p(&["musicology"]), &p(&["music"])));
    }

    #[test]
    fn a_parent_is_not_inside_its_child() {
        assert!(!is_inside(&p(&["music"]), &p(&["music", "rock"])));
    }
}
