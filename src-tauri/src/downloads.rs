//! What the app is busy doing, and the queue behind it.
//!
//! Downloading was one command that either ran or refused: click a track, wait,
//! and find out afterwards. That is fine for one track and useless for fifty --
//! there is no room to run them at once (each is a yt-dlp resolve and an ffmpeg
//! copy), so a playlist has to become a queue, and a queue nobody can see is
//! indistinguishable from nothing happening.
//!
//! So this owns three things: the queue, the one worker that drains it, and a
//! snapshot of both that the window can show.
//!
//! **Caching is in here too, quietly.** When a track is left part-way through,
//! the player fetches the rest in the background -- real work, real bandwidth,
//! and nothing the user asked for. It belongs in the same panel as the
//! downloads and nowhere near the same prominence: a download is a promise the
//! user made, and caching is housekeeping they merely allowed.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use sqlx::{Row, SqlitePool};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::covers::CoverStore;
use crate::db::Db;

/// Carries the whole snapshot, as the player and queue events do.
///
/// One event with everything beats several partial ones the frontend has to
/// stitch into a consistent view -- and this is small: a few dozen rows of
/// title and state.
pub const ACTIVITY_EVENT: &str = "download-activity";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: u64,
    pub track_id: i64,
    pub title: String,
    pub artist: Option<String>,
    /// The batch this belongs to, when it came from one.
    pub group_id: Option<u64>,
    pub state: JobState,
    /// Why it failed, in the words the rest of the app would have used.
    pub error: Option<String>,
}

/// A batch, so fifty rows can collapse into one line that says what it is.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: u64,
    pub name: String,
}

/// A track being filled into the streaming cache in the background.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Caching {
    pub track_id: i64,
    pub title: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub jobs: Vec<Job>,
    pub groups: Vec<Group>,
    pub caching: Vec<Caching>,
}

/// The queue, its worker's doorbell, and the snapshot everyone reads.
#[derive(Debug)]
pub struct Downloads {
    activity: Mutex<Activity>,
    next_id: AtomicU64,
    /// Rings the worker. Carries nothing -- the queue itself is the state, so
    /// a message only ever means "look again".
    wake: UnboundedSender<()>,
}

impl Downloads {
    pub fn new() -> (Self, UnboundedReceiver<()>) {
        let (wake, doorbell) = mpsc::unbounded_channel();

        (
            Self {
                activity: Mutex::new(Activity::default()),
                next_id: AtomicU64::new(1),
                wake,
            },
            doorbell,
        )
    }

    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> Activity {
        self.activity
            .lock()
            .map(|activity| activity.clone())
            .unwrap_or_default()
    }

    /// Applies `edit`, tells the window, and returns what it now says.
    fn revise<R: Runtime>(&self, app: &AppHandle<R>, edit: impl FnOnce(&mut Activity)) -> Activity {
        let snapshot = {
            let Ok(mut activity) = self.activity.lock() else {
                return Activity::default();
            };
            edit(&mut activity);
            activity.clone()
        };

        let _ = app.emit(ACTIVITY_EVENT, &snapshot);
        snapshot
    }

    /// The next job to run, marked as running.
    ///
    /// Taken from the shared state rather than from a channel so that the
    /// queue the worker drains and the queue the panel shows are the same
    /// list -- otherwise cancelling a job the panel can see would leave the
    /// worker still holding it.
    fn take_next(&self, app: &AppHandle) -> Option<Job> {
        let mut taken = None;

        self.revise(app, |activity| {
            if let Some(job) = activity
                .jobs
                .iter_mut()
                .find(|job| job.state == JobState::Queued)
            {
                job.state = JobState::Running;
                taken = Some(job.clone());
            }
        });

        taken
    }

    fn finish(&self, app: &AppHandle, id: u64, outcome: Result<(), String>) {
        self.revise(app, |activity| {
            let Some(job) = activity.jobs.iter_mut().find(|job| job.id == id) else {
                return;
            };

            match outcome {
                Ok(()) => {
                    job.state = JobState::Done;
                    job.error = None;
                }
                Err(message) => {
                    job.state = JobState::Failed;
                    job.error = Some(message);
                }
            }
        });
    }

    /// Adds tracks to the queue, skipping any already queued or running.
    ///
    /// Returns how many were actually added, so the caller can say "already
    /// queued" rather than silently doing nothing.
    fn enqueue(&self, app: &AppHandle, group: Option<Group>, tracks: Vec<(i64, String, Option<String>)>) -> usize {
        let mut added = 0;

        self.revise(app, |activity| {
            if let Some(group) = group.clone() {
                activity.groups.push(group);
            }

            for (track_id, title, artist) in tracks {
                // A track already waiting or running must not be queued twice:
                // two writes to one file is the thing `DownloadLock` exists to
                // prevent, and this is the cheaper place to notice.
                let pending = activity.jobs.iter().any(|job| {
                    job.track_id == track_id
                        && matches!(job.state, JobState::Queued | JobState::Running)
                });
                if pending {
                    continue;
                }

                activity.jobs.push(Job {
                    id: self.id(),
                    track_id,
                    title,
                    artist,
                    group_id: group.as_ref().map(|g| g.id),
                    state: JobState::Queued,
                    error: None,
                });
                added += 1;
            }

            // A group nothing was added to would show as an empty heading.
            if added == 0 {
                if let Some(group) = &group {
                    activity.groups.retain(|existing| existing.id != group.id);
                }
            }
        });

        if added > 0 {
            let _ = self.wake.send(());
        }

        added
    }

    /// Drops a job that has not started. A running one is left alone --
    /// stopping it mid-write is a different, larger promise.
    fn cancel(&self, app: &AppHandle, id: u64) {
        self.revise(app, |activity| {
            activity
                .jobs
                .retain(|job| !(job.id == id && job.state == JobState::Queued));
            prune_groups(activity);
        });
    }

    fn clear_finished(&self, app: &AppHandle) {
        self.revise(app, |activity| {
            activity
                .jobs
                .retain(|job| matches!(job.state, JobState::Queued | JobState::Running));
            prune_groups(activity);
        });
    }
}

/// Forgets groups nothing points at any more.
fn prune_groups(activity: &mut Activity) {
    activity.groups.retain(|group| {
        activity
            .jobs
            .iter()
            .any(|job| job.group_id == Some(group.id))
    });
}

/// Runs queued downloads, one at a time, forever.
///
/// One at a time is not a limitation to work around: each job is a yt-dlp
/// resolve followed by an ffmpeg copy of the whole track, and running six of
/// those together would make all six slower while saturating the connection
/// the app is also streaming through.
pub fn spawn_worker(app: AppHandle, pool: SqlitePool, covers: CoverStore, mut doorbell: UnboundedReceiver<()>) {
    tauri::async_runtime::spawn(async move {
        loop {
            // Drain everything waiting before sleeping again, so a burst of
            // fifty rings the doorbell once and still empties the queue.
            while let Some(job) = app.state::<Downloads>().take_next(&app) {
                let outcome =
                    crate::download::fetch_track(&app, &pool, &covers, job.track_id).await;
                app.state::<Downloads>().finish(&app, job.id, outcome);
            }

            if doorbell.recv().await.is_none() {
                break;
            }
        }
    });
}

// --- background caching ------------------------------------------------

/// Notes that a track is being filled into the streaming cache.
///
/// Called from the player, which is where the decision is made. Silent when
/// the state is unavailable: this is an indicator, and nothing should fail
/// because an indicator could not be updated.
pub fn caching_started<R: Runtime>(app: &AppHandle<R>, track_id: i64, title: String) {
    let Some(downloads) = app.try_state::<Downloads>() else {
        return;
    };

    downloads.revise(app, |activity| {
        if activity.caching.iter().any(|c| c.track_id == track_id) {
            return;
        }
        activity.caching.push(Caching { track_id, title });
    });
}

pub fn caching_finished<R: Runtime>(app: &AppHandle<R>, track_id: i64) {
    let Some(downloads) = app.try_state::<Downloads>() else {
        return;
    };

    downloads.revise(app, |activity| {
        activity.caching.retain(|c| c.track_id != track_id);
    });
}

// --- commands ----------------------------------------------------------

#[tauri::command]
pub async fn download_activity(downloads: State<'_, Downloads>) -> Result<Activity, String> {
    Ok(downloads.snapshot())
}

/// Queues one track.
///
/// Replaces the old "download now" command entirely, so every download goes
/// through one queue and the panel is never showing half the story.
#[tauri::command]
pub async fn download_track(
    app: AppHandle,
    db: State<'_, Db>,
    downloads: State<'_, Downloads>,
    track_id: i64,
) -> Result<(), String> {
    let row = sqlx::query("SELECT title, artist, source, state FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_optional(&db.pool)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("That track no longer exists.")?;

    let source: String = row.get("source");
    let state: String = row.get("state");

    if source == "local" {
        return Err("That track is already a local file.".to_string());
    }
    if state == "downloaded" {
        return Err("That track is already downloaded.".to_string());
    }

    let added = downloads.enqueue(
        &app,
        None,
        vec![(track_id, row.get("title"), row.get("artist"))],
    );

    if added == 0 {
        return Err("That track is already downloading.".to_string());
    }

    Ok(())
}

/// Queues everything in a playlist that is not already on disk.
///
/// Local files and tracks already downloaded are skipped rather than refused:
/// "download this playlist" means "have all of it offline", and half of it
/// already being there is not a reason to do nothing.
#[tauri::command]
pub async fn download_playlist(
    app: AppHandle,
    db: State<'_, Db>,
    downloads: State<'_, Downloads>,
    playlist_id: i64,
) -> Result<usize, String> {
    let name: String = sqlx::query_scalar("SELECT name FROM playlists WHERE id = ?")
        .bind(playlist_id)
        .fetch_optional(&db.pool)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("That playlist no longer exists.")?;

    let rows = sqlx::query(
        "SELECT t.id, t.title, t.artist
         FROM playlist_tracks pt
         JOIN tracks t ON t.id = pt.track_id
         WHERE pt.playlist_id = ?
           AND t.source <> 'local'
           AND t.state <> 'downloaded'
         ORDER BY pt.position",
    )
    .bind(playlist_id)
    .fetch_all(&db.pool)
    .await
    .map_err(|e| e.to_string())?;

    if rows.is_empty() {
        return Err("Everything in that playlist is already on this device.".to_string());
    }

    let tracks: Vec<(i64, String, Option<String>)> = rows
        .into_iter()
        .map(|row| (row.get("id"), row.get("title"), row.get("artist")))
        .collect();

    let group = Group {
        id: downloads.id(),
        name,
    };

    Ok(downloads.enqueue(&app, Some(group), tracks))
}

#[tauri::command]
pub async fn cancel_download(
    app: AppHandle,
    downloads: State<'_, Downloads>,
    job_id: u64,
) -> Result<(), String> {
    downloads.cancel(&app, job_id);
    Ok(())
}

#[tauri::command]
pub async fn clear_finished_downloads(
    app: AppHandle,
    downloads: State<'_, Downloads>,
) -> Result<(), String> {
    downloads.clear_finished(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity_with(jobs: Vec<Job>, groups: Vec<Group>) -> Activity {
        Activity {
            jobs,
            groups,
            caching: Vec::new(),
        }
    }

    fn job(id: u64, group_id: Option<u64>, state: JobState) -> Job {
        Job {
            id,
            track_id: id as i64,
            title: format!("Track {id}"),
            artist: None,
            group_id,
            state,
            error: None,
        }
    }

    #[test]
    fn a_group_survives_while_any_of_its_jobs_do() {
        let mut activity = activity_with(
            vec![job(1, Some(9), JobState::Done), job(2, Some(9), JobState::Queued)],
            vec![Group {
                id: 9,
                name: "Mine".into(),
            }],
        );

        activity.jobs.retain(|j| j.state != JobState::Done);
        prune_groups(&mut activity);

        assert_eq!(activity.groups.len(), 1, "one job still belongs to it");
    }

    #[test]
    fn a_group_goes_when_its_last_job_does() {
        let mut activity = activity_with(
            vec![job(1, Some(9), JobState::Done)],
            vec![Group {
                id: 9,
                name: "Mine".into(),
            }],
        );

        activity.jobs.clear();
        prune_groups(&mut activity);

        assert!(
            activity.groups.is_empty(),
            "a heading with nothing under it is not a group"
        );
    }

    #[test]
    fn an_ungrouped_job_never_keeps_a_group_alive() {
        let mut activity = activity_with(
            vec![job(1, None, JobState::Queued)],
            vec![Group {
                id: 9,
                name: "Mine".into(),
            }],
        );

        prune_groups(&mut activity);
        assert!(activity.groups.is_empty());
    }
}
