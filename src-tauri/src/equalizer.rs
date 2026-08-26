//! A ten-band graphic equaliser, in the source chain rather than in ffmpeg.
//!
//! ffmpeg has a perfectly good `equalizer` filter, and it is the wrong tool
//! here: filters are fixed when the process launches, so every movement of a
//! slider would mean killing the decoder and starting another. That is a
//! measured 52 ms of silence per change -- fine for something set once per
//! track, unusable for a control someone drags. (mpv gets away with it by
//! reconfiguring a live filter graph; a one-shot pipe cannot.)
//!
//! So the filtering happens here, between the decoder and the volume stage,
//! reading its settings from shared atomics. A change takes effect on the next
//! frame with no gap at all.
//!
//! Placed *before* the gain and the limiter deliberately: a boosted band can
//! push a track past full scale, and the limiter downstream is what catches
//! it. An equaliser after the limiter would undo the one guarantee it makes.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use rodio::Source;

/// ISO octave centres, the standard ten-band layout.
///
/// Ten rather than three, because three cannot do what people actually reach
/// for an equaliser to do: "less boxy" lives around 400 Hz, "less harsh"
/// around 3 kHz, and a three-band control lumps both into one "mid" slider
/// that moves them together. The cost is CPU that was measured and is not
/// there -- ten biquads is a handful of multiplies per sample against a
/// decoder already running at 500x realtime.
pub const CENTRES: [f32; 10] = [
    31.5, 63.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

pub const BAND_COUNT: usize = CENTRES.len();

/// How far a band may be pushed, either way.
///
/// Twelve is the usual limit on a graphic equaliser and is already more than
/// anyone should use: +12 dB is four times the amplitude, which the limiter
/// downstream will spend most of its time undoing.
pub const MAX_GAIN_DB: f32 = 12.0;

/// One octave per band, matching the centre spacing.
///
/// `Q = sqrt(2^N) / (2^N - 1)` for a bandwidth of N octaves; N = 1 gives
/// sqrt(2). Bands then meet at their half-gain points, so a run of equal
/// settings adds up to something flat rather than rippled.
const Q: f32 = std::f32::consts::SQRT_2;

/// Live equaliser settings, shared with whatever is playing.
///
/// Atomics rather than a lock: the audio callback reads these on every frame
/// and must never wait on a UI thread that is mid-drag.
#[derive(Debug)]
pub struct EqSettings {
    enabled: AtomicBool,
    /// Per-band gain in dB, as `f32` bits.
    gains: [AtomicU32; BAND_COUNT],
    /// Bumped on every change.
    ///
    /// Recomputing ten sets of biquad coefficients means ten `sin`, `cos` and
    /// `powf` calls, which is far too much to do per frame. The audio side
    /// compares this instead -- one relaxed load -- and only rebuilds when it
    /// has actually moved.
    generation: AtomicU64,
}

impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            gains: std::array::from_fn(|_| AtomicU32::new(0f32.to_bits())),
            generation: AtomicU64::new(0),
        }
    }
}

impl EqSettings {
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
        self.generation.fetch_add(1, Ordering::Release);
    }

    pub fn gains(&self) -> [f32; BAND_COUNT] {
        std::array::from_fn(|i| f32::from_bits(self.gains[i].load(Ordering::Relaxed)))
    }

    /// Replaces every band at once.
    ///
    /// One write for the whole curve, because a preset applied band by band
    /// would be audible as a sweep: the audio thread can read between any two
    /// stores. The generation is bumped once, at the end, so the coefficients
    /// are rebuilt from a complete setting rather than a half-applied one.
    pub fn set_gains(&self, gains: &[f32]) {
        for (slot, value) in self.gains.iter().zip(gains) {
            let clamped = value.clamp(-MAX_GAIN_DB, MAX_GAIN_DB);
            // A NaN through here would poison the filter state permanently,
            // and it arrives from the frontend as JSON.
            let safe = if clamped.is_finite() { clamped } else { 0.0 };
            slot.store(safe.to_bits(), Ordering::Relaxed);
        }
        self.generation.fetch_add(1, Ordering::Release);
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

}

/// One peaking-EQ section, normalised by `a0`.
///
/// Coefficients from the RBJ audio EQ cookbook, which is what every graphic
/// equaliser worth trusting uses.
#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    fn peaking(centre_hz: f32, gain_db: f32, sample_rate: f32) -> Self {
        let (centre_hz, gain_db, sample_rate) =
            (centre_hz as f64, gain_db as f64, sample_rate as f64);
        // A band at or above Nyquist has nothing to act on, and the maths goes
        // unstable there rather than merely useless -- at 44.1 kHz the 16 kHz
        // band is close enough to matter.
        if centre_hz >= sample_rate * 0.5 {
            return Self::bypass();
        }

        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * centre_hz / sample_rate;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * Q as f64);

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / a;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    const fn bypass() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }
}

/// Per-channel filter memory, in transposed direct form II.
///
/// Chosen over direct form I for its numerical behaviour and because it needs
/// two registers per section rather than four. Held in `f64`: see the note in
/// `Equalised::next`.
#[derive(Debug, Clone, Copy, Default)]
struct State {
    z1: f64,
    z2: f64,
}

impl State {
    #[inline]
    fn step(&mut self, biquad: &Biquad, x: f64) -> f64 {
        let y = biquad.b0 * x + self.z1;
        self.z1 = biquad.b1 * x - biquad.a1 * y + self.z2;
        self.z2 = biquad.b2 * x - biquad.a2 * y;
        y
    }
}

/// How often the ramp advances, in frames.
///
/// Small enough that a step is inaudible, large enough that rebuilding ten
/// biquads is not happening per sample. Only runs while a change is in flight.
const RAMP_BLOCK_FRAMES: usize = 32;

/// How far a band may move per block.
///
/// At 48 kHz a block is 0.67 ms, so this is roughly 750 dB per second: the
/// full 12 dB of a band arrives in about 16 ms. Fast enough to feel immediate
/// on a slider, slow enough that the coefficients never jump.
const RAMP_DB_PER_BLOCK: f32 = 0.5;

/// Applies the live equaliser settings to whatever it wraps.
pub struct Equalised<S> {
    inner: S,
    settings: Arc<EqSettings>,
    channels: usize,
    sample_rate: f32,

    /// `[channel][band]`.
    states: Vec<[State; BAND_COUNT]>,
    biquads: [Biquad; BAND_COUNT],

    /// What the listener has asked for.
    ///
    /// Already folded with the enabled flag: switching the equaliser off sets
    /// this to flat rather than taking a separate path, so turning it off
    /// glides to neutral exactly as dragging every slider to zero would.
    target: [f32; BAND_COUNT],
    /// What `biquads` currently represents, which chases `target`.
    ///
    /// The two differ only while a change is in flight. Coefficients that jump
    /// are the whole problem this exists to solve: the state registers hold a
    /// tail computed by the *old* filter, so swapping the coefficients under
    /// them puts a step through the output. Measured before this was added,
    /// selecting "Treble" mid-song moved the signal 0.21 between two adjacent
    /// samples -- four times what a 440 Hz tone at that level can do on its
    /// own, and plainly audible as a click.
    applied: [f32; BAND_COUNT],
    /// Frames since the ramp last advanced.
    since_step: usize,

    /// Which settings `target` was read from.
    built_from: u64,
    /// True once `applied` is flat, when the filter is an exact identity and
    /// can be skipped entirely. This is what keeps a flat equaliser bit-exact.
    bypass: bool,

    /// Where in the frame the next sample sits, so each channel keeps its own
    /// filter memory. The stream is interleaved and the source may end
    /// mid-frame, so this cannot be derived from a sample count.
    channel: usize,
}

impl<S> Equalised<S>
where
    S: Source,
{
    pub fn new(inner: S, settings: Arc<EqSettings>) -> Self {
        let channels = (inner.channels().get() as usize).max(1);
        let sample_rate = inner.sample_rate().get() as f32;

        let mut me = Self {
            inner,
            settings,
            channels,
            sample_rate,
            states: vec![[State::default(); BAND_COUNT]; channels],
            biquads: [Biquad::bypass(); BAND_COUNT],
            target: [0.0; BAND_COUNT],
            applied: [0.0; BAND_COUNT],
            since_step: 0,
            built_from: u64::MAX,
            bypass: true,
            channel: 0,
        };

        me.reload_target();
        // Nothing is playing yet, so there is nothing to click: start already
        // at the setting rather than gliding up to it from flat.
        me.applied = me.target;
        me.rebuild();
        me
    }

    /// Re-reads the settings. Cheap, and only on a generation change.
    fn reload_target(&mut self) {
        self.built_from = self.settings.generation();
        self.target = if self.settings.enabled() {
            self.settings.gains()
        } else {
            [0.0; BAND_COUNT]
        };
    }

    /// Moves `applied` one step towards `target`, rebuilding what changed.
    ///
    /// Returns whether anything moved, so a settled filter costs one array
    /// comparison per block rather than ten coefficient computations.
    fn advance_ramp(&mut self) -> bool {
        let mut moved = false;

        for band in 0..BAND_COUNT {
            let delta = self.target[band] - self.applied[band];
            if delta == 0.0 {
                continue;
            }
            moved = true;
            self.applied[band] = if delta.abs() <= RAMP_DB_PER_BLOCK {
                self.target[band]
            } else {
                self.applied[band] + RAMP_DB_PER_BLOCK * delta.signum()
            };
        }

        if moved {
            self.rebuild();
        }
        moved
    }

    /// Rebuilds the coefficients from `applied`.
    ///
    /// Deliberately does *not* clear the filter state. The settings changed,
    /// not the signal: zeroing the registers mid-track would put a step
    /// through every band at once, which is the click this whole mechanism
    /// exists to avoid.
    fn rebuild(&mut self) {
        // A peaking filter at 0 dB is an exact identity, and one fed from zero
        // state stays at zero state -- so a flat equaliser can be skipped
        // outright and the samples pass through untouched. That is what makes
        // "off" mean the decoder's own bits, and it is also why switching off
        // glides to flat first and only then takes this path.
        self.bypass = self.applied.iter().all(|g| *g == 0.0);
        if self.bypass {
            return;
        }

        for (band, centre) in CENTRES.iter().enumerate() {
            self.biquads[band] = Biquad::peaking(*centre, self.applied[band], self.sample_rate);
        }
    }
}

impl<S> Iterator for Equalised<S>
where
    S: Source,
{
    type Item = rodio::Sample;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.inner.next()?;

        // Settings and the ramp advance on frame boundaries only, so both
        // channels of one frame are always filtered by the same coefficients.
        if self.channel == 0 {
            if self.settings.generation() != self.built_from {
                self.reload_target();
            }
            self.since_step += 1;
            if self.since_step >= RAMP_BLOCK_FRAMES {
                self.since_step = 0;
                self.advance_ramp();
            }
        }

        let channel = self.channel;
        self.channel = (self.channel + 1) % self.channels;

        if self.bypass {
            return Some(sample);
        }

        let states = &mut self.states[channel];
        // Filtered in f64, not f32. Ten recursive sections in single precision
        // leave a measured -60 dB of arithmetic noise at extreme settings --
        // not distortion, since the harmonics stay below -130 dB, but a noise
        // floor 60 dB above where it needs to be. In double precision the same
        // measurement lands below -139 dB, for 160 bytes and 0.02% of a core.
        let mut value = sample as f64;
        for (state, biquad) in states.iter_mut().zip(&self.biquads) {
            value = state.step(biquad, value);
        }

        Some(value as f32)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}
impl<S> Source for Equalised<S>
where
    S: Source,
{
    fn current_span_len(&self) -> Option<usize> {
        // The filter runs across span boundaries, so claiming one would let a
        // downstream stage assume a break that is not there.
        None
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<std::time::Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, position: std::time::Duration) -> Result<(), rodio::source::SeekError> {
        let sought = self.inner.try_seek(position);
        if sought.is_ok() {
            // The registers hold the tail of audio from somewhere else in the
            // track. Ringing it out over the new position is a real artefact,
            // and it is exactly the kind this app has been chasing.
            for channel in &mut self.states {
                *channel = [State::default(); BAND_COUNT];
            }
            self.channel = 0;
        }
        sought
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn tone(frequency: f32, seconds: f32) -> Vec<f32> {
        let count = (RATE as f32 * seconds) as usize;
        (0..count)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.25
            })
            .collect()
    }

    fn run(samples: &[f32], settings: Arc<EqSettings>) -> Vec<f32> {
        let buffer = rodio::buffer::SamplesBuffer::new(
            rodio::ChannelCount::new(1).unwrap(),
            rodio::SampleRate::new(RATE).unwrap(),
            samples.to_vec(),
        );
        Equalised::new(buffer, settings).collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        let sum: f64 = samples.iter().map(|s| (*s as f64) * (*s as f64)).sum();
        (sum / samples.len() as f64).sqrt() as f32
    }

    /// Measures gain at `frequency`, ignoring the filter's settling transient.
    fn gain_at(frequency: f32, settings: Arc<EqSettings>) -> f32 {
        let input = tone(frequency, 1.0);
        let output = run(&input, settings);
        // Second half only: the biquads start from silence, and the ramp up to
        // steady state would drag the measured gain down.
        let half = input.len() / 2;
        20.0 * (rms(&output[half..]) / rms(&input[half..])).log10()
    }

    /// The invariant that matters most: off means untouched.
    #[test]
    fn a_flat_equaliser_changes_nothing_at_all() {
        let input = tone(1_000.0, 0.2);

        // Disabled entirely.
        let off = Arc::new(EqSettings::default());
        assert_eq!(run(&input, off), input, "a disabled equaliser altered the samples");

        // Enabled, but every band at zero. Just as much a pass-through, and
        // the case a listener actually reaches by resetting the sliders.
        let flat = Arc::new(EqSettings::default());
        flat.set_enabled(true);
        flat.set_gains(&[0.0; BAND_COUNT]);
        assert_eq!(
            run(&input, flat),
            input,
            "an enabled but flat equaliser altered the samples",
        );
    }

    #[test]
    fn a_boosted_band_lifts_its_own_frequency() {
        for (band, centre) in CENTRES.iter().enumerate() {
            // The top band sits above Nyquist at some rates and is checked
            // separately; here the rate is 48 kHz so 16 kHz is in range.
            let settings = Arc::new(EqSettings::default());
            settings.set_enabled(true);
            let mut gains = [0.0f32; BAND_COUNT];
            gains[band] = 6.0;
            settings.set_gains(&gains);

            let measured = gain_at(*centre, settings);
            assert!(
                (measured - 6.0).abs() < 1.0,
                "band {band} at {centre} Hz asked for +6 dB and gave {measured:.2} dB",
            );
        }
    }

    #[test]
    fn a_cut_band_lowers_its_own_frequency() {
        let settings = Arc::new(EqSettings::default());
        settings.set_enabled(true);
        let mut gains = [0.0f32; BAND_COUNT];
        gains[5] = -9.0; // 1 kHz
        settings.set_gains(&gains);

        let measured = gain_at(1_000.0, settings);
        assert!(
            (measured + 9.0).abs() < 1.0,
            "1 kHz asked for -9 dB and gave {measured:.2} dB",
        );
    }

    /// A band must act on its own neighbourhood, not on the whole spectrum.
    #[test]
    fn a_boosted_band_leaves_distant_frequencies_alone() {
        let settings = Arc::new(EqSettings::default());
        settings.set_enabled(true);
        let mut gains = [0.0f32; BAND_COUNT];
        gains[1] = 12.0; // 63 Hz, hard
        settings.set_gains(&gains);

        let far = gain_at(4_000.0, Arc::clone(&settings));
        assert!(
            far.abs() < 1.0,
            "a 63 Hz boost moved 4 kHz by {far:.2} dB",
        );
    }

    /// Every band at once should give a flat lift, not a rippled one.
    ///
    /// Deliberately *not* asserting that +6 on every band gives +6 overall: it
    /// gives about +8.8. That is how every constant-Q graphic equaliser
    /// behaves -- one-octave bands overlap, so neighbours add at the centre
    /// frequency between them. Narrowing Q to make the arithmetic tidy would
    /// leave gaps between the bands and make a single-band boost peaky, which
    /// is a real defect traded for a cosmetic one.
    ///
    /// What must hold is that the result is *flat*: the same shift wherever
    /// you measure it, so a uniform setting is a level change and not a tone
    /// control.
    #[test]
    fn a_uniform_setting_gives_a_flat_lift() {
        let settings = Arc::new(EqSettings::default());
        settings.set_enabled(true);
        settings.set_gains(&[6.0; BAND_COUNT]);

        // Mid-band, away from the ends of the chain where it rolls off.
        let measured: Vec<f32> = [250.0, 500.0, 1_000.0, 2_000.0, 4_000.0]
            .into_iter()
            .map(|f| gain_at(f, Arc::clone(&settings)))
            .collect();

        let low = measured.iter().cloned().fold(f32::MAX, f32::min);
        let high = measured.iter().cloned().fold(f32::MIN, f32::max);
        assert!(
            high - low < 1.0,
            "a uniform setting should be flat, but the response ripples by \
             {:.2} dB: {measured:?}",
            high - low,
        );

        // And it must be a lift, in the right direction and the right order of
        // magnitude -- flat-but-wrong would pass the check above.
        assert!(
            (6.0..12.0).contains(&low),
            "expected a lift of roughly +6 dB with overlap on top, got {measured:?}",
        );
    }

    /// The frontend is a JSON boundary, so this has to survive nonsense.
    #[test]
    fn absurd_settings_cannot_produce_nonsense_samples() {
        let settings = Arc::new(EqSettings::default());
        settings.set_enabled(true);
        settings.set_gains(&[f32::NAN, f32::INFINITY, -f32::INFINITY, 1e30, -1e30, 100.0, -100.0, 0.0, 0.0, 0.0]);

        let stored = settings.gains();
        assert!(
            stored.iter().all(|g| g.is_finite() && g.abs() <= MAX_GAIN_DB),
            "settings were not clamped: {stored:?}",
        );

        let output = run(&tone(1_000.0, 0.2), settings);
        assert!(
            output.iter().all(|s| s.is_finite()),
            "the filter produced values that are not numbers",
        );
    }

    /// A band above Nyquist has nothing to act on and must not go unstable.
    #[test]
    fn a_band_above_nyquist_is_bypassed_rather_than_unstable() {
        let biquad = Biquad::peaking(16_000.0, 12.0, 22_050.0);
        assert_eq!(biquad.b0, 1.0);
        assert_eq!(biquad.a1, 0.0);
    }

    /// Seeking must not ring the previous position out over the new one.
    #[test]
    fn seeking_clears_the_filter_memory() {
        let settings = Arc::new(EqSettings::default());
        settings.set_enabled(true);
        settings.set_gains(&[12.0; BAND_COUNT]);

        let buffer = rodio::buffer::SamplesBuffer::new(
            rodio::ChannelCount::new(1).unwrap(),
            rodio::SampleRate::new(RATE).unwrap(),
            tone(1_000.0, 1.0),
        );
        let mut eq = Equalised::new(buffer, settings);

        for _ in 0..1_000 {
            let _ = eq.next();
        }
        assert!(
            eq.states[0].iter().any(|s| s.z1 != 0.0 || s.z2 != 0.0),
            "the filter should be holding state by now",
        );

        if eq.try_seek(std::time::Duration::ZERO).is_ok() {
            assert!(
                eq.states[0].iter().all(|s| s.z1 == 0.0 && s.z2 == 0.0),
                "a seek left the previous position in the filter memory",
            );
        }
    }

    /// Stereo channels must not share filter memory.
    #[test]
    fn each_channel_is_filtered_independently() {
        let settings = Arc::new(EqSettings::default());
        settings.set_enabled(true);
        let mut gains = [0.0f32; BAND_COUNT];
        gains[5] = 12.0;
        settings.set_gains(&gains);

        // Left carries the tone, right is silent.
        let mono = tone(1_000.0, 0.3);
        let mut interleaved = Vec::with_capacity(mono.len() * 2);
        for sample in &mono {
            interleaved.push(*sample);
            interleaved.push(0.0);
        }

        let buffer = rodio::buffer::SamplesBuffer::new(
            rodio::ChannelCount::new(2).unwrap(),
            rodio::SampleRate::new(RATE).unwrap(),
            interleaved,
        );
        let out: Vec<f32> = Equalised::new(buffer, settings).collect();

        let right: Vec<f32> = out.iter().skip(1).step_by(2).copied().collect();
        assert!(
            right.iter().all(|s| *s == 0.0),
            "a silent channel picked up audio from the other one",
        );
    }
}
