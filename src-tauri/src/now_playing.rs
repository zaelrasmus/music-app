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
            let row = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<f64>, Option<String>)>(
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
            let cover_url = cover_key
                .map(|key| covers.join(key))
                .filter(|path| path.exists())
                .map(|path| format!("file://{}", path.display()));

            bridge.send(Update::Track {
                title,
                artist,
                album,
                duration: duration_secs
                    .filter(|secs| secs.is_finite() && *secs > 0.0)
                    .map(Duration::from_secs_f64),
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
