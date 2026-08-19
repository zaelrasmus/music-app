use std::path::{Path, PathBuf};
use std::process::Command;

use tauri::{AppHandle, Manager};

/// Stops Windows opening a console window for a child process.
///
/// A GUI application has no console of its own, so Windows creates one for
/// every child it spawns -- a black window that appears over the UI and stays
/// for as long as the child lives. That is a flicker for a quick yt-dlp resolve
/// and a window parked on top of the app for the entire length of a track,
/// since ffmpeg runs for the whole of it.
///
/// Invisible in development, because `tauri dev` runs from a terminal the child
/// can inherit. It only appears in the built app, which is the one place it
/// matters.
///
/// A no-op off Windows, where child processes have no such notion.
pub fn quiet(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// `CREATE_NO_WINDOW`, from the Windows process-creation flags.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// External programs the app shells out to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// Search and stream/download resolution.
    YtDlp,
    /// Decoding anything rodio cannot handle natively: Opus files, and every
    /// remote stream.
    Ffmpeg,
}

impl Tool {
    fn base_name(self) -> &'static str {
        match self {
            Tool::YtDlp => "yt-dlp",
            Tool::Ffmpeg => "ffmpeg",
        }
    }
}

#[cfg(windows)]
const EXE_SUFFIX: &str = ".exe";
#[cfg(not(windows))]
const EXE_SUFFIX: &str = "";

#[derive(Debug, Clone)]
pub struct Resolved {
    pub path: PathBuf,
}

/// Sub-directory of app data holding staged, updatable copies of the tools.
///
/// One constant rather than two string literals: `seed` writes here and
/// `resolve` reads here, and the two drifting apart would look exactly like
/// an update that silently never took effect.
pub(crate) const STAGED_DIR: &str = "bin";

/// Extension of the file recording which bundle the staged copy came from.
const MARKER_EXTENSION: &str = "seed";

/// Extension of the half-written copy, before it is renamed into place.
const PENDING_EXTENSION: &str = "pending";

/// What `seed` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seed {
    /// The staged copy already came from this bundle; nothing was written.
    Unchanged,
    /// The bundled binary was copied into app data.
    Refreshed,
}

/// Stages the bundled copy of `tool` in app data, where it can update itself.
///
/// yt-dlp rewrites its own executable when it updates, which needs a writable
/// directory. The install directory is not one on Windows without elevation,
/// so a binary that only ever lives there can never be updated -- the app
/// would be stuck with whatever shipped until it is reinstalled.
///
/// Call this *before* `resolve`, not after. From the first launch onwards
/// every caller then gets the same app-data path, so a binary replaced
/// underneath is picked up by the next spawn rather than at the next launch.
///
/// Only ever staged from a bundled copy, never from PATH: a developer's own
/// yt-dlp is a fallback for running the app, not something to install into
/// the user's profile behind their back.
pub fn seed(app: &AppHandle, tool: Tool) -> Result<Seed, String> {
    let file_name = format!("{}{EXE_SUFFIX}", tool.base_name());

    let source = bundled_candidates(tool, &file_name)
        .into_iter()
        .find(|candidate| is_executable_file(candidate))
        .ok_or_else(|| format!("No bundled {} to stage.", tool.base_name()))?;

    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Could not find the app data directory: {e}"))?;

    stage(&source, &dir.join(STAGED_DIR).join(&file_name))
}

/// Copies `source` to `target`, unless the copy already there came from it.
///
/// Split out from `seed` because everything interesting is here and none of it
/// needs an `AppHandle`.
///
/// The comparison is against the *bundle*, not against version numbers. A
/// staged copy that has since updated itself past the bundle is therefore
/// rolled back when the app is upgraded, which is deliberate: it costs at most
/// the days between the bundled build and the newest one, the update check
/// wins them straight back, and the alternative is running two yt-dlp
/// processes on every launch just to compare versions.
fn stage(source: &Path, target: &Path) -> Result<Seed, String> {
    let stamp = bundle_stamp(source)?;

    if target.is_file() {
        let marker = std::fs::read_to_string(target.with_extension(MARKER_EXTENSION));
        if marker.is_ok_and(|recorded| recorded.trim() == stamp) {
            return Ok(Seed::Unchanged);
        }
    }

    let dir = target
        .parent()
        .ok_or_else(|| "The staging path has no directory.".to_string())?;
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;

    // Copied beside the target and renamed, because an interrupted copy
    // straight onto the target would leave a truncated executable exactly
    // where `resolve` looks first -- worse than no staged copy at all.
    //
    // The rename fails on Windows if the target is currently running, which
    // only a second instance of the app can arrange. That is why the caller
    // treats a failure here as "carry on with what is already there".
    let pending = target.with_extension(PENDING_EXTENSION);
    std::fs::copy(source, &pending)
        .map_err(|e| format!("Could not stage {}: {e}", source.display()))?;
    std::fs::rename(&pending, target)
        .map_err(|e| format!("Could not replace {}: {e}", target.display()))?;

    // Written last. A marker missing after a successful copy costs one
    // redundant copy on the next launch; a marker written before the copy
    // would claim a binary that is not there.
    std::fs::write(target.with_extension(MARKER_EXTENSION), &stamp)
        .map_err(|e| format!("Could not record the staged version: {e}"))?;

    Ok(Seed::Refreshed)
}

/// Identifies the bundle a staged copy came from.
///
/// Length and modification time, not a hash: this runs on every launch, the
/// file is ~18 MB, and the question being asked is only "is this the same file
/// the installer put there", which a rebuild or a reinstall always changes.
fn bundle_stamp(source: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(source)
        .map_err(|e| format!("Could not read {}: {e}", source.display()))?;

    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_secs())
        .unwrap_or_default();

    Ok(format!("{}-{modified}", meta.len()))
}

/// Finds `tool`, preferring the most up-to-date copy.
///
/// Order matters and is deliberate:
///
/// 1. **App data** -- yt-dlp breaks whenever YouTube changes, so it must be
///    updatable without reinstalling the app. A shipped sidecar lives in the
///    install directory, which is not writable without elevation on Windows,
///    so `yt-dlp -U` cannot be the update path. Writing a fresh copy into app
///    data and preferring it here is what makes updates possible at all.
/// 2. **Bundled sidecar** -- the version shipped with the app; a floor, not a
///    ceiling.
/// 3. **PATH** -- last resort so a developer with these tools installed can run
///    the app before any binaries have been staged.
pub fn resolve(app: &AppHandle, tool: Tool) -> Result<Resolved, String> {
    let file_name = format!("{}{EXE_SUFFIX}", tool.base_name());

    if let Ok(dir) = app.path().app_data_dir() {
        let candidate = dir.join(STAGED_DIR).join(&file_name);
        if is_executable_file(&candidate) {
            return Ok(Resolved { path: candidate });
        }
    }

    for candidate in bundled_candidates(tool, &file_name) {
        if is_executable_file(&candidate) {
            return Ok(Resolved { path: candidate });
        }
    }

    if let Some(candidate) = find_on_path(&file_name) {
        return Ok(Resolved { path: candidate });
    }

    Err(format!(
        "Could not find {}. Place it in the app's bin folder, bundle it as a \
         sidecar, or install it on your PATH.",
        tool.base_name()
    ))
}

/// Locations the bundled copy might live in.
///
/// `tool` is read only by the development branch below, which reconstructs the
/// triple-suffixed name; a release build derives everything from `file_name`.
#[cfg_attr(not(debug_assertions), allow(unused_variables))]
fn bundled_candidates(tool: Tool, file_name: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    // Bundled: Tauri strips the target triple and drops the binary next to the
    // app executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(file_name));
        }
    }

    // Development: the staged sidecar still carries its triple suffix, and the
    // executable sits in target/debug rather than beside it.
    #[cfg(debug_assertions)]
    {
        let staged = Path::new(env!("CARGO_MANIFEST_DIR")).join("binaries");
        candidates.push(staged.join(format!(
            "{}-{}{EXE_SUFFIX}",
            tool.base_name(),
            current_target_triple()
        )));
        candidates.push(staged.join(file_name));
    }

    candidates
}

/// The triple Tauri uses when naming staged sidecars.
#[cfg(debug_assertions)]
fn current_target_triple() -> &'static str {
    // Set by build.rs; falls back to the host triple this crate was built for.
    option_env!("TAURI_ENV_TARGET_TRIPLE").unwrap_or(DEFAULT_TRIPLE)
}

#[cfg(all(debug_assertions, target_os = "windows", target_arch = "x86_64"))]
const DEFAULT_TRIPLE: &str = "x86_64-pc-windows-msvc";
#[cfg(all(debug_assertions, target_os = "macos", target_arch = "aarch64"))]
const DEFAULT_TRIPLE: &str = "aarch64-apple-darwin";
#[cfg(all(debug_assertions, target_os = "macos", target_arch = "x86_64"))]
const DEFAULT_TRIPLE: &str = "x86_64-apple-darwin";
#[cfg(all(debug_assertions, target_os = "linux", target_arch = "x86_64"))]
const DEFAULT_TRIPLE: &str = "x86_64-unknown-linux-gnu";

fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn find_on_path(file_name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(file_name))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_do_not_carry_an_extension() {
        // The extension is added once, in one place; baking it into the name
        // would double it up on Windows.
        assert_eq!(Tool::YtDlp.base_name(), "yt-dlp");
        assert_eq!(Tool::Ffmpeg.base_name(), "ffmpeg");
    }

    #[test]
    fn a_missing_binary_is_not_found_on_path() {
        assert!(find_on_path("definitely-not-a-real-binary-xyzzy").is_none());
    }

    /// A source bundle and an empty staging directory, both freshly made.
    fn bundle(name: &str, bytes: &[u8]) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("music-app-seed-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let source = dir.join("bundled-yt-dlp");
        std::fs::write(&source, bytes).unwrap();

        (source, dir.join(STAGED_DIR).join("yt-dlp"))
    }

    #[test]
    fn staging_copies_the_bundle_into_a_directory_that_does_not_exist_yet() {
        let (source, target) = bundle("first-run", b"bundled");

        assert_eq!(stage(&source, &target), Ok(Seed::Refreshed));
        assert_eq!(std::fs::read(&target).unwrap(), b"bundled");
    }

    #[test]
    fn staging_the_same_bundle_twice_writes_once() {
        let (source, target) = bundle("unchanged", b"bundled");
        stage(&source, &target).unwrap();

        // Stands in for a self-update: whatever is at the target now, the
        // second launch must leave it alone. Comparing lengths or timestamps
        // would only prove the file was not *changed*, not that it was never
        // rewritten with identical bytes.
        std::fs::write(&target, b"updated-itself").unwrap();

        assert_eq!(stage(&source, &target), Ok(Seed::Unchanged));
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"updated-itself",
            "an unchanged bundle must not overwrite a newer staged copy"
        );
    }

    #[test]
    fn a_changed_bundle_replaces_the_staged_copy() {
        let (source, target) = bundle("upgrade", b"bundled");
        stage(&source, &target).unwrap();

        // What installing a new version of the app does.
        std::fs::write(&source, b"bundled-but-newer").unwrap();

        assert_eq!(stage(&source, &target), Ok(Seed::Refreshed));
        assert_eq!(std::fs::read(&target).unwrap(), b"bundled-but-newer");
    }

    #[test]
    fn a_staged_copy_with_no_marker_is_staged_again() {
        let (source, target) = bundle("no-marker", b"bundled");
        stage(&source, &target).unwrap();
        std::fs::remove_file(target.with_extension(MARKER_EXTENSION)).unwrap();

        assert_eq!(
            stage(&source, &target),
            Ok(Seed::Refreshed),
            "an unexplained binary is not evidence of anything, and re-staging \
             costs one copy"
        );
    }

    #[test]
    fn staging_leaves_nothing_half_written_behind() {
        let (source, target) = bundle("pending", b"bundled");
        stage(&source, &target).unwrap();

        assert!(
            !target.with_extension(PENDING_EXTENSION).exists(),
            "the copy is renamed into place, not left beside it"
        );
    }
}
