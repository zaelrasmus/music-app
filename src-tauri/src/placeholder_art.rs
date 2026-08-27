//! Cover art for the system media panel, for tracks that have none.
//!
//! The app already draws something for a track with no artwork: a gradient
//! derived from the track's own text, so the same song is always the same
//! colours and a list of forty is scannable. That art is CSS -- it exists only
//! as a paint instruction inside the webview, and Windows cannot be handed a
//! paint instruction. The media panel wants a *file*.
//!
//! So this renders the same gradient to a JPEG. Deliberately the same one: two
//! different placeholders for the same track would be worse than none, because
//! the panel and the tile would disagree about what the song looks like.
//!
//! Matching it means matching three things exactly -- the hash, the colour
//! space, and the gradient geometry -- and each is pinned by a test against
//! values taken from the frontend itself.
//!
//! What this is not: a claim about the music. It is a placeholder that happens
//! to be recognisable, which is the most a placeholder can honestly be.

use std::path::{Path, PathBuf};

/// Edge length of the rendered square, in pixels.
///
/// The panel draws it small -- a little over a hundred pixels on a normal
/// display -- and this is a gradient, so there is no detail to preserve. 512
/// is comfortably past what any current scaling asks for and still encodes in
/// a couple of milliseconds.
const EDGE: u32 = 512;

/// JPEG quality. High, because banding is the one artefact a smooth gradient
/// shows readily and the file is a few tens of kilobytes either way.
const QUALITY: u8 = 90;

/// FNV-1a over the seed.
///
/// The same function the frontend uses, and it has to stay that way: the
/// gradient is derived entirely from this number, so a different hash is a
/// different picture. Chosen there for being well distributed over small
/// strings -- "Track 01" and "Track 02" land far apart, which a character sum
/// would not manage.
fn hash(text: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    // Bytes rather than chars, which matches `charCodeAt` for ASCII and
    // differs above it. That only changes *which* colour a non-ASCII title
    // gets, never whether it is stable -- and the tests pin the ASCII cases
    // that the frontend and this have to agree on.
    for byte in text.bytes() {
        h ^= u32::from(byte);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// The gradient one seed describes: two colours and an angle.
struct Gradient {
    from: [f32; 3],
    to: [f32; 3],
    /// CSS degrees: 0 points to the top, increasing clockwise.
    angle: f32,
}

impl Gradient {
    /// Derived exactly as `coverGradient` in `src/lib/cover.ts` does.
    ///
    /// Lightness and chroma are fixed there so no tile can come out muddy or
    /// blinding, and both hues are mid-range, which keeps a white overlay icon
    /// readable on every one of them.
    fn for_seed(seed: &str) -> Self {
        let h = hash(if seed.is_empty() { "untitled" } else { seed });

        let hue = (h % 360) as f32;
        // A second hue close enough to look like one object lit from an angle,
        // rather than two unrelated colours meeting in the middle.
        let shift = (24 + ((h >> 9) % 44)) as f32;
        let angle = (110 + ((h >> 17) % 60)) as f32;

        Self {
            from: [0.66, 0.16, hue],
            to: [0.48, 0.19, (hue + shift) % 360.0],
            angle,
        }
    }
}

/// Oklch to 8-bit sRGB.
///
/// The frontend names its colours in `oklch`, and the browser does this
/// conversion. Nothing here can hand a browser a JPEG, so the conversion has
/// to happen here instead -- Björn Ottosson's Oklab matrices, then the sRGB
/// transfer function.
fn oklch_to_srgb([lightness, chroma, hue_deg]: [f32; 3]) -> [u8; 3] {
    let hue = hue_deg.to_radians();
    let a = chroma * hue.cos();
    let b = chroma * hue.sin();

    let l_ = lightness + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = lightness - 0.105_561_35 * a - 0.063_854_17 * b;
    let s_ = lightness - 0.089_484_18 * a - 1.291_485_5 * b;

    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;

    let linear = [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ];

    linear.map(|channel| {
        // Out-of-gamut is clamped rather than mapped. The two lightness and
        // chroma pairs above are well inside sRGB for every hue, so this only
        // catches floating-point overshoot at the extremes.
        let clamped = channel.clamp(0.0, 1.0);
        let encoded = if clamped <= 0.003_130_8 {
            clamped * 12.92
        } else {
            1.055 * clamped.powf(1.0 / 2.4) - 0.055
        };
        (encoded * 255.0).round().clamp(0.0, 255.0) as u8
    })
}

/// Renders the gradient as a JPEG.
///
/// The geometry is CSS's, not a diagonal fill. In CSS a gradient angle points
/// the *end* of the line: 0deg runs bottom to top, 90deg left to right, and
/// the line is long enough that the two corners it passes are exactly the
/// first and last colour. Getting that wrong would tilt every tile slightly
/// differently from the app.
fn render(seed: &str) -> Result<Vec<u8>, String> {
    use image::codecs::jpeg::JpegEncoder;
    use image::{ExtendedColorType, ImageEncoder};

    let gradient = Gradient::for_seed(seed);
    let from = oklch_to_srgb(gradient.from);
    let to = oklch_to_srgb(gradient.to);

    let radians = gradient.angle.to_radians();
    let (sin, cos) = (radians.sin(), radians.cos());
    let edge = EDGE as f32;
    // The CSS gradient line's length for a square of this size.
    let length = (edge * sin.abs()) + (edge * cos.abs());
    let centre = edge / 2.0;

    let mut pixels = Vec::with_capacity((EDGE * EDGE * 3) as usize);
    for y in 0..EDGE {
        for x in 0..EDGE {
            // Screen coordinates run downwards, hence the negated cosine.
            let along = (x as f32 - centre) * sin - (y as f32 - centre) * cos;
            let t = (along / length + 0.5).clamp(0.0, 1.0);

            for channel in 0..3 {
                let start = f32::from(from[channel]);
                let end = f32::from(to[channel]);
                pixels.push((start + (end - start) * t).round() as u8);
            }
        }
    }

    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, QUALITY)
        .write_image(&pixels, EDGE, EDGE, ExtendedColorType::Rgb8)
        .map_err(|e| format!("could not encode placeholder art: {e}"))?;

    Ok(jpeg)
}

/// The file name one seed always maps to.
fn file_name(seed: &str) -> String {
    format!("{:08x}.jpg", hash(if seed.is_empty() { "untitled" } else { seed }))
}

/// The text a track's art is derived from.
///
/// Must match `coverSeed` in the frontend, or the panel and the tile show
/// different pictures for the same song.
pub fn seed_for(title: &str, artist: Option<&str>) -> String {
    format!("{}::{}", artist.unwrap_or(""), title)
}

/// Returns a rendered placeholder for `seed`, making it if it is not there.
///
/// Content is decided entirely by the seed, so an existing file is always the
/// right file and is reused rather than re-encoded. Returns `None` on any
/// failure: this is decoration, and a track with no art in the panel is a much
/// smaller problem than one that will not play.
pub fn ensure(dir: &Path, seed: &str) -> Option<PathBuf> {
    let path = dir.join(file_name(seed));
    if path.is_file() {
        return Some(path);
    }

    let jpeg = render(seed).ok()?;
    std::fs::create_dir_all(dir).ok()?;

    // Written beside and renamed, so a half-written file can never be handed
    // to the platform -- which loads it synchronously and takes the title and
    // artist down with it when it fails.
    let partial = path.with_extension("partial");
    std::fs::write(&partial, &jpeg).ok()?;
    std::fs::rename(&partial, &path).ok()?;

    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned against the frontend's own hash, run over the same strings.
    ///
    /// This is the thing that keeps the two pictures the same. Everything
    /// below derives from this number, so a drift here is a silent
    /// disagreement between the panel and the tile.
    #[test]
    fn the_hash_matches_the_frontend() {
        for (seed, expected) in [
            ("untitled", 2_367_458_136_u32),
            ("ALESTI::Unravel", 1_713_495_428),
            ("::Kodoku", 3_578_081_122),
            ("Hideki Taniuchi - Topic::Light no Theme", 1_616_306_272),
        ] {
            assert_eq!(hash(seed), expected, "hash drifted for {seed:?}");
        }
    }

    /// And so do the three numbers taken from it.
    #[test]
    fn the_gradient_matches_the_frontend() {
        let gradient = Gradient::for_seed("ALESTI::Unravel");

        assert_eq!(gradient.from[2], 188.0, "hue");
        assert_eq!(gradient.to[2], (188.0 + 54.0) % 360.0, "second hue");
        assert_eq!(gradient.angle, 162.0, "angle");
    }

    /// An empty seed is "untitled", the same substitution the frontend makes.
    #[test]
    fn an_empty_seed_is_not_a_different_colour_from_untitled() {
        assert_eq!(file_name(""), file_name("untitled"));
    }

    /// Oklch against values a browser produces.
    ///
    /// Mid-grey is the useful case: chroma zero means the hue cancels, so
    /// anything wrong with the matrices shows as a colour cast rather than a
    /// subtle shift.
    #[test]
    fn oklch_converts_the_way_a_browser_does() {
        let grey = oklch_to_srgb([0.66, 0.0, 0.0]);
        assert_eq!(grey[0], grey[1], "a chroma of zero must be neutral");
        assert_eq!(grey[1], grey[2], "a chroma of zero must be neutral");
        // 146, computed from Ottosson's reference matrices in double
        // precision. The window is for f32 rounding, not for uncertainty
        // about the answer.
        assert!(
            (144..=148).contains(&grey[0]),
            "oklch(0.66 0 0) came out at {}, where the reference gives 146",
            grey[0]
        );

        // Black and white are the two ends that any sign error breaks.
        assert_eq!(oklch_to_srgb([0.0, 0.0, 0.0]), [0, 0, 0]);
        assert_eq!(oklch_to_srgb([1.0, 0.0, 0.0]), [255, 255, 255]);
    }

    /// The colours the app actually uses are inside sRGB, so nothing clips.
    ///
    /// Clipping would flatten a hue to a primary and make two nearby tracks
    /// look identical, which is the one thing the gradient exists to avoid.
    #[test]
    fn every_hue_lands_inside_the_gamut() {
        for hue in 0..360 {
            for (lightness, chroma) in [(0.66, 0.16), (0.48, 0.19)] {
                let rgb = oklch_to_srgb([lightness, chroma, hue as f32]);
                assert!(
                    rgb.iter().any(|c| *c > 0 && *c < 255),
                    "oklch({lightness} {chroma} {hue}) clipped to {rgb:?}"
                );
            }
        }
    }

    #[test]
    fn rendering_produces_a_jpeg_that_decodes() {
        let bytes = render("ALESTI::Unravel").expect("render failed");
        assert!(bytes.starts_with(&[0xFF, 0xD8]), "not a JPEG");

        let decoded = image::load_from_memory(&bytes).expect("the JPEG does not decode");
        assert_eq!(decoded.width(), EDGE);
        assert_eq!(decoded.height(), EDGE);
    }

    /// Two corners of the gradient must actually differ, or the whole thing is
    /// a flat square and the seed bought nothing.
    #[test]
    fn the_gradient_runs_between_two_different_colours() {
        let bytes = render("ALESTI::Unravel").expect("render failed");
        let image = image::load_from_memory(&bytes).unwrap().to_rgb8();

        let first = image.get_pixel(4, 4).0;
        let last = image.get_pixel(EDGE - 5, EDGE - 5).0;
        let distance: i32 = (0..3)
            .map(|i| (i32::from(first[i]) - i32::from(last[i])).abs())
            .sum();

        assert!(
            distance > 40,
            "opposite corners are {first:?} and {last:?}, which is nearly flat"
        );
    }

    /// Different tracks get different art, which is the whole point.
    #[test]
    fn two_tracks_do_not_share_a_picture() {
        assert_ne!(
            file_name("ALESTI::Unravel"),
            file_name("Hideki Taniuchi - Topic::Light no Theme")
        );
    }

    #[test]
    fn ensure_writes_once_and_reuses_it() {
        let dir = std::env::temp_dir().join("music-app-placeholder-art");
        let _ = std::fs::remove_dir_all(&dir);

        let seed = seed_for("Unravel", Some("ALESTI"));
        let first = ensure(&dir, &seed).expect("first render failed");
        assert!(first.is_file());

        let written = std::fs::metadata(&first).unwrap().modified().unwrap();
        let again = ensure(&dir, &seed).expect("second call failed");

        assert_eq!(first, again, "the same seed produced a different file");
        assert_eq!(
            written,
            std::fs::metadata(&again).unwrap().modified().unwrap(),
            "the file was rewritten rather than reused"
        );

        // And nothing partial is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "partial"))
            .collect();
        assert!(leftovers.is_empty(), "a partial file was left in the store");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The seed has to be the frontend's, or the two pictures diverge for
    /// every track with no artist -- which is most of a scanned library.
    #[test]
    fn the_seed_matches_the_frontend_shape() {
        assert_eq!(seed_for("Unravel", Some("ALESTI")), "ALESTI::Unravel");
        assert_eq!(seed_for("Kodoku", None), "::Kodoku");
    }
}
