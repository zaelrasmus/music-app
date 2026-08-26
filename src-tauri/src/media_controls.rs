//! The Windows media flyout, and the media keys that come with it.
//!
//! Registering with the System Media Transport Controls is what puts the track
//! in the panel above the volume slider and on the lock screen -- and, less
//! obviously, it is what makes the ⏯ ⏭ ⏮ keys on a keyboard or a pair of
//! headphones reach this app at all. Windows routes those keys to whichever
//! app currently holds the session; an app that never registers is simply not
//! a candidate, which is why pressing pause on a headset did nothing.
//!
//! # Why a thread of its own
//!
//! `MediaControls` holds COM pointers and is neither `Send` nor `Sync`, so it
//! cannot be put in Tauri's managed state or touched from the coordinator. It
//! therefore lives on one thread that owns it outright and takes instructions
//! down a channel -- and a `Sender` *is* `Send`, which is the whole trick. The
//! window handle crosses as a bare `isize` for the same reason.
//!
//! Everything here is best-effort. A failure to register costs the flyout and
//! the media keys; it must never cost playback, so every error is swallowed
//! after being logged once.

use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

/// What the owning thread is told to do.
pub enum Update {
    /// A new track: everything the flyout shows.
    Track {
        title: String,
        artist: Option<String>,
        album: Option<String>,
        duration: Option<Duration>,
        /// A `file://` URL, because that is what the platform wants.
        cover_url: Option<String>,
    },
    Playing(Duration),
    Paused(Duration),
    Stopped,
}

/// A handle to the media-controls thread.
///
/// Cloneable and `Send`, so the player event sink can hold one. Sending after
/// the thread has gone is deliberately not an error: the app is shutting down,
/// and nothing useful can be done about it.
#[derive(Clone)]
pub struct MediaBridge {
    tx: Sender<Update>,
}

impl MediaBridge {
    pub fn send(&self, update: Update) {
        let _ = self.tx.send(update);
    }
}

/// Which button the listener pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Stop,
}

#[cfg(windows)]
pub fn spawn(
    hwnd: isize,
    on_button: impl Fn(Button) + Send + 'static,
) -> Option<MediaBridge> {
    use souvlaki::{
        MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition,
        PlatformConfig,
    };

    let (tx, rx): (Sender<Update>, Receiver<Update>) = std::sync::mpsc::channel();

    let started = std::thread::Builder::new()
        .name("media-controls".to_string())
        .spawn(move || {
            let config = PlatformConfig {
                // Ignored on Windows; the D-Bus backend uses it.
                dbus_name: "music_app",
                display_name: "Music App",
                hwnd: Some(hwnd as *mut std::ffi::c_void),
            };

            let mut controls = match MediaControls::new(config) {
                Ok(controls) => controls,
                Err(e) => {
                    eprintln!("media controls unavailable: {e:?}");
                    return;
                }
            };

            // Buttons arrive on a WinRT thread, not this one, so the callback
            // has to stand on its own -- hence handing the caller a plain
            // `Button` rather than anything borrowed from here.
            if let Err(e) = controls.attach(move |event: MediaControlEvent| {
                let button = match event {
                    MediaControlEvent::Play => Some(Button::Play),
                    MediaControlEvent::Pause => Some(Button::Pause),
                    MediaControlEvent::Toggle => Some(Button::Toggle),
                    MediaControlEvent::Next => Some(Button::Next),
                    MediaControlEvent::Previous => Some(Button::Previous),
                    MediaControlEvent::Stop => Some(Button::Stop),
                    // Seeking and volume from the flyout are not wired up:
                    // the panel offers them inconsistently across Windows
                    // builds, and a control that works sometimes is worse
                    // than one that is plainly absent.
                    _ => None,
                };
                if let Some(button) = button {
                    on_button(button);
                }
            }) {
                eprintln!("could not attach media controls: {e:?}");
                return;
            }

            // Held so the metadata can be re-sent with each playback change.
            // Windows clears the panel if a playback update arrives with no
            // metadata behind it, which shows as the track name vanishing the
            // moment you press pause.
            let mut current: Option<Update> = None;

            while let Ok(update) = rx.recv() {
                match &update {
                    Update::Track { .. } => {
                        apply_metadata(&mut controls, &update);
                        current = Some(update);
                    }
                    Update::Playing(at) => {
                        if let Some(track) = &current {
                            apply_metadata(&mut controls, track);
                        }
                        let _ = controls.set_playback(MediaPlayback::Playing {
                            progress: Some(MediaPosition(*at)),
                        });
                    }
                    Update::Paused(at) => {
                        if let Some(track) = &current {
                            apply_metadata(&mut controls, track);
                        }
                        let _ = controls.set_playback(MediaPlayback::Paused {
                            progress: Some(MediaPosition(*at)),
                        });
                    }
                    Update::Stopped => {
                        let _ = controls.set_playback(MediaPlayback::Stopped);
                        current = None;
                    }
                }
            }

            // The channel closed, so the app is going away. Clearing the
            // session stops a dead player sitting in the flyout.
            let _ = controls.set_playback(MediaPlayback::Stopped);

            /// Sends the metadata, retrying without the artwork if that fails.
            ///
            /// Not defensiveness for its own sake. The platform loads the
            /// thumbnail as part of the same update and commits the whole lot
            /// at the end, so one unreadable cover file discards the title and
            /// the artist with it -- the panel goes blank rather than merely
            /// artless. A blank session is also one Windows has little reason
            /// to keep pointing the media keys at.
            fn apply_metadata(controls: &mut MediaControls, update: &Update) {
                let Update::Track {
                    title,
                    artist,
                    album,
                    duration,
                    cover_url,
                } = update
                else {
                    return;
                };

                fn send(
                    controls: &mut MediaControls,
                    update: &Update,
                    cover: Option<&str>,
                ) -> Result<(), souvlaki::Error> {
                    let Update::Track {
                        title,
                        artist,
                        album,
                        duration,
                        ..
                    } = update
                    else {
                        return Ok(());
                    };
                    controls.set_metadata(MediaMetadata {
                        title: Some(title),
                        artist: artist.as_deref(),
                        album: album.as_deref(),
                        duration: *duration,
                        cover_url: cover,
                    })
                }

                let _ = (title, artist, album, duration);

                if let Err(e) = send(controls, update, cover_url.as_deref()) {
                    eprintln!("media panel rejected the artwork ({e:?}); retrying without it");
                    if let Err(e) = send(controls, update, None) {
                        eprintln!("media panel rejected the metadata: {e:?}");
                    }
                }
            }
        });

    match started {
        Ok(_) => Some(MediaBridge { tx }),
        Err(e) => {
            eprintln!("could not start the media-controls thread: {e}");
            None
        }
    }
}

/// Nothing to register with, so nothing to do.
///
/// A stub rather than conditional call sites: the caller reads the same either
/// way, and adding macOS or MPRIS later is a change here rather than a change
/// everywhere this is used.
#[cfg(not(windows))]
pub fn spawn(_hwnd: isize, _on_button: impl Fn(Button) + Send + 'static) -> Option<MediaBridge> {
    None
}
