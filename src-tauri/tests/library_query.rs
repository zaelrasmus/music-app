//! Tag filtering and full-text search, against real SQLite with real
//! migrations.
//!
//! Everything interesting here is SQL: relational division for AND-semantics,
//! FTS5 diacritic folding, and the triggers that keep the index honest. None of
//! it can be verified without a database.

use sqlx::SqlitePool;

async fn fixture(name: &str) -> (music_app_lib::db::Db, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!("music-app-query-{name}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let db = music_app_lib::db::init(&base).await.unwrap();
    (db, base)
}

async fn add_track(pool: &SqlitePool, title: &str, artist: Option<&str>, album: Option<&str>) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO tracks (source, title, artist, album, local_path, state)
         VALUES ('local', ?, ?, ?, ?, 'present') RETURNING id",
    )
    .bind(title)
    .bind(artist)
    .bind(album)
    .bind(format!("D:\\music\\{title}.mp3"))
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn add_tag(pool: &SqlitePool, name: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO tags (name, name_key) VALUES (?, ?)
         ON CONFLICT (name_key) DO UPDATE SET name = name RETURNING id",
    )
    .bind(name)
    .bind(name.trim().to_lowercase())
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn tag_track(pool: &SqlitePool, track: i64, tag: i64) {
    sqlx::query("INSERT INTO track_tags (track_id, tag_id) VALUES (?, ?)")
        .bind(track)
        .bind(tag)
        .execute(pool)
        .await
        .unwrap();
}

/// The same expression `to_fts_expression` builds.
fn fts(input: &str) -> String {
    input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect::<Vec<_>>()
        .join(" AND ")
}

async fn search(pool: &SqlitePool, query: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT t.title FROM tracks t
         JOIN tracks_fts ON tracks_fts.rowid = t.id
         WHERE tracks_fts MATCH ?
         ORDER BY bm25(tracks_fts, 10.0, 5.0, 2.0)",
    )
    .bind(fts(query))
    .fetch_all(pool)
    .await
    .unwrap()
}

// --- full-text search --------------------------------------------------

/// The reason for `remove_diacritics 2`: nobody types the accent.
#[tokio::test]
async fn search_ignores_accents_in_both_directions() {
    let (db, base) = fixture("accents").await;
    add_track(&db.pool, "Canción Triste", Some("Café Tacvba"), None).await;

    assert_eq!(search(&db.pool, "cancion").await.len(), 1, "unaccented query");
    assert_eq!(search(&db.pool, "canción").await.len(), 1, "accented query");
    assert_eq!(search(&db.pool, "cafe").await.len(), 1, "artist, unaccented");

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn search_matches_prefixes_so_it_works_while_typing() {
    let (db, base) = fixture("prefix").await;
    add_track(&db.pool, "Paranoid Android", Some("Radiohead"), None).await;

    for query in ["par", "para", "parano", "radioh"] {
        assert_eq!(search(&db.pool, query).await.len(), 1, "query {query}");
    }

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn multiple_words_narrow_rather_than_widen() {
    let (db, base) = fixture("narrow").await;
    add_track(&db.pool, "Paranoid Android", Some("Radiohead"), None).await;
    add_track(&db.pool, "Paranoid", Some("Black Sabbath"), None).await;

    assert_eq!(search(&db.pool, "paranoid").await.len(), 2);
    assert_eq!(
        search(&db.pool, "paranoid radiohead").await.len(),
        1,
        "adding a word must AND, not OR"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Editing a title must be searchable immediately -- that is what the AFTER
/// UPDATE trigger is for.
#[tokio::test]
async fn editing_a_track_updates_the_index() {
    let (db, base) = fixture("update").await;
    let id = add_track(&db.pool, "Wrong Title", Some("Someone"), None).await;

    sqlx::query("UPDATE tracks SET title = 'Bohemian Rhapsody' WHERE id = ?")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();

    assert_eq!(search(&db.pool, "bohemian").await.len(), 1, "new title found");
    assert!(
        search(&db.pool, "wrong").await.is_empty(),
        "the old title must be gone from the index"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The scanner touches these columns for every unchanged file on every rescan.
/// The trigger is scoped to the indexed columns so that stays cheap -- and, as
/// this checks, correct.
#[tokio::test]
async fn a_scan_style_update_leaves_the_index_intact() {
    let (db, base) = fixture("scan-update").await;
    add_track(&db.pool, "Paranoid Android", Some("Radiohead"), None).await;

    sqlx::query("UPDATE tracks SET last_seen_scan = 42, state = 'present'")
        .execute(&db.pool)
        .await
        .unwrap();

    assert_eq!(
        search(&db.pool, "paranoid").await.len(),
        1,
        "an unrelated column change must not disturb the index"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn a_deleted_track_leaves_the_index() {
    let (db, base) = fixture("delete").await;
    let id = add_track(&db.pool, "Temporary", None, None).await;

    sqlx::query("DELETE FROM tracks WHERE id = ?")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();

    assert!(search(&db.pool, "temporary").await.is_empty());

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Weighting exists so a title match beats an album match on the same word.
#[tokio::test]
async fn a_title_hit_outranks_an_album_hit() {
    let (db, base) = fixture("ranking").await;
    add_track(&db.pool, "Something Else", Some("A"), Some("Grace")).await;
    add_track(&db.pool, "Grace", Some("B"), Some("Other")).await;

    let results = search(&db.pool, "grace").await;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0], "Grace", "the title match should rank first");

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

// --- tag filtering -----------------------------------------------------

async fn by_tags_all(pool: &SqlitePool, tags: &[i64]) -> Vec<String> {
    // The same relational division `query_library` builds.
    let placeholders = tags.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT t.title FROM tracks t
         JOIN track_tags tt ON tt.track_id = t.id
         WHERE tt.tag_id IN ({placeholders})
         GROUP BY t.id
         HAVING COUNT(DISTINCT tt.tag_id) = ?
         ORDER BY t.title"
    );

    let mut query = sqlx::query_scalar(sqlx::AssertSqlSafe(sql));
    for tag in tags {
        query = query.bind(*tag);
    }
    query.bind(tags.len() as i64).fetch_all(pool).await.unwrap()
}

#[tokio::test]
async fn filtering_by_several_tags_requires_all_of_them() {
    let (db, base) = fixture("tags-and").await;

    let chill = add_tag(&db.pool, "Chill").await;
    let spanish = add_tag(&db.pool, "Spanish").await;

    let both = add_track(&db.pool, "Both", None, None).await;
    let only_chill = add_track(&db.pool, "OnlyChill", None, None).await;

    tag_track(&db.pool, both, chill).await;
    tag_track(&db.pool, both, spanish).await;
    tag_track(&db.pool, only_chill, chill).await;

    assert_eq!(by_tags_all(&db.pool, &[chill]).await, vec!["Both", "OnlyChill"]);
    assert_eq!(
        by_tags_all(&db.pool, &[chill, spanish]).await,
        vec!["Both"],
        "AND semantics: only the track carrying both"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The whole point of `name_key`: SQLite's NOCASE would fold neither of these.
#[tokio::test]
async fn accented_tag_names_do_not_duplicate() {
    let (db, base) = fixture("tag-case").await;

    let first = add_tag(&db.pool, "Canción").await;
    let second = add_tag(&db.pool, "CANCIÓN").await;
    let third = add_tag(&db.pool, "  canción  ").await;

    assert_eq!(first, second, "case must not create a second tag");
    assert_eq!(first, third, "nor should surrounding space");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn deleting_a_tag_keeps_the_tracks() {
    let (db, base) = fixture("tag-delete").await;

    let tag = add_tag(&db.pool, "Temporary").await;
    let track = add_track(&db.pool, "Kept", None, None).await;
    tag_track(&db.pool, track, tag).await;

    sqlx::query("DELETE FROM tags WHERE id = ?")
        .bind(tag)
        .execute(&db.pool)
        .await
        .unwrap();

    let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track_tags")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let tracks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    assert_eq!(links, 0);
    assert_eq!(tracks, 1);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}
