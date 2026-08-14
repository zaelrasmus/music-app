//! Playlist ordering, exercised through the real SQL.
//!
//! Reordering is a single span-shifting UPDATE and removal closes its gap, so
//! the invariant worth protecting is that positions stay dense and the order
//! is what the user asked for. These run against a real SQLite file with the
//! real migrations, because the interesting behaviour lives in the SQL.

use sqlx::SqlitePool;

async fn fixture(name: &str) -> (music_app_lib::db::Db, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!("music-app-playlists-{name}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let db = music_app_lib::db::init(&base).await.unwrap();
    (db, base)
}

/// Local tracks are the cheapest to create and the ordering logic is
/// source-agnostic.
async fn seed_tracks(pool: &SqlitePool, count: usize) -> Vec<i64> {
    let mut ids = Vec::new();

    for index in 0..count {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO tracks (source, title, local_path, state)
             VALUES ('local', ?, ?, 'present') RETURNING id",
        )
        .bind(format!("Track {index}"))
        .bind(format!("D:\\music\\{index}.mp3"))
        .fetch_one(pool)
        .await
        .unwrap();
        ids.push(id);
    }

    ids
}

async fn new_playlist(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("INSERT INTO playlists (name) VALUES ('Test') RETURNING id")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn add(pool: &SqlitePool, playlist: i64, tracks: &[i64]) {
    let mut next: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_tracks WHERE playlist_id = ?",
    )
    .bind(playlist)
    .fetch_one(pool)
    .await
    .unwrap();

    for track in tracks {
        sqlx::query(
            "INSERT INTO playlist_tracks (playlist_id, track_id, position)
             VALUES (?, ?, ?) ON CONFLICT (playlist_id, track_id) DO NOTHING",
        )
        .bind(playlist)
        .bind(track)
        .bind(next)
        .execute(pool)
        .await
        .unwrap();
        next += 1;
    }
}

/// The move, exactly as `reorder_playlist_track` issues it.
async fn move_track(pool: &SqlitePool, playlist: i64, track: i64, new_position: i64) {
    let old: i64 = sqlx::query_scalar(
        "SELECT position FROM playlist_tracks WHERE playlist_id = ? AND track_id = ?",
    )
    .bind(playlist)
    .bind(track)
    .fetch_one(pool)
    .await
    .unwrap();

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
    .bind(playlist)
    .bind(old)
    .bind(new_position)
    .execute(pool)
    .await
    .unwrap();
}

async fn order(pool: &SqlitePool, playlist: i64) -> Vec<i64> {
    sqlx::query_scalar(
        "SELECT track_id FROM playlist_tracks WHERE playlist_id = ? ORDER BY position",
    )
    .bind(playlist)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn positions(pool: &SqlitePool, playlist: i64) -> Vec<i64> {
    sqlx::query_scalar(
        "SELECT position FROM playlist_tracks WHERE playlist_id = ? ORDER BY position",
    )
    .bind(playlist)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn moving_a_track_down_shifts_the_span_up() {
    let (db, base) = fixture("move-down").await;
    let tracks = seed_tracks(&db.pool, 5).await;
    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &tracks).await;

    // [0 1 2 3 4] -> move index 1 to index 3 -> [0 2 3 1 4]
    move_track(&db.pool, playlist, tracks[1], 3).await;

    assert_eq!(
        order(&db.pool, playlist).await,
        vec![tracks[0], tracks[2], tracks[3], tracks[1], tracks[4]]
    );
    assert_eq!(
        positions(&db.pool, playlist).await,
        vec![0, 1, 2, 3, 4],
        "positions must stay dense"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn moving_a_track_up_shifts_the_span_down() {
    let (db, base) = fixture("move-up").await;
    let tracks = seed_tracks(&db.pool, 5).await;
    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &tracks).await;

    // [0 1 2 3 4] -> move index 3 to index 1 -> [0 3 1 2 4]
    move_track(&db.pool, playlist, tracks[3], 1).await;

    assert_eq!(
        order(&db.pool, playlist).await,
        vec![tracks[0], tracks[3], tracks[1], tracks[2], tracks[4]]
    );
    assert_eq!(positions(&db.pool, playlist).await, vec![0, 1, 2, 3, 4]);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The edges are where off-by-one errors live.
#[tokio::test]
async fn moving_to_either_end_works() {
    let (db, base) = fixture("move-ends").await;
    let tracks = seed_tracks(&db.pool, 4).await;
    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &tracks).await;

    move_track(&db.pool, playlist, tracks[3], 0).await;
    assert_eq!(
        order(&db.pool, playlist).await,
        vec![tracks[3], tracks[0], tracks[1], tracks[2]]
    );

    move_track(&db.pool, playlist, tracks[3], 3).await;
    assert_eq!(
        order(&db.pool, playlist).await,
        vec![tracks[0], tracks[1], tracks[2], tracks[3]]
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// A no-op move must not scramble anything.
#[tokio::test]
async fn moving_a_track_onto_itself_changes_nothing() {
    let (db, base) = fixture("move-self").await;
    let tracks = seed_tracks(&db.pool, 4).await;
    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &tracks).await;

    move_track(&db.pool, playlist, tracks[2], 2).await;

    assert_eq!(order(&db.pool, playlist).await, tracks);
    assert_eq!(positions(&db.pool, playlist).await, vec![0, 1, 2, 3]);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Removing must close its gap, or a UI ordinal stops matching a stored
/// position and every later reorder lands in the wrong place.
#[tokio::test]
async fn removing_a_track_keeps_positions_dense() {
    let (db, base) = fixture("remove").await;
    let tracks = seed_tracks(&db.pool, 5).await;
    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &tracks).await;

    let removed_position: i64 = sqlx::query_scalar(
        "SELECT position FROM playlist_tracks WHERE playlist_id = ? AND track_id = ?",
    )
    .bind(playlist)
    .bind(tracks[1])
    .fetch_one(&db.pool)
    .await
    .unwrap();

    sqlx::query("DELETE FROM playlist_tracks WHERE playlist_id = ? AND track_id = ?")
        .bind(playlist)
        .bind(tracks[1])
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE playlist_tracks SET position = position - 1
         WHERE playlist_id = ? AND position > ?",
    )
    .bind(playlist)
    .bind(removed_position)
    .execute(&db.pool)
    .await
    .unwrap();

    assert_eq!(
        order(&db.pool, playlist).await,
        vec![tracks[0], tracks[2], tracks[3], tracks[4]]
    );
    assert_eq!(positions(&db.pool, playlist).await, vec![0, 1, 2, 3]);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The composite primary key is the whole duplicate story.
#[tokio::test]
async fn the_same_track_cannot_be_added_twice() {
    let (db, base) = fixture("duplicates").await;
    let tracks = seed_tracks(&db.pool, 2).await;
    let playlist = new_playlist(&db.pool).await;

    add(&db.pool, playlist, &tracks).await;
    add(&db.pool, playlist, &tracks).await;

    assert_eq!(order(&db.pool, playlist).await, tracks);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Deleting a playlist takes its membership rows and leaves the tracks alone.
#[tokio::test]
async fn deleting_a_playlist_does_not_delete_its_tracks() {
    let (db, base) = fixture("cascade").await;
    let tracks = seed_tracks(&db.pool, 3).await;
    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &tracks).await;

    sqlx::query("DELETE FROM playlists WHERE id = ?")
        .bind(playlist)
        .execute(&db.pool)
        .await
        .unwrap();

    let memberships: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlist_tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let surviving: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    assert_eq!(memberships, 0, "membership rows should cascade away");
    assert_eq!(surviving, 3, "the tracks themselves must survive");

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// A playlist is allowed to hold all three kinds of track at once. This is the
/// shape the player has to cope with, and the reason playlists exist at all.
#[tokio::test]
async fn a_playlist_can_mix_local_downloaded_and_streamed_tracks() {
    let (db, base) = fixture("mixed").await;

    let local: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, local_path, state)
         VALUES ('local', 'Local', 'D:\\a.mp3', 'present') RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let downloaded: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, state, yt_video_id, local_path)
         VALUES ('youtube', 'Downloaded', 'downloaded', 'aaaaaaaaaaa', 'D:\\b.m4a')
         RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let streamed: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, state, yt_video_id)
         VALUES ('youtube', 'Streamed', 'saved', 'bbbbbbbbbbb') RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &[local, downloaded, streamed]).await;

    assert_eq!(
        order(&db.pool, playlist).await,
        vec![local, downloaded, streamed]
    );

    // The player receives exactly this: a list of ids, resolved one at a time
    // through `get_playable_source`. No playlist-specific playback path.
    let states: Vec<String> = sqlx::query_scalar(
        "SELECT t.state FROM playlist_tracks pt JOIN tracks t ON t.id = pt.track_id
         WHERE pt.playlist_id = ? ORDER BY pt.position",
    )
    .bind(playlist)
    .fetch_all(&db.pool)
    .await
    .unwrap();

    assert_eq!(states, vec!["present", "downloaded", "saved"]);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}
