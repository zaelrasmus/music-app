//! Lyrics: finding them, parsing them, and being honest about what was found.
//!
//! # The shape that matters
//!
//! A line either has a time or it does not, and [`LyricLine::at_ms`] is an
//! `Option` for exactly that reason. Unsynced lyrics reach the UI with no
//! timestamps at all, so no amount of frontend enthusiasm can scroll them in
//! time with the music.
//!
//! That is a deliberate reaction to how this is usually done. The obvious
//! shortcut -- spread `n` plain lines evenly across a track's duration and
//! hand them over as if they were synced -- produces something that *looks*
//! authoritative and is wrong for the entire song, because verses, choruses,
//! an eight-second intro and a fade-out are not evenly spaced. Static text is
//! honest. Confidently wrong highlighting is worse than none, so the type
//! makes it unrepresentable.
//!
//! # Four answers, not two
//!
//! [`Kind`] has three variants and the whole result is an `Option`, because
//! "no lyrics" is not one state:
//!
//! ```text
//! Synced        scrolling, karaoke
//! Plain         static text
//! Instrumental  "this track has no vocals" -- a positive, correct answer
//! None          we genuinely do not know
//! ```
//!
//! Collapsing `Instrumental` into `None` is what makes a library of game
//! soundtracks feel like the feature is broken. It is not broken; those tracks
//! have no words, and providers say so.
//!
//! # What this module does not do
//!
//! No network. Providers arrive next; the `lyrics` table they will fill is
//! already read from here, so nothing above this layer changes when they do.
//!
//! `SYLT` -- ID3v2's own synchronised-lyrics frame -- is not read. lofty can
//! parse it, but only from a raw `Id3v2Tag`, which means opening and parsing
//! the file a second time (see `scanner`'s note on how slow lofty can be on a
//! file it dislikes). Practically nothing writes SYLT; taggers that store
//! timed lyrics put LRC text in `USLT`, and that path is covered. The seam is
//! [`embedded`] if that ever stops being true.

use std::path::Path;

use serde::Serialize;
use sqlx::{Row, SqlitePool};
use tauri::State;

use crate::db::Db;

/// A sidecar larger than this is not lyrics.
const MAX_SIDECAR_BYTES: u64 = 1024 * 1024;

/// How far the user may shift lyrics against the audio, either way.
///
/// Wide enough for the worst intro card on a YouTube rip, narrow enough that
/// a fat-fingered value cannot push every line off the end of the track.
const MAX_OFFSET_MS: i64 = 30_000;

/// Below this many timestamped lines, a "synced" parse is a false positive.
const MIN_SYNCED_LINES: usize = 2;

/// One line, with a time if it has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricLine {
    /// Milliseconds from the start of the recording. `None` on unsynced
    /// lyrics, and there is no way to invent it later.
    pub at_ms: Option<i64>,
    /// May be empty: an LRC timestamp with no words marks an instrumental
    /// gap, which the UI draws rather than skips.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Synced,
    Plain,
    Instrumental,
}

/// What to show for one track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackLyrics {
    pub kind: Kind,
    /// Empty when `kind` is [`Kind::Instrumental`].
    pub lines: Vec<LyricLine>,
    /// Where these came from, for the UI to attribute: `sidecar`, `embedded`,
    /// or a provider name.
    pub origin: String,
    /// This track's own shift, in milliseconds, added on top of whatever
    /// `[offset:]` the LRC itself carried. Positive means the lyrics run late
    /// against this audio and need pulling earlier.
    pub offset_ms: i64,
    /// The same lines, romanised, when the provider had them.
    ///
    /// A parallel track sharing one set of timestamps rather than a
    /// different set of lyrics, so the reader can swap between them without
    /// the highlight moving.
    pub romaji: Option<Vec<LyricLine>>,
}

// --- identity ------------------------------------------------------------

/// A song, as opposed to a row in `tracks`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// Cleaned, for asking a provider. `None` when nothing usable is left.
    pub artist: Option<String>,
    /// Cleaned, for asking a provider.
    pub title: String,
    /// Normalised, for `lyrics.identity_key`.
    pub key: String,
}

/// Bracketed noise that is about the *upload*, not about the song.
///
/// Matched against a bracket group's entire contents, case-insensitively, so
/// removal only ever happens on an exact known phrase. Anything unrecognised
/// is left alone -- which is the point. "(KITSUN3POWR REMIX V3)", "(Live)",
/// "(Acoustic)" and "(Zebrahead Ver)" name a *different recording*, and
/// stripping them would file a remix under the original's lyrics.
const TITLE_NOISE: &[&str] = &[
    "official",
    "official video",
    "official music video",
    "official audio",
    "official visualizer",
    "official lyric video",
    "official lyrics video",
    "lyric video",
    "lyrics video",
    "lyrics",
    "lyric",
    "audio",
    "music video",
    "visualizer",
    "hd",
    "hq",
    "full hd",
    "4k",
    "1080p",
    "720p",
    "mv",
    "m/v",
    "bga",
];

/// Bracket pairs seen in the wild, including the Japanese ones that show up
/// throughout a library built from YouTube.
const BRACKETS: &[(char, char)] = &[
    ('(', ')'),
    ('[', ']'),
    ('{', '}'),
    ('\u{3010}', '\u{3011}'),
    ('\u{ff08}', '\u{ff09}'),
];

/// Works out which song a track row is about.
pub fn identify(artist: Option<&str>, title: &str) -> Identity {
    let artist = artist.map(clean_artist).filter(|a| !a.is_empty());
    // The artist has to be known before the title can be cleaned: half of
    // what makes a title wrong is the artist's own name sitting in front of
    // it.
    let title = clean_title(strip_artist_prefix(title, artist.as_deref()));

    // A unit separator, so a title containing whatever we joined with cannot
    // collide with a different (artist, title) pair.
    let key = format!(
        "{}\u{001f}{}",
        artist.as_deref().unwrap_or_default().to_lowercase(),
        title.to_lowercase()
    );

    Identity { artist, title, key }
}

/// What a *channel* is called, as opposed to what the artist is called.
///
/// A remote track's artist is whoever uploaded it, and uploaders decorate
/// their names. "- Topic" is YouTube's own auto-generated channel for a
/// catalogue artist; the rest are what people add by hand.
///
/// This matters more than it looks, because lrclib matches `artist_name` as a
/// *prefix*. Being wrong in the direction of too long is fatal rather than
/// merely imprecise:
///
/// ```text
/// artist_name=ivycomb        -> 3 candidates, one exact
/// artist_name=Ivycomb        -> 3 candidates, one exact
/// artist_name=Ivycomb Music  -> 0
/// ```
///
/// One word of channel decoration was the whole difference between finding a
/// song's lyrics and finding nothing at all.
const CHANNEL_SUFFIXES: &[&str] = &[
    "- topic",
    "official channel",
    "official artist channel",
    "official",
    "music",
    "records",
    "recordings",
    "channel",
    "vevo",
    "tv",
];

/// Strips channel decoration from an uploader's name.
///
/// Applied repeatedly, because they stack: "Someone Official Music" is two.
///
/// Never strips everything -- a channel actually called "Music" keeps its
/// name, since an empty artist is worse than a decorated one. And when this
/// guesses wrong, the cost is a search that finds nothing and a user who picks
/// from [`search`] by hand, rather than a wrong lyric shown confidently.
fn clean_artist(artist: &str) -> String {
    let mut artist = artist.trim();
    while let Some(shorter) = strip_channel_suffix(artist) {
        artist = shorter;
    }
    collapse_whitespace(artist.trim_matches(|c: char| c.is_whitespace() || c == '-'))
}

/// One layer of decoration, or `None`.
///
/// The boundary check is what keeps this from eating names: without it "MTV"
/// loses its "TV" and becomes "M". It does still take the "Records" off "The
/// Records", which is a real band -- and the reason that is tolerable is that
/// a wrong strip costs a search that finds nothing, which lands the user in
/// [`search`] to pick by hand. A wrong *match* would cost them a song's worth
/// of confidently mistimed words.
fn strip_channel_suffix(artist: &str) -> Option<&str> {
    for suffix in CHANNEL_SUFFIXES {
        // Sliced by byte offset without lowercasing first: `to_lowercase` can
        // change a string's length, so an index taken from the lowered copy
        // does not necessarily land on a character boundary in this one.
        // `continue`, not `?`: a suffix longer than the name means *this*
        // suffix does not fit, not that none of the others can.
        let Some(start) = artist.len().checked_sub(suffix.len()) else {
            continue;
        };
        let Some(tail) = artist.get(start..) else {
            continue;
        };
        if !tail.eq_ignore_ascii_case(suffix) {
            continue;
        }

        let head = &artist[..start];
        let boundary = |c: char| c.is_whitespace() || c == '-' || c == '|';
        if head.is_empty() || !head.ends_with(boundary) {
            continue;
        }

        let trimmed = head.trim_end_matches(boundary);
        if trimmed.is_empty() {
            continue;
        }
        return Some(trimmed);
    }
    None
}

/// Words introducing a guest, wherever they appear.
///
/// A featured artist is *artist* information that happens to have been typed
/// into the title, and providers file it where it belongs: lrclib holds this
/// song as `ALESTI feat. Siamese` / `Unravel`, not as `ALESTI` /
/// `Unravel (feat. Siamese)`. Asking it the second way returns nothing at all.
const FEATURE_MARKERS: &[&str] = &["feat", "feat.", "ft", "ft.", "featuring", "w/"];

/// Removes the artist's own name from the front of their song's title.
///
/// `"ALESTI - Unravel"` is how a filename becomes a title, and asking lrclib
/// for a track called that finds nothing — the artist is already a separate
/// field, and repeating it inside the title only makes the title wrong.
///
/// Only ever strips a prefix that *is* the artist, so a song genuinely called
/// "Vancouver - Something" keeps its name unless the artist is Vancouver.
fn strip_artist_prefix<'a>(title: &'a str, artist: Option<&str>) -> &'a str {
    let Some(artist) = artist.map(str::trim).filter(|a| !a.is_empty()) else {
        return title;
    };

    let trimmed = title.trim();
    // `get` rather than a slice: an artist whose byte length lands inside a
    // multi-byte character would otherwise panic here.
    let Some(head) = trimmed.get(..artist.len()) else {
        return title;
    };
    if !head.eq_ignore_ascii_case(artist) {
        return title;
    }

    let rest = trimmed[artist.len()..].trim_start();
    let Some(rest) = rest.strip_prefix(['-', '\u{2013}', '\u{2014}', '|', ':']) else {
        return title;
    };

    let rest = rest.trim_start();
    // "ALESTI -" on its own is not a title; keep what we had.
    if rest.is_empty() { title } else { rest }
}

/// Removes bracketed groups that describe the upload rather than the song.
fn clean_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut rest = title.trim();

    'outer: while !rest.is_empty() {
        for (open, close) in BRACKETS {
            let Some(start) = rest.find(*open) else {
                continue;
            };
            let after_open = &rest[start + open.len_utf8()..];
            let Some(len) = after_open.find(*close) else {
                continue;
            };

            let inner = &after_open[..len];
            let tail = &after_open[len + close.len_utf8()..];

            if is_noise(inner) || is_feature(inner) {
                out.push_str(&rest[..start]);
                out.push(' ');
                rest = tail;
                continue 'outer;
            }
        }
        // Nothing removable left; the remainder is the title.
        out.push_str(rest);
        break;
    }

    let out = collapse_whitespace(&out);
    let trimmed = out.trim_matches(|c: char| c.is_whitespace() || c == '-' || c == '|');
    let trimmed = collapse_whitespace(trimmed);

    // Removing every bracket group is how a title that is nothing but noise
    // becomes empty. An empty title matches everything, so keep the original.
    if trimmed.is_empty() {
        collapse_whitespace(title)
    } else {
        trimmed
    }
}

fn is_noise(inner: &str) -> bool {
    let inner = inner.trim().to_lowercase();
    TITLE_NOISE.iter().any(|noise| *noise == inner)
}

/// A bracketed group that introduces a guest, like "(feat. Siamese)".
///
/// Matched on the *first word only*, so "(feat. Siamese)" goes and
/// "(Featuring the Void)" — a title, not a credit — would too. That is the
/// accepted cost: guessing wrong here loses a credit from a title, which
/// providers do not index on anyway.
fn is_feature(inner: &str) -> bool {
    let first = inner.split_whitespace().next().unwrap_or_default();
    FEATURE_MARKERS
        .iter()
        .any(|marker| marker.eq_ignore_ascii_case(first))
}

/// The artist before any collaborators.
///
/// Applied to *both* sides of a comparison, which is what makes it safe: the
/// same truncation on both means an odd split cancels out rather than causing
/// a mismatch. "Malcolm X" reduces to "malcolm" on both sides and still
/// compares equal.
///
/// Without it, this song is not confidently matched at all. We hold the artist
/// as `ALESTI`; lrclib's timed copy is filed under `ALESTI feat. Siamese`, and
/// string equality says those are different people — so the only row whose
/// artist matched exactly was an untimed one, and a plain lyric would have won
/// over a synced one for no reason but a guest credit.
fn primary_artist(artist: &str) -> String {
    const COLLABORATORS: &[&str] = &[
        "feat", "feat.", "ft", "ft.", "featuring", "x", "and", "vs", "vs.", "with", "&",
    ];

    let folded = fold(artist);

    // Punctuation first: "ALESTI/Siamese", "A, B", "A & B".
    let mut cut = folded.find(['&', ',', '/', ';']).unwrap_or(folded.len());

    // Then whole words, so an artist called "Ampersand" is left alone.
    let mut offset = 0usize;
    for token in folded.split(' ') {
        if offset > 0
            && offset < cut
            && COLLABORATORS
                .iter()
                .any(|word| word.eq_ignore_ascii_case(token))
        {
            cut = offset;
            break;
        }
        offset += token.len() + 1;
    }

    folded[..cut].trim().to_string()
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// --- LRC -----------------------------------------------------------------

/// LRC header tags. Everything here is *about* the file, not part of it.
///
/// Veluna's parser has no such list, so `[ar:System Of A Down]` parses as a
/// timestamp of zero -- its minutes field is `"ar".parse().unwrap_or(0.0)` --
/// and renders the band's name as the first lyric of the song. lrclib happens
/// to send clean payloads with no header tags, which is why that survives
/// there and would not survive a `.lrc` off disk.
const META_KEYS: &[&str] = &[
    "ar", "ti", "al", "au", "by", "re", "ve", "length", "offset", "la", "tool", "encoding", "id",
];

/// Parses LRC, or decides this is not LRC.
///
/// Returns `None` rather than an empty vector when the text has no usable
/// timing, so the caller falls through to treating it as plain text.
pub fn parse_lrc(text: &str) -> Option<Vec<LyricLine>> {
    let mut tag_offset_ms: i64 = 0;
    let mut lines: Vec<LyricLine> = Vec::new();
    let mut untimed = 0usize;

    for raw in text.lines() {
        let line = raw.trim_start_matches('\u{feff}').trim();
        if line.is_empty() {
            continue;
        }

        let mut rest = line;
        let mut times: Vec<i64> = Vec::new();
        let mut saw_meta = false;

        // Leading bracket groups, in order, until one is neither a timestamp
        // nor a known header tag -- at which point it is part of the lyric.
        // `[00:12.00][Chorus] la la` keeps "[Chorus] la la" as its text.
        while let Some(open) = rest.strip_prefix('[') {
            let Some(end) = open.find(']') else { break };
            let group = &open[..end];
            let after = &open[end + 1..];

            if let Some(ms) = parse_timestamp(group) {
                times.push(ms);
                rest = after;
                continue;
            }

            let Some((key, value)) = split_meta(group) else {
                break;
            };
            if key.eq_ignore_ascii_case("offset") {
                if let Ok(ms) = value.trim().parse::<i64>() {
                    tag_offset_ms = ms.clamp(-MAX_OFFSET_MS, MAX_OFFSET_MS);
                }
            }
            saw_meta = true;
            rest = after;
        }

        let text = rest.trim().to_string();

        if times.is_empty() {
            // Prose. Counted so a plain lyric sheet with one stray bracket
            // cannot be mistaken for a synced one.
            if !saw_meta && !text.is_empty() {
                untimed += 1;
            }
            continue;
        }

        for at_ms in times {
            lines.push(LyricLine {
                at_ms: Some(at_ms),
                text: text.clone(),
            });
        }
    }

    if lines.len() < MIN_SYNCED_LINES || untimed > lines.len() {
        return None;
    }

    // The header's own correction, applied here so the stored text stays
    // exactly what the provider or the file said. Positive shifts earlier.
    if tag_offset_ms != 0 {
        for line in &mut lines {
            if let Some(at) = line.at_ms.as_mut() {
                *at = (*at - tag_offset_ms).max(0);
            }
        }
    }

    // A repeated chorus is written `[00:41.20][02:15.60]text`, which arrives
    // here out of order.
    lines.sort_by_key(|line| line.at_ms);
    lines.dedup();

    Some(lines)
}

/// `mm:ss`, `mm:ss.xx`, `mm:ss:xx` or `hh:mm:ss.xx`, in milliseconds.
///
/// The three-field forms are genuinely ambiguous -- `[01:02:03]` is either an
/// hour and change or a minute and change -- and are told apart by whether the
/// last field carries an explicit fractional point. Without one it is read as
/// the legacy `mm:ss:centiseconds`, because a lyrics file for a track over an
/// hour long is not a thing that happens and a two-digit centisecond field is.
fn parse_timestamp(group: &str) -> Option<i64> {
    let group = group.trim();
    let parts: Vec<&str> = group.split(':').collect();

    let (hours, minutes, seconds_field) = match parts.as_slice() {
        [m, s] => (0i64, parse_u(m)?, *s),
        [h, m, s] if s.contains('.') => (parse_u(h)?, parse_u(m)?, *s),
        [m, s, cs] => {
            let minutes = parse_u(m)?;
            let seconds = parse_u(s)?;
            let frac = parse_fraction(cs)?;
            return Some(minutes * 60_000 + seconds * 1_000 + frac);
        }
        _ => return None,
    };

    let (seconds, frac) = match seconds_field.split_once('.') {
        Some((s, f)) => (parse_u(s)?, parse_fraction(f)?),
        None => (parse_u(seconds_field)?, 0),
    };

    Some(hours * 3_600_000 + minutes * 60_000 + seconds * 1_000 + frac)
}

fn parse_u(field: &str) -> Option<i64> {
    let field = field.trim();
    if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    field.parse().ok()
}

/// A fractional second. One digit is tenths, two centiseconds, three
/// milliseconds; anything longer is truncated rather than rejected.
fn parse_fraction(field: &str) -> Option<i64> {
    let field = field.trim();
    if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: i64 = field[..field.len().min(3)].parse().ok()?;
    Some(match field.len() {
        1 => value * 100,
        2 => value * 10,
        _ => value,
    })
}

fn split_meta(group: &str) -> Option<(&str, &str)> {
    let (key, value) = group.split_once(':')?;
    let key = key.trim();
    META_KEYS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(key))
        .then_some((key, value))
}

/// Plain text, with header tags dropped and stanza breaks kept.
///
/// Blank lines are structure in a lyric sheet, so runs of them collapse to one
/// rather than disappearing.
pub fn parse_plain(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    for raw in text.lines() {
        let line = raw.trim_start_matches('\u{feff}').trim();

        if is_meta_only(line) {
            continue;
        }
        if line.is_empty() && (out.is_empty() || matches!(out.last(), Some(last) if last.is_empty()))
        {
            continue;
        }
        out.push(line.to_string());
    }

    while matches!(out.last(), Some(last) if last.is_empty()) {
        out.pop();
    }
    out
}

/// True for `[ar:...]` and friends, and for a line that is nothing but those.
fn is_meta_only(line: &str) -> bool {
    let mut rest = line.trim();
    let mut saw = false;
    while let Some(open) = rest.strip_prefix('[') {
        let Some(end) = open.find(']') else {
            return false;
        };
        if split_meta(&open[..end]).is_none() {
            return false;
        }
        saw = true;
        rest = open[end + 1..].trim_start();
    }
    saw && rest.is_empty()
}

// --- local sources -------------------------------------------------------

/// An `.lrc` beside the audio file.
///
/// First in the chain, ahead of the file's own tags: someone who put a `.lrc`
/// next to a track did it deliberately and more recently than whoever wrote
/// the tag.
pub fn sidecar(audio: &Path) -> Option<String> {
    // Two spellings for Linux, where they are different files. On Windows the
    // second lookup simply finds the first one again.
    for extension in ["lrc", "LRC"] {
        let path = audio.with_extension(extension);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_SIDECAR_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if let Some(text) = decode(&bytes) {
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// UTF-8, or UTF-16 if it announces itself with a byte-order mark.
///
/// A BOM has to be handled or the leading `[` of the first timestamp is never
/// seen and a perfectly good file parses as prose. UTF-16 is worth the twenty
/// lines because Windows text editors still write it by default and `.lrc` is
/// a format people edit by hand.
///
/// Anything else -- Shift-JIS, GB18030, Latin-1 with accents -- is refused
/// rather than decoded lossily, because mojibake displayed as lyrics is worse
/// than no lyrics and much harder to explain.
fn decode(bytes: &[u8]) -> Option<String> {
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8(rest.to_vec()).ok(),
        [0xFF, 0xFE, rest @ ..] => decode_utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => decode_utf16(rest, u16::from_be_bytes),
        _ => String::from_utf8(bytes.to_vec()).ok(),
    }
}

fn decode_utf16(bytes: &[u8], to_u16: fn([u8; 2]) -> u16) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| to_u16([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

/// Lyrics stored in the file's own tags.
///
/// Both keys are tried because ID3v2 -- which is every MP3, and this library
/// is 1003 of them -- has no `Lyrics` item at all. lofty maps `USLT` to
/// `UnsyncLyrics` and says so in its own source: `ItemKey::Lyrics` "is **not**
/// supported in ID3v2". Vorbis comments and MP4 use `Lyrics`.
///
/// "Unsync" names the *frame*, not the contents. Taggers routinely write LRC
/// text straight into `USLT`, so what comes back here is often fully
/// timestamped -- which is why the caller parses it rather than assuming.
pub fn embedded(audio: &Path) -> Option<String> {
    use lofty::file::TaggedFileExt;

    let tagged = lofty::read_from_path(audio).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;

    for key in [
        lofty::tag::ItemKey::UnsyncLyrics,
        lofty::tag::ItemKey::Lyrics,
    ] {
        if let Some(text) = tag.get_string(key) {
            if !text.trim().is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

/// Reads text of unknown shape into whichever kind it turns out to be.
fn from_text(
    text: &str,
    romaji: Option<&str>,
    origin: &str,
    offset_ms: i64,
) -> Option<TrackLyrics> {
    // Only ever offered alongside timed lyrics: romanised lines are useful
    // because they can be followed, and an untimed block of them is just the
    // song again in a different alphabet.
    let romanised = romaji.and_then(parse_lrc);

    if let Some(lines) = parse_lrc(text) {
        return Some(TrackLyrics {
            kind: Kind::Synced,
            lines,
            origin: origin.to_string(),
            offset_ms,
            romaji: romanised,
        });
    }

    let plain = parse_plain(text);
    if plain.is_empty() {
        return None;
    }

    Some(TrackLyrics {
        kind: Kind::Plain,
        lines: plain
            .into_iter()
            .map(|text| LyricLine { at_ms: None, text })
            .collect(),
        origin: origin.to_string(),
        offset_ms,
        romaji: None,
    })
}

// --- resolution ----------------------------------------------------------

/// Everything known about one track that bears on finding its lyrics.
struct TrackRow {
    title: String,
    artist: Option<String>,
    local_path: Option<String>,
    /// Seconds, and the only thing that separates the right lyrics from a
    /// cover of the same song when the artist is unknown.
    duration_secs: Option<i64>,
    offset_ms: i64,
}

async fn load(pool: &SqlitePool, track_id: i64) -> Result<TrackRow, String> {
    let row = sqlx::query(
        "SELECT title, artist, local_path, duration_secs, lyrics_offset_ms \
         FROM tracks WHERE id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "That track is not in the library.".to_string())?;

    Ok(TrackRow {
        title: row.try_get("title").map_err(|e| e.to_string())?,
        artist: row.try_get("artist").ok().flatten(),
        local_path: row.try_get("local_path").ok().flatten(),
        // Both of these are INTEGER columns and both are decoded as `i64`.
        // Reading an INTEGER as `f64` fails at runtime, and behind a swallowed
        // error it silently disables whatever depends on it -- the single most
        // expensive bug class in this codebase, three times over.
        duration_secs: row
            .try_get::<Option<i64>, _>("duration_secs")
            .ok()
            .flatten()
            .filter(|secs| *secs > 0),
        offset_ms: row.try_get::<i64, _>("lyrics_offset_ms").unwrap_or(0),
    })
}

/// The chain: sidecar, then the file's tags, then whatever a provider found
/// earlier for this song.
///
/// Local reads are not cached in `lyrics`. They cost a millisecond, they
/// belong to one file rather than to a song, and storing them would only
/// create a way for the cache to disagree with the file on disk.
pub async fn resolve(pool: &SqlitePool, track_id: i64) -> Result<Option<TrackLyrics>, String> {
    let track = load(pool, track_id).await?;

    if let Some(path) = track.local_path.clone() {
        let offset = track.offset_ms;
        // lofty parses the whole file, and `scanner` documents a case where
        // that took 341 seconds. Not on the async runtime's thread.
        let found = tauri::async_runtime::spawn_blocking(move || {
            let path = Path::new(&path);
            sidecar(path)
                .and_then(|text| from_text(&text, None, "sidecar", offset))
                .or_else(|| embedded(path).and_then(|text| from_text(&text, None, "embedded", offset)))
        })
        .await
        .map_err(|e| e.to_string())?;

        if found.is_some() {
            return Ok(found);
        }
    }

    let identity = identify(track.artist.as_deref(), &track.title);
    Ok(cached(pool, &identity, track.offset_ms).await?.into_hit())
}

/// What the cache has to say about a song.
///
/// Three states rather than two, because "nothing to show" and "do not ask
/// again" are the same to a reader and opposite to a fetcher.
enum Cached {
    Hit(TrackLyrics),
    /// Never asked, or asked long enough ago to be worth asking again.
    Worth,
    /// Asked recently. There is nothing there.
    Empty,
}

impl Cached {
    fn into_hit(self) -> Option<TrackLyrics> {
        match self {
            Cached::Hit(lyrics) => Some(lyrics),
            _ => None,
        }
    }
}

/// How long a "nothing found" stands before it is worth asking again.
///
/// Not forever. lrclib's catalogue is contributed by its users, so a song with
/// no lyrics today can have them next month, and a permanent negative would
/// make the app permanently wrong about it. Not zero either -- without a hold
/// the answer costs a request on every single play.
const NEGATIVE_HOLDS_FOR_SECS: i64 = 14 * 24 * 60 * 60;

/// What a provider found for this song, if one was ever asked.
async fn cached(
    pool: &SqlitePool,
    identity: &Identity,
    offset_ms: i64,
) -> Result<Cached, String> {
    let Some(row) = sqlx::query(
        "SELECT synced, plain, romaji, instrumental, provider, \
                unixepoch() - fetched_at AS age_secs \
         FROM lyrics WHERE identity_key = ?",
    )
    .bind(&identity.key)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?
    else {
        return Ok(Cached::Worth);
    };

    let provider: String = row.try_get("provider").unwrap_or_default();
    let synced: Option<String> = row.try_get("synced").ok().flatten();
    let plain: Option<String> = row.try_get("plain").ok().flatten();
    let romaji: Option<String> = row.try_get("romaji").ok().flatten();
    let instrumental: i64 = row.try_get("instrumental").unwrap_or(0);
    let age_secs: i64 = row.try_get("age_secs").unwrap_or(0);

    if let Some(text) = synced.filter(|t| !t.trim().is_empty()) {
        if let Some(found) = from_text(&text, romaji.as_deref(), &provider, offset_ms) {
            return Ok(Cached::Hit(found));
        }
    }
    if let Some(text) = plain.filter(|t| !t.trim().is_empty()) {
        if let Some(found) = from_text(&text, romaji.as_deref(), &provider, offset_ms) {
            return Ok(Cached::Hit(found));
        }
    }
    if instrumental == 1 {
        return Ok(Cached::Hit(TrackLyrics {
            kind: Kind::Instrumental,
            lines: Vec::new(),
            origin: provider,
            offset_ms,
            romaji: None,
        }));
    }

    // The negative cache: a row with nothing in it means the question was
    // asked and the answer was no.
    if age_secs < NEGATIVE_HOLDS_FOR_SECS {
        Ok(Cached::Empty)
    } else {
        Ok(Cached::Worth)
    }
}

// --- what a provider found -----------------------------------------------

/// One result, in the shape the ranking cares about.
///
/// Providers keep their own wire types and convert into this, which is what
/// stopped mattering as an abstraction only while there was one of them. The
/// two here answer completely different protocols — a fielded REST search
/// against lrclib, a form POST then a second fetch against NetEase — so the
/// thing worth sharing is the *answer*, not the mechanism.
#[derive(Debug, Clone, Default)]
pub struct Match {
    /// Which provider said so, and what gets stored in `lyrics.provider`.
    pub provider: &'static str,
    /// That provider's own row id, so a shown candidate can be picked later.
    pub id: i64,
    pub artist: String,
    pub title: String,
    pub duration_secs: Option<f64>,
    /// A positive answer with no words in it.
    pub instrumental: bool,
    /// LRC source text.
    pub synced: Option<String>,
    pub plain: Option<String>,
    /// Timed romanisation, where the provider has one. lrclib never does.
    pub romaji: Option<String>,
}

impl Match {
    /// Whether this says anything at all. A row with no lyrics and no
    /// instrumental flag is a stub somebody created and never filled in.
    pub fn is_useful(&self) -> bool {
        self.instrumental || self.synced.is_some() || self.plain.is_some()
    }
}

// --- choosing between candidates -----------------------------------------

/// How much longer our copy may be than the release, in seconds.
///
/// Asymmetric on purpose. An upload *adds* to a recording -- an intro card, an
/// outro, and in two tracks measured here 14.6 and 5.6 seconds of trailing
/// silence -- and essentially never removes from it. A symmetric window wide
/// enough to admit those would also admit a completely different edit of the
/// song, which is the failure this is guarding against.
const MAX_PADDING_SECS: f64 = 25.0;

/// How much shorter our copy may be.
///
/// Tight, because there is no benign reason for it. A release that runs
/// materially longer than what we hold is a different cut: extended, live, or
/// somebody's full-album rip filed as one track.
const MAX_SHORTFALL_SECS: f64 = 5.0;

fn fold(text: &str) -> String {
    collapse_whitespace(text).to_lowercase()
}

/// Words that name a *different recording* of the same song.
///
/// The exact inverse of [`TITLE_NOISE`]: that list is content a title can lose
/// without changing which recording it names, this one is content that changes
/// it entirely. A remix, a live take and a cover share a name with the
/// original and share none of its timings.
const VERSION_MARKERS: &[&str] = &[
    "remix",
    "remixes",
    "live",
    "acoustic",
    "cover",
    "covered",
    "instrumental",
    "version",
    "ver",
    "edit",
    "extended",
    "remaster",
    "remastered",
    "demo",
    "karaoke",
    "nightcore",
    "slowed",
    "reverb",
    "sped",
    "8d",
    "mashup",
    "bootleg",
    "rework",
    "reimagined",
    "reimagining",
];

/// Splits a title into comparable words, dropping punctuation entirely.
///
/// So `Chop Suey!` and `Chop Suey` are the same title, which they are.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

/// A title with `artist` taken off the front and the upload noise removed,
/// using the artist **exactly as given**.
///
/// [`identify`] cleans the artist first, which is right when the name came
/// from an uploader and wrong when it came from a folder: `clean_artist`
/// strips "Music" as channel decoration, so a folder called `Epic Music`
/// becomes an artist called `Epic`. A directory somebody made is not a channel
/// handle, so `infer` needs the name left alone and the title cleaned anyway.
pub(crate) fn title_without(artist: &str, title: &str) -> String {
    clean_title(strip_artist_prefix(title, Some(artist)))
}

/// Whether a phrase says only *which version this is* and nothing else.
///
/// Exposed for `infer`, which splits `"Artist - Title"` filenames and must not
/// split `"Song - Remix"`: the dash there separates a song from its version,
/// not an artist from a song, and treating "Remix" as a title would leave the
/// row named after a word.
///
/// Shares this module's vocabulary on purpose. A second list would drift, and
/// the two uses want exactly the same words.
pub(crate) fn is_only_version_marker(text: &str) -> bool {
    let words = words(text);
    !words.is_empty() && words.iter().all(|w| VERSION_MARKERS.contains(&w.as_str()))
}

/// Whether two titles name the same recording.
///
/// Equal word-for-word, or one is the other plus *catalogue context* -- an
/// album name, a disc and track number, an "OST" prefix. This library is full
/// of the second shape, because its titles came from filenames:
///
/// ```text
/// ours   [Official] Celeste Original Soundtrack - 06 - Checking In
/// lrclib Checking In
/// ```
///
/// Refusing that pair costs a real match on a large part of this library.
/// Accepting *any* overlap would be far worse -- `Infinite` covers
/// `Infinite (KITSUN3POWR REMIX V3)`, and the original's timings are wrong for
/// every line of a remix. So the extra words decide: catalogue context is
/// allowed through, anything naming a different recording is not.
///
/// Deliberately conservative in one place it costs us: `(Album Version)` is
/// usually the same recording and is refused anyway, because "version" cannot
/// be told from "Zebrahead Ver" without knowing what the words mean.
fn title_covers(ours: &str, theirs: &str) -> bool {
    let ours = words(ours);
    let theirs = words(theirs);

    if ours.is_empty() || theirs.is_empty() {
        return false;
    }

    let (long, short) = if ours.len() >= theirs.len() {
        (&ours, &theirs)
    } else {
        (&theirs, &ours)
    };

    let Some(start) = long
        .windows(short.len())
        .position(|window| window == short.as_slice())
    else {
        return false;
    };

    long[..start]
        .iter()
        .chain(&long[start + short.len()..])
        .all(|word| !VERSION_MARKERS.contains(&word.as_str()))
}

/// Picks the right row out of what a provider sent back, or refuses.
///
/// Refusing matters as much as choosing. `q=Chop Suey` returns an August Burns
/// Red cover, a fifty-second stub and three System of a Down rows of 202, 224
/// and 227 seconds -- so taking `results[0]`, which is what a first
/// implementation always does, is a coin flip between the song and a cover of
/// it. Wrong lyrics scrolling in perfect sync are worse than none: they look
/// like the app is confident.
///
/// Two gates and then an order. A candidate must be about this song at all --
/// close enough in length, and named right when there is nothing else to go on
/// -- and only then is it ranked.
/// What the ranking concluded.
///
/// The middle variant is the point. A title-only search for a song whose
/// artist cannot be confirmed returns several rows that all fit: searching
/// "Vancouver" against a 223-second track offers `Jeff Buckley | Vancouver |
/// 220s` — exact title, three seconds out, and completely the wrong song.
/// Ranking picks one of those about as well as a coin does.
///
/// So when nothing distinguishes the candidates, this says so rather than
/// guessing, and the user is shown the list. Being asked once is a smaller
/// cost than a song's worth of confidently mistimed words, and it is the only
/// answer here that is actually true.
pub enum Choice<'a> {
    /// One candidate stands out: its artist matched, or it was the only row
    /// that fit at all.
    Confident(&'a Match),
    /// Several fit and nothing separates them. Ordered best-guess first.
    Ambiguous(Vec<&'a Match>),
    /// Nothing came close enough to be worth showing.
    Nothing,
}

/// Assertion helpers.
///
/// Test-only because production code matches all three arms — a caller that
/// only wanted to know whether there was an answer would be throwing away the
/// distinction this enum exists to carry.
#[cfg(test)]
impl<'a> Choice<'a> {
    /// The one answer, when there is one.
    pub fn confident(&self) -> Option<&'a Match> {
        match self {
            Choice::Confident(candidate) => Some(candidate),
            _ => None,
        }
    }

    /// Nothing fit. Distinct from being unable to choose *between* things
    /// that fit, which is [`Choice::Ambiguous`].
    pub fn is_nothing(&self) -> bool {
        matches!(self, Choice::Nothing)
    }
}

pub fn choose<'a>(
    candidates: &'a [Match],
    identity: &Identity,
    ours_secs: Option<f64>,
) -> Choice<'a> {
    let wanted_artist = identity.artist.as_deref().map(primary_artist);
    let wanted_title = fold(&identity.title);

    let mut ranked: Vec<(bool, bool, bool, bool, f64, &'a Match)> = candidates
        .iter()
        .filter_map(|candidate| {
            if !candidate.is_useful() {
                return None;
            }

            // Compared by primary artist, so a guest credit on one side and
            // not the other is not treated as a different musician.
            let artist_match = wanted_artist
                .as_deref()
                .is_some_and(|wanted| primary_artist(&candidate.artist) == wanted);
            let title_exact = words(&candidate.title) == words(&wanted_title);

            // A row whose title cannot even be reconciled with ours is not
            // this song, however well its length happens to line up. lrclib
            // matches loosely and `q=` matches very loosely.
            if !title_covers(&wanted_title, &candidate.title) {
                return None;
            }

            // With no artist, the title and the length are the *only* things
            // holding the match together, so both have to be there.
            if wanted_artist.is_none() && ours_secs.is_none() {
                return None;
            }

            let distance = match (ours_secs, candidate.duration_secs) {
                (Some(ours), Some(theirs)) => {
                    let delta = ours - theirs;
                    if !(-MAX_SHORTFALL_SECS..=MAX_PADDING_SECS).contains(&delta) {
                        return None;
                    }
                    delta.abs()
                }
                // No length to compare. Acceptable only when both names carry
                // the match on their own; sorted last so any candidate that
                // *could* be checked wins over one that could not.
                _ => {
                    if !(artist_match && title_exact) {
                        return None;
                    }
                    f64::MAX
                }
            };

            Some((
                artist_match,
                title_exact,
                candidate.synced.is_some(),
                candidate.romaji.is_some(),
                distance,
                candidate,
            ))
        })
        .collect();

    // Name first, then timing. Within the gate, a row with synced lyrics beats
    // a closer-matching row without them: lrclib's durations are whatever the
    // contributor's own file was, so two seconds of difference says much less
    // than the presence of real timestamps.
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.cmp(&a.1))
            .then(b.2.cmp(&a.2))
            // A romanised copy only ever exists because one was asked for --
            // NetEase is queried at all only when the lyrics are in a script
            // this reader cannot follow -- so preferring it here cannot
            // outrank anything for a song where it would not help.
            .then(b.3.cmp(&a.3))
            .then(a.4.total_cmp(&b.4))
    });

    let Some(best) = ranked.first() else {
        return Choice::Nothing;
    };

    // Two ways to be sure. The artist we asked for is the artist that came
    // back -- which after `clean_artist` covers every properly tagged track --
    // or the gate admitted exactly one row, so there is nothing to confuse it
    // with.
    if best.0 || ranked.len() == 1 {
        return Choice::Confident(best.5);
    }

    Choice::Ambiguous(ranked.iter().map(|entry| entry.5).collect())
}

// --- fetching ------------------------------------------------------------

/// One provider request at a time, app-wide.
///
/// Not a performance decision. lrclib is donated infrastructure, and a queue
/// change or a fast skip through ten tracks would otherwise fan out into ten
/// simultaneous requests from one desktop client.
static FETCH_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// The floor between two requests, on top of the gate above.
const MIN_GAP: std::time::Duration = std::time::Duration::from_millis(250);

static LAST_REQUEST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Holds off until [`MIN_GAP`] has passed since the last request went out.
async fn pace() {
    // The guard is dropped before the await: a `std` mutex held across one
    // would block every other task on this thread, and it is only ever held
    // for a comparison.
    let wait = {
        let last = LAST_REQUEST.lock().unwrap_or_else(|e| e.into_inner());
        last.and_then(|at| MIN_GAP.checked_sub(at.elapsed()))
    };
    if let Some(wait) = wait {
        tokio::time::sleep(wait).await;
    }
    *LAST_REQUEST.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
}

/// Writes what a provider said, including when it said nothing.
///
/// The empty row is the point. Without it every play of a track with no lyrics
/// is another request, and this library has hundreds of tracks that will never
/// match anything.
async fn store(pool: &SqlitePool, key: &str, chosen: Option<&Match>) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO lyrics \
             (identity_key, synced, plain, romaji, instrumental, provider, fetched_at) \
         VALUES (?, ?, ?, ?, ?, ?, unixepoch()) \
         ON CONFLICT (identity_key) DO UPDATE SET \
             synced = excluded.synced, \
             plain = excluded.plain, \
             romaji = excluded.romaji, \
             instrumental = excluded.instrumental, \
             provider = excluded.provider, \
             fetched_at = excluded.fetched_at",
    )
    .bind(key)
    .bind(chosen.and_then(|c| c.synced.as_deref()))
    .bind(chosen.and_then(|c| c.plain.as_deref()))
    .bind(chosen.and_then(|c| c.romaji.as_deref()))
    .bind(i64::from(chosen.is_some_and(|c| c.instrumental)))
    // A negative row still records who was asked, so "nobody had it" and
    // "nobody has been asked" stay distinguishable in the table itself.
    .bind(chosen.map_or(crate::lrclib::PROVIDER, |c| c.provider))
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Asks a provider, and remembers the answer either way.
///
/// Everything cheap has already been tried by the time this runs: the caller
/// is expected to have gone through [`resolve`] first, and this checks the
/// cache again after taking the gate so a burst of tracks that turn out to be
/// the same song makes one request between them.
/// One row the user can pick, described well enough to pick it by.
///
/// Carries `id` because a choice has to survive the round trip back: the list
/// is rendered, the user clicks, and [`pick`] fetches that exact row. It also
/// carries the two facts that make one row obviously better than another —
/// how far its length is from ours, and whether it is actually timed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub id: i64,
    /// Which provider it came from, so picking it later asks the right one.
    pub provider: &'static str,
    pub title: String,
    pub artist: String,
    pub duration_secs: Option<f64>,
    /// Seconds this is longer or shorter than the track. `None` when either
    /// length is unknown.
    pub delta_secs: Option<f64>,
    pub synced: bool,
    /// Whether a timed romanisation came with it.
    pub romaji: bool,
    pub instrumental: bool,
}

impl Candidate {
    fn of(found: &Match, ours_secs: Option<f64>) -> Self {
        Self {
            id: found.id,
            provider: found.provider,
            title: found.title.clone(),
            artist: found.artist.clone(),
            duration_secs: found.duration_secs,
            delta_secs: ours_secs.zip(found.duration_secs).map(|(a, b)| a - b),
            synced: found.synced.is_some(),
            romaji: found.romaji.is_some(),
            instrumental: found.instrumental,
        }
    }
}

/// What a lookup produced: lyrics, or a question.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Lookup {
    pub lyrics: Option<TrackLyrics>,
    /// Non-empty only when nothing could be chosen with confidence. Handed
    /// back with the answer so the panel can offer the list immediately,
    /// rather than saying "nothing found" and making the user ask again for
    /// results that were already in hand.
    pub candidates: Vec<Candidate>,
}

impl Lookup {
    fn found(lyrics: Option<TrackLyrics>) -> Self {
        Self {
            lyrics,
            candidates: Vec::new(),
        }
    }
}

pub async fn fetch(pool: &SqlitePool, track_id: i64) -> Result<Lookup, String> {
    let track = load(pool, track_id).await?;
    let identity = identify(track.artist.as_deref(), &track.title);
    let ours = track.duration_secs.map(|s| s as f64);

    if identity.title.trim().is_empty() {
        return Ok(Lookup::found(None));
    }
    // Nothing to ask with and nothing to check an answer against.
    if identity.artist.is_none() && ours.is_none() {
        return Ok(Lookup::found(None));
    }

    match cached(pool, &identity, track.offset_ms).await? {
        Cached::Hit(lyrics) => return Ok(Lookup::found(Some(lyrics))),
        Cached::Empty => return Ok(Lookup::found(None)),
        Cached::Worth => {}
    }

    let _permit = FETCH_GATE
        .acquire()
        .await
        .map_err(|_| "The lyrics fetcher has shut down.".to_string())?;

    // Someone else may have answered this exact question while we queued.
    match cached(pool, &identity, track.offset_ms).await? {
        Cached::Hit(lyrics) => return Ok(Lookup::found(Some(lyrics))),
        Cached::Empty => return Ok(Lookup::found(None)),
        Cached::Worth => {}
    }

    pace().await;

    // A failure to reach lrclib is not an answer. Returning early rather than
    // storing keeps a dropped connection from hiding a song for a fortnight.
    let candidates = ask_providers(&identity, ours).await?;

    match choose(&candidates, &identity, ours) {
        Choice::Confident(chosen) => {
            store(pool, &identity.key, Some(chosen)).await?;
            Ok(Lookup::found(
                cached(pool, &identity, track.offset_ms).await?.into_hit(),
            ))
        }
        Choice::Ambiguous(shortlist) => {
            // Still written as a negative, so replaying this track does not
            // ask lrclib the same unanswerable question every time. The list
            // travels back with this reply instead, and asking again is
            // something the user does on purpose.
            store(pool, &identity.key, None).await?;
            Ok(Lookup {
                lyrics: None,
                candidates: shortlist
                    .into_iter()
                    .map(|candidate| Candidate::of(candidate, ours))
                    .collect(),
            })
        }
        Choice::Nothing => {
            store(pool, &identity.key, None).await?;
            Ok(Lookup::found(None))
        }
    }
}

/// Asks the providers in order, stopping as soon as one settles it.
///
/// lrclib first: one request, CC0 data, and better coverage of everything sung
/// in English. NetEase second, and only when lrclib produced nothing usable —
/// it costs a search plus a fetch per candidate, and it is a private API that
/// can stop answering without notice.
///
/// "Nothing usable" is judged with [`choose`] rather than by counting rows,
/// because lrclib returning twenty songs that all fail the gate is the same
/// outcome as it returning none, and it is exactly the case NetEase is here
/// for: a Japanese track whose romanised lyrics lrclib has never held.
///
/// A provider that errors is skipped rather than fatal. Only *every* provider
/// failing is worth reporting, because that is a connection problem rather
/// than an answer, and it must not be cached as one.
async fn ask_providers(identity: &Identity, ours: Option<f64>) -> Result<Vec<Match>, String> {
    let mut failures = Vec::new();

    let lrclib = match crate::lrclib::find(identity).await {
        Ok(found) => found,
        Err(why) => {
            failures.push(why);
            Vec::new()
        }
    };

    // Whether a second provider can add anything.
    //
    // "lrclib found nothing" is the obvious trigger and on its own it is the
    // wrong one, which cost this feature most of its point: lrclib holds the
    // Japanese lyrics of a Japanese song perfectly well — it simply never
    // holds a *romanisation*. Stopping as soon as it answered meant the one
    // thing NetEase is here for was almost never fetched.
    //
    // So the question is not "did lrclib answer" but "is its answer one a
    // reader of this library can follow". Latin lyrics gain nothing from a
    // romanisation and cost a request to discover that, so they end here.
    let worth_asking = match choose(&lrclib, identity, ours) {
        Choice::Nothing => true,
        Choice::Confident(best) => wants_romanising(best),
        // Already unable to choose; more candidates would only be more to
        // choose between. The picker asks both providers anyway.
        Choice::Ambiguous(_) => false,
    };

    if !worth_asking {
        return Ok(lrclib);
    }

    pace().await;
    match crate::netease::find(identity).await {
        // Whatever lrclib offered is kept alongside: it failed the gate, but
        // it is still a real answer to show if the user ends up choosing by
        // hand.
        Ok(found) => Ok(lrclib.into_iter().chain(found).collect()),
        Err(why) => {
            failures.push(why);
            if failures.len() == 2 {
                return Err(failures.join("; "));
            }
            Ok(lrclib)
        }
    }
}

/// Whether a romanisation would help someone read these.
///
/// True for scripts a Latin-alphabet reader cannot sound out at all — kana,
/// Han, Hangul. Deliberately *not* "contains non-ASCII": `Ōrīōn`, `Amnéhilesie`
/// and `Manos Te Faltarán` are already readable, and romanising them would be
/// a request spent to change nothing.
fn wants_romanising(found: &Match) -> bool {
    /// Enough to be the language of the song rather than one borrowed word in
    /// an English lyric.
    const SHARE: f64 = 0.2;

    let text = found
        .synced
        .as_deref()
        .or(found.plain.as_deref())
        .unwrap_or_default();

    let mut letters = 0usize;
    let mut unreadable = 0usize;

    for character in text.chars() {
        if !character.is_alphabetic() {
            continue;
        }
        letters += 1;
        if is_unromanised(character) {
            unreadable += 1;
        }
    }

    letters > 0 && unreadable as f64 / letters as f64 >= SHARE
}

/// Kana, Han and Hangul: the scripts this is for.
fn is_unromanised(character: char) -> bool {
    matches!(character,
        '\u{3040}'..='\u{30FF}'      // hiragana and katakana
        | '\u{3400}'..='\u{4DBF}'    // CJK extension A
        | '\u{4E00}'..='\u{9FFF}'    // CJK unified
        | '\u{F900}'..='\u{FAFF}'    // CJK compatibility
        | '\u{AC00}'..='\u{D7AF}'    // hangul syllables
        | '\u{1100}'..='\u{11FF}'    // hangul jamo
    )
}

/// Everything lrclib has for a track, for the user to choose from.
///
/// Always goes to the network: this only runs because someone asked, and the
/// reason they asked is that what was cached was wrong or missing.
///
/// `query` replaces the track's own tags when given, which is the whole point
/// of a manual search — for 94% of this library the tags are the reason the
/// automatic lookup failed.
pub async fn search(
    pool: &SqlitePool,
    track_id: i64,
    query: Option<String>,
) -> Result<Vec<Candidate>, String> {
    let track = load(pool, track_id).await?;
    let ours = track.duration_secs.map(|s| s as f64);

    let _permit = FETCH_GATE
        .acquire()
        .await
        .map_err(|_| "The lyrics fetcher has shut down.".to_string())?;
    pace().await;

    // Both providers, always, and their results merged.
    //
    // Unlike the automatic path this does not stop at the first one that
    // answers: someone who opened this did so because the automatic answer was
    // wrong or missing, and the row they want may well be the one the ranking
    // just declined. Showing half the shelf would be the same mistake again.
    let identity = identify(track.artist.as_deref(), &track.title);
    let typed = query.as_deref().map(str::trim).filter(|q| !q.is_empty());

    let (from_lrclib, from_netease) = match typed {
        Some(query) => (
            crate::lrclib::search(query).await,
            crate::netease::search(query).await,
        ),
        None => (
            crate::lrclib::find(&identity).await,
            crate::netease::find(&identity).await,
        ),
    };

    let found: Vec<Match> = from_lrclib
        .into_iter()
        .flatten()
        .chain(from_netease.into_iter().flatten())
        .collect();

    // Unranked and ungated on purpose. This is the escape hatch from the
    // ranking being wrong, so filtering it by the same rules would defeat it;
    // `delta_secs` lets the user apply the judgement themselves.
    Ok(found
        .iter()
        .map(|candidate| Candidate::of(candidate, ours))
        .collect())
}

/// Takes the user's word for it.
///
/// Stored under the song's identity key like any other answer, so it is found
/// again on every row of the same song and is never re-asked: `fetch` only
/// goes to the network on `Cached::Worth`, and this leaves a hit.
pub async fn pick(
    pool: &SqlitePool,
    track_id: i64,
    lyrics_id: i64,
    provider: &str,
) -> Result<Option<TrackLyrics>, String> {
    let track = load(pool, track_id).await?;
    let identity = identify(track.artist.as_deref(), &track.title);

    let chosen = {
        let _permit = FETCH_GATE
            .acquire()
            .await
            .map_err(|_| "The lyrics fetcher has shut down.".to_string())?;
        pace().await;
        // Ids are only meaningful to the provider that issued them, which is
        // why a candidate carries the name of who said it.
        match provider {
            crate::netease::PROVIDER => crate::netease::get(lyrics_id).await?,
            _ => crate::lrclib::get(lyrics_id).await?,
        }
    };

    store(pool, &identity.key, Some(&chosen)).await?;

    Ok(cached(pool, &identity, track.offset_ms).await?.into_hit())
}

// --- commands ------------------------------------------------------------

/// What can be shown right now, without asking anyone.
///
/// Deliberately separate from [`fetch_track_lyrics`]. This one reads a file
/// and a table and returns in milliseconds, so the panel can paint immediately
/// instead of opening onto a spinner that is usually unnecessary.
#[tauri::command]
pub async fn track_lyrics(db: State<'_, Db>, track_id: i64) -> Result<Option<TrackLyrics>, String> {
    resolve(&db.pool, track_id).await
}

/// The slow half: ask a provider.
///
/// Called only when [`track_lyrics`] came back empty, which is what lets the
/// UI show "searching" for exactly as long as something is actually being
/// searched for.
#[tauri::command]
pub async fn fetch_track_lyrics(db: State<'_, Db>, track_id: i64) -> Result<Lookup, String> {
    fetch(&db.pool, track_id).await
}

/// Ask again, on purpose, and show everything.
#[tauri::command]
pub async fn search_lyrics(
    db: State<'_, Db>,
    track_id: i64,
    query: Option<String>,
) -> Result<Vec<Candidate>, String> {
    search(&db.pool, track_id, query).await
}

/// The user picked one.
#[tauri::command]
pub async fn pick_lyrics(
    db: State<'_, Db>,
    track_id: i64,
    lyrics_id: i64,
    provider: String,
) -> Result<Option<TrackLyrics>, String> {
    pick(&db.pool, track_id, lyrics_id, &provider).await
}

/// Shifts this track's lyrics against its audio.
///
/// Per track and not per song: a YouTube upload with a three-second intro card
/// needs the shift and the release does not, and both point at one `lyrics`
/// row.
#[tauri::command]
pub async fn set_lyrics_offset(
    db: State<'_, Db>,
    track_id: i64,
    offset_ms: i64,
) -> Result<i64, String> {
    let offset_ms = offset_ms.clamp(-MAX_OFFSET_MS, MAX_OFFSET_MS);

    sqlx::query("UPDATE tracks SET lyrics_offset_ms = ? WHERE id = ?")
        .bind(offset_ms)
        .bind(track_id)
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(offset_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(lines: &[LyricLine], index: usize) -> (i64, &str) {
        let line = &lines[index];
        (line.at_ms.expect("synced line"), line.text.as_str())
    }

    // --- timestamps ------------------------------------------------------

    #[test]
    fn centiseconds_are_the_default_fraction() {
        let lines = parse_lrc("[00:00.63]one\n[01:05.90]two").unwrap();
        assert_eq!(at(&lines, 0), (630, "one"));
        assert_eq!(at(&lines, 1), (65_900, "two"));
    }

    #[test]
    fn a_three_digit_fraction_is_milliseconds() {
        let lines = parse_lrc("[00:00.630]one\n[00:01.005]two").unwrap();
        assert_eq!(at(&lines, 0).0, 630);
        assert_eq!(at(&lines, 1).0, 1_005);
    }

    #[test]
    fn a_one_digit_fraction_is_tenths() {
        let lines = parse_lrc("[00:00.6]one\n[00:01.2]two").unwrap();
        assert_eq!(at(&lines, 0).0, 600);
        assert_eq!(at(&lines, 1).0, 1_200);
    }

    #[test]
    fn a_timestamp_may_carry_no_fraction_at_all() {
        let lines = parse_lrc("[00:12]one\n[01:30]two").unwrap();
        assert_eq!(at(&lines, 0).0, 12_000);
        assert_eq!(at(&lines, 1).0, 90_000);
    }

    /// The two three-field forms, told apart by the fractional point.
    ///
    /// Both are real and they disagree by a factor of sixty. `[01:02:03.50]`
    /// has an explicit fraction, so its leading field can only be hours;
    /// `[01:02:03]` has none, so the trailing field is centiseconds and the
    /// leading one is minutes. Asserting both in one test is the point --
    /// either rule alone is easy to satisfy and wrong.
    #[test]
    fn the_three_field_forms_are_told_apart_by_their_fraction() {
        let hours = parse_lrc("[01:02:03.50]one\n[01:02:04.00]two").unwrap();
        assert_eq!(at(&hours, 0).0, 3_723_500);

        let legacy = parse_lrc("[01:02:03]one\n[01:02:04]two").unwrap();
        assert_eq!(at(&legacy, 0).0, 62_030);
    }

    // --- the Veluna bug --------------------------------------------------

    /// Header tags are not lyrics.
    ///
    /// Veluna's parser splits a bracket group on its first colon and reads the
    /// left half as minutes with `.unwrap_or(0.0)`, so `[ar:System Of A Down]`
    /// becomes a line at time zero reading the band name -- the artist header
    /// displayed as the opening lyric. It survives there only because lrclib
    /// sends payloads with no header tags; a `.lrc` off disk has them.
    #[test]
    fn header_tags_never_become_lyrics() {
        let text = "[ar:System Of A Down]\n\
                    [ti:Chop Suey]\n\
                    [al:Toxicity]\n\
                    [by:someone]\n\
                    [length:03:30]\n\
                    [00:12.00]Wake up\n\
                    [00:15.00]Grab a brush";

        let lines = parse_lrc(text).unwrap();

        assert_eq!(lines.len(), 2, "only the two timed lines are lyrics");
        assert_eq!(at(&lines, 0), (12_000, "Wake up"));
        assert_eq!(at(&lines, 1), (15_000, "Grab a brush"));
        assert!(
            !lines.iter().any(|l| l.text.contains("System Of A Down")),
            "the artist header was rendered as a lyric"
        );
    }

    /// `[length:03:30]` is a header whose value looks exactly like a
    /// timestamp. Split naively it becomes a line at 3m30s reading nothing.
    #[test]
    fn a_header_whose_value_looks_like_a_timestamp_is_still_a_header() {
        let lines = parse_lrc("[length:03:30]\n[00:01.00]a\n[00:02.00]b").unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.at_ms.unwrap() < 10_000));
    }

    /// An unrecognised group is text, not a tag to be swallowed.
    #[test]
    fn an_unknown_bracket_group_stays_in_the_text() {
        let lines = parse_lrc("[00:12.00][Chorus] la la\n[00:20.00]next").unwrap();
        assert_eq!(at(&lines, 0), (12_000, "[Chorus] la la"));
    }

    // --- structure -------------------------------------------------------

    /// A chorus written once with several timestamps appears at each of them,
    /// and the result comes back in playing order rather than file order.
    #[test]
    fn a_repeated_chorus_lands_at_every_timestamp() {
        let text = "[00:41.20][02:15.60]chorus\n[01:00.00]verse";
        let lines = parse_lrc(text).unwrap();

        assert_eq!(lines.len(), 3);
        assert_eq!(at(&lines, 0), (41_200, "chorus"));
        assert_eq!(at(&lines, 1), (60_000, "verse"));
        assert_eq!(at(&lines, 2), (135_600, "chorus"));
    }

    /// The `[offset:]` header, which is the format's own answer to lyrics that
    /// run against the audio. Positive pulls them earlier.
    #[test]
    fn the_offset_header_shifts_every_line() {
        let lines = parse_lrc("[offset:+250]\n[00:10.00]a\n[00:20.00]b").unwrap();
        assert_eq!(at(&lines, 0).0, 9_750);
        assert_eq!(at(&lines, 1).0, 19_750);

        let back = parse_lrc("[offset:-250]\n[00:10.00]a\n[00:20.00]b").unwrap();
        assert_eq!(at(&back, 0).0, 10_250);
    }

    /// A shift large enough to push a line before the start clamps at zero
    /// rather than going negative and sorting ahead of everything.
    #[test]
    fn an_offset_cannot_push_a_line_before_the_track_starts() {
        let lines = parse_lrc("[offset:+5000]\n[00:01.00]a\n[00:20.00]b").unwrap();
        assert_eq!(at(&lines, 0).0, 0);
    }

    /// A timestamp with no words is an instrumental gap and is kept, so the
    /// UI can draw something during it instead of holding the previous line.
    #[test]
    fn a_timestamp_with_no_words_survives() {
        let lines = parse_lrc("[00:10.00]a\n[00:45.00]\n[01:10.00]b").unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(at(&lines, 1), (45_000, ""));
    }

    #[test]
    fn a_utf8_bom_does_not_hide_the_first_line() {
        let lines = parse_lrc("\u{feff}[00:01.00]first\n[00:02.00]second").unwrap();
        assert_eq!(at(&lines, 0), (1_000, "first"));
    }

    // --- refusing to guess -----------------------------------------------

    #[test]
    fn plain_text_is_not_mistaken_for_lrc() {
        assert!(parse_lrc("Wake up\nGrab a brush\nHide the scars").is_none());
    }

    /// A plain lyric sheet with one stray bracketed timestamp is still a plain
    /// lyric sheet. Without the untimed count, two accidental timestamps would
    /// be enough to scroll forty lines of prose in fake sync.
    #[test]
    fn a_sheet_of_prose_with_a_couple_of_timestamps_is_not_synced() {
        let mut text = String::from("[00:01.00]a\n[00:02.00]b\n");
        for n in 0..20 {
            text.push_str(&format!("line {n}\n"));
        }
        assert!(parse_lrc(&text).is_none());
    }

    #[test]
    fn one_timestamp_is_not_enough_to_call_it_synced() {
        assert!(parse_lrc("[00:01.00]only one").is_none());
    }

    /// Shape pinned against a real lrclib payload, fetched from
    /// `/api/get?artist_name=Set It Off&track_name=Duality`.
    #[test]
    fn a_real_lrclib_payload_parses() {
        let text = "[00:00.63] I cannot quite contain or explain my evil ways\n\
                    [00:05.90] Or explain why I am not sane\n\
                    [00:09.03] All I can say is this is your warning\n\
                    [00:14.20] I cannot quite contain or explain my evil ways";

        let lines = parse_lrc(text).unwrap();
        assert_eq!(lines.len(), 4);
        assert_eq!(
            at(&lines, 0),
            (630, "I cannot quite contain or explain my evil ways")
        );
        assert_eq!(at(&lines, 3).0, 14_200);
    }

    // --- plain text ------------------------------------------------------

    #[test]
    fn header_tags_are_dropped_from_plain_text_too() {
        let plain = parse_plain("[ar:Someone]\n[ti:Something]\nWake up\nGrab a brush");
        assert_eq!(plain, vec!["Wake up", "Grab a brush"]);
    }

    #[test]
    fn stanza_breaks_survive_but_runs_of_them_collapse() {
        let plain = parse_plain("\n\nfirst\n\n\n\nsecond\n\n");
        assert_eq!(plain, vec!["first", "", "second"]);
    }

    // --- identity --------------------------------------------------------

    #[test]
    fn the_topic_suffix_is_stripped_from_an_artist() {
        let id = identify(Some("Hideki Taniuchi - Topic"), "Kodoku");
        assert_eq!(id.artist.as_deref(), Some("Hideki Taniuchi"));
    }

    /// The bug this was written for.
    ///
    /// "Vancouver" is in the library twice — once from SoundCloud as
    /// `ivycomb`, once from YouTube as `Ivycomb Music`. lrclib has the song
    /// and matches `artist_name` as a prefix, so:
    ///
    /// ```text
    /// artist_name=ivycomb        -> 3 candidates, one exact
    /// artist_name=Ivycomb Music  -> 0
    /// ```
    ///
    /// One word of channel decoration was the entire difference between
    /// finding the lyrics and finding nothing, and the two rows are the same
    /// song — so getting this right also collapses them onto one cache entry.
    #[test]
    fn a_channel_name_is_reduced_to_the_artists() {
        assert_eq!(
            identify(Some("Ivycomb Music"), "Vancouver").artist.as_deref(),
            Some("Ivycomb")
        );

        // And the two rows now agree about which song this is.
        assert_eq!(
            identify(Some("Ivycomb Music"), "Vancouver").key,
            identify(Some("ivycomb"), "Vancouver").key
        );

        for (channel, artist) in [
            ("Monstercat Records", "Monstercat"),
            ("Somebody Official", "Somebody"),
            ("Somebody Official Music", "Somebody"),
            ("Somebody VEVO", "Somebody"),
            ("Somebody - Topic", "Somebody"),
            ("Rock TV", "Rock"),
            ("Somebody Official Channel", "Somebody"),
        ] {
            assert_eq!(
                identify(Some(channel), "x").artist.as_deref(),
                Some(artist),
                "{channel}"
            );
        }
    }

    /// The stripping has to stop at word boundaries, or it eats names.
    ///
    /// "MTV" ends in "tv" and is not a channel called "M". A name that is
    /// *only* decoration keeps itself, because an artist of "" matches every
    /// song ever recorded.
    #[test]
    fn a_name_is_never_mistaken_for_decoration() {
        for name in ["MTV", "Music", "Official", "VEVO", "Records"] {
            assert_eq!(
                identify(Some(name), "x").artist.as_deref(),
                Some(name),
                "{name} was treated as decoration"
            );
        }
    }

    #[test]
    fn upload_noise_is_stripped_from_a_title() {
        assert_eq!(identify(None, "17_10 (OFFICIAL MUSIC VIDEO)").title, "17_10");
        assert_eq!(identify(None, "[Official] Checking In").title, "Checking In");
        assert_eq!(
            identify(None, "Shore in the Rain\u{3010}BGA\u{3011}").title,
            "Shore in the Rain"
        );
        assert_eq!(identify(None, "Duality (Lyrics)").title, "Duality");
    }

    /// An artist's own name is not part of their song's title.
    ///
    /// `"ALESTI - Unravel"` is what a filename becomes, and lrclib answers a
    /// query for a track called that with nothing: the artist is already its
    /// own field, and repeating it inside the title only makes the title
    /// wrong.
    #[test]
    fn an_artists_own_name_is_stripped_from_the_front_of_a_title() {
        for (artist, title) in [
            ("ALESTI", "ALESTI - Unravel"),
            ("ALESTI", "alesti — Unravel"),
            ("ALESTI", "ALESTI | Unravel"),
            ("ALESTI", "ALESTI: Unravel"),
        ] {
            assert_eq!(identify(Some(artist), title).title, "Unravel", "{title}");
        }
    }

    /// Only ever the artist's *own* name, and only with a separator after it.
    #[test]
    fn a_title_that_merely_starts_with_a_word_keeps_it() {
        // A different artist entirely: nothing to strip.
        assert_eq!(
            identify(Some("Somebody"), "ALESTI - Unravel").title,
            "ALESTI - Unravel"
        );
        // The name, but as the first word of the title rather than a prefix.
        assert_eq!(
            identify(Some("Vancouver"), "Vancouver Nights").title,
            "Vancouver Nights"
        );
        // Nothing left over is not an improvement, so the prefix strip
        // declines; the dangling separator is then tidied by the ordinary
        // title cleaning, which is why this is "ALESTI" and not "".
        assert_eq!(identify(Some("ALESTI"), "ALESTI -").title, "ALESTI");
    }

    /// A guest credit is artist information that got typed into the title.
    ///
    /// lrclib files this song as `ALESTI feat. Siamese` / `Unravel`. Asking it
    /// for a track called `Unravel (feat. Siamese)` returns zero.
    #[test]
    fn a_guest_credit_is_not_part_of_the_title() {
        for title in [
            "Unravel (feat. Siamese)",
            "Unravel (ft. Siamese)",
            "Unravel [featuring Siamese]",
        ] {
            assert_eq!(identify(None, title).title, "Unravel", "{title}");
        }

        // Both at once, which is how it is actually stored.
        assert_eq!(
            identify(Some("ALESTI"), "ALESTI - Unravel (feat. Siamese)").title,
            "Unravel"
        );
    }

    /// The dangerous half. A version marker names a *different recording*, so
    /// stripping it would file a remix under the original's lyrics -- and the
    /// original's timings would be wrong for every line of it.
    #[test]
    fn a_version_marker_is_never_stripped() {
        for title in [
            "Infinite (KITSUN3POWR REMIX V3)",
            "His World (Zebrahead Ver)",
            "Duality (Live)",
            "Duality (Acoustic)",
            "Chop Suey (Cover)",
            "Something (Instrumental)",
        ] {
            let cleaned = identify(None, title).title;
            assert_eq!(cleaned, title, "{title} lost the marker that identifies it");
        }
    }

    /// The reason the cache is keyed on identity at all: a local file and a
    /// YouTube upload of one song are two rows and must share one entry.
    #[test]
    fn the_same_song_from_two_sources_shares_a_key() {
        let local = identify(Some("Set It Off"), "Duality");
        let remote = identify(Some("Set It Off - Topic"), "Duality (Official Music Video)");

        assert_eq!(local.key, remote.key);
    }

    #[test]
    fn different_songs_do_not_share_a_key() {
        let original = identify(Some("Set It Off"), "Duality");
        let remix = identify(Some("Set It Off"), "Duality (KITSUN3POWR REMIX V3)");
        let other = identify(Some("Slipknot"), "Duality");

        assert_ne!(original.key, remix.key);
        assert_ne!(original.key, other.key);
    }

    /// A title made entirely of noise would otherwise clean down to nothing,
    /// and an empty title matches every song in the database.
    #[test]
    fn a_title_that_is_only_noise_keeps_itself() {
        assert_eq!(
            identify(None, "(Official Music Video)").title,
            "(Official Music Video)"
        );
    }

    // --- decoding --------------------------------------------------------

    #[test]
    fn a_utf16_sidecar_with_a_bom_decodes() {
        let mut le = vec![0xFF, 0xFE];
        for unit in "[00:01.00]hi".encode_utf16() {
            le.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode(&le).as_deref(), Some("[00:01.00]hi"));

        let mut be = vec![0xFE, 0xFF];
        for unit in "[00:01.00]hi".encode_utf16() {
            be.extend_from_slice(&unit.to_be_bytes());
        }
        assert_eq!(decode(&be).as_deref(), Some("[00:01.00]hi"));
    }

    /// Refused rather than decoded lossily. Mojibake shown as lyrics reads as
    /// a bug in the app rather than as an unsupported encoding.
    #[test]
    fn a_non_utf8_sidecar_is_refused_rather_than_mangled() {
        // Shift-JIS for a Japanese syllable -- valid text, invalid UTF-8.
        assert_eq!(decode(&[0x82, 0xA0, 0x82, 0xA2]), None);
    }

    // --- on disk and in the database -------------------------------------

    /// A minimal but valid PCM WAV, so lofty has a real file to tag.
    fn write_wav(path: &Path) {
        const SAMPLES: u32 = 4410;
        let data_len = SAMPLES * 2;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&44100u32.to_le_bytes());
        bytes.extend_from_slice(&88200u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend_from_slice(&vec![0u8; data_len as usize]);

        std::fs::write(path, bytes).expect("should write test wav");
    }

    fn write_embedded(path: &Path, text: &str) {
        use lofty::config::WriteOptions;
        use lofty::tag::{ItemKey, ItemValue, Tag, TagExt, TagItem, TagType};

        let mut tag = Tag::new(TagType::Id3v2);
        tag.insert(TagItem::new(
            ItemKey::UnsyncLyrics,
            ItemValue::Text(text.to_string()),
        ));
        tag.save_to_path(path, WriteOptions::default())
            .expect("should write an id3v2 lyrics frame");
    }

    async fn fixture(name: &str) -> (crate::db::Db, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("music-app-lyrics-{name}"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let db = crate::db::init(&base.join("data")).await.unwrap();
        (db, base)
    }

    async fn insert_local(pool: &SqlitePool, path: &Path, title: &str) -> i64 {
        sqlx::query(
            "INSERT INTO tracks (source, title, artist, local_path, state, in_library) \
             VALUES ('local', ?, 'Set It Off', ?, 'present', 1)",
        )
        .bind(title)
        .bind(path.to_str().unwrap())
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn insert_remote(pool: &SqlitePool, title: &str, artist: &str) -> i64 {
        sqlx::query(
            "INSERT INTO tracks (source, title, artist, remote_id, remote_url, state, in_library) \
             VALUES ('youtube', ?, ?, 'vid1', 'https://example.invalid/x', 'saved', 1)",
        )
        .bind(title)
        .bind(artist)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    const LRC: &str = "[00:10.00]first\n[00:20.00]second";

    #[tokio::test]
    async fn an_lrc_beside_the_file_is_found() {
        let (db, base) = fixture("sidecar").await;
        let audio = base.join("song.wav");
        write_wav(&audio);
        std::fs::write(base.join("song.lrc"), LRC).unwrap();

        let id = insert_local(&db.pool, &audio, "Duality").await;
        let found = resolve(&db.pool, id).await.unwrap().expect("lyrics");

        assert_eq!(found.kind, Kind::Synced);
        assert_eq!(found.origin, "sidecar");
        assert_eq!(found.lines.len(), 2);

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// "Unsync" names the ID3v2 frame, not the contents.
    ///
    /// Taggers write LRC text straight into `USLT` because ID3v2's real synced
    /// frame is binary and awkward, so embedded lyrics are frequently
    /// timestamped. Treating the frame name as the answer would render a
    /// perfectly good synced lyric as a static block.
    ///
    /// Also the only test that proves `ItemKey::UnsyncLyrics` is the right key
    /// at all: ID3v2 has no `ItemKey::Lyrics`, so reading that alone would
    /// find nothing in any MP3.
    #[tokio::test]
    async fn lrc_text_in_an_unsync_tag_is_read_as_synced() {
        let (db, base) = fixture("embedded").await;
        let audio = base.join("song.wav");
        write_wav(&audio);
        write_embedded(&audio, LRC);

        let id = insert_local(&db.pool, &audio, "Duality").await;
        let found = resolve(&db.pool, id).await.unwrap().expect("lyrics");

        assert_eq!(found.kind, Kind::Synced);
        assert_eq!(found.origin, "embedded");
        assert_eq!(found.lines[0].at_ms, Some(10_000));

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Someone who dropped a `.lrc` next to a track did it deliberately, and
    /// more recently than whoever wrote the tag.
    #[tokio::test]
    async fn a_sidecar_wins_over_the_files_own_tags() {
        let (db, base) = fixture("precedence").await;
        let audio = base.join("song.wav");
        write_wav(&audio);
        write_embedded(&audio, "[00:10.00]from the tag\n[00:20.00]also the tag");
        std::fs::write(
            base.join("song.lrc"),
            "[00:10.00]from the sidecar\n[00:20.00]also the sidecar",
        )
        .unwrap();

        let id = insert_local(&db.pool, &audio, "Duality").await;
        let found = resolve(&db.pool, id).await.unwrap().expect("lyrics");

        assert_eq!(found.origin, "sidecar");
        assert_eq!(found.lines[0].text, "from the sidecar");

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn an_oversized_sidecar_is_ignored() {
        let (db, base) = fixture("oversized").await;
        let audio = base.join("song.wav");
        write_wav(&audio);

        let mut huge = String::new();
        while huge.len() as u64 <= MAX_SIDECAR_BYTES {
            huge.push_str("[00:10.00]padding padding padding padding padding\n");
        }
        std::fs::write(base.join("song.lrc"), &huge).unwrap();

        let id = insert_local(&db.pool, &audio, "Duality").await;
        assert_eq!(resolve(&db.pool, id).await.unwrap(), None);

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A stream has no file to read, so it falls straight through to whatever
    /// a provider found for this *song* -- which is the point of keying the
    /// cache on identity rather than on the row.
    #[tokio::test]
    async fn a_stream_reads_the_cache_written_for_the_same_song() {
        let (db, base) = fixture("cache").await;

        // Written as if a provider had answered for the local copy.
        let identity = identify(Some("Set It Off"), "Duality");
        sqlx::query(
            "INSERT INTO lyrics (identity_key, synced, provider) VALUES (?, ?, 'lrclib')",
        )
        .bind(&identity.key)
        .bind(LRC)
        .execute(&db.pool)
        .await
        .unwrap();

        // A different row, a different source, a messier title, the same song.
        let id = insert_remote(
            &db.pool,
            "Duality (OFFICIAL MUSIC VIDEO)",
            "Set It Off - Topic",
        )
        .await;

        let found = resolve(&db.pool, id).await.unwrap().expect("lyrics");
        assert_eq!(found.kind, Kind::Synced);
        assert_eq!(found.origin, "lrclib");

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The state a library of game soundtracks lives in.
    ///
    /// lrclib answers `instrumental: true` for the Celeste and Hideki Taniuchi
    /// tracks in this library. That is a correct, confident answer and has to
    /// survive as one -- folded into "nothing found" it reads as the feature
    /// being broken on half the library.
    #[tokio::test]
    async fn an_instrumental_row_is_a_positive_answer() {
        let (db, base) = fixture("instrumental").await;

        let identity = identify(Some("Lena Raine"), "Checking In");
        sqlx::query(
            "INSERT INTO lyrics (identity_key, instrumental, provider) VALUES (?, 1, 'lrclib')",
        )
        .bind(&identity.key)
        .execute(&db.pool)
        .await
        .unwrap();

        let id = insert_remote(&db.pool, "Checking In", "Lena Raine").await;
        let found = resolve(&db.pool, id).await.unwrap().expect("an answer");

        assert_eq!(found.kind, Kind::Instrumental);
        assert!(found.lines.is_empty());

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The negative cache. A row with nothing in it means the question was
    /// asked and the answer was no; the reader cannot tell it from never
    /// having asked, and does not need to.
    #[tokio::test]
    async fn an_empty_cache_row_reads_as_nothing_found() {
        let (db, base) = fixture("negative").await;

        let identity = identify(Some("Nobody"), "Untitled");
        sqlx::query("INSERT INTO lyrics (identity_key, provider) VALUES (?, 'lrclib')")
            .bind(&identity.key)
            .execute(&db.pool)
            .await
            .unwrap();

        let id = insert_remote(&db.pool, "Untitled", "Nobody").await;
        assert_eq!(resolve(&db.pool, id).await.unwrap(), None);

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The per-track shift reaches the reader, and cannot be set to a value
    /// that would push every line off the end of the song.
    #[tokio::test]
    async fn the_offset_is_clamped_and_travels_with_the_lyrics() {
        let (db, base) = fixture("offset").await;
        let audio = base.join("song.wav");
        write_wav(&audio);
        std::fs::write(base.join("song.lrc"), LRC).unwrap();
        let id = insert_local(&db.pool, &audio, "Duality").await;

        sqlx::query("UPDATE tracks SET lyrics_offset_ms = 900 WHERE id = ?")
            .bind(id)
            .execute(&db.pool)
            .await
            .unwrap();

        let found = resolve(&db.pool, id).await.unwrap().expect("lyrics");
        assert_eq!(found.offset_ms, 900);

        // The line times themselves are untouched: the shift is the reader's
        // to apply, so changing it never means reparsing.
        assert_eq!(found.lines[0].at_ms, Some(10_000));

        let stored: i64 = sqlx::query_scalar("SELECT lyrics_offset_ms FROM tracks WHERE id = ?")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(stored, 900);

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_absurd_offset_is_refused() {
        assert_eq!(999_999i64.clamp(-MAX_OFFSET_MS, MAX_OFFSET_MS), MAX_OFFSET_MS);
        assert_eq!(
            (-999_999i64).clamp(-MAX_OFFSET_MS, MAX_OFFSET_MS),
            -MAX_OFFSET_MS
        );
    }

    // --- choosing between candidates -------------------------------------

    fn candidate(artist: &str, title: &str, duration: f64) -> Match {
        Match {
            provider: "lrclib",
            artist: artist.to_string(),
            title: title.to_string(),
            duration_secs: Some(duration),
            plain: Some("words".to_string()),
            ..Default::default()
        }
    }

    fn synced_candidate(artist: &str, title: &str, duration: f64) -> Match {
        Match {
            synced: Some(LRC.to_string()),
            plain: None,
            ..candidate(artist, title, duration)
        }
    }

    /// The shape of a real `q=Chop Suey` response, in the order lrclib
    /// returned it: a cover first, a junk stub second, the song third.
    ///
    /// Taking `results[0]` -- which is what every first implementation does --
    /// picks the August Burns Red cover. The lyrics would scroll in perfect
    /// sync and be the wrong song from the first line.
    #[test]
    fn the_first_result_is_not_automatically_the_answer() {
        let candidates = [
            synced_candidate("August Burns Red", "Chop Suey!", 227.0),
            synced_candidate("System Of A Down", "Chop Suey!", 50.0),
            synced_candidate("System of a Down", "Chop Suey!", 224.0),
        ];

        let identity = identify(Some("System Of A Down"), "Chop Suey!");
        let chosen = choose(&candidates, &identity, Some(224.0)).confident().expect("a confident match");

        assert_eq!(chosen.artist, "System of a Down");
        assert_eq!(chosen.duration_secs, Some(224.0));
    }

    /// The fifty-second row in that same response is real data in lrclib and
    /// is not the song.
    #[test]
    fn a_stub_of_the_wrong_length_is_rejected() {
        let candidates = [synced_candidate("System Of A Down", "Chop Suey!", 50.0)];
        let identity = identify(Some("System Of A Down"), "Chop Suey!");

        assert!(choose(&candidates, &identity, Some(224.0)).is_nothing());
    }

    /// The asymmetry, and the reason the window is not symmetric.
    ///
    /// Two tracks in this library were measured carrying 14.6 and 5.6 seconds
    /// of trailing silence, so their stored duration runs that much longer
    /// than the release the lyrics were timed against. A symmetric window
    /// tight enough to be useful would reject exactly those.
    #[test]
    fn trailing_silence_does_not_reject_the_right_lyrics() {
        let candidates = [synced_candidate("Set It Off", "Duality", 224.0)];
        let identity = identify(Some("Set It Off"), "Duality");

        let padded = choose(&candidates, &identity, Some(224.0 + 14.6));
        assert!(
            padded.confident().is_some(),
            "14.6s of trailing silence rejected a match"
        );

        let very_padded = choose(&candidates, &identity, Some(224.0 + 30.0));
        assert!(
            very_padded.is_nothing(),
            "half a minute over is a different thing"
        );
    }

    /// The other direction is not benign: a release that runs well past our
    /// copy is an extended cut, a live take, or an album rip filed as a track.
    #[test]
    fn a_release_much_longer_than_our_copy_is_rejected() {
        let candidates = [synced_candidate("Set It Off", "Duality", 244.0)];
        let identity = identify(Some("Set It Off"), "Duality");

        assert!(choose(&candidates, &identity, Some(224.0)).is_nothing());
    }

    /// Catalogue context is allowed through; a different recording is not.
    ///
    /// This library's titles came from filenames, so the real shape of a match
    /// is lrclib holding `Checking In` while we hold
    /// `Celeste Original Soundtrack - 06 - Checking In`. Refusing that pair
    /// costs a large share of the library. Accepting *any* overlap would be
    /// far worse: `Infinite` covers `Infinite (KITSUN3POWR REMIX V3)`, whose
    /// timings are wrong for every line.
    #[test]
    fn catalogue_context_is_allowed_but_a_different_recording_is_not() {
        let identity = identify(None, "Celeste Original Soundtrack - 06 - Checking In");
        let candidates = [synced_candidate("Lena Raine", "Checking In", 425.0)];
        assert!(
            choose(&candidates, &identity, Some(425.0)).confident().is_some(),
            "an OST prefix should not cost the match"
        );

        let remix = identify(None, "Infinite (KITSUN3POWR REMIX V3)");
        let original = [synced_candidate("Whoever", "Infinite", 224.0)];
        assert!(
            choose(&original, &remix, Some(224.0)).is_nothing(),
            "a remix took the original's lyrics"
        );
    }

    /// Punctuation is not part of a title's identity.
    #[test]
    fn punctuation_does_not_break_a_title_match() {
        let identity = identify(None, "Chop Suey!");
        let candidates = [synced_candidate("System of a Down", "Chop Suey", 224.0)];
        assert!(choose(&candidates, &identity, Some(224.0)).confident().is_some());
    }

    /// A title with nothing in common is refused however well its length fits.
    #[test]
    fn an_unrelated_title_is_refused_even_at_the_right_length() {
        let identity = identify(None, "Duality");
        let candidates = [synced_candidate("Whoever", "Something Else", 224.0)];

        assert!(choose(&candidates, &identity, Some(224.0)).is_nothing());
    }

    /// Every version marker, against the titles they actually appear in here.
    #[test]
    fn no_version_marker_ever_covers_a_plain_title() {
        for (ours, theirs) in [
            ("His World (Zebrahead Ver)", "His World"),
            ("Duality (Live)", "Duality"),
            ("Duality (Acoustic)", "Duality"),
            ("Chop Suey (Cover)", "Chop Suey"),
            ("Something (Instrumental)", "Something"),
            ("Duality (Radio Edit)", "Duality"),
            ("Duality (Nightcore)", "Duality"),
            ("Duality (Slowed + Reverb)", "Duality"),
        ] {
            assert!(
                !title_covers(ours, theirs),
                "{ours:?} was treated as {theirs:?}"
            );
        }
    }

    /// Neither a name to ask with nor a length to check against. Guessing here
    /// would be picking a stranger's song out of a list.
    #[test]
    fn without_an_artist_or_a_duration_nothing_is_chosen() {
        let candidates = [synced_candidate("Whoever", "Duality", 224.0)];
        let identity = identify(None, "Duality");

        assert!(choose(&candidates, &identity, None).is_nothing());
    }

    /// lrclib's durations are whatever the contributor's own file was, so a
    /// couple of seconds says much less than the presence of real timestamps.
    #[test]
    fn synced_lyrics_beat_a_closer_length_without_them() {
        let candidates = [
            candidate("Set It Off", "Duality", 224.0),
            synced_candidate("Set It Off", "Duality", 226.0),
        ];
        let identity = identify(Some("Set It Off"), "Duality");

        let chosen = choose(&candidates, &identity, Some(224.0)).confident().expect("a confident match");
        assert!(chosen.synced.is_some(), "picked the row with no timings");
    }

    /// The answer most of this library deserves.
    #[test]
    fn an_instrumental_candidate_can_be_chosen() {
        let candidates = [Match {
            provider: "lrclib",
            artist: "Lena Raine".to_string(),
            title: "Checking In".to_string(),
            duration_secs: Some(425.0),
            instrumental: true,
            ..Default::default()
        }];
        let identity = identify(Some("Lena Raine"), "Checking In");

        let chosen = choose(&candidates, &identity, Some(425.0)).confident().expect("a confident match");
        assert!(chosen.instrumental);
    }

    #[test]
    fn a_row_with_nothing_in_it_is_never_chosen() {
        let candidates = [Match {
            provider: "lrclib",
            artist: "Set It Off".to_string(),
            title: "Duality".to_string(),
            duration_secs: Some(224.0),
            ..Default::default()
        }];
        let identity = identify(Some("Set It Off"), "Duality");

        assert!(choose(&candidates, &identity, Some(224.0)).is_nothing());
    }

    /// When nothing separates the candidates, say so instead of picking one.
    ///
    /// Taken from the real answer to a title-only search for "Vancouver"
    /// against a 223-second track: several songs share the name, and three of
    /// them are within seconds of the right length. Jeff Buckley's is three
    /// seconds out with an exactly matching title — it would win a ranking,
    /// and it is the wrong song.
    #[test]
    fn several_songs_that_merely_share_a_name_are_a_question_not_an_answer() {
        let candidates = [
            synced_candidate("Jeff Buckley", "Vancouver", 220.0),
            synced_candidate("Véronique Sanson", "Vancouver", 227.0),
            synced_candidate("Kingfishr", "Vancouver", 239.0),
        ];
        let identity = identify(None, "Vancouver");

        let Choice::Ambiguous(shortlist) = choose(&candidates, &identity, Some(223.0)) else {
            panic!("picked one of three unrelated songs that share a title");
        };
        assert_eq!(shortlist.len(), 2, "239s is too far out to be offered");
    }

    /// A guest credit does not make it somebody else's song.
    ///
    /// The eight rows lrclib returns for this one are split: the copy filed
    /// under exactly `ALESTI` has no timings, and the timed copy is filed
    /// under `ALESTI feat. Siamese`. String equality says those are different
    /// artists, so it would confidently hand back the untimed lyric and leave
    /// the synced one sitting there.
    #[test]
    fn a_guest_credit_does_not_make_it_a_different_artist() {
        let candidates = [
            candidate("ALESTI", "Unravel", 215.0),
            Match {
                synced: Some(LRC.to_string()),
                plain: None,
                ..candidate("ALESTI feat. Siamese", "Unravel", 216.0)
            },
        ];

        let identity = identify(Some("ALESTI"), "ALESTI - Unravel (feat. Siamese)");
        let chosen = choose(&candidates, &identity, Some(216.0))
            .confident()
            .expect("a guest credit made this unanswerable");

        assert!(
            chosen.synced.is_some(),
            "settled for a plain lyric when a timed one was there"
        );
    }

    /// The looseness has to stop somewhere.
    ///
    /// Truncating at a collaborator is safe because it happens to both sides
    /// of the comparison, so an odd split cancels out. Two genuinely different
    /// artists must still be different.
    #[test]
    fn a_longer_name_is_not_the_same_artist() {
        assert_eq!(primary_artist("Malcolm X"), primary_artist("Malcolm X"));
        assert_ne!(primary_artist("Queen"), primary_artist("Queen Latifah"));
        assert_eq!(primary_artist("Queen"), primary_artist("Queen & David Bowie"));
        assert_eq!(primary_artist("ALESTI"), primary_artist("ALESTI/Siamese"));
        assert_eq!(primary_artist("ALESTI"), primary_artist("ALESTI feat. Siamese"));
        assert_ne!(primary_artist("Siamese"), primary_artist("ALESTI"));
    }

    // --- when a second provider is worth asking --------------------------

    fn with_lyrics(text: &str) -> Match {
        Match {
            synced: Some(text.to_string()),
            ..candidate("Someone", "Something", 200.0)
        }
    }

    /// The trigger for asking NetEase at all.
    ///
    /// "lrclib found nothing" is the obvious rule and is the wrong one: lrclib
    /// holds a Japanese song's Japanese lyrics perfectly well, it just never
    /// holds a romanisation. Keying off that would mean the one thing the
    /// second provider exists for was almost never fetched.
    #[test]
    fn only_a_script_this_reader_cannot_follow_wants_romanising() {
        assert!(wants_romanising(&with_lyrics(
            "[00:00.62]教えて教えてよ その仕組みを"
        )));
        assert!(wants_romanising(&with_lyrics("[00:00.62]잊지 말아요")));

        assert!(!wants_romanising(&with_lyrics(
            "[00:00.62]Wake up, grab a brush and put a little makeup"
        )));
    }

    /// Not "contains non-ASCII".
    ///
    /// `Ōrīōn`, `Amnéhilesie` and `Manos Te Faltarán` are all in this library
    /// and all already readable. Romanising them would be a request spent to
    /// change nothing.
    #[test]
    fn accented_latin_is_already_readable() {
        for line in [
            "[00:00.62]Ōrīōn no shita de",
            "[00:00.62]Manos te faltarán para tocarme",
            "[00:00.62]Amnéhilesie, être ou ne pas être",
        ] {
            assert!(
                !wants_romanising(&with_lyrics(line)),
                "would have romanised {line}"
            );
        }
    }

    /// One borrowed word does not make a song Japanese.
    #[test]
    fn a_single_foreign_word_does_not_trigger_it() {
        assert!(!wants_romanising(&with_lyrics(
            "[00:00.62]We say 愛 and mean it, over and over and over again"
        )));
    }

    /// Where two matches are otherwise equal, the one that can be read wins.
    #[test]
    fn a_romanised_match_wins_a_tie() {
        let plain_jp = Match {
            provider: "lrclib",
            synced: Some(LRC.to_string()),
            ..candidate("TK", "unravel", 240.0)
        };
        let romanised = Match {
            provider: "netease",
            synced: Some(LRC.to_string()),
            romaji: Some(LRC.to_string()),
            ..candidate("TK", "unravel", 240.0)
        };

        let identity = identify(Some("TK"), "unravel");
        let candidates = [plain_jp, romanised];
        let chosen = choose(&candidates, &identity, Some(240.0))
            .confident()
            .expect("a confident match");

        assert_eq!(chosen.provider, "netease");
        assert!(chosen.romaji.is_some());
    }

    /// Knowing the artist is what makes it an answer again.
    #[test]
    fn a_matching_artist_settles_it() {
        let candidates = [
            synced_candidate("Jeff Buckley", "Vancouver", 220.0),
            synced_candidate("ivycomb", "Vancouver", 223.0),
        ];

        let chosen = choose(&candidates, &identify(Some("Ivycomb Music"), "Vancouver"), Some(223.0))
            .confident()
            .expect("the artist matched, so there is nothing to ask about");

        assert_eq!(chosen.artist, "ivycomb");
    }

    /// One survivor is not a dilemma.
    ///
    /// Asking about a list of one would be pedantry rather than honesty, and
    /// it is the common shape for the untagged half of this library.
    #[test]
    fn a_single_survivor_is_answer_enough() {
        let candidates = [
            synced_candidate("Whoever", "Vancouver", 223.0),
            synced_candidate("Someone Else", "Vancouver", 400.0),
        ];

        assert!(
            choose(&candidates, &identify(None, "Vancouver"), Some(223.0))
                .confident()
                .is_some()
        );
    }

    // --- the negative cache ----------------------------------------------

    async fn write_negative(pool: &SqlitePool, key: &str, age_secs: i64) {
        sqlx::query(
            "INSERT INTO lyrics (identity_key, provider, fetched_at) \
             VALUES (?, 'lrclib', unixepoch() - ?)",
        )
        .bind(key)
        .bind(age_secs)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Without this row, every play of a track with no lyrics is another
    /// request. This library has hundreds that will never match anything.
    #[tokio::test]
    async fn a_fresh_negative_is_not_worth_asking_again() {
        let (db, base) = fixture("negative-fresh").await;
        let identity = identify(Some("Nobody"), "Untitled");
        write_negative(&db.pool, &identity.key, 60).await;

        let state = cached(&db.pool, &identity, 0).await.unwrap();
        assert!(matches!(state, Cached::Empty));

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// And it does not stand forever. lrclib's catalogue is contributed by its
    /// users, so a song with nothing today can have lyrics next month; a
    /// permanent negative would make the app permanently wrong about it.
    #[tokio::test]
    async fn a_stale_negative_is_worth_asking_again() {
        let (db, base) = fixture("negative-stale").await;
        let identity = identify(Some("Nobody"), "Untitled");
        write_negative(&db.pool, &identity.key, NEGATIVE_HOLDS_FOR_SECS + 60).await;

        let state = cached(&db.pool, &identity, 0).await.unwrap();
        assert!(matches!(state, Cached::Worth));

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn storing_nothing_writes_a_negative_rather_than_no_row() {
        let (db, base) = fixture("store-negative").await;
        let identity = identify(Some("Nobody"), "Untitled");

        store(&db.pool, &identity.key, None).await.unwrap();

        assert!(matches!(
            cached(&db.pool, &identity, 0).await.unwrap(),
            Cached::Empty
        ));

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A chosen candidate round-trips: stored as the provider's own text,
    /// parsed back on the way out.
    #[tokio::test]
    async fn a_stored_candidate_comes_back_synced() {
        let (db, base) = fixture("store-synced").await;
        let identity = identify(Some("Set It Off"), "Duality");
        let chosen = synced_candidate("Set It Off", "Duality", 224.0);

        store(&db.pool, &identity.key, Some(&chosen)).await.unwrap();

        let Cached::Hit(found) = cached(&db.pool, &identity, 0).await.unwrap() else {
            panic!("stored lyrics did not read back");
        };
        assert_eq!(found.kind, Kind::Synced);
        assert_eq!(found.origin, "lrclib");
        assert_eq!(found.lines[0].at_ms, Some(10_000));

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A second answer for the same song replaces the first rather than
    /// failing on the unique key -- which is what makes a stale negative
    /// recoverable at all.
    #[tokio::test]
    async fn a_later_answer_replaces_an_earlier_one() {
        let (db, base) = fixture("store-replace").await;
        let identity = identify(Some("Set It Off"), "Duality");

        store(&db.pool, &identity.key, None).await.unwrap();
        let chosen = synced_candidate("Set It Off", "Duality", 224.0);
        store(&db.pool, &identity.key, Some(&chosen)).await.unwrap();

        assert!(matches!(
            cached(&db.pool, &identity, 0).await.unwrap(),
            Cached::Hit(_)
        ));

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The request that is never sent.
    ///
    /// With no artist and no duration there is nothing to ask with and nothing
    /// to check an answer against, so this returns without touching the
    /// network -- which is also why the test can run offline.
    #[tokio::test]
    async fn a_track_with_no_artist_and_no_duration_is_never_asked_about() {
        let (db, base) = fixture("no-question").await;
        let id = sqlx::query(
            "INSERT INTO tracks (source, title, remote_id, remote_url, state, in_library) \
             VALUES ('youtube', 'Untitled', 'vid9', 'https://example.invalid/x', 'saved', 1)",
        )
        .execute(&db.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let looked = fetch(&db.pool, id).await.unwrap();
        assert_eq!(looked.lyrics, None);
        assert!(looked.candidates.is_empty());

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lyrics")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0, "a question that was never asked was cached anyway");

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A fresh negative short-circuits before the gate, so no request goes out
    /// even when a duration is known.
    #[tokio::test]
    async fn a_cached_negative_stops_the_fetch_before_the_network() {
        let (db, base) = fixture("fetch-negative").await;
        let id = sqlx::query(
            "INSERT INTO tracks (source, title, artist, duration_secs, remote_id, remote_url, \
                                 state, in_library) \
             VALUES ('youtube', 'Untitled', 'Nobody', 224, 'vid8', \
                     'https://example.invalid/x', 'saved', 1)",
        )
        .execute(&db.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let identity = identify(Some("Nobody"), "Untitled");
        write_negative(&db.pool, &identity.key, 60).await;

        let looked = fetch(&db.pool, id).await.unwrap();
        assert_eq!(looked.lyrics, None);
        assert!(looked.candidates.is_empty());

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// How much of a real library this can even ask about.
    ///
    /// Not an assertion -- a measurement, to be read rather than passed. Every
    /// provider worth using wants an artist, so the share of rows that reach
    /// [`identify`] with one is the ceiling on what phase two can find, and it
    /// is worth knowing that number before writing the fetcher rather than
    /// after. Same shape as `ytmusic_probe`: a question about the outside
    /// world, run deliberately.
    ///
    /// ```text
    /// MUSIC_APP_LIBRARY=%APPDATA%/com.kiza2.music-app/library.db \
    ///   cargo test --lib lyrics_coverage -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "reads a real library; set MUSIC_APP_LIBRARY"]
    async fn lyrics_coverage() {
        let Ok(path) = std::env::var("MUSIC_APP_LIBRARY") else {
            eprintln!("SKIP: MUSIC_APP_LIBRARY is not set");
            return;
        };

        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{path}?mode=ro"))
            .await
            .expect("library database");

        let rows = sqlx::query("SELECT title, artist FROM tracks WHERE in_library = 1")
            .fetch_all(&pool)
            .await
            .expect("tracks");

        let mut with_artist = 0usize;
        let mut cleaned_title = 0usize;
        let mut keys = std::collections::HashSet::new();

        for row in &rows {
            let title: String = row.try_get("title").unwrap_or_default();
            let artist: Option<String> = row.try_get("artist").ok().flatten();

            let identity = identify(artist.as_deref(), &title);
            if identity.artist.is_some() {
                with_artist += 1;
            }
            if identity.title != title {
                cleaned_title += 1;
            }
            keys.insert(identity.key);
        }

        let total = rows.len();
        eprintln!("in library:          {total}");
        eprintln!(
            "usable artist:       {with_artist}  ({:.1}%)",
            100.0 * with_artist as f64 / total.max(1) as f64
        );
        eprintln!("titles cleaned:      {cleaned_title}");
        eprintln!(
            "distinct songs:      {}  ({} rows share one)",
            keys.len(),
            total - keys.len()
        );
    }

    /// Vancouver, end to end, against the real lrclib.
    ///
    /// The reported bug: the song is in the library twice and lrclib has its
    /// lyrics, but the app found nothing. Both rows go through the whole path
    /// here — clean the artist, ask lrclib, rank what comes back — because the
    /// unit tests above can only prove what this code does with candidates
    /// somebody typed out, not that the request it sends returns them.
    ///
    /// ```text
    /// cargo test --lib live_vancouver -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "talks to lrclib"]
    async fn live_vancouver_finds_its_lyrics_from_either_row() {
        // Exactly as the two rows are stored.
        for artist in ["ivycomb", "Ivycomb Music"] {
            let identity = identify(Some(artist), "Vancouver");
            eprintln!("\n{artist:?} -> asking as {:?}", identity.artist);

            let candidates = crate::lrclib::find(&identity).await.expect("lrclib");
            eprintln!("  {} candidates", candidates.len());

            match choose(&candidates, &identity, Some(223.0)) {
                Choice::Confident(found) => {
                    eprintln!(
                        "  confident: [{}] [{}] {:?}s synced={}",
                        found.artist,
                        found.title,
                        found.duration_secs,
                        found.synced.is_some()
                    );
                    assert!(
                        found.artist.to_lowercase().contains("ivycomb"),
                        "picked somebody else's song: {}",
                        found.artist
                    );
                    assert!(found.synced.is_some(), "no timings");
                }
                Choice::Ambiguous(shortlist) => {
                    for candidate in &shortlist {
                        eprintln!("  would ask: [{}] [{}]", candidate.artist, candidate.title);
                    }
                    panic!("{artist:?}: still cannot tell which song this is");
                }
                Choice::Nothing => panic!("{artist:?}: found nothing at all — the reported bug"),
            }
        }
    }

    /// A guest credit is not a different song, and not a different artist.
    ///
    /// Stored as `ALESTI` / `ALESTI - Unravel (feat. Siamese)`. Both halves of
    /// that title defeat the query on their own — measured against the real
    /// service:
    ///
    /// ```text
    /// track_name=ALESTI - Unravel (feat. Siamese) -> 0
    /// track_name=Unravel (feat. Siamese)          -> 0
    /// track_name=Unravel                          -> 8
    /// ```
    ///
    /// And getting the query right is only half of it. Of those eight, the row
    /// whose artist reads exactly `ALESTI` has no timings, while the timed one
    /// is filed under `ALESTI feat. Siamese` — so matching artists by string
    /// equality would confidently return a plain lyric and leave the synced
    /// one on the table.
    #[tokio::test]
    #[ignore = "talks to lrclib"]
    async fn live_a_guest_credit_does_not_hide_a_song() {
        let identity = identify(Some("ALESTI"), "ALESTI - Unravel (feat. Siamese)");
        eprintln!("asking for {:?} / {:?}", identity.artist, identity.title);
        assert_eq!(identity.title, "Unravel", "the query title is still wrong");

        let candidates = crate::lrclib::find(&identity).await.expect("lrclib");
        eprintln!("{} candidates", candidates.len());

        let Choice::Confident(found) = choose(&candidates, &identity, Some(216.0)) else {
            panic!("a guest credit still made this unanswerable");
        };

        eprintln!(
            "confident: [{}] [{}] {:?}s synced={}",
            found.artist,
            found.title,
            found.duration_secs,
            found.synced.is_some()
        );
        assert!(
            found.artist.to_lowercase().starts_with("alesti"),
            "picked somebody else: {}",
            found.artist
        );
        assert!(
            found.synced.is_some(),
            "settled for a plain lyric when a timed one was there"
        );
    }

    /// The whole point of a second provider, end to end.
    ///
    /// lrclib is asked first and holds no romanisation of anything; NetEase
    /// returns the original and a `romalrc` sharing one set of timestamps. So
    /// this asserts what the reader will actually be handed: two parallel
    /// tracks of the same length, which is what lets the Romaji switch not
    /// move the highlight.
    #[tokio::test]
    #[ignore = "talks to lrclib and NetEase"]
    async fn live_a_japanese_song_comes_back_with_romaji() {
        let identity = identify(Some("TK from 凛として時雨"), "unravel");
        let candidates = ask_providers(&identity, Some(240.0)).await.expect("providers");

        eprintln!("{} candidates", candidates.len());
        for candidate in &candidates {
            eprintln!(
                "  [{}] [{}] {:?}s provider={} synced={} romaji={}",
                candidate.artist,
                candidate.title,
                candidate.duration_secs,
                candidate.provider,
                candidate.synced.is_some(),
                candidate.romaji.is_some(),
            );
        }

        let Choice::Confident(found) = choose(&candidates, &identity, Some(240.0)) else {
            panic!("could not settle on a version of unravel");
        };

        let romaji = found
            .romaji
            .as_deref()
            .expect("no romanisation, which is the only reason NetEase is here");

        let original = parse_lrc(found.synced.as_deref().expect("no timings")).expect("lrc");
        let romanised = parse_lrc(romaji).expect("romaji lrc");

        eprintln!(
            "  chose {} — {} lines, {} romanised",
            found.provider,
            original.len(),
            romanised.len()
        );

        // Same timestamps, so switching between them cannot move the
        // highlight. Not required to be identical in length -- providers do
        // drop the odd blank line -- but they must line up at the start.
        assert_eq!(
            original.first().map(|line| line.at_ms),
            romanised.first().map(|line| line.at_ms),
            "the two tracks do not share a timeline"
        );
        // Romanised, not merely present: a romalrc that came back as the
        // original text would satisfy every other assertion here.
        let latin = romanised
            .iter()
            .filter(|line| !line.text.trim().is_empty())
            .filter(|line| !line.text.chars().any(is_unromanised))
            .count();
        assert!(
            latin * 2 > romanised.len(),
            "only {latin} of {} lines are readable in Latin script",
            romanised.len()
        );
    }

    /// What phase two actually finds, end to end, on real rows.
    ///
    /// The number that matters and the only way to get it is to ask. Samples a
    /// bounded slice of the library so this stays polite to a service somebody
    /// donates -- it is a measurement, not a backfill, and the app has no bulk
    /// pass for exactly the same reason.
    ///
    /// ```text
    /// MUSIC_APP_LIBRARY=... cargo test --lib live_coverage -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "talks to lrclib about a real library"]
    async fn live_coverage() {
        const SAMPLE: i64 = 30;

        let Ok(path) = std::env::var("MUSIC_APP_LIBRARY") else {
            eprintln!("SKIP: MUSIC_APP_LIBRARY is not set");
            return;
        };

        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{path}?mode=ro"))
            .await
            .expect("library database");

        let rows = sqlx::query(
            "SELECT title, artist, duration_secs FROM tracks \
             WHERE in_library = 1 AND duration_secs > 0 \
             ORDER BY id LIMIT ?",
        )
        .bind(SAMPLE)
        .fetch_all(&pool)
        .await
        .expect("tracks");

        let (mut synced, mut plain, mut instrumental, mut nothing, mut refused) = (0, 0, 0, 0, 0);
        let mut ambiguous = 0;

        for row in &rows {
            let title: String = row.try_get("title").unwrap_or_default();
            let artist: Option<String> = row.try_get("artist").ok().flatten();
            let duration: Option<i64> = row.try_get("duration_secs").ok().flatten();
            let identity = identify(artist.as_deref(), &title);

            pace().await;
            // The real chain, both providers, exactly as the app asks.
            let candidates = match ask_providers(&identity, duration.map(|s| s as f64)).await {
                Ok(found) => found,
                Err(e) => {
                    eprintln!("  ERROR {title}: {e}");
                    continue;
                }
            };

            let chosen = choose(&candidates, &identity, duration.map(|s| s as f64));
            let verdict = match chosen {
                Choice::Confident(c) if c.synced.is_some() => {
                    synced += 1;
                    "synced"
                }
                Choice::Confident(c) if c.instrumental => {
                    instrumental += 1;
                    "instrumental"
                }
                Choice::Confident(_) => {
                    plain += 1;
                    "plain"
                }
                Choice::Ambiguous(shortlist) => {
                    ambiguous += 1;
                    eprintln!("      (would ask: {} candidates)", shortlist.len());
                    "ask the user"
                }
                Choice::Nothing if candidates.is_empty() => {
                    nothing += 1;
                    "nothing offered"
                }
                Choice::Nothing => {
                    refused += 1;
                    "refused"
                }
            };

            eprintln!(
                "  {verdict:<15} {:>3} offered  {title}",
                candidates.len()
            );
        }

        eprintln!("\n  of {} sampled:", rows.len());
        eprintln!("    synced        {synced}");
        eprintln!("    plain         {plain}");
        eprintln!("    instrumental  {instrumental}");
        eprintln!("    ask the user  {ambiguous}   (several fit, nothing separated them)");
        eprintln!("    refused       {refused}   (candidates came back, none passed the gate)");
        eprintln!("    nothing       {nothing}   (lrclib had no rows at all)");
    }
}
