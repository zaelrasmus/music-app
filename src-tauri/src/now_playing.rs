//! Keeps the system media panel in step with what is actually playing.
//!
//! A [`PlayerEvents`] sink that wraps the ordinary one. Everything still goes
//! to the webview exactly as before; this additionally translates the parts
//! Windows cares about and sends them to the media-controls thread.
//!
//! Wrapping rather than hooking the coordinator is deliberate. The coordinator
//! has no `AppHandle` and no opinion about platforms, and it should stay that
//! way -- "there is a panel above the volume slider that wants to know the
//! album name" is not a fact about queue management.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::SqlitePool;
use tauri::{AppHandle, Runtime};

use crate::media_controls::{MediaBridge, Update};
use crate::player::{PlaybackState, PlayerEvents, PlayerProgress, PlayerStatus, QueueState};

/// Hand-written rather than derived: `derive(Clone)` would add a `R: Clone`
/// bound, and a Tauri `Runtime` is not `Clone` even though `AppHandle<R>` is.
pub struct NowPlaying<R: Runtime> {
    app: AppHandle<R>,
    bridge: MediaBridge,
    pool: SqlitePool,
    covers: PathBuf,
    /// The last track pushed to the panel, so an unchanged one is not looked
    /// up again -- `state` fires on every pause, volume nudge and queue edit.
    shown: Arc<Mutex<Option<i64>>>,
    /// The most recent position, so a pause can report where it paused.
    ///
    /// Taken from progress events rather than asked for, because the panel is
    /// updated from `state`, which does not carry one.
    position: Arc<Mutex<Duration>>,
}

impl<R: Runtime> NowPlaying<R> {
    pub fn new(
        app: AppHandle<R>,
        bridge: MediaBridge,
        pool: SqlitePool,
        covers: PathBuf,
    ) -> Self {
        Self {
            app,
            bridge,
            pool,
            covers,
            shown: Arc::new(Mutex::new(None)),
            position: Arc::new(Mutex::new(Duration::ZERO)),
        }
    }

    /// Looks the track up and hands it to the panel.
    ///
    /// Off the calling thread because this runs from the coordinator's own
    /// loop: a database round trip there would stall the thing that answers
    /// the next-track question.
    fn announce(&self, track_id: i64) {
        let pool = self.pool.clone();
        let bridge = self.bridge.clone();
        let covers = self.covers.clone();

        tauri::async_runtime::spawn(async move {
            // `i64`, because that is what the column is. Asking sqlx for an
            // `f64` from an INTEGER column is a decode error rather than a
            // conversion, and the `let Ok(Some(..))` below turns that into a
            // silent early return -- so nothing was ever sent, and Windows
            // showed the application identifier where the title belongs.
            let row = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<i64>, Option<String>)>(
                "SELECT title, artist, album, duration_secs, cover_key FROM tracks WHERE id = ?",
            )
            .bind(track_id)
            .fetch_optional(&pool)
            .await;

            let Ok(Some((title, artist, album, duration_secs, cover_key))) = row else {
                return;
            };

            // A `file://` prefix on the *raw* path, deliberately not a real URL.
            //
            // souvlaki strips the prefix with `trim_start_matches` and hands
            // what is left straight to a Win32 API, so anything a proper URL
            // encoder does is damage: `Url::from_file_path` turns a space into
            // `%20` and prepends a slash before the drive letter, and the
            // resulting `/D:/My%20Music/...` opens nothing.
            //
            // That matters more than a missing thumbnail. The cover is loaded
            // *before* the display updater is committed, so a path that fails
            // takes the title, artist and album down with it -- an empty panel
            // rather than one without artwork.
            // Real artwork if the track has any, and the same gradient the
            // app draws in its own tiles if it has not. Most of a scanned
            // library carries no embedded art, so without the fallback the
            // panel is blank for nearly every local file -- and a blank panel
            // is one Windows has little reason to keep the media keys pointed
            // at.
            let cover_path = cover_key
                .map(|key| covers.join(key))
                .filter(|path| path.exists())
                .or_else(|| {
                    let seed = crate::placeholder_art::seed_for(&title, artist.as_deref());
                    crate::placeholder_art::ensure(&covers.join("generated"), &seed)
                });

            let cover_url = cover_path.map(|path| format!("file://{}", path.display()));

            bridge.send(Update::Track {
                title,
                artist,
                album,
                duration: duration_secs
                    .filter(|secs| *secs > 0)
                    .map(|secs| Duration::from_secs(secs as u64)),
                cover_url,
            });
        });
    }
}

impl<R: Runtime> PlayerEvents for NowPlaying<R> {
    fn state(&self, status: PlayerStatus) {
        // The panel first, so a media-key press is reflected without waiting
        // on the webview.
        let track_id = status.track_id;
        let changed = {
            let mut shown = self.shown.lock().unwrap();
            if *shown != track_id {
                *shown = track_id;
                true
            } else {
                false
            }
        };

        if changed {
            if let Some(track_id) = track_id {
                self.announce(track_id);
            }
        }

        let at = *self.position.lock().unwrap();
        self.bridge.send(match status.state {
            PlaybackState::Playing => Update::Playing(at),
            // Loading counts as playing: the panel showing "paused" for the
            // seconds a stream takes to resolve invites a second press of a
            // button that is already doing the right thing.
            PlaybackState::Loading => Update::Playing(at),
            PlaybackState::Paused => Update::Paused(at),
            PlaybackState::Stopped => Update::Stopped,
        });

        self.app.state(status);
    }

    fn progress(&self, progress: PlayerProgress) {
        // Recorded, not forwarded. The panel is told where things are when the
        // state changes; sending it five updates a second would be a great
        // deal of traffic for a readout Windows extrapolates on its own.
        *self.position.lock().unwrap() = Duration::from_secs_f64(progress.position_secs.max(0.0));
        self.app.progress(progress);
    }

    fn error(&self, message: String) {
        self.app.error(message);
    }

    fn queue(&self, queue: QueueState) {
        self.app.queue(queue);
    }

    fn caching(&self, track_id: i64, title: Option<String>) {
        self.app.caching(track_id, title);
    }
}

impl<R: Runtime> Clone for NowPlaying<R> {
    fn clone(&self) -> Self {
        Self {
            app: self.app.clone(),
            bridge: self.bridge.clone(),
            pool: self.pool.clone(),
            covers: self.covers.clone(),
            shown: Arc::clone(&self.shown),
            position: Arc::clone(&self.position),
        }
    }
}

/// The query that feeds the Windows media panel.
///
/// The panel showed the app's own identifier where Spotify shows a title, an
/// artist and a cover. That is what Windows falls back to when a session has
/// no metadata at all -- so the question was never "why does it look plain",
/// it was "why is nothing being sent".
#[cfg(test)]
mod announce_tests {
    use super::*;

    async fn pool_with_track(name: &str) -> SqlitePool {
        let dir = std::env::temp_dir().join(format!("music-app-now-playing-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::init(&dir).await.unwrap();

        sqlx::query(
            "INSERT INTO tracks (source, title, artist, album, local_path, state, \
             duration_secs, cover_key, in_library) \
             VALUES ('local', 'Unravel', 'ALESTI', 'Singles', '/tmp/x.wav', 'present', \
                     241, 'abc.jpg', 1)",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        db.pool
    }

    /// `duration_secs` is an INTEGER column. Asking sqlx for an `f64` is a
    /// decode error, not a conversion -- and the caller's `let Ok(Some(..))`
    /// turns that error into a silent early return, so the panel is never told
    /// anything at all.
    #[tokio::test]
    async fn the_track_the_panel_announces_decodes() {
        let pool = pool_with_track("i64").await;
        let id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
            .fetch_one(&pool)
            .await
            .unwrap();

        let row = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<i64>, Option<String>)>(
            "SELECT title, artist, album, duration_secs, cover_key FROM tracks WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&pool)
        .await;

        let row = row.expect("the announce query failed to decode");
        let (title, artist, album, duration, cover) = row.expect("no row");

        assert_eq!(title, "Unravel");
        assert_eq!(artist.as_deref(), Some("ALESTI"));
        assert_eq!(album.as_deref(), Some("Singles"));
        assert_eq!(duration, Some(241));
        assert_eq!(cover.as_deref(), Some("abc.jpg"));
    }

    /// And the shape that was there, kept as the evidence: it does not decode,
    /// which is the whole of why the panel was blank.
    #[tokio::test]
    async fn the_previous_shape_could_never_have_worked() {
        let pool = pool_with_track("f64").await;
        let id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
            .fetch_one(&pool)
            .await
            .unwrap();

        let row = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<f64>, Option<String>)>(
            "SELECT title, artist, album, duration_secs, cover_key FROM tracks WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&pool)
        .await;

        assert!(
            row.is_err(),
            "an INTEGER column decoded as f64 succeeded, so this was not the \
             reason the media panel had no metadata -- look again"
        );
    }
}

/// The same decode question, for `Row::try_get` rather than `query_as`.
///
/// `sample_playing_track` reads `duration_secs` this way and turns a failure
/// into `None` with `.ok().flatten()`, then returns early -- so if this errors,
/// cold-stream loudness sampling never runs at all.
#[cfg(test)]
mod try_get_tests {
    use sqlx::Row;

    #[tokio::test]
    async fn duration_secs_read_by_try_get() {
        let dir = std::env::temp_dir().join("music-app-try-get");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::init(&dir).await.unwrap();

        sqlx::query(
            "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
             VALUES ('local', 'T', '/tmp/t.wav', 'present', 304)",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        let row = sqlx::query("SELECT duration_secs FROM tracks")
            .fetch_one(&db.pool)
            .await
            .unwrap();

        let as_f64 = row.try_get::<Option<f64>, _>("duration_secs");
        let as_i64 = row.try_get::<Option<i64>, _>("duration_secs");

        eprintln!("try_get as f64: {as_f64:?}");
        eprintln!("try_get as i64: {as_i64:?}");

        assert_eq!(as_i64.expect("i64 decode failed"), Some(304));
        assert!(
            as_f64.is_err(),
            "f64 decoded fine, so this is not why cold-stream sampling declines"
        );
    }
}
