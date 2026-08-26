//! Noticing that the audio output device changed.
//!
//! The engine opens the default output once and holds it for the life of the
//! process. That is correct right up until the device stops being the default
//! one: unplug headphones, connect a USB DAC, let a Bluetooth headset
//! reconnect, and the stream is bound to an endpoint that is no longer where
//! sound is supposed to go. Audio stops and never comes back.
//!
//! There is a second, quieter failure. `AudioEngine::output_rate` is published
//! once at startup and every decode is built to match it, precisely so rodio's
//! resampler -- "simple linear interpolation", measured at -33 dB of added
//! distortion -- never runs. Switching from a 48 kHz device to a 44.1 kHz one
//! leaves that number stale, and every track after it is quietly resampled.
//!
//! # Why polling
//!
//! cpal has no device-change notification on any platform. The alternatives
//! are all backend-specific: `IMMNotificationClient` over COM on Windows, a
//! PipeWire or PulseAudio subscription on Linux, `AudioObjectAddPropertyListener`
//! on macOS. Three implementations, three sets of bugs, one shared reaction.
//!
//! So this polls, through cpal, which already speaks all of those backends.
//! One implementation compiles and behaves identically on WASAPI, ALSA,
//! PulseAudio and PipeWire, which is what makes the Linux port a matter of
//! building for Linux rather than writing this module again.
//!
//! The cost of that choice is latency, bounded by [`PROBE_INTERVAL`]. A device
//! switch is heard up to a second late. That is worth measuring against the
//! alternative -- a change that is never noticed at all.
//!
//! This module is deliberately the only place that knows *how* a change is
//! detected. Swapping in a real notification client later means replacing
//! [`watch`] and nothing else; [`current_default`] and [`DeviceIdentity`] are
//! what the engine actually depends on.

use std::time::Duration;

use rodio::cpal::traits::{DeviceTrait, HostTrait};

/// How often the operating system is asked what the default output is.
///
/// One second, because this is a background probe of a system that changes a
/// handful of times a day and because the reaction to a change -- reopening a
/// stream and restarting a decode -- costs far more than the probe does.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(1);

/// How long a watcher may go without saying anything before it checks whether
/// anyone is still listening.
///
/// `report` is how a watcher learns it is redundant -- the send fails once the
/// engine has gone -- and a watcher only calls it when something *changed*. On
/// a machine where nothing ever does, that is never, and the thread outlives
/// the engine it was watching for: harmless in this app, where the engine
/// lives as long as the process, and thirty abandoned threads in a test run
/// that builds thirty engines.
///
/// So every so often it reports the identity it already reported. The engine
/// compares that against what it has open, finds no difference, and does
/// nothing -- the report exists to be *delivered*, not acted on.
const LIVENESS_INTERVAL: Duration = Duration::from_secs(60);

/// A fingerprint of the system's default output device.
///
/// Compared for equality and nothing else, so what matters is that every field
/// which would require a new stream is in it:
///
/// - **`id`** distinguishes two devices with the same name, which is exactly
///   what a pair of identical USB interfaces looks like. cpal documents it as
///   stable across restarts, and it is what an in-app device picker would
///   store, so it is the identity proper.
/// - **`name`** stands in where a backend cannot produce an id, and is the
///   only field a person can read.
/// - **`sample_rate`** and **`channels`** catch the device staying put and
///   being *reconfigured*, which the id alone would miss -- and which is the
///   case that silently degrades quality rather than stopping sound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    /// cpal's stable identifier. `None` when the backend cannot produce one.
    pub id: Option<String>,
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

/// What the operating system currently considers the default output.
///
/// `None` means there is no default output device, which is an ordinary state
/// -- the last one was unplugged, or the machine genuinely has none -- and not
/// an error.
///
/// Deliberately describes the *device*, not any stream opened on it. rodio may
/// settle on a configuration other than the one the device advertises, and an
/// engine that compared its stream's negotiated config against this probe
/// would see a difference on every poll and reopen forever.
pub fn current_default() -> Option<DeviceIdentity> {
    let device = rodio::cpal::default_host().default_output_device()?;
    let config = device.default_output_config().ok()?;

    Some(DeviceIdentity {
        id: device.id().ok().map(|id| id.to_string()),
        // `description()` rather than the deprecated `name()`, which is a
        // thin wrapper over it. Only the name is taken: the rest is
        // manufacturer and connection detail that a person picking a device
        // would want and an equality check would only make noisier.
        name: device.description().map_or_else(
            |_| "the audio device".to_string(),
            |described| described.name().to_string(),
        ),
        sample_rate: config.sample_rate(),
        channels: config.channels(),
    })
}

/// Watches the default output and reports every change to `report`.
///
/// Runs on its own thread rather than on the engine's loop. The engine wakes
/// every 50 ms to notice a track ending, and that cadence is what decides how
/// quickly one track hands over to the next; enumerating audio endpoints on
/// it -- a COM call on Windows -- would put that work directly in the path of
/// every gapless handover.
///
/// Only *changes* are reported, so the steady state costs the engine nothing:
/// no message, no wakeup, no perturbation of its poll. The state it cannot
/// report -- a stream that died while the default device stayed the same --
/// is the engine's to notice, through the error callback on the stream itself.
///
/// `report` returns whether to keep watching, which is how the thread ends:
/// when the engine is gone its channel send fails, and this stops rather than
/// probing a device nobody is listening to for the life of the process.
pub fn watch<F>(report: F)
where
    F: FnMut(Option<DeviceIdentity>) -> bool + Send + 'static,
{
    #[cfg(windows)]
    notify::watch(report);

    #[cfg(not(windows))]
    poll(report);
}

/// Runs [`poll_loop`] on a thread of its own.
///
/// The portable path. On Windows the fallback runs on the watcher's existing
/// thread instead, so this is only the entry point for platforms that have no
/// notification implementation yet.
#[cfg_attr(windows, allow(dead_code))]
fn poll<F>(mut report: F)
where
    F: FnMut(Option<DeviceIdentity>) -> bool + Send + 'static,
{
    std::thread::Builder::new()
        .name("audio-device-poll".to_string())
        .spawn(move || poll_loop(&mut report))
        .expect("device watch thread should spawn");
}

/// Asks the system what the default is, over and over, until nobody is
/// listening.
///
/// Costs a probe and a CPU wakeup every [`PROBE_INTERVAL`] for the life of the
/// process, whether or not anything ever changes: measured at 3.2 ms a probe,
/// so roughly 11 seconds of CPU an hour and 3,600 wakeups. That is the price
/// of an implementation that needs writing only once, which is why it is the
/// fallback everywhere and the only path where nothing better exists yet.
fn poll_loop<F>(report: &mut F)
where
    F: FnMut(Option<DeviceIdentity>) -> bool,
{
    // Deliberately *not* seeded from a probe. Seeding would race the engine's
    // own first open: both would read the device within milliseconds of each
    // other, and on the rare occasion they disagreed the disagreement would be
    // invisible for ever. Starting at "no device" means the first probe always
    // reports, and the engine answers "that is what I already opened" for free.
    let mut last: Option<DeviceIdentity> = None;
    let mut spoke = std::time::Instant::now();

    loop {
        std::thread::sleep(PROBE_INTERVAL);

        let seen = current_default();
        if seen == last && spoke.elapsed() < LIVENESS_INTERVAL {
            continue;
        }

        last = seen.clone();
        spoke = std::time::Instant::now();
        if !report(seen) {
            return;
        }
    }
}


/// Being told about device changes instead of asking.
///
/// WASAPI publishes endpoint changes to anyone who registers for them, which
/// is the whole of what [`poll_loop`] is emulating -- at 3.2 ms and a CPU
/// wakeup a second, forever, to learn nothing on all but a handful of
/// occasions a day. This costs nothing until something actually happens, and
/// then reports it at once rather than up to [`PROBE_INTERVAL`] late.
///
/// The shape either way is a thread that reports a [`DeviceIdentity`] when it
/// changes. Only *how it wakes up* differs, which is what keeps the engine
/// free of any of this.
#[cfg(windows)]
mod notify {
    use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
    use std::time::Duration;

    use windows::core::{implement, Result as WinResult, PCWSTR};
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
        IMMNotificationClient_Impl, MMDeviceEnumerator, DEVICE_STATE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    use super::{current_default, DeviceIdentity};

    /// How long to let things settle before asking what the device now is.
    ///
    /// One user action produces a burst: WASAPI reports the default changing
    /// once per role -- console, multimedia, communications -- plus a property
    /// change or two. Answering each would reopen the audio stream three or
    /// four times for one unplugged cable.
    ///
    /// It is also a real settle. The notification arrives when the *default*
    /// changes, which can be a moment before the endpoint will describe itself
    /// accurately, and a probe that beats it reads the old configuration.
    const SETTLE: Duration = Duration::from_millis(200);

    /// Ceiling on how long a burst may hold a report back.
    ///
    /// A device that notifies continuously -- a flaky USB interface
    /// enumerating in a loop -- must not keep resetting [`SETTLE`] and
    /// postpone the answer for ever.
    const SETTLE_CEILING: Duration = Duration::from_secs(1);

    /// The registered callback. Everything it does is send on a channel.
    ///
    /// Deliberately so. These fire on threads owned by the Windows audio
    /// service, and doing real work in one -- enumerating endpoints, opening a
    /// stream -- blocks that service for every application on the machine. So
    /// the callback only records that *something* happened, and the watcher
    /// thread works out what.
    #[implement(IMMNotificationClient)]
    struct Notifier {
        wake: Sender<()>,
    }

    impl Notifier_Impl {
        /// A failed send means the watcher has stopped, which is not an error
        /// worth reporting to the audio service.
        fn nudge(&self) {
            let _ = self.wake.send(());
        }
    }

    #[allow(non_snake_case)]
    impl IMMNotificationClient_Impl for Notifier_Impl {
        /// A device was enabled, disabled, unplugged or plugged in.
        fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> WinResult<()> {
            self.nudge();
            Ok(())
        }

        fn OnDeviceAdded(&self, _id: &PCWSTR) -> WinResult<()> {
            self.nudge();
            Ok(())
        }

        fn OnDeviceRemoved(&self, _id: &PCWSTR) -> WinResult<()> {
            self.nudge();
            Ok(())
        }

        /// The one this exists for. Fires once per role, hence [`SETTLE`].
        fn OnDefaultDeviceChanged(
            &self,
            _flow: EDataFlow,
            _role: ERole,
            _id: &PCWSTR,
        ) -> WinResult<()> {
            self.nudge();
            Ok(())
        }

        /// Carries the quiet case: a device that stayed put and was
        /// *reconfigured*. Changing the shared-mode format in the Sound
        /// control panel from 48 kHz to 44.1 arrives here and nowhere else,
        /// and ignoring it would leave every later decode built for a rate the
        /// device no longer runs at.
        fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> WinResult<()> {
            self.nudge();
            Ok(())
        }
    }

    /// Starts the watcher thread.
    ///
    /// Always succeeds from the caller's point of view. Whether the
    /// notification path could be set up is only knowable on the thread that
    /// sets it up -- COM, the enumerator and the registration all have to live
    /// there -- so the decision to fall back is made there too, and the
    /// callback never has to travel back across the boundary.
    pub(super) fn watch<F>(report: F)
    where
        F: FnMut(Option<DeviceIdentity>) -> bool + Send + 'static,
    {
        std::thread::Builder::new()
            .name("audio-device-watch".to_string())
            .spawn(move || run(report))
            .expect("device watch thread should spawn");
    }

    fn run<F>(mut report: F)
    where
        F: FnMut(Option<DeviceIdentity>) -> bool + Send + 'static,
    {
        // Multithreaded apartment: nothing here pumps a message loop, and the
        // callbacks arrive on the audio service's own threads regardless. An
        // `S_FALSE` return means COM was already initialised on this thread,
        // which is a success -- `is_err` is what distinguishes that from a
        // real failure.
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if com.is_err() {
            // Falls back rather than failing. Registering a COM callback has
            // more ways to go wrong than sleeping in a loop does, and losing
            // device handling entirely would be a far worse outcome than the
            // CPU a poll costs.
            super::poll_loop(&mut report);
            return;
        }

        let registered = unsafe { register() };
        let Ok((enumerator, client, wakes)) = registered else {
            unsafe { CoUninitialize() };
            super::poll_loop(&mut report);
            return;
        };

        // Not seeded from a probe, for the same reason the poll is not: it
        // would race the engine's own first open, and a disagreement between
        // the two would then be invisible for ever. The first notification
        // reports, and the engine answers "that is what I already opened".
        let mut last: Option<DeviceIdentity> = None;

        loop {
            // The timeout is not a poll. Nothing is asked of the device when
            // it expires; it exists so a watcher on a machine where the audio
            // hardware never changes still finds out that the engine it was
            // watching for has gone. See `LIVENESS_INTERVAL`.
            let woken = match wakes.recv_timeout(super::LIVENESS_INTERVAL) {
                Ok(()) => true,
                Err(RecvTimeoutError::Timeout) => false,
                // The callback was dropped, which cannot happen while this
                // thread holds the registration.
                Err(RecvTimeoutError::Disconnected) => break,
            };

            let seen = if woken {
                settle(&wakes);
                current_default()
            } else {
                // Repeat what was last said rather than asking again: the
                // point is to reach the engine, and a probe here would be the
                // very cost this module exists to avoid.
                last.clone()
            };

            if woken && seen == last {
                continue;
            }

            last = seen.clone();
            if !report(seen) {
                break;
            }
        }

        // The engine has gone, or the audio service stopped talking to us.
        // Unregistering matters: the service holds the callback's pointer
        // until told otherwise.
        unsafe {
            let _ = enumerator.UnregisterEndpointNotificationCallback(&client);
            CoUninitialize();
        }
    }

    /// Creates the enumerator and registers the callback on it.
    ///
    /// Both are handed back because unregistering needs the same enumerator
    /// and the same client pointer that registered.
    ///
    /// # Safety
    ///
    /// Must run on a thread where COM is initialised, and what it returns must
    /// not outlive that initialisation.
    unsafe fn register() -> WinResult<(IMMDeviceEnumerator, IMMNotificationClient, Receiver<()>)> {
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;

        let (wake, wakes) = std::sync::mpsc::channel();
        let client: IMMNotificationClient = Notifier { wake }.into();

        unsafe { enumerator.RegisterEndpointNotificationCallback(&client) }?;

        // Read once and discarded, to fault the endpoint into life while
        // nobody is waiting. Without it the first real notification pays for
        // the enumeration, which is exactly the moment not to.
        let _ = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) };

        Ok((enumerator, client, wakes))
    }

    /// Waits for a burst of notifications to stop, within a ceiling.
    fn settle(wakes: &Receiver<()>) {
        let deadline = std::time::Instant::now() + SETTLE_CEILING;

        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return;
            }

            match wakes.recv_timeout(SETTLE.min(remaining)) {
                // Still arriving: the burst has not finished.
                Ok(()) => continue,
                // Quiet for `SETTLE`, or there is nobody left to hear from.
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// That the COM plumbing actually works on a real machine.
        ///
        /// Everything else here is unobservable without hardware to unplug, so
        /// this checks the one thing that can be checked automatically and is
        /// also the one thing most likely to be wrong: that the enumerator can
        /// be created and the callback registered at all. A failure here is
        /// what sends the watcher down the polling fallback for ever.
        #[test]
        #[ignore = "needs a real audio endpoint"]
        fn the_callback_registers_with_the_audio_service() {
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            assert!(!com.is_err(), "COM would not initialise: {com:?}");

            let registered = unsafe { register() };
            let ok = registered.is_ok();

            if let Ok((enumerator, client, _wakes)) = registered {
                unsafe {
                    let _ = enumerator.UnregisterEndpointNotificationCallback(&client);
                }
            }
            unsafe { CoUninitialize() };

            assert!(
                ok,
                "registering for endpoint notifications failed, so every device \
                 change would be found by polling instead"
            );
        }

        /// A burst of notifications collapses into one report.
        ///
        /// One unplugged cable produces a default change per role plus a
        /// property change or two. Without this, each would reopen the audio
        /// stream -- so the track would restart three or four times over.
        #[test]
        fn a_burst_of_notifications_settles_into_one() {
            let (wake, wakes) = std::sync::mpsc::channel();

            // What the audio service does: several callbacks in quick
            // succession, then silence.
            std::thread::spawn(move || {
                for _ in 0..5 {
                    if wake.send(()).is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                // Held open, so `settle` ends on its timeout rather than on a
                // disconnect -- which is the case that actually happens.
                std::thread::sleep(Duration::from_secs(2));
            });

            wakes.recv().expect("the burst never started");
            settle(&wakes);

            assert!(
                wakes.try_recv().is_err(),
                "settle returned with notifications still queued, so the burst \
                 would be answered more than once"
            );
        }

        /// And the ceiling: a device that never stops notifying must not hold
        /// the report back for ever.
        #[test]
        fn a_device_that_never_settles_is_answered_anyway() {
            let (wake, wakes) = std::sync::mpsc::channel();

            std::thread::spawn(move || {
                while wake.send(()).is_ok() {
                    std::thread::sleep(Duration::from_millis(10));
                }
            });

            wakes.recv().expect("the burst never started");
            let start = std::time::Instant::now();
            settle(&wakes);
            let waited = start.elapsed();

            assert!(
                waited < SETTLE_CEILING + Duration::from_millis(500),
                "settle waited {waited:?} on a device notifying continuously, \
                 which would postpone the answer indefinitely"
            );
        }

        /// A live observer, for checking the real thing by hand.
        ///
        /// Run it and then change your default output device -- unplug
        /// headphones, switch in the volume flyout, change the format in the
        /// Sound control panel -- and each change should print within a moment.
        ///
        /// `cargo test --lib watch_devices_live -- --ignored --nocapture`
        #[test]
        #[ignore = "interactive: change your audio device while it runs"]
        fn watch_devices_live() {
            let (tx, rx) = std::sync::mpsc::channel();
            super::watch(move |seen| tx.send(seen).is_ok());

            eprintln!("watching for 30s -- change your default output device now");
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            let mut seen_any = false;

            while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(identity) => {
                        seen_any = true;
                        eprintln!("  {:>6.2?}  {identity:?}", std::time::Instant::now());
                    }
                    Err(_) => break,
                }
            }

            eprintln!(
                "{}",
                if seen_any {
                    "at least one report arrived"
                } else {
                    "nothing reported -- either nothing changed, or it is not working"
                }
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn identity(id: &str, rate: u32) -> DeviceIdentity {
        DeviceIdentity {
            id: Some(id.to_string()),
            name: "Speakers".to_string(),
            sample_rate: rate,
            channels: 2,
        }
    }

    /// The case the whole module exists for, and the one a name-only
    /// fingerprint would miss: the same device, reconfigured to another rate.
    ///
    /// Missing it is not silence -- it is every track after the change being
    /// run through rodio's linear-interpolation resampler, which the engine's
    /// own tests measure at 33 dB below the music.
    #[test]
    fn a_reconfigured_device_is_a_different_identity() {
        assert_ne!(identity("wasapi:a", 48_000), identity("wasapi:a", 44_100));
    }

    /// And the case a rate-only fingerprint would miss: two devices that both
    /// run at 48 kHz. The stream is bound to an endpoint, so switching between
    /// them needs a new one however alike they are.
    #[test]
    fn two_devices_at_the_same_rate_are_different_identities() {
        assert_ne!(identity("wasapi:a", 48_000), identity("wasapi:b", 48_000));
    }

    /// Identical devices differ only by id -- a pair of the same USB
    /// interface, which is why the id is carried at all.
    #[test]
    fn the_id_separates_devices_that_share_a_name() {
        let mut second = identity("wasapi:b", 48_000);
        second.name = "Speakers".to_string();

        assert_eq!(second.name, identity("wasapi:a", 48_000).name);
        assert_ne!(second, identity("wasapi:a", 48_000));
    }

    /// Nothing changed must compare equal, or the engine reopens on every
    /// poll -- which would restart the decoder once a second forever.
    #[test]
    fn an_unchanged_device_is_the_same_identity() {
        assert_eq!(identity("wasapi:a", 48_000), identity("wasapi:a", 48_000));
    }

    /// What the probe costs, on a real machine.
    ///
    /// Ignored because it needs an audio device. Run with
    /// `cargo test --lib probe_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a real output device"]
    fn probe_cost() {
        const RUNS: u32 = 50;

        // Warm: the first call on a thread initialises COM on Windows, which
        // is not what a once-a-second probe pays.
        let _ = current_default();

        let start = std::time::Instant::now();
        for _ in 0..RUNS {
            let _ = current_default();
        }
        let each = start.elapsed() / RUNS;

        eprintln!("device probe: {each:?} each over {RUNS} runs");
        eprintln!("{:?}", current_default());
        eprintln!(
            "duty cycle at one probe per {:?}: {:.4}%",
            PROBE_INTERVAL,
            each.as_secs_f64() / PROBE_INTERVAL.as_secs_f64() * 100.0
        );
    }

    /// Where the probe's time actually goes.
    ///
    /// Ignored: needs a real device. Run with
    /// `cargo test --lib probe_breakdown -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a real output device"]
    fn probe_breakdown() {
        const RUNS: u32 = 200;

        fn time(label: &str, mut body: impl FnMut()) -> Duration {
            body();
            let start = std::time::Instant::now();
            for _ in 0..RUNS {
                body();
            }
            let each = start.elapsed() / RUNS;
            eprintln!("{label:<34} {each:>10.3?}");
            each
        }

        eprintln!("--- per call, mean of {RUNS} ---");

        let enumerate = time("default_output_device()", || {
            let _ = rodio::cpal::default_host().default_output_device();
        });

        let with_id = time("  + id()", || {
            if let Some(device) = rodio::cpal::default_host().default_output_device() {
                let _ = device.id();
            }
        });

        let with_description = time("  + description()", || {
            if let Some(device) = rodio::cpal::default_host().default_output_device() {
                let _ = device.id();
                let _ = device.description();
            }
        });

        let full = time("  + default_output_config() [now]", || {
            let _ = current_default();
        });

        eprintln!();
        eprintln!("attributable to id():          {:>10.3?}", with_id.saturating_sub(enumerate));
        eprintln!(
            "attributable to description(): {:>10.3?}",
            with_description.saturating_sub(with_id)
        );
        eprintln!(
            "attributable to config():      {:>10.3?}",
            full.saturating_sub(with_description)
        );
        eprintln!();
        eprintln!(
            "at one probe per {:?}: {:.4}% of one core, {} wakeups/hour",
            PROBE_INTERVAL,
            full.as_secs_f64() / PROBE_INTERVAL.as_secs_f64() * 100.0,
            3600 / PROBE_INTERVAL.as_secs().max(1),
        );
        eprintln!(
            "                    {:.2} s of CPU per hour",
            full.as_secs_f64() * (3600.0 / PROBE_INTERVAL.as_secs_f64()),
        );
    }
}
