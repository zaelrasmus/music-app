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
}
