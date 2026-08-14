use serde::Serialize;
use sqlx::{FromRow, SqliteConnection};
use tauri::State;

use crate::db::Db;
use crate::tracks::Track;

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub cover_path: Option<String>,
    pub created_at: i64,
    /// Shown in the list so a playlist's size is visible without opening it.
    pub track_count: i64,
}

/// A playlist together with its tracks, in order.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDetail {
    pub playlist: Playlist,
    pub tracks: Vec<Track>,
}

/// What `add_tracks_to_playlist` did.
///
/// Adding is idempotent, so "nothing happened" is a normal outcome and the UI
/// needs to be able to say so rather than implying a failure.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddOutcome {
    pub added: usize,
    /// Already present, so left alone.
    pub skipped: usize,
}

fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("A playlist needs a name.".to_string());
    }
    Ok(name.to_string())
}

#[tauri::command]
pub async fn create_playlist(db: State<'_, Db>, name: String) -> Result<Playlist, String> {
    let name = clean_name(&name)?;

    // Names are deliberately not unique: two playlists called "Chill" is the
    // user's business.
    let id: i64 = sqlx::query_scalar("INSERT INTO playlists (name) VALUES (?) RETURNING id")
        .bind(&name)
        .fetch_one(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    load_playlist(&db.pool, id).await
}

#[tauri::command]
pub async fn rename_playlist(
    db: State<'_, Db>,
    playlist_id: i64,
    name: String,
) -> Result<(), String> {
    let name = clean_name(&name)?;

    let affected = sqlx::query("UPDATE playlists SET name = ? WHERE id = ?")
        .bind(&name)
        .bind(playlist_id)
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected();

    if affected == 0 {
        return Err("That playlist no longer exists.".to_string());
    }

    Ok(())
}

#[tauri::command]
pub async fn delete_playlist(db: State<'_, Db>, playlist_id: i64) -> Result<(), String> {
    // playlist_tracks rows go with it via ON DELETE CASCADE; the tracks
    // themselves are untouched.
    sqlx::query("DELETE FROM playlists WHERE id = ?")
        .bind(playlist_id)
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub async fn list_playlists(db: State<'_, Db>) -> Result<Vec<Playlist>, String> {
    sqlx::query_as(
        "SELECT p.id, p.name, p.cover_path, p.created_at,
                COUNT(pt.track_id) AS track_count
         FROM playlists p
         LEFT JOIN playlist_tracks pt ON pt.playlist_id = p.id
         GROUP BY p.id
         ORDER BY p.created_at DESC, p.id DESC",
    )
    .fetch_all(&db.pool)
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_playlist(db: State<'_, Db>, playlist_id: i64) -> Result<PlaylistDetail, String> {
    let playlist = load_playlist(&db.pool, playlist_id).await?;

    // `added_at` breaks ties so the order is deterministic even if positions
    // were ever to collide.
    let tracks: Vec<Track> = sqlx::query_as(
        "SELECT t.id, t.source, t.title, t.artist, t.album, t.duration_secs, t.state
         FROM playlist_tracks pt
         JOIN tracks t ON t.id = pt.track_id
         WHERE pt.playlist_id = ?
         ORDER BY pt.position, pt.added_at, t.id",
    )
    .bind(playlist_id)
    .fetch_all(&db.pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(PlaylistDetail { playlist, tracks })
}

/// Appends tracks, ignoring any already present.
#[tauri::command]
pub async fn add_tracks_to_playlist(
    db: State<'_, Db>,
    playlist_id: i64,
    track_ids: Vec<i64>,
) -> Result<AddOutcome, String> {
    let mut tx = db.pool.begin().await.map_err(|e| e.to_string())?;

    ensure_playlist_exists(&mut tx, playlist_id).await?;

    // Read inside the transaction: two concurrent adds would otherwise pick
    // the same starting position.
    let mut next: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_tracks WHERE playlist_id = ?",
    )
    .bind(playlist_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    let mut outcome = AddOutcome {
        added: 0,
        skipped: 0,
    };

    for track_id in track_ids {
        let inserted = sqlx::query(
            "INSERT INTO playlist_tracks (playlist_id, track_id, position)
             VALUES (?, ?, ?)
             ON CONFLICT (playlist_id, track_id) DO NOTHING",
        )
        .bind(playlist_id)
        .bind(track_id)
        .bind(next)
        .execute(&mut *tx)
        .await
        .map_err(|e| describe_track_error(&e, track_id))?
        .rows_affected();

        if inserted > 0 {
            outcome.added += 1;
            next += 1;
        } else {
            outcome.skipped += 1;
        }
    }

    tx.commit().await.map_err(|e| e.to_string())?;

    Ok(outcome)
}

#[tauri::command]
pub async fn remove_track_from_playlist(
    db: State<'_, Db>,
    playlist_id: i64,
    track_id: i64,
) -> Result<(), String> {
    let mut tx = db.pool.begin().await.map_err(|e| e.to_string())?;

    let Some(position) = position_of(&mut tx, playlist_id, track_id).await? else {
        // Already gone: the goal is that it is absent, and it is.
        return Ok(());
    };

    sqlx::query("DELETE FROM playlist_tracks WHERE playlist_id = ? AND track_id = ?")
        .bind(playlist_id)
        .bind(track_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    // Close the gap, so positions stay dense and a UI ordinal keeps matching a
    // stored position.
    sqlx::query(
        "UPDATE playlist_tracks SET position = position - 1
         WHERE playlist_id = ? AND position > ?",
    )
    .bind(playlist_id)
    .bind(position)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    tx.commit().await.map_err(|e| e.to_string())
}

/// Moves a track to `new_position`, shifting everything between.
#[tauri::command]
pub async fn reorder_playlist_track(
    db: State<'_, Db>,
    playlist_id: i64,
    track_id: i64,
    new_position: i64,
) -> Result<(), String> {
    let mut tx = db.pool.begin().await.map_err(|e| e.to_string())?;

    let Some(old_position) = position_of(&mut tx, playlist_id, track_id).await? else {
        return Err("That track is not in this playlist.".to_string());
    };

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM playlist_tracks WHERE playlist_id = ?")
            .bind(playlist_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;

    let new_position = new_position.clamp(0, (count - 1).max(0));

    if new_position != old_position {
        // One statement for the whole move: the row lands on its new position
        // and everything it passed shifts one step the other way. This is why
        // (playlist_id, position) must not be UNIQUE -- mid-statement, two
        // rows briefly share a position.
        sqlx::query(
            "UPDATE playlist_tracks
             SET position = CASE
                 WHEN position = ?2 THEN ?3
                 WHEN ?2 < ?3 AND position > ?2 AND position <= ?3 THEN position - 1
                 WHEN ?2 > ?3 AND position >= ?3 AND position < ?2 THEN position + 1
                 ELSE position
             END
             WHERE playlist_id = ?1",
        )
        .bind(playlist_id)
        .bind(old_position)
        .bind(new_position)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }

    tx.commit().await.map_err(|e| e.to_string())
}

// --- helpers -----------------------------------------------------------

async fn load_playlist(pool: &sqlx::SqlitePool, playlist_id: i64) -> Result<Playlist, String> {
    sqlx::query_as(
        "SELECT p.id, p.name, p.cover_path, p.created_at,
                COUNT(pt.track_id) AS track_count
         FROM playlists p
         LEFT JOIN playlist_tracks pt ON pt.playlist_id = p.id
         WHERE p.id = ?
         GROUP BY p.id",
    )
    .bind(playlist_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "That playlist no longer exists.".to_string())
}

async fn ensure_playlist_exists(
    tx: &mut SqliteConnection,
    playlist_id: i64,
) -> Result<(), String> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM playlists WHERE id = ?")
        .bind(playlist_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    exists
        .map(|_| ())
        .ok_or_else(|| "That playlist no longer exists.".to_string())
}

async fn position_of(
    tx: &mut SqliteConnection,
    playlist_id: i64,
    track_id: i64,
) -> Result<Option<i64>, String> {
    sqlx::query_scalar(
        "SELECT position FROM playlist_tracks WHERE playlist_id = ? AND track_id = ?",
    )
    .bind(playlist_id)
    .bind(track_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| e.to_string())
}

/// Foreign keys are enforced, so a bad track id surfaces as a constraint
/// violation rather than a silent no-op. Say what actually went wrong.
fn describe_track_error(error: &sqlx::Error, track_id: i64) -> String {
    let text = error.to_string();
    if text.contains("FOREIGN KEY") {
        format!("Track {track_id} no longer exists.")
    } else {
        text
    }
}
