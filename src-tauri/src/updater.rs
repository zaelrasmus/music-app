//! Keeping yt-dlp current.
//!
//! yt-dlp is the one part of this app with an adversary. YouTube changes what
//! it serves and to whom, and a build that worked last month starts handing
//! back URLs that are refused at play time -- with no error from yt-dlp
//! itself, because from its point of view the extraction succeeded.
//!
//! So the app cannot ship a binary and be done with it. It ships one as a
//! floor, stages it somewhere writable (`sidecar::seed`), and from then on
//! lets yt-dlp update itself.
//!
//! **Why `--update-to` rather than a downloader of our own.** yt-dlp already
//! knows how to fetch its channel's latest build, verify its hash, and replace
//! its own executable on a platform that will not let a running one be
//! deleted. Reimplementing that means a release-API client, hash verification
//! and an atomic-replace dance, all to arrive where `--update-to` already
//! arrives, by a route nobody else tests. Measured on this machine: 16.4s for
//! a real update, 2.6s for a check that finds nothing to do.
//!
//! **Why nightly.** A stable release is a nightly that was blessed later --
//! both are cut from master and pass the same CI. The only difference that
//! reaches a user whose player has stopped working is how many days old the
//! fix is.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc::Sender;

use crate::sidecar::{self, Tool};

/// The yt-dlp release channel to track.
///
/// Passed on every update rather than only the first, because it is idempotent
/// and self-healing: a staged copy that somehow ended up on `stable` -- an app
/// data directory from an older install, a binary the user dropped in
/// themselves -- is moved back onto this channel by the next check, and costs
/// nothing when it is already there.
const CHANNEL: &str = "nightly";

/// How long a scheduled check stays good for.
///
/// Nightly builds once a day, so asking more often than this can only get the
/// same answer again.
const SCHEDULED_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a failure-driven check stays good for.
///
/// Short, because the point is to recover within one sitting, but not zero: a
/// provider having a bad hour would otherwise re-check on every failed track.
const SUSPECTED_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// How long to stay out of the way at startup.
///
/// Nothing here is on the path to a usable window, and the first thing a user
/// does is rarely to play a remote track.
const STARTUP_DELAY: Duration = Duration::from_secs(20);

/// The event carrying a new [`Status`] to the frontend.
pub const STATUS_EVENT: &str = "yt-dlp-status";

/// Why an update check is being made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Daily.
    Scheduled,
    /// The app has just been installed or upgraded, so what is staged is the
    /// bundled binary -- current when the installer was built, which may have
    /// been long before it was run.
    Installed,
    /// Something failed in a way that a stale yt-dlp would explain.
    Suspected,
    /// The user asked, in Settings.
    Manual,
}

impl Trigger {
    /// How recent the last check has to be for this trigger to skip.
    fn cooldown(self) -> Option<Duration> {
        match self {
            Trigger::Scheduled => Some(SCHEDULED_INTERVAL),
            Trigger::Suspected => Some(SUSPECTED_INTERVAL),
            // Neither of these is rate limited. A user presses the button
            // *because* they think something is wrong, and "nothing happened"
            // is the one answer they cannot act on; a fresh install has no
            // history worth respecting.
            Trigger::Installed | Trigger::Manual => None,
        }
    }
}

/// What the frontend shows.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// yt-dlp's own version string, or `None` before it has been asked.
    pub version: Option<String>,
    pub channel: &'static str,
    /// When a check last completed, in unix seconds.
    pub checked_at: Option<i64>,
    pub updating: bool,
    /// Set by a check that failed, cleared by one that does not.
    pub error: Option<String>,
    /// Whether the last completed check installed something.
    pub updated: bool,
}

/// Shared update state, managed by Tauri.
#[derive(Debug)]
pub struct Updater {
    /// One update at a time. A nudge arriving during one is dropped rather
    /// than queued: it would ask the same question and get the same answer.
    running: AtomicBool,
    status: Mutex<Status>,
}

impl Default for Updater {
    fn default() -> Self {
        Self::new()
    }
}

impl Updater {
    pub fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            status: Mutex::new(Status {
                channel: CHANNEL,
                ..Status::default()
            }),
        }
    }

    fn snapshot(&self) -> Status {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_default()
    }

    /// Applies `edit` and tells the frontend, returning what it now says.
    fn revise(&self, app: &AppHandle, edit: impl FnOnce(&mut Status)) -> Status {
        let snapshot = {
            let Ok(mut status) = self.status.lock() else {
                return Status::default();
            };
            edit(&mut status);
            status.clone()
        };

        // The panel is usually not open when this changes, and an event
        // nobody is listening for costs nothing.
        let _ = app.emit(STATUS_EVENT, &snapshot);
        snapshot
    }
}

/// Where a nudge from anywhere in the app is posted.
///
/// A global, deliberately. The call sites that know yt-dlp looks stale are the
/// ones furthest from an `AppHandle`: the coordinator has none on purpose, and
/// the download retry loop is a free function three layers down. Threading a
/// handle through all of them, to reach a singleton, would be ceremony around
/// something that is already process-wide.
///
/// It carries a plain enum rather than the `AppHandle` itself for a reason
/// worth writing down: a `static` holding an `AppHandle` makes the whole Wry
/// runtime reachable from every binary built out of this crate, including the
/// unit-test one -- which then links `user32`, `gdi32`, `comctl32` and
/// `dwmapi` and dies on launch with `STATUS_ENTRYPOINT_NOT_FOUND`, before a
/// single test runs. One task owns the handle instead, and everyone else
/// sends it a message.
///
/// Capacity one, and a failed send is dropped: a burst of failing tracks
/// should ask once, not once per track.
static NUDGES: OnceLock<Sender<Trigger>> = OnceLock::new();

/// Starts the updater and takes the first look.
///
/// `staged` says whether [`sidecar::seed`] has just written a fresh copy,
/// which is how this learns the app was installed or upgraded.
pub fn spawn(app: &AppHandle, staged: bool) {
    let (nudges, mut incoming) = tokio::sync::mpsc::channel(1);
    if NUDGES.set(nudges).is_err() {
        return;
    }

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;

        let first = if staged {
            Trigger::Installed
        } else {
            Trigger::Scheduled
        };
        let _ = check(&app, first).await;

        while let Some(trigger) = incoming.recv().await {
            let _ = check(&app, trigger).await;
        }
    });
}

/// Asks for a check without needing an `AppHandle` or an async context.
///
/// Fire and forget: the caller is in the middle of reporting a failure to the
/// user and has nothing to do with the answer.
pub fn nudge(trigger: Trigger) {
    if let Some(nudges) = NUDGES.get() {
        let _ = nudges.try_send(trigger);
    }
}

/// Whether yt-dlp's own complaint sounds like a build that has aged out.
///
/// Extraction failures are the honest signal: they mean yt-dlp reached the
/// service and could not make sense of what came back. Everything else it says
/// -- private, removed, geo-blocked, offline -- is about the track or the
/// network, and no update will change any of it.
pub fn looks_stale(stderr: &str) -> bool {
    let lowered = stderr.to_lowercase();

    [
        "nsig extraction failed",
        "signature extraction failed",
        "unable to extract",
        "failed to extract",
        "requested format is not available",
        "unable to download api page",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
}

/// Runs a check, unless one is already running or the last was recent enough.
///
/// Returns the status either way, so the manual command can say "already up to
/// date" without a second round trip.
pub async fn check(app: &AppHandle, trigger: Trigger) -> Result<Status, String> {
    if let Some(cooldown) = trigger.cooldown() {
        if time_since_last_check(app).is_some_and(|age| age < cooldown) {
            return Ok(app.state::<Updater>().snapshot());
        }
    }

    // `swap` rather than a load and a store: two nudges arriving together
    // must not both see `false`.
    if app.state::<Updater>().running.swap(true, Ordering::SeqCst) {
        return Ok(app.state::<Updater>().snapshot());
    }

    app.state::<Updater>().revise(&app, |status| {
        status.updating = true;
        status.error = None;
    });

    let outcome = run(&app).await;

    app.state::<Updater>().running.store(false, Ordering::SeqCst);

    let status = app.state::<Updater>().revise(&app, |status| {
        status.updating = false;
        match &outcome {
            Ok(finished) => {
                status.version = Some(finished.version.clone());
                status.updated = finished.updated;
                status.checked_at = Some(now());
                status.error = None;
            }
            Err(e) => {
                status.updated = false;
                status.error = Some(e.clone());
            }
        }
    });

    match outcome {
        // Recorded only on success. A failed check must not buy itself a day
        // of silence -- that is exactly the day the app is broken.
        Ok(_) => {
            record_check(&app);
            Ok(status)
        }
        Err(e) => Err(e),
    }
}

/// What one completed update learned.
struct Finished {
    version: String,
    updated: bool,
}

/// Updates the staged binary, and proves the result still runs.
async fn run(app: &AppHandle) -> Result<Finished, String> {
    let path = sidecar::resolve(app, Tool::YtDlp)?.path;
    let before = version(&path).await.ok();

    let output = {
        let path = path.clone();
        tauri::async_runtime::spawn_blocking(move || {
            sidecar::quiet(&mut Command::new(&path))
                .args(["--update-to", CHANNEL])
                .output()
        })
        .await
        .map_err(|e| format!("The update task failed: {e}"))?
        .map_err(|e| format!("Could not start yt-dlp: {e}"))?
    };

    if !output.status.success() {
        return Err(explain(&String::from_utf8_lossy(&output.stderr)));
    }

    // Read back rather than parsed out of the update's own prose. That makes
    // one call answer both questions: whether anything changed, and whether
    // what was installed can be executed at all.
    let after = match version(&path).await {
        Ok(version) => version,
        Err(e) => {
            restore(&path)?;
            return Err(format!(
                "The new yt-dlp would not run, so the previous one was put back: {e}"
            ));
        }
    };

    // ~18 MB, and yt-dlp cannot delete it itself: while it is doing the
    // updating, that file is its own running image.
    let _ = std::fs::remove_file(previous(&path));

    Ok(Finished {
        updated: before.as_deref() != Some(after.as_str()),
        version: after,
    })
}

/// Asks the binary what it is.
async fn version(path: &Path) -> Result<String, String> {
    let path = path.to_path_buf();

    let output = tauri::async_runtime::spawn_blocking(move || {
        sidecar::quiet(&mut Command::new(&path)).arg("--version").output()
    })
    .await
    .map_err(|e| format!("The version task failed: {e}"))?
    .map_err(|e| format!("Could not start yt-dlp: {e}"))?;

    if !output.status.success() {
        return Err("yt-dlp did not report a version.".to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Where yt-dlp leaves the binary it replaced.
fn previous(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".old");
    PathBuf::from(name)
}

/// Puts the previous binary back, after an update that will not run.
fn restore(path: &Path) -> Result<(), String> {
    let previous = previous(path);
    if !previous.is_file() {
        return Err("There is no previous yt-dlp to go back to.".to_string());
    }

    std::fs::rename(&previous, path)
        .map_err(|e| format!("Could not restore the previous yt-dlp: {e}"))
}

/// Turns an update failure into something worth showing.
fn explain(stderr: &str) -> String {
    let lowered = stderr.to_lowercase();

    if lowered.contains("permission") || lowered.contains("access is denied") {
        return "yt-dlp could not replace itself. Another copy of the app may \
                still be running."
            .to_string();
    }
    if lowered.contains("urlopen") || lowered.contains("network") || lowered.contains("resolve") {
        return "Could not reach GitHub to check for a yt-dlp update.".to_string();
    }

    stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .next_back()
        .unwrap_or("The yt-dlp update failed.")
        .trim()
        .to_string()
}

// --- when the last check happened --------------------------------------

/// Beside the binary rather than in the settings store: this is bookkeeping
/// about a file, it means nothing without that file, and it should go when a
/// user deletes the directory to start again.
fn stamp_path(app: &AppHandle) -> Option<PathBuf> {
    Some(
        app.path()
            .app_data_dir()
            .ok()?
            .join(sidecar::STAGED_DIR)
            .join("yt-dlp.checked"),
    )
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}

fn time_since_last_check(app: &AppHandle) -> Option<Duration> {
    let recorded: i64 = std::fs::read_to_string(stamp_path(app)?)
        .ok()?
        .trim()
        .parse()
        .ok()?;

    // A stamp from the future is a clock that has been changed, not a check
    // that has not happened yet, so it counts as due rather than as
    // infinitely fresh.
    let age = now().checked_sub(recorded).filter(|age| *age >= 0)?;
    Some(Duration::from_secs(age as u64))
}

fn record_check(app: &AppHandle) {
    let Some(path) = stamp_path(app) else {
        return;
    };
    let _ = std::fs::write(path, now().to_string());
}

// --- commands ----------------------------------------------------------

/// What Settings shows: the version, when it was last checked, and any error.
///
/// Reads the version off the binary the first time it is asked, so the panel
/// says something true rather than "unknown" until an update has run.
#[tauri::command]
pub async fn yt_dlp_status(app: AppHandle) -> Status {
    let known = app.state::<Updater>().snapshot();
    if known.version.is_some() || known.updating {
        return known;
    }

    let Ok(found) = sidecar::resolve(&app, Tool::YtDlp) else {
        return known;
    };
    let Ok(version) = version(&found.path).await else {
        return known;
    };

    let checked_at = time_since_last_check(&app).map(|age| now() - age.as_secs() as i64);

    app.state::<Updater>().revise(&app, |status| {
        status.version = Some(version);
        status.checked_at = checked_at;
    })
}

/// The button in Settings.
#[tauri::command]
pub async fn update_yt_dlp(app: AppHandle) -> Result<Status, String> {
    check(&app, Trigger::Manual).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extraction_failure_looks_stale() {
        assert!(looks_stale(
            "ERROR: [youtube] dQw4w9WgXcQ: nsig extraction failed: Some formats may be missing"
        ));
        assert!(looks_stale(
            "ERROR: [youtube] abc: Unable to extract player response"
        ));
        assert!(looks_stale("ERROR: Requested format is not available"));
    }

    #[test]
    fn an_unavailable_track_does_not() {
        // The distinction the whole trigger rests on. Every one of these is a
        // fact about the track or the network, and no update changes any of
        // them -- firing on them would mean a GitHub round trip per failure.
        for stderr in [
            "ERROR: [youtube] abc: Private video. Sign in if you have been granted access",
            "ERROR: [youtube] abc: Video unavailable. This video has been removed",
            "ERROR: [youtube] abc: The uploader has not made this video available in your country",
            "ERROR: unable to download webpage: <urlopen error [Errno 11001]>",
        ] {
            assert!(!looks_stale(stderr), "should not have fired on: {stderr}");
        }
    }

    #[test]
    fn the_previous_binary_sits_beside_the_new_one() {
        // yt-dlp appends to the whole file name. Replacing the extension
        // instead would look for `yt-dlp.old` and never find the 18 MB
        // `yt-dlp.exe.old` that is actually there.
        assert_eq!(
            previous(Path::new("C:/app/bin/yt-dlp.exe")),
            PathBuf::from("C:/app/bin/yt-dlp.exe.old")
        );
    }

    #[test]
    fn only_a_person_or_a_new_install_may_ask_twice_in_a_row() {
        assert_eq!(Trigger::Manual.cooldown(), None);
        assert_eq!(Trigger::Installed.cooldown(), None);
        assert!(
            Trigger::Suspected.cooldown().unwrap() < Trigger::Scheduled.cooldown().unwrap(),
            "a failure must be able to re-check sooner than the daily sweep"
        );
    }
}
