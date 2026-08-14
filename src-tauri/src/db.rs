use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::SqlitePool;

/// Managed Tauri state. Commands take this as `State<'_, Db>`.
pub struct Db {
    pub pool: SqlitePool,
}

/// Opens (creating if needed) the library database and runs pending migrations.
///
/// The PRAGMAs live on `SqliteConnectOptions` rather than being executed after
/// the pool is built, because sqlx applies connect options to *every* connection
/// the pool opens. `foreign_keys` is per-connection and defaults to OFF, so
/// running it once against the pool would leave the other connections without
/// constraint enforcement.
pub async fn init(app_data_dir: &Path) -> Result<Db, Box<dyn std::error::Error>> {
    // `create_if_missing` creates the database file, not its parent directory,
    // and app_data_dir does not exist yet on a fresh install.
    std::fs::create_dir_all(app_data_dir)?;

    let options = SqliteConnectOptions::new()
        .filename(app_data_dir.join("library.db"))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        // SQLite serializes writers; wait rather than erroring with SQLITE_BUSY.
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(Db { pool })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards against a malformed or conflicting migration reaching a release.
    #[tokio::test]
    async fn migrations_apply_to_a_fresh_database() {
        let dir = std::env::temp_dir().join(format!("music-app-test-{}", std::process::id()));
        let db = init(&dir).await.expect("migrations should apply cleanly");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_folders")
            .fetch_one(&db.pool)
            .await
            .expect("library_folders should exist");

        assert_eq!(count, 0);

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The upgrade path for a database that already has tracks in it.
    ///
    /// The fresh-database test above cannot catch any of what follows: 0007
    /// rebuilds `tracks`, and a rebuild is where rows get dropped, `remote_url`
    /// fails to backfill, or -- the failure 0006 warns about in capitals -- the
    /// FTS index is left orphaned and searches quietly return nothing.
    ///
    /// Applies the migration SQL directly rather than through sqlx's migrator,
    /// because the risk being tested is in the SQL, and the migrator's own
    /// bookkeeping is already covered above.
    #[tokio::test]
    async fn upgrading_a_populated_database_keeps_its_tracks_and_its_search() {
        // One connection: an in-memory database is per-connection, so a pool
        // would hand each query its own empty database.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory database");

        // --- the world as it was before this migration ---
        for file in [
            "0001_library_folders.sql",
            "0002_tracks.sql",
            "0003_track_state_constraints.sql",
            "0004_playlists.sql",
            "0005_tags.sql",
            "0006_track_search.sql",
        ] {
            let sql = std::fs::read_to_string(format!("./migrations/{file}"))
                .unwrap_or_else(|e| panic!("reading {file}: {e}"));
            // Read from disk, so not a `&'static str`. These are the project's
            // own migration files, not anything user-supplied.
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("applying {file}: {e}"));
        }

        sqlx::raw_sql(
            "INSERT INTO tracks (source, title, artist, local_path, state)
             VALUES ('local', 'Paranoid Android', 'Radiohead', 'D:/music/pa.mp3', 'present');
             INSERT INTO tracks (source, title, state, yt_video_id, yt_channel)
             VALUES ('youtube', 'Never Gonna Give You Up', 'saved',
                     'dQw4w9WgXcQ', 'Rick Astley');",
        )
        .execute(&pool)
        .await
        .expect("seeding pre-migration rows");

        // --- the migration under test ---
        let sql = std::fs::read_to_string("./migrations/0007_multi_source_tracks.sql").unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(&pool)
            .await
            .expect("0007 should apply to a populated database");

        // Nothing lost in the rebuild.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 2, "the rebuild dropped rows");

        // A YouTube URL *is* derivable from its id, which is what makes the
        // backfill possible at all -- and precisely the assumption that stops
        // holding for SoundCloud.
        let (remote_id, remote_url): (String, String) = sqlx::query_as(
            "SELECT remote_id, remote_url FROM tracks WHERE source = 'youtube'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(remote_id, "dQw4w9WgXcQ");
        assert_eq!(remote_url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");

        // Local rows keep NULL remote columns and their path.
        let local_path: String =
            sqlx::query_scalar("SELECT local_path FROM tracks WHERE source = 'local'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(local_path, "D:/music/pa.mp3");

        // --- the silent failure 0006 warns about ---
        let hits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tracks_fts WHERE tracks_fts MATCH 'paranoid'",
        )
        .fetch_one(&pool)
        .await
        .expect("tracks_fts must still exist and be queryable");
        assert_eq!(hits, 1, "the FTS index was not backfilled after the rebuild");

        // And the triggers came back, not just the table.
        sqlx::raw_sql(
            "INSERT INTO tracks (source, title, local_path, state)
             VALUES ('local', 'Weird Fishes', 'D:/music/wf.mp3', 'present');",
        )
        .execute(&pool)
        .await
        .unwrap();

        let hits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tracks_fts WHERE tracks_fts MATCH 'fishes'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(hits, 1, "the AFTER INSERT trigger was not recreated");

        pool.close().await;
    }

    /// The point of `UNIQUE (source, remote_id)` rather than a global unique
    /// column: SoundCloud ids are plain integers, so one provider's id must not
    /// be able to block another's forever.
    #[tokio::test]
    async fn the_same_remote_id_is_allowed_on_two_different_providers() {
        let dir = std::env::temp_dir().join(format!("music-app-ids-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = init(&dir).await.expect("migrations apply");

        sqlx::raw_sql(
            "INSERT INTO tracks (source, title, state, remote_id, remote_url)
             VALUES ('soundcloud', 'One More Time', 'saved', '199428706',
                     'https://soundcloud.com/daft-punk-id/one-more-time');",
        )
        .execute(&db.pool)
        .await
        .expect("a SoundCloud track inserts");

        // Same id, different provider.
        sqlx::raw_sql(
            "INSERT INTO tracks (source, title, state, remote_id, remote_url)
             VALUES ('youtube', 'Something Else', 'saved', '199428706',
                     'https://www.youtube.com/watch?v=dQw4w9WgXcQ');",
        )
        .execute(&db.pool)
        .await
        .expect("the same id on another provider must not collide");

        // Same id *and* provider still collides, which is what makes saving
        // idempotent.
        let duplicate = sqlx::raw_sql(
            "INSERT INTO tracks (source, title, state, remote_id, remote_url)
             VALUES ('soundcloud', 'Duplicate', 'saved', '199428706',
                     'https://soundcloud.com/daft-punk-id/one-more-time');",
        )
        .execute(&db.pool)
        .await;
        assert!(duplicate.is_err(), "a provider's own ids must stay unique");

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A remote row with no URL is unplayable, so the schema refuses it rather
    /// than letting it fail later at resolve time.
    #[tokio::test]
    async fn a_remote_track_without_a_url_is_rejected() {
        let dir = std::env::temp_dir().join(format!("music-app-nourl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = init(&dir).await.expect("migrations apply");

        let result = sqlx::raw_sql(
            "INSERT INTO tracks (source, title, state, remote_id)
             VALUES ('soundcloud', 'No URL', 'saved', '199428706');",
        )
        .execute(&db.pool)
        .await;

        assert!(result.is_err(), "remote rows must carry a URL");

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
