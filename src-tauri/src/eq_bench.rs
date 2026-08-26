//! What the equaliser actually costs, in time and in memory.
//!
//! Run with `cargo test --release --lib eq_bench -- --ignored --nocapture`.
//! Release matters: a debug build measures the absence of inlining, not the
//! filter.

#[cfg(test)]
mod bench {
    use std::sync::Arc;
    use std::time::Instant;

    use crate::equalizer::{EqSettings, Equalised, BAND_COUNT};

    const RATE: u32 = 48_000;
    /// Five minutes of stereo, which is a long track.
    const SECONDS: usize = 300;

    fn signal() -> Vec<f32> {
        let frames = RATE as usize * SECONDS;
        (0..frames * 2)
            .map(|i| {
                let t = (i / 2) as f32 / RATE as f32;
                0.3 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
            })
            .collect()
    }

    fn run(samples: Vec<f32>, settings: Arc<EqSettings>) -> (f64, usize) {
        let buffer = rodio::buffer::SamplesBuffer::new(
            rodio::ChannelCount::new(2).unwrap(),
            rodio::SampleRate::new(RATE).unwrap(),
            samples,
        );
        let started = Instant::now();
        let out: Vec<f32> = Equalised::new(buffer, settings).collect();
        let elapsed = started.elapsed().as_secs_f64();
        (elapsed, out.len())
    }

    #[test]
    #[ignore = "benchmark"]
    fn what_the_equaliser_costs() {
        let input = signal();

        // Off: the path almost every listener is on.
        let off = Arc::new(EqSettings::default());
        let (bypass, _) = run(input.clone(), off);

        // On, every band working.
        let on = Arc::new(EqSettings::default());
        on.set_enabled(true);
        on.set_gains(&[6.0, -4.0, 3.0, -2.0, 5.0, -3.0, 4.0, -5.0, 2.0, 6.0]);
        let (filtered, samples) = run(input, on);

        let realtime = SECONDS as f64;
        eprintln!("--- {SECONDS}s of 48 kHz stereo ({samples} samples) ---");
        eprintln!(
            "  equaliser off: {bypass:.4}s  ({:.0}x realtime)",
            realtime / bypass,
        );
        eprintln!(
            "  equaliser on:  {filtered:.4}s  ({:.0}x realtime)",
            realtime / filtered,
        );
        eprintln!(
            "  the filtering itself: {:.4}s for {SECONDS}s of audio = {:.4}% of one core",
            filtered - bypass,
            100.0 * (filtered - bypass) / realtime,
        );

        // Memory: the filter state, which is the only thing that scales.
        let per_channel = std::mem::size_of::<[[f64; 2]; BAND_COUNT]>();
        eprintln!(
            "  filter state: {} bytes per channel, {} bytes for stereo",
            per_channel,
            per_channel * 2,
        );
        eprintln!(
            "  (the same in f32 would be {} bytes for stereo)",
            std::mem::size_of::<[[f32; 2]; BAND_COUNT]>() * 2,
        );
    }
}
