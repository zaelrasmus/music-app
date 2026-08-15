use serde::Serialize;
use sqlx::{FromRow, SqlitePool};
use tauri::State;

use crate::db::Db;

/// The colours a tag may be given.
///
/// A closed list rather than free-form input. Each name resolves to a hue in
/// the frontend palette, where lightness and chroma come from the active theme
/// -- which is the whole reason a chip stays readable in both light and dark.
/// An arbitrary colour could not make that promise.
pub const TAG_COLORS: [&str; 10] = [
    "rose", "orange", "amber", "lime", "emerald", "teal", "sky", "indigo", "violet", "fuchsia",
];

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    pub id: i64,
    pub name: String,
    /// How many tracks carry it, so unused tags are visible.
    pub track_count: i64,
    /// A name from [`TAG_COLORS`], or `None` for "pick one for me".
    pub color: Option<String>,
}

/// One tag on one track. Returned flat for the whole library in a single query
/// -- the alternative is one request per row to render chips.
#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct TrackTag {
    pub track_id: i64,
    pub tag_id: i64,
    pub name: String,
    pub color: Option<String>,
}

/// Uniqueness key for a tag name.
///
/// `to_lowercase` is Unicode-aware, so "CANCIÓN" and "canción" collapse to one
/// tag. SQLite's NOCASE collation would not: it folds ASCII only.
fn tag_key(name: &str) -> String {
    name.trim().to_lowercase()
}

fn clean_tag_name(name: &str) -> Result<(String, String), String> {
    let display = name.trim();
    if display.is_empty() {
        return Err("A tag needs a name.".to_string());
    }
    if display.chars().count() > 60 {
        return Err("That tag name is too long.".to_string());
    }

    Ok((display.to_string(), tag_key(display)))
}

/// Attaches a tag to a track, creating the tag if it does not exist yet.
#[tauri::command]
pub async fn assign_tag(db: State<'_, Db>, track_id: i64, name: String) -> Result<Tag, String> {
    let (display, key) = clean_tag_name(&name)?;

    let mut tx = db.pool.begin().await.map_err(|e| e.to_string())?;

    // Get-or-create in one statement. A SELECT-then-INSERT would race with
    // itself; the no-op SET exists purely so RETURNING fires on the conflict
    // path too.
    let tag_id: i64 = sqlx::query_scalar(
        "INSERT INTO tags (name, name_key) VALUES (?, ?)
         ON CONFLICT (name_key) DO UPDATE SET name = name
         RETURNING id",
    )
    .bind(&display)
    .bind(&key)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query(
        "INSERT INTO track_tags (track_id, tag_id) VALUES (?, ?)
         ON CONFLICT (track_id, tag_id) DO NOTHING",
    )
    .bind(track_id)
    .bind(tag_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if e.to_string().contains("FOREIGN KEY") {
            "That track no longer exists.".to_string()
        } else {
            e.to_string()
        }
    })?;

    tx.commit().await.map_err(|e| e.to_string())?;

    load_tag(&db.pool, tag_id).await
}

#[tauri::command]
pub async fn remove_tag_from_track(
    db: State<'_, Db>,
    track_id: i64,
    tag_id: i64,
) -> Result<(), String> {
    sqlx::query("DELETE FROM track_tags WHERE track_id = ? AND tag_id = ?")
        .bind(track_id)
        .bind(tag_id)
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub async fn list_tags(db: State<'_, Db>) -> Result<Vec<Tag>, String> {
    sqlx::query_as(
        "SELECT g.id, g.name, g.color, COUNT(tt.track_id) AS track_count
         FROM tags g
         LEFT JOIN track_tags tt ON tt.tag_id = g.id
         GROUP BY g.id
         ORDER BY g.name_key",
    )
    .fetch_all(&db.pool)
    .await
    .map_err(|e| e.to_string())
}

/// Every (track, tag) pair in the library, in one round trip.
///
/// The UI needs chips on every row; fetching per row would be one IPC call per
/// track. Tags are few and short, so the whole set is cheaper than the calls.
#[tauri::command]
pub async fn list_track_tags(db: State<'_, Db>) -> Result<Vec<TrackTag>, String> {
    sqlx::query_as(
        "SELECT tt.track_id, tt.tag_id, g.name, g.color
         FROM track_tags tt
         JOIN tags g ON g.id = tt.tag_id
         ORDER BY g.name_key",
    )
    .fetch_all(&db.pool)
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn rename_tag(db: State<'_, Db>, tag_id: i64, name: String) -> Result<Tag, String> {
    let (display, key) = clean_tag_name(&name)?;

    let result = sqlx::query("UPDATE tags SET name = ?, name_key = ? WHERE id = ?")
        .bind(&display)
        .bind(&key)
        .bind(tag_id)
        .execute(&db.pool)
        .await;

    match result {
        Ok(outcome) if outcome.rows_affected() == 0 => {
            Err("That tag no longer exists.".to_string())
        }
        Ok(_) => load_tag(&db.pool, tag_id).await,
        Err(e) if e.to_string().contains("UNIQUE") => {
            // Renaming onto an existing name would need merging the two tags'
            // tracks, which is a different operation with different intent.
            Err(format!("A tag called \"{display}\" already exists."))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Checks a colour against the palette.
///
/// `None` is legal and means "unset it, colour this one automatically again".
/// Anything else has to be a name we can actually render -- storing a name the
/// frontend does not know would leave the chip silently uncoloured.
fn clean_tag_color(color: Option<&str>) -> Result<Option<String>, String> {
    let Some(color) = color else {
        return Ok(None);
    };

    if TAG_COLORS.contains(&color) {
        Ok(Some(color.to_string()))
    } else {
        Err(format!("\"{color}\" is not a colour this app knows."))
    }
}

/// Sets a tag's colour, or clears it back to automatic with `None`.
#[tauri::command]
pub async fn set_tag_color(
    db: State<'_, Db>,
    tag_id: i64,
    color: Option<String>,
) -> Result<Tag, String> {
    let color = clean_tag_color(color.as_deref())?;

    let outcome = sqlx::query("UPDATE tags SET color = ? WHERE id = ?")
        .bind(&color)
        .bind(tag_id)
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    if outcome.rows_affected() == 0 {
        return Err("That tag no longer exists.".to_string());
    }

    load_tag(&db.pool, tag_id).await
}

/// The palette, so the picker does not have to hardcode its own copy.
#[tauri::command]
pub fn list_tag_colors() -> Vec<String> {
    TAG_COLORS.iter().map(|c| c.to_string()).collect()
}

#[tauri::command]
pub async fn delete_tag(db: State<'_, Db>, tag_id: i64) -> Result<(), String> {
    // track_tags rows go with it via ON DELETE CASCADE; tracks are untouched.
    sqlx::query("DELETE FROM tags WHERE id = ?")
        .bind(tag_id)
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

async fn load_tag(pool: &SqlitePool, tag_id: i64) -> Result<Tag, String> {
    sqlx::query_as(
        "SELECT g.id, g.name, g.color, COUNT(tt.track_id) AS track_count
         FROM tags g
         LEFT JOIN track_tags tt ON tt.tag_id = g.id
         WHERE g.id = ?
         GROUP BY g.id",
    )
    .bind(tag_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "That tag no longer exists.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason this is not `COLLATE NOCASE`.
    #[test]
    fn accented_names_fold_to_one_key() {
        assert_eq!(tag_key("Canción"), tag_key("CANCIÓN"));
        assert_eq!(tag_key("Café"), tag_key("CAFÉ"));
        assert_eq!(tag_key("  Rock  "), tag_key("rock"));
    }

    #[test]
    fn distinct_names_keep_distinct_keys() {
        assert_ne!(tag_key("rock"), tag_key("rocks"));
    }

    #[test]
    fn a_blank_name_is_rejected() {
        assert!(clean_tag_name("   ").is_err());
        assert!(clean_tag_name("").is_err());
    }

    #[test]
    fn a_colour_outside_the_palette_is_refused() {
        // Not pedantry: the frontend maps a name to a hue, so a name it does
        // not know renders as no colour at all rather than as an error.
        assert!(clean_tag_color(Some("#ff00ff")).is_err());
        assert!(clean_tag_color(Some("puce")).is_err());
        assert!(clean_tag_color(Some("Emerald")).is_err());
    }

    #[test]
    fn every_palette_colour_is_accepted_and_none_clears() {
        for color in TAG_COLORS {
            assert_eq!(clean_tag_color(Some(color)).unwrap().as_deref(), Some(color));
        }
        assert_eq!(clean_tag_color(None).unwrap(), None);
    }

    #[test]
    fn display_casing_survives_normalisation() {
        let (display, key) = clean_tag_name("  Slowed + Reverb ").unwrap();
        assert_eq!(display, "Slowed + Reverb");
        assert_eq!(key, "slowed + reverb");
    }
}
