use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

/// External programs the app shells out to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// Search and stream/download resolution.
    YtDlp,
    /// Decoding anything rodio cannot handle natively (Opus, chiefly).
    /// Wired up in Part B; declared here so resolution has one home.
    #[allow(dead_code)]
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

/// Where a tool was found. Useful in errors -- "yt-dlp is missing" is much less
/// helpful than knowing which locations were tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A newer copy the user (or the app) dropped in app data.
    AppData,
    /// The copy bundled with the app.
    Bundled,
    /// Found on PATH. Convenient in development; not something a shipped app
    /// should rely on, since the version is unknown.
    Path,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub path: PathBuf,
    pub origin: Origin,
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
        let candidate = dir.join("bin").join(&file_name);
        if is_executable_file(&candidate) {
            return Ok(Resolved {
                path: candidate,
                origin: Origin::AppData,
            });
        }
    }

    for candidate in bundled_candidates(tool, &file_name) {
        if is_executable_file(&candidate) {
            return Ok(Resolved {
                path: candidate,
                origin: Origin::Bundled,
            });
        }
    }

    if let Some(candidate) = find_on_path(&file_name) {
        return Ok(Resolved {
            path: candidate,
            origin: Origin::Path,
        });
    }

    Err(format!(
        "Could not find {}. Place it in the app's bin folder, bundle it as a \
         sidecar, or install it on your PATH.",
        tool.base_name()
    ))
}

/// Locations the bundled copy might live in.
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
}
