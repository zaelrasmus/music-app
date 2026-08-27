//! Filling in details the files never carried.
//!
//! 1003 of this library's 1019 local tracks have no artist tag and no album
//! tag. Every artist feature in the app -- the artist list, artist playlists,
//! grouping, and finding lyrics at all -- therefore applies to the other
//! sixteen. A dialog per track is a thousand dialogs, so this is the half of
//! the metadata editor that actually moves the number.
//!
//! Two signals exist, and neither is trustworthy alone:
//!
//!   * **The title.** 555 of the 1003 are shaped `"Artist - Title"`, because
//!     that is what a filename becomes when there is no tag to read. This is
//!     computable and is what this module proposes.
//!
//!   * **The folder.** Sometimes the artist (`Creo`, `ElRichMC`, `Nanolith`),
//!     sometimes a genre (`Artcore` with 310 tracks, `Math Rock` with 103),
//!     sometimes a soundtrack (`Celeste`, `Hotline Miami`, `Katana Zero`).
//!     Measured on this library it is the artist **about half the time**, so
//!     nothing here applies it. It is offered, per folder, for a person to
//!     accept -- which is the one judgement the app cannot make and a person
//!     makes at a glance.
//!
//! Nothing in this module writes. It proposes, and `apply_track_details`
//! writes what came back. That split is the point: an inference applied
//! silently across a thousand rows is indistinguishable from corruption.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tauri::State;

use crate::db::Db;
use crate::lyrics;

/// What can stand between an artist and a title.
///
/// **Whitespace on both sides is required**, and that is the whole safety of
/// the split: `"Jean-Michel Jarre - Oxygene"` has to yield `Jean-Michel Jarre`
/// and not `Jean`. A bare hyphen appears inside names far more often than it
/// separates one from a title.
const SEPARATORS: &[&str] = &[" - ", " \u{2013} ", " \u{2014} "];

/// Shortest either side may be.
///
/// One character is initials, a stray dash, or a numbering scheme, and never
/// an artist worth proposing.
const MIN_SIDE: usize = 2;

/// Longest a proposed artist may be.
///
/// Past this the left side is a sentence, which means the dash was doing
/// something else -- a subtitle, a description, a date.
const MAX_ARTIST: usize = 60;

/// One suggestion for one track.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub artist: String,
    /// The title with the artist taken off the front and the noise removed.
    pub title: String,
}

/// A track that has no artist, and what could be done about it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackProposal {
    pub track_id: i64,
    pub current_title: String,
    /// Read out of the title's own `"Artist - Title"` shape, when it has one.
    ///
    /// `None` is the common case for 385 of these tracks and is not a failure
    /// -- it means the title says nothing about who made it, and the folder is
    /// the only thing left to go on.
    pub from_title: Option<Proposal>,
    /// The same track read as though the folder were the artist.
    ///
    /// Always present, never preferred. Offered so that accepting a folder is
    /// one gesture for everything in it.
    pub from_folder: Proposal,
}

/// A folder holding tracks with no artist.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderGroup {
    /// Full path, which is what identifies it.
    pub path: String,
    /// Last component, which is what a person reads and what gets proposed.
    pub name: String,
    pub total: i64,
    /// How many of them the title alone can answer for.
    ///
    /// The number that says whether this folder is worth opening: high means
    /// the filenames carry the artists, low means the folder name is the only
    /// thing on offer and the decision is entirely the user's.
    pub from_titles: i64,
}

/// One row's worth of "yes, write this".
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackEdit {
    pub track_id: i64,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
}

/// Quote characters a title can be wrapped in.
const QUOTES: &[(char, char)] = &[
    ('"', '"'),
    ('\'', '\''),
    ('\u{201c}', '\u{201d}'),
    ('\u{2018}', '\u{2019}'),
];

/// Decoration that follows an artist's name in a video title.
///
/// `'SUPERHEROES' by Soul Extract _ Epic Rock` — the genre tacked on the end
/// is the uploader describing their channel, not part of who made it.
const ARTIST_TAILS: &[&str] = &[" _ ", " | ", " \u{2022} ", " - "];

/// `"Past And Language" by Toe` — the title first, in quotes, artist after.
///
/// A whole folder of this library is named this way. Worth its own rule
/// because the ordinary split gets it exactly backwards: it would call the
/// song "Toe" and the artist "Past And Language".
///
/// The quotes are what make it safe. Without them `by` is an ordinary word and
/// `"Stand by Me"` would lose its second half.
fn split_quoted_by(title: &str) -> Option<(String, String)> {
    let title = title.trim();
    let mut chars = title.chars();
    let open = chars.next()?;
    let close = QUOTES
        .iter()
        .find_map(|(o, c)| (*o == open).then_some(*c))?;

    let rest = &title[open.len_utf8()..];
    let end = rest.find(close)?;
    let quoted = rest[..end].trim();
    let after = rest[end + close.len_utf8()..].trim_start();

    let artist = after.strip_prefix("by ").or_else(|| after.strip_prefix("By "))?;

    // Cut the channel's own branding off the end of the name.
    let artist = ARTIST_TAILS
        .iter()
        .filter_map(|tail| artist.find(tail))
        .min()
        .map_or(artist, |at| &artist[..at])
        .trim();

    if quoted.chars().count() < MIN_SIDE || artist.chars().count() < MIN_SIDE {
        return None;
    }
    if artist.chars().count() > MAX_ARTIST {
        return None;
    }

    Some((artist.to_string(), quoted.to_string()))
}

/// Whether a title opens with a quote mark.
///
/// Such a title is naming *itself*, so whatever follows a dash is not the
/// artist -- `'The Pirates Rise' - Johannes Bornlof` is a song and its
/// composer in that order, and splitting it names the artist after the song.
fn starts_quoted(title: &str) -> bool {
    let Some(open) = title.trim().chars().next() else {
        return false;
    };
    QUOTES.iter().any(|(o, _)| *o == open)
}

/// Splits `"Artist - Title"` at the first separator, if it looks like one.
///
/// Deliberately the *first* separator rather than the last:
/// `"Celeste Original Soundtrack - 06 - Checking In"` is a catalogue path, and
/// the artist is at the front of it.
fn split_leading_artist(title: &str) -> Option<(&str, &str)> {
    let title = title.trim();

    if starts_quoted(title) {
        return None;
    }

    let (at, sep) = SEPARATORS
        .iter()
        .filter_map(|sep| title.find(sep).map(|at| (at, *sep)))
        .min_by_key(|(at, _)| *at)?;

    let left = title[..at].trim();
    let right = title[at + sep.len()..].trim();

    if left.chars().count() < MIN_SIDE || right.chars().count() < MIN_SIDE {
        return None;
    }
    if left.chars().count() > MAX_ARTIST {
        return None;
    }
    // "06 - Checking In". A number in front is a track position, and calling
    // it an artist would file a whole album under "06".
    if left.chars().all(|c| !c.is_alphabetic()) {
        return None;
    }
    // "Duality - Remix" separates a song from its version, not an artist from
    // a song. Splitting it would name the track "Remix" and the artist
    // "Duality", which is wrong twice.
    if lyrics::is_only_version_marker(right) {
        return None;
    }

    Some((left, right))
}

/// What the title alone suggests.
///
/// The cleaning is `lyrics::identify`, unchanged and on purpose: it already
/// knows to take the artist's own name off the front of a title, to drop
/// `(Official Music Video)`, and to leave `(feat. …)` where it belongs. A
/// second implementation here would be a second set of rules to keep in step.
pub fn propose_from_title(title: &str) -> Option<Proposal> {
    // A filename whose spaces became underscores is still a filename, and
    // `Aaron_Smith_-_Dancin_KRONO_Remix` is unreadable as either an artist or
    // a title. `_-_` is what proves the whole name is encoded that way -- a
    // lone underscore is not, which is why `AcuticNotes - I_K` keeps its own.
    let normalised = if title.contains("_-_") {
        title.replace('_', " ")
    } else {
        title.to_string()
    };

    if let Some((artist, quoted)) = split_quoted_by(&normalised) {
        // Already the right way round, and the quotes were the title's own
        // punctuation rather than part of its name.
        return Some(Proposal { artist, title: quoted });
    }

    let (artist, _) = split_leading_artist(&normalised)?;
    finish(artist, &normalised)
}

/// The same track read as though `folder` named the artist.
///
/// The folder name is used **verbatim**. `lyrics::identify` would clean it,
/// and its cleaning is built for uploader handles: it strips "Music" as
/// channel decoration, which turns a folder called `Epic Music` into an artist
/// called `Epic`. A directory is not a channel.
pub fn propose_from_folder(folder: &str, title: &str) -> Proposal {
    let artist = collapse(folder);
    // Never empty: `clean_title` keeps the original when its rules would
    // remove everything, so a row always has something to offer.
    let title = lyrics::title_without(&artist, title);

    Proposal { artist, title }
}

/// Trims and squeezes runs of whitespace, so a folder named with a stray
/// double space proposes the same artist as one without.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Runs a candidate artist and a raw title through the lyrics identity rules.
fn finish(artist: &str, title: &str) -> Option<Proposal> {
    let identity = lyrics::identify(Some(artist), title);
    let artist = identity.artist?;
    if artist.is_empty() || identity.title.is_empty() {
        return None;
    }
    Some(Proposal { artist, title: identity.title })
}

/// The directory a file sits in.
///
/// Written by hand rather than with `Path::parent` because the separator has
/// to be read the same way on every platform: these paths were recorded on
/// Windows and carry backslashes, and a test running anywhere must see the
/// same answer the app does.
fn parent_of(path: &str) -> Option<&str> {
    let at = path.rfind(['/', '\\'])?;
    (at > 0).then(|| &path[..at])
}

/// The last component of a directory path, which is what a person calls it.
fn folder_name(path: &str) -> &str {
    match path.rfind(['/', '\\']) {
        Some(at) => &path[at + 1..],
        None => path,
    }
}

/// Every local track the library cannot name an artist for.
///
/// `state = 'present'` because a file that is not on disk is not something to
/// tidy -- it would come back as a proposal on every visit and never stop.
const NEEDS_ARTIST: &str = "SELECT id, local_path, title FROM tracks \
     WHERE source = 'local' AND state = 'present' AND local_path IS NOT NULL \
       AND (artist IS NULL OR trim(artist) = '')";

struct Row {
    id: i64,
    path: String,
    title: String,
}

async fn needs_artist(pool: &SqlitePool) -> Result<Vec<Row>, String> {
    sqlx::query_as::<_, (i64, String, String)>(NEEDS_ARTIST)
        .fetch_all(pool)
        .await
        .map_err(|e| e.to_string())
        .map(|rows| {
            rows.into_iter()
                .map(|(id, path, title)| Row { id, path, title })
                .collect()
        })
}

/// The folders worth visiting, biggest first.
///
/// Split from the command so its tests run *this* code rather than a
/// paraphrase of it -- the same trade `tracks.rs` makes by naming
/// `SET_IN_LIBRARY`. A command needs a Tauri app to have `State<Db>` at all,
/// and a test that reimplements the body proves only that the test is
/// self-consistent.
pub async fn group_untagged(pool: &SqlitePool) -> Result<Vec<FolderGroup>, String> {
    let rows = needs_artist(pool).await?;

    // Grouped in Rust rather than in SQL: taking a parent directory out of a
    // path in SQLite means nested `rtrim`/`replace` tricks that are unreadable
    // and get the Windows separator wrong.
    let mut groups: std::collections::HashMap<&str, (i64, i64)> = std::collections::HashMap::new();
    for row in &rows {
        let Some(parent) = parent_of(&row.path) else {
            continue;
        };
        let entry = groups.entry(parent).or_insert((0, 0));
        entry.0 += 1;
        if propose_from_title(&row.title).is_some() {
            entry.1 += 1;
        }
    }

    let mut out: Vec<FolderGroup> = groups
        .into_iter()
        .map(|(path, (total, from_titles))| FolderGroup {
            name: folder_name(path).to_string(),
            path: path.to_string(),
            total,
            from_titles,
        })
        .collect();

    // Biggest first: the work is worth doing in the order that fixes the most
    // tracks, and a folder of three can wait.
    out.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

#[tauri::command]
pub async fn untagged_folders(db: State<'_, Db>) -> Result<Vec<FolderGroup>, String> {
    group_untagged(&db.pool).await
}

/// What could be done with each track in one folder.
pub async fn proposals_in(pool: &SqlitePool, folder: &str) -> Result<Vec<TrackProposal>, String> {
    let rows = needs_artist(pool).await?;
    let name = folder_name(folder).to_string();

    let mut out: Vec<TrackProposal> = rows
        .into_iter()
        .filter(|row| parent_of(&row.path) == Some(folder))
        .map(|row| TrackProposal {
            from_title: propose_from_title(&row.title),
            from_folder: propose_from_folder(&name, &row.title),
            track_id: row.id,
            current_title: row.title,
        })
        .collect();

    out.sort_by(|a, b| a.current_title.cmp(&b.current_title));
    Ok(out)
}

#[tauri::command]
pub async fn folder_proposals(
    db: State<'_, Db>,
    folder: String,
) -> Result<Vec<TrackProposal>, String> {
    proposals_in(&db.pool, &folder).await
}

/// Writes the edits a person accepted.
///
/// One transaction, so a folder of three hundred is one gesture that either
/// happened or did not -- half a folder renamed, with no way to tell which
/// half, would be worse than none of it.
///
/// Validation matches [`crate::tracks::update_track_metadata`], because these
/// rows are indistinguishable from ones typed by hand once written: a blank
/// title is refused, and a blank artist or album is NULL rather than `''`.
pub async fn write_details(pool: &SqlitePool, edits: &[TrackEdit]) -> Result<usize, String> {
    if edits.is_empty() {
        return Ok(0);
    }

    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let mut written = 0usize;

    for edit in edits {
        let title = edit.title.trim();
        if title.is_empty() {
            // Returning here drops the transaction, which rolls back every
            // edit already applied in it. That is the intended behaviour and
            // `one_bad_edit_writes_none_of_them` is what holds it.
            return Err("A title is required.".to_string());
        }
        let blank = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };

        written += sqlx::query("UPDATE tracks SET title = ?, artist = ?, album = ? WHERE id = ?")
            .bind(title)
            .bind(blank(&edit.artist))
            .bind(blank(&edit.album))
            .bind(edit.track_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?
            .rows_affected() as usize;
    }

    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(written)
}

#[tauri::command]
pub async fn apply_track_details(
    db: State<'_, Db>,
    edits: Vec<TrackEdit>,
) -> Result<usize, String> {
    write_details(&db.pool, &edits).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artist_of(title: &str) -> Option<String> {
        propose_from_title(title).map(|p| p.artist)
    }

    #[test]
    fn a_filename_shaped_title_gives_up_its_artist() {
        let p = propose_from_title("Alex Norre - Always Forgotten").unwrap();
        assert_eq!(p.artist, "Alex Norre");
        assert_eq!(p.title, "Always Forgotten", "the artist leaves the title");
    }

    /// The reason the separator requires spaces around it. A hyphen inside a
    /// name is far more common than one separating a name from a song, and
    /// splitting on the bare character would file this under "Jean".
    #[test]
    fn a_hyphenated_name_survives() {
        assert_eq!(
            artist_of("Jean-Michel Jarre - Oxygene").as_deref(),
            Some("Jean-Michel Jarre")
        );
        assert_eq!(artist_of("Jean-Michel Jarre"), None, "no separator at all");
    }

    /// `"Duality - Remix"` is a song and its version, not an artist and a
    /// song. Splitting it names the track "Remix" and the artist "Duality".
    #[test]
    fn a_version_suffix_is_not_an_artist_separator() {
        assert_eq!(artist_of("Duality - Remix"), None);
        assert_eq!(artist_of("Something - Extended Version"), None);
        assert_eq!(
            artist_of("Slipknot - Duality").as_deref(),
            Some("Slipknot"),
            "a real artist in the same shape still splits"
        );
    }

    /// A leading number is a track position. Reading it as an artist would
    /// file a whole album under "06".
    #[test]
    fn a_track_number_is_not_an_artist() {
        assert_eq!(artist_of("06 - Checking In"), None);
        assert_eq!(artist_of("01 - Opening"), None);
    }

    /// The first separator, not the last: this library's soundtrack rips are
    /// `Album - NN - Title`, and the artist is at the front of that.
    #[test]
    fn the_split_takes_the_first_separator() {
        let p = propose_from_title("Celeste Original Soundtrack - 06 - Checking In").unwrap();
        assert_eq!(p.artist, "Celeste Original Soundtrack");
        assert_eq!(p.title, "06 - Checking In");
    }

    /// Not a second set of cleaning rules -- `lyrics::identify` already knows
    /// these, and this asserts that the reuse is real rather than assumed.
    #[test]
    fn the_lyrics_cleaning_rules_apply_to_what_comes_out() {
        let p = propose_from_title("ALESTI - Unravel (Official Music Video)").unwrap();
        assert_eq!(p.artist, "ALESTI");
        assert_eq!(p.title, "Unravel", "bracketed upload noise goes");

        let guest = propose_from_title("ALESTI - Unravel (feat. Siamese)").unwrap();
        assert_eq!(
            guest.title, "Unravel",
            "a guest credit is not part of the song's name"
        );
    }

    /// A folder is a directory somebody made, not an uploader handle. The
    /// lyrics module strips "Music" as channel decoration -- correct for
    /// `Ivycomb Music`, and it silently renamed a folder of 69 tracks from
    /// "Epic Music" to "Epic" until this was pinned.
    #[test]
    fn a_folder_name_is_not_treated_as_a_channel_handle() {
        assert_eq!(propose_from_folder("Epic Music", "Arkana").artist, "Epic Music");
        assert_eq!(propose_from_folder("Silent Partner", "Reverie").artist, "Silent Partner");
        assert_eq!(
            propose_from_folder("  Math   Rock ", "Path").artist,
            "Math Rock",
            "stray spacing still names one folder"
        );
    }

    /// `Aaron_Smith_-_Dancin_KRONO_Remix` is a filename whose spaces became
    /// underscores. `_-_` is what proves it; a lone underscore is not.
    #[test]
    fn an_underscored_filename_is_read_as_the_filename_it_was() {
        let p = propose_from_title("Aaron_Smith_-_Dancin_KRONO_Remix").unwrap();
        assert_eq!(p.artist, "Aaron Smith");
        assert_eq!(p.title, "Dancin KRONO Remix");

        let kept = propose_from_title("AcuticNotes - I_K").unwrap();
        assert_eq!(kept.title, "I_K", "a lone underscore belongs to the title");
    }

    /// A whole folder of this library is `"Title" by Artist`. The ordinary
    /// split gets it exactly backwards.
    #[test]
    fn a_quoted_title_names_itself_before_its_artist() {
        let p = propose_from_title("\"Past And Language\" by Toe").unwrap();
        assert_eq!(p.artist, "Toe");
        assert_eq!(p.title, "Past And Language");

        let decorated = propose_from_title("'SUPERHEROES' by Soul Extract _ Epic Rock").unwrap();
        assert_eq!(
            decorated.artist, "Soul Extract",
            "the channel's own branding is not part of the name"
        );
    }

    /// The guard the case above earns: a title in quotes is naming itself, so
    /// what follows a dash is its composer, not its artist. Splitting
    /// `'The Pirates Rise' - Johannes Bornlof` names the artist after the song.
    #[test]
    fn a_quoted_title_is_never_split_at_a_dash() {
        assert_eq!(artist_of("'The Pirates Rise' - Johannes Bornlof"), None);
        assert_eq!(artist_of("\"Path\" - Toe"), None);
    }

    /// `by` on its own is an ordinary word, and the quotes are what make the
    /// rule safe.
    #[test]
    fn an_unquoted_by_is_left_alone() {
        assert_eq!(propose_from_title("Stand by Me"), None);
    }

    #[test]
    fn an_empty_side_is_not_a_split() {
        assert_eq!(artist_of("- Untitled"), None);
        assert_eq!(artist_of("Artist - "), None);
        assert_eq!(artist_of("A - B"), None, "one character each side");
    }

    /// A long left side means the dash was doing something other than naming
    /// an artist.
    #[test]
    fn a_sentence_is_not_an_artist() {
        let long = "A very long descriptive phrase that runs on well past what any artist is called";
        assert_eq!(artist_of(&format!("{long} - Song")), None);
    }

    /// The folder reading is always available, because for 385 of these
    /// tracks it is the only thing on offer.
    #[test]
    fn a_folder_can_always_be_read_as_the_artist() {
        let p = propose_from_folder("Creo", "Reverie");
        assert_eq!(p.artist, "Creo");
        assert_eq!(p.title, "Reverie");

        // And when the title repeats the folder, the repeat comes off.
        let repeated = propose_from_folder("Creo", "Creo - Reverie");
        assert_eq!(repeated.artist, "Creo");
        assert_eq!(repeated.title, "Reverie");
    }

    #[test]
    fn paths_split_the_same_way_on_either_separator() {
        assert_eq!(parent_of(r"D:\kiza2\Music\Creo\Song.mp3"), Some(r"D:\kiza2\Music\Creo"));
        assert_eq!(parent_of("/home/x/Music/Creo/Song.mp3"), Some("/home/x/Music/Creo"));
        assert_eq!(folder_name(r"D:\kiza2\Music\Creo"), "Creo");
        assert_eq!(folder_name("/home/x/Music/Creo"), "Creo");
        assert_eq!(parent_of("Song.mp3"), None, "nothing to group under");
    }

    // --- against the database ---------------------------------------------

    async fn fixture(name: &str) -> (crate::db::Db, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("music-app-infer-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::init(&dir).await.unwrap();
        (db, dir)
    }

    async fn insert(pool: &SqlitePool, path: &str, title: &str, artist: Option<&str>) -> i64 {
        sqlx::query(
            "INSERT INTO tracks (source, title, artist, local_path, state, in_library) \
             VALUES ('local', ?, ?, ?, 'present', 1)",
        )
        .bind(title)
        .bind(artist)
        .bind(path)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// A track that already has an artist is not a job. Without this the list
    /// would be the whole library forever and the count would never fall.
    #[tokio::test]
    async fn only_tracks_with_no_artist_are_listed() {
        let (db, dir) = fixture("only-untagged").await;
        insert(&db.pool, r"D:\Music\Creo\a.mp3", "Creo - Reverie", None).await;
        insert(&db.pool, r"D:\Music\Creo\b.mp3", "Idle", Some("Creo")).await;
        insert(&db.pool, r"D:\Music\Creo\c.mp3", "Blank", Some("   ")).await;

        let rows = needs_artist(&db.pool).await.unwrap();
        let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, vec!["Creo - Reverie", "Blank"], "'   ' is not an artist");

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `fromTitles` against `total` is what tells someone whether a folder is
    /// answerable from its filenames or needs their judgement.
    #[tokio::test]
    async fn a_folder_reports_how_much_the_titles_can_answer() {
        let (db, dir) = fixture("counts").await;
        insert(&db.pool, r"D:\Music\Artcore\a.mp3", "Agnostic - AiSS", None).await;
        insert(&db.pool, r"D:\Music\Artcore\b.mp3", "AcuticNotes - Julius", None).await;
        insert(&db.pool, r"D:\Music\Artcore\c.mp3", "7thSense", None).await;
        insert(&db.pool, r"D:\Music\Creo\d.mp3", "Reverie", None).await;

        let folders = group_untagged(&db.pool).await.unwrap();
        assert_eq!(folders.len(), 2);

        let artcore = &folders[0];
        assert_eq!(artcore.name, "Artcore", "biggest folder first");
        assert_eq!(artcore.total, 3);
        assert_eq!(artcore.from_titles, 2, "'7thSense' has no separator");

        assert_eq!(folders[1].name, "Creo");
        assert_eq!(folders[1].from_titles, 0);

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Applying is the only thing here that writes, and it must land on the
    /// rows it was given and no others.
    #[tokio::test]
    async fn applying_writes_exactly_what_was_accepted() {
        let (db, dir) = fixture("apply").await;
        let a = insert(&db.pool, r"D:\Music\Creo\a.mp3", "Creo - Reverie", None).await;
        let b = insert(&db.pool, r"D:\Music\Creo\b.mp3", "Creo - Idle", None).await;

        let written = write_details(
            &db.pool,
            &[TrackEdit {
                track_id: a,
                title: "Reverie".into(),
                artist: Some("Creo".into()),
                album: Some("  ".into()),
            }],
        )
        .await
        .unwrap();
        assert_eq!(written, 1);

        let (title, artist, album): (String, Option<String>, Option<String>) =
            sqlx::query_as("SELECT title, artist, album FROM tracks WHERE id = ?")
                .bind(a)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(title, "Reverie");
        assert_eq!(artist.as_deref(), Some("Creo"));
        assert_eq!(album, None, "a blank album is unknown, not an empty name");

        let untouched: Option<String> = sqlx::query_scalar("SELECT artist FROM tracks WHERE id = ?")
            .bind(b)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(untouched, None, "a track nobody accepted is left alone");

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What the inference actually proposes, on the real library.
    ///
    /// The numbers in this module's header came from here. Reading them is the
    /// only way to know whether the guards are set where they should be: too
    /// strict and the folders show nothing to accept, too loose and the review
    /// fills with nonsense nobody wants to read past.
    ///
    /// Read-only, and it writes nothing at all.
    ///
    /// ```text
    /// MUSIC_APP_LIBRARY=... cargo test --lib live_proposals -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "reads the real library"]
    async fn live_proposals() {
        let Ok(path) = std::env::var("MUSIC_APP_LIBRARY") else {
            eprintln!("SKIP: MUSIC_APP_LIBRARY is not set");
            return;
        };

        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{path}?mode=ro"))
            .await
            .expect("library database");

        let folders = group_untagged(&pool).await.unwrap();
        let total: i64 = folders.iter().map(|f| f.total).sum();
        let answered: i64 = folders.iter().map(|f| f.from_titles).sum();

        eprintln!(
            "\n{} folders, {total} tracks with no artist, {answered} answerable from the title \
             ({}%)\n",
            folders.len(),
            answered * 100 / total.max(1)
        );

        for folder in folders.iter().take(8) {
            eprintln!(
                "--- {} ({} tracks, {} from titles)",
                folder.name, folder.total, folder.from_titles
            );
            let rows = proposals_in(&pool, &folder.path).await.unwrap();
            for row in rows.iter().take(4) {
                match &row.from_title {
                    Some(p) => {
                        eprintln!("    {:?}\n      -> {} / {}", row.current_title, p.artist, p.title)
                    }
                    None => eprintln!(
                        "    {:?}\n      -> (folder) {} / {}",
                        row.current_title, row.from_folder.artist, row.from_folder.title
                    ),
                }
            }
        }
    }

    /// The whole folder or none of it. A half-applied rename would leave no
    /// way to tell which half.
    #[tokio::test]
    async fn one_bad_edit_writes_none_of_them() {
        let (db, dir) = fixture("atomic").await;
        let a = insert(&db.pool, r"D:\Music\Creo\a.mp3", "Creo - Reverie", None).await;
        let b = insert(&db.pool, r"D:\Music\Creo\b.mp3", "Creo - Idle", None).await;

        let edits = vec![
            TrackEdit {
                track_id: a,
                title: "Reverie".into(),
                artist: Some("Creo".into()),
                album: None,
            },
            TrackEdit {
                track_id: b,
                title: "   ".into(),
                artist: Some("Creo".into()),
                album: None,
            },
        ];

        let outcome = write_details(&db.pool, &edits).await;
        assert!(outcome.is_err(), "a blank title has to stop the batch");

        let first: Option<String> = sqlx::query_scalar("SELECT artist FROM tracks WHERE id = ?")
            .bind(a)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(
            first, None,
            "the edit before the bad one must have rolled back too"
        );

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

}
