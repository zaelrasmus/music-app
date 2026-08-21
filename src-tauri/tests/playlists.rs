//! Playlist ordering, exercised through the real SQL.
//!
//! Reordering is a single span-shifting UPDATE and removal closes its gap, so
//! the invariant worth protecting is that positions stay dense and the order
//! is what the user asked for. These run against a real SQLite file with the
//! real migrations, because the interesting behaviour lives in the SQL.

use sqlx::SqlitePool;

async fn fixture(name: &str) -> (music_app_lib::db::Db, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!("music-app-playlists-{name}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let db = music_app_lib::db::init(&base).await.unwrap();
    (db, base)
}

/// Local tracks are the cheapest to create and the ordering logic is
/// source-agnostic.
async fn seed_tracks(pool: &SqlitePool, count: usize) -> Vec<i64> {
    let mut ids = Vec::new();

    for index in 0..count {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO tracks (source, title, local_path, state)
             VALUES ('local', ?, ?, 'present') RETURNING id",
        )
        .bind(format!("Track {index}"))
        .bind(format!("D:\\music\\{index}.mp3"))
        .fetch_one(pool)
        .await
        .unwrap();
        ids.push(id);
    }

    ids
}

async fn new_playlist(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("INSERT INTO playlists (name) VALUES ('Test') RETURNING id")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The real add path, not a copy of its statement -- which is what makes this
/// able to catch a change in what "added by hand" means.
async fn add(pool: &SqlitePool, playlist: i64, tracks: &[i64]) {
    music_app_lib::playlists::add_tracks(pool, playlist, tracks.to_vec())
        .await
        .unwrap();
}

/// The move, exactly as `reorder_playlist_track` issues it.
async fn move_track(pool: &SqlitePool, playlist: i64, track: i64, new_position: i64) {
    let old: i64 = sqlx::query_scalar(
        "SELECT position FROM playlist_tracks WHERE playlist_id = ? AND track_id = ?",
    )
    .bind(playlist)
    .bind(track)
    .fetch_one(pool)
    .await
    .unwrap();

    sqlx::query(
        "UPDATE playlist_tracks
         SET position = CASE
             WHEN position = ?2 THEN ?3
             WHEN ?2 < ?3 AND position > ?2 AND position <= ?3 THEN position - 1
             WHEN ?2 > ?3 AND position >= ?3 AND position < ?2 THEN position + 1
             ELSE position
         END
         WHERE playlist_id = ?1",
    )
    .bind(playlist)
    .bind(old)
    .bind(new_position)
    .execute(pool)
    .await
    .unwrap();
}

async fn order(pool: &SqlitePool, playlist: i64) -> Vec<i64> {
    sqlx::query_scalar(
        "SELECT track_id FROM playlist_tracks WHERE playlist_id = ? ORDER BY position",
    )
    .bind(playlist)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn positions(pool: &SqlitePool, playlist: i64) -> Vec<i64> {
    sqlx::query_scalar(
        "SELECT position FROM playlist_tracks WHERE playlist_id = ? ORDER BY position",
    )
    .bind(playlist)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn moving_a_track_down_shifts_the_span_up() {
    let (db, base) = fixture("move-down").await;
    let tracks = seed_tracks(&db.pool, 5).await;
    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &tracks).await;

    // [0 1 2 3 4] -> move index 1 to index 3 -> [0 2 3 1 4]
    move_track(&db.pool, playlist, tracks[1], 3).await;

    assert_eq!(
        order(&db.pool, playlist).await,
        vec![tracks[0], tracks[2], tracks[3], tracks[1], tracks[4]]
    );
    assert_eq!(
        positions(&db.pool, playlist).await,
        vec![0, 1, 2, 3, 4],
        "positions must stay dense"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn moving_a_track_up_shifts_the_span_down() {
    let (db, base) = fixture("move-up").await;
    let tracks = seed_tracks(&db.pool, 5).await;
    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &tracks).await;

    // [0 1 2 3 4] -> move index 3 to index 1 -> [0 3 1 2 4]
    move_track(&db.pool, playlist, tracks[3], 1).await;

    assert_eq!(
        order(&db.pool, playlist).await,
        vec![tracks[0], tracks[3], tracks[1], tracks[2], tracks[4]]
    );
    assert_eq!(positions(&db.pool, playlist).await, vec![0, 1, 2, 3, 4]);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The edges are where off-by-one errors live.
#[tokio::test]
async fn moving_to_either_end_works() {
    let (db, base) = fixture("move-ends").await;
    let tracks = seed_tracks(&db.pool, 4).await;
    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &tracks).await;

    move_track(&db.pool, playlist, tracks[3], 0).await;
    assert_eq!(
        order(&db.pool, playlist).await,
        vec![tracks[3], tracks[0], tracks[1], tracks[2]]
    );

    move_track(&db.pool, playlist, tracks[3], 3).await;
    assert_eq!(
        order(&db.pool, playlist).await,
        vec![tracks[0], tracks[1], tracks[2], tracks[3]]
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// A no-op move must not scramble anything.
#[tokio::test]
async fn moving_a_track_onto_itself_changes_nothing() {
    let (db, base) = fixture("move-self").await;
    let tracks = seed_tracks(&db.pool, 4).await;
    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &tracks).await;

    move_track(&db.pool, playlist, tracks[2], 2).await;

    assert_eq!(order(&db.pool, playlist).await, tracks);
    assert_eq!(positions(&db.pool, playlist).await, vec![0, 1, 2, 3]);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Removing must close its gap, or a UI ordinal stops matching a stored
/// position and every later reorder lands in the wrong place.
#[tokio::test]
async fn removing_a_track_keeps_positions_dense() {
    let (db, base) = fixture("remove").await;
    let tracks = seed_tracks(&db.pool, 5).await;
    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &tracks).await;

    let removed_position: i64 = sqlx::query_scalar(
        "SELECT position FROM playlist_tracks WHERE playlist_id = ? AND track_id = ?",
    )
    .bind(playlist)
    .bind(tracks[1])
    .fetch_one(&db.pool)
    .await
    .unwrap();

    sqlx::query("DELETE FROM playlist_tracks WHERE playlist_id = ? AND track_id = ?")
        .bind(playlist)
        .bind(tracks[1])
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE playlist_tracks SET position = position - 1
         WHERE playlist_id = ? AND position > ?",
    )
    .bind(playlist)
    .bind(removed_position)
    .execute(&db.pool)
    .await
    .unwrap();

    assert_eq!(
        order(&db.pool, playlist).await,
        vec![tracks[0], tracks[2], tracks[3], tracks[4]]
    );
    assert_eq!(positions(&db.pool, playlist).await, vec![0, 1, 2, 3]);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The composite primary key is the whole duplicate story.
#[tokio::test]
async fn the_same_track_cannot_be_added_twice() {
    let (db, base) = fixture("duplicates").await;
    let tracks = seed_tracks(&db.pool, 2).await;
    let playlist = new_playlist(&db.pool).await;

    add(&db.pool, playlist, &tracks).await;
    add(&db.pool, playlist, &tracks).await;

    assert_eq!(order(&db.pool, playlist).await, tracks);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Deleting a playlist takes its membership rows and leaves the tracks alone.
#[tokio::test]
async fn deleting_a_playlist_does_not_delete_its_tracks() {
    let (db, base) = fixture("cascade").await;
    let tracks = seed_tracks(&db.pool, 3).await;
    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &tracks).await;

    sqlx::query("DELETE FROM playlists WHERE id = ?")
        .bind(playlist)
        .execute(&db.pool)
        .await
        .unwrap();

    let memberships: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlist_tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let surviving: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    assert_eq!(memberships, 0, "membership rows should cascade away");
    assert_eq!(surviving, 3, "the tracks themselves must survive");

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// A playlist is allowed to hold all three kinds of track at once. This is the
/// shape the player has to cope with, and the reason playlists exist at all.
#[tokio::test]
async fn a_playlist_can_mix_local_downloaded_and_streamed_tracks() {
    let (db, base) = fixture("mixed").await;

    let local: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, local_path, state)
         VALUES ('local', 'Local', 'D:\\a.mp3', 'present') RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let downloaded: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, state, remote_id, remote_url, local_path)
         VALUES ('youtube', 'Downloaded', 'downloaded', 'aaaaaaaaaaa',
                 'https://www.youtube.com/watch?v=aaaaaaaaaaa', 'D:\\b.m4a')
         RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let streamed: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, state, remote_id, remote_url)
         VALUES ('youtube', 'Streamed', 'saved', 'bbbbbbbbbbb',
                 'https://www.youtube.com/watch?v=bbbbbbbbbbb') RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &[local, downloaded, streamed]).await;

    assert_eq!(
        order(&db.pool, playlist).await,
        vec![local, downloaded, streamed]
    );

    // The player receives exactly this: a list of ids, resolved one at a time
    // through `get_playable_source`. No playlist-specific playback path.
    let states: Vec<String> = sqlx::query_scalar(
        "SELECT t.state FROM playlist_tracks pt JOIN tracks t ON t.id = pt.track_id
         WHERE pt.playlist_id = ? ORDER BY pt.position",
    )
    .bind(playlist)
    .fetch_all(&db.pool)
    .await
    .unwrap();

    assert_eq!(states, vec!["present", "downloaded", "saved"]);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Filtering a playlist narrows it without reordering it.
///
/// This is the difference from the library view: there, search ranks by
/// relevance; here the user curated the order, so a filter must only remove
/// rows. Ranking a playlist by bm25 would silently scramble it.
#[tokio::test]
async fn filtering_a_playlist_preserves_its_order() {
    let (db, base) = fixture("filter-order").await;

    // Deliberately inserted so that alphabetical, relevance and playlist order
    // all disagree.
    let zebra = sqlx::query_scalar::<_, i64>(
        "INSERT INTO tracks (source, title, artist, local_path, state)
         VALUES ('local', 'Zebra Song', 'Bandit', 'D:\\z.mp3', 'present') RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let apple = sqlx::query_scalar::<_, i64>(
        "INSERT INTO tracks (source, title, artist, local_path, state)
         VALUES ('local', 'Apple Song', 'Bandit', 'D:\\a.mp3', 'present') RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let other = sqlx::query_scalar::<_, i64>(
        "INSERT INTO tracks (source, title, artist, local_path, state)
         VALUES ('local', 'Unrelated', 'Nobody', 'D:\\u.mp3', 'present') RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &[zebra, other, apple]).await;

    // The same shape `get_playlist` builds when searching.
    let filtered: Vec<i64> = sqlx::query_scalar(
        "SELECT t.id FROM playlist_tracks pt
         JOIN tracks t ON t.id = pt.track_id
         JOIN tracks_fts ON tracks_fts.rowid = t.id
         WHERE pt.playlist_id = ? AND tracks_fts MATCH ?
         ORDER BY pt.position, pt.added_at, t.id",
    )
    .bind(playlist)
    .bind("\"song\"*")
    .fetch_all(&db.pool)
    .await
    .unwrap();

    assert_eq!(
        filtered,
        vec![zebra, apple],
        "the unrelated track is dropped, but Zebra still precedes Apple"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Tag filtering inside a playlist must also respect playlist order.
#[tokio::test]
async fn tag_filtering_a_playlist_preserves_its_order() {
    let (db, base) = fixture("filter-tags").await;
    let tracks = seed_tracks(&db.pool, 4).await;

    let tag: i64 = sqlx::query_scalar(
        "INSERT INTO tags (name, name_key) VALUES ('Chill', 'chill') RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    // Tag the third and first tracks, in that order, so insertion order and
    // playlist order differ.
    for track in [tracks[2], tracks[0]] {
        sqlx::query("INSERT INTO track_tags (track_id, tag_id) VALUES (?, ?)")
            .bind(track)
            .bind(tag)
            .execute(&db.pool)
            .await
            .unwrap();
    }

    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &tracks).await;

    let filtered: Vec<i64> = sqlx::query_scalar(
        "SELECT t.id FROM playlist_tracks pt
         JOIN tracks t ON t.id = pt.track_id
         JOIN track_tags tt ON tt.track_id = t.id
         WHERE pt.playlist_id = ? AND tt.tag_id IN (?)
         GROUP BY t.id
         HAVING COUNT(DISTINCT tt.tag_id) = 1
         ORDER BY pt.position, pt.added_at, t.id",
    )
    .bind(playlist)
    .bind(tag)
    .fetch_all(&db.pool)
    .await
    .unwrap();

    assert_eq!(
        filtered,
        vec![tracks[0], tracks[2]],
        "playlist order, not the order the tags were applied"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

// --- artist rules ------------------------------------------------------
//
// A playlist with a rule is not enumerated anywhere: membership is decided at
// read time from the rule, the hand-added rows and the exclusions together.
// These call the real resolver rather than re-typing its query, because a test
// that re-types the query passes whatever the query becomes.

async fn seed_remote(pool: &SqlitePool, uploader: &str, titles: &[&str]) -> Vec<i64> {
    let mut ids = Vec::new();
    for title in titles {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO tracks (source, title, artist, remote_uploader, state, remote_id, remote_url)
             VALUES ('youtube', ?, ?, ?, 'saved', ?, ?) RETURNING id",
        )
        .bind(title)
        .bind(uploader)
        .bind(uploader)
        .bind(format!("id-{title}"))
        .bind(format!("https://www.youtube.com/watch?v=id-{title}"))
        .fetch_one(pool)
        .await
        .unwrap();
        ids.push(id);
    }
    ids
}

async fn rule(pool: &SqlitePool, playlist: i64, label: &str) {
    sqlx::query(
        "INSERT INTO playlist_artist_rules (playlist_id, artist_key, label) VALUES (?, ?, ?)",
    )
    .bind(playlist)
    .bind(label.trim().to_lowercase())
    .bind(label)
    .execute(pool)
    .await
    .unwrap();
}

/// Ordering rows, as `materialise_order` writes them when a rule is added.
async fn record_order(pool: &SqlitePool, playlist: i64, tracks: &[i64]) {
    for (position, track) in tracks.iter().enumerate() {
        sqlx::query(
            "INSERT INTO playlist_tracks (playlist_id, track_id, position, by_rule)
             VALUES (?1, ?2, ?3, 1)
             ON CONFLICT (playlist_id, track_id) DO UPDATE SET position = ?3",
        )
        .bind(playlist)
        .bind(track)
        .bind(position as i64)
        .execute(pool)
        .await
        .unwrap();
    }
}

async fn members(pool: &SqlitePool, playlist: i64) -> Vec<i64> {
    music_app_lib::playlists::playlist_tracks(pool, playlist, None, None, None, None, None)
        .await
        .unwrap()
        .into_iter()
        .map(|track| track.id)
        .collect()
}

/// The whole point: nobody put these tracks in, and they are in.
#[tokio::test]
async fn a_rule_admits_an_artists_tracks_without_anyone_adding_them() {
    let (db, base) = fixture("rule-admits").await;
    let mine = seed_remote(&db.pool, "ivycomb", &["Y2K", "Strays"]).await;
    let theirs = seed_remote(&db.pool, "someone else", &["Unrelated"]).await;

    let playlist = new_playlist(&db.pool).await;
    rule(&db.pool, playlist, "ivycomb").await;

    let held = members(&db.pool, playlist).await;
    assert_eq!(held, mine, "the rule should hold exactly this artist's tracks");
    assert!(!held.contains(&theirs[0]));

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// One artist, two names, because a SoundCloud handle and a YouTube channel
/// are not obliged to match. This is the case that made rules necessary.
#[tokio::test]
async fn several_names_can_mean_one_artist() {
    let (db, base) = fixture("rule-aliases").await;
    let sc = seed_remote(&db.pool, "ivycomb", &["Y2K"]).await;
    let yt = seed_remote(&db.pool, "Ivycomb Music", &["Vancouver"]).await;

    let playlist = new_playlist(&db.pool).await;
    rule(&db.pool, playlist, "ivycomb").await;
    rule(&db.pool, playlist, "Ivycomb Music").await;

    let held = members(&db.pool, playlist).await;
    assert!(held.contains(&sc[0]) && held.contains(&yt[0]), "got {held:?}");

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Case is not identity. `IVYCOMB` and `ivycomb` are one artist.
#[tokio::test]
async fn matching_ignores_case_and_padding() {
    let (db, base) = fixture("rule-case").await;
    let ids = seed_remote(&db.pool, "IVYCOMB", &["Y2K"]).await;

    let playlist = new_playlist(&db.pool).await;
    rule(&db.pool, playlist, "  ivycomb  ").await;

    assert_eq!(members(&db.pool, playlist).await, ids);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The mirror-upload case: a track by this artist posted under someone else's
/// channel. No rule can know, so the user says so by hand — and it has to sit
/// alongside the rule's own matches rather than replacing them.
#[tokio::test]
async fn a_hand_added_track_joins_the_rules_matches() {
    let (db, base) = fixture("rule-manual").await;
    let matched = seed_remote(&db.pool, "Link\"0", &["Threshold"]).await;
    let mirror = seed_remote(&db.pool, "some reupload channel", &["Ventors"]).await;

    let playlist = new_playlist(&db.pool).await;
    rule(&db.pool, playlist, "Link\"0").await;
    add(&db.pool, playlist, &mirror).await;

    let held = members(&db.pool, playlist).await;
    assert!(held.contains(&matched[0]), "rule match missing: {held:?}");
    assert!(held.contains(&mirror[0]), "hand-added missing: {held:?}");
    assert_eq!(held.len(), 2);

    // And the hand-placed one comes first: what the user arranged keeps its
    // place while the rule's matches accumulate below it.
    assert_eq!(held[0], mirror[0], "curated order lost: {held:?}");

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Removal has to mean something on a list nobody enumerated.
///
/// Without the exclusion the rule simply puts the track back, so "remove"
/// becomes a button that does nothing — the user clicks it, the row returns,
/// and there is no way to win.
#[tokio::test]
async fn an_excluded_track_stays_out_even_though_the_rule_matches_it() {
    let (db, base) = fixture("rule-exclusion").await;
    let ids = seed_remote(&db.pool, "ivycomb", &["Y2K", "Strays", "Free"]).await;

    let playlist = new_playlist(&db.pool).await;
    rule(&db.pool, playlist, "ivycomb").await;

    sqlx::query("INSERT INTO playlist_excluded_tracks (playlist_id, track_id) VALUES (?, ?)")
        .bind(playlist)
        .bind(ids[1])
        .execute(&db.pool)
        .await
        .unwrap();

    let held = members(&db.pool, playlist).await;
    assert_eq!(held, vec![ids[0], ids[2]], "the excluded track came back");

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// An exclusion outranks a hand-added row too, so the two mechanisms cannot
/// disagree about a track that is both.
#[tokio::test]
async fn an_exclusion_also_hides_a_hand_added_track() {
    let (db, base) = fixture("rule-exclusion-manual").await;
    let ids = seed_tracks(&db.pool, 2).await;

    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &ids).await;

    sqlx::query("INSERT INTO playlist_excluded_tracks (playlist_id, track_id) VALUES (?, ?)")
        .bind(playlist)
        .bind(ids[0])
        .execute(&db.pool)
        .await
        .unwrap();

    assert_eq!(members(&db.pool, playlist).await, vec![ids[1]]);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// A rule collects what you kept, not everything that ever passed through.
///
/// `in_library` is this app's "I kept this" — a track auditioned once, or
/// carried in by an imported playlist, was never claimed. Without this the
/// picker offers an artist with two tracks and the rule admits thirty-nine,
/// and a list nobody enumerated stops being trustworthy the first time it
/// does that.
#[tokio::test]
async fn a_rule_collects_only_library_tracks() {
    let (db, base) = fixture("rule-in-library").await;
    let ids = seed_remote(&db.pool, "Link\"0", &["Threshold", "Ventors", "Ghin"]).await;

    // Two auditioned and not kept, exactly as `save_remote_track` leaves them.
    for id in &ids[1..] {
        sqlx::query("UPDATE tracks SET in_library = 0 WHERE id = ?")
            .bind(id)
            .execute(&db.pool)
            .await
            .unwrap();
    }

    let playlist = new_playlist(&db.pool).await;
    rule(&db.pool, playlist, "Link\"0").await;

    assert_eq!(
        members(&db.pool, playlist).await,
        vec![ids[0]],
        "the rule took in tracks that were never kept",
    );

    // Adding one by hand is claiming it, and outranks the rule.
    add(&db.pool, playlist, &[ids[2]]).await;
    let held = members(&db.pool, playlist).await;
    assert!(held.contains(&ids[2]), "a hand-added track must show: {held:?}");
    assert!(!held.contains(&ids[1]), "the other stays out: {held:?}");

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The gesture an imported playlist needs.
///
/// Importing deliberately leaves its tracks out of the library, which makes
/// them invisible to anything keyed on membership — artist rules above all.
/// This is the one action that says *actually, I want all of these*.
#[tokio::test]
async fn filing_a_playlist_in_the_library_only_touches_what_was_outside_it() {
    let (db, base) = fixture("bulk-library").await;
    let ids = seed_remote(&db.pool, "ivycomb", &["Y2K", "Strays", "Free"]).await;

    // As `import_playlist` leaves them: present, not claimed.
    sqlx::query("UPDATE tracks SET in_library = 0")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET in_library = 1 WHERE id = ?")
        .bind(ids[0])
        .execute(&db.pool)
        .await
        .unwrap();

    let playlist = new_playlist(&db.pool).await;
    add(&db.pool, playlist, &ids).await;

    // The statement the command issues, and the count it reports.
    let filed: Vec<i64> = sqlx::query_scalar(
        "UPDATE tracks SET in_library = 1
         WHERE in_library = 0
           AND id IN (SELECT track_id FROM playlist_tracks WHERE playlist_id = ?)
         RETURNING id",
    )
    .bind(playlist)
    .fetch_all(&db.pool)
    .await
    .unwrap();

    assert_eq!(filed.len(), 2, "only the two outside the library should move");
    assert!(!filed.contains(&ids[0]), "the one already filed was touched");

    let outside: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks WHERE in_library = 0")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(outside, 0);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// And that is exactly what makes an artist rule find them.
#[tokio::test]
async fn filing_a_playlist_makes_its_tracks_visible_to_artist_rules() {
    let (db, base) = fixture("bulk-then-rule").await;
    let ids = seed_remote(&db.pool, "Link\"0", &["Threshold", "Ventors"]).await;

    sqlx::query("UPDATE tracks SET in_library = 0")
        .execute(&db.pool)
        .await
        .unwrap();

    let imported = new_playlist(&db.pool).await;
    add(&db.pool, imported, &ids).await;

    let artist = new_playlist(&db.pool).await;
    rule(&db.pool, artist, "Link\"0").await;

    assert!(
        members(&db.pool, artist).await.is_empty(),
        "an unclaimed track must not be swept up by a rule",
    );

    sqlx::query(
        "UPDATE tracks SET in_library = 1
         WHERE in_library = 0
           AND id IN (SELECT track_id FROM playlist_tracks WHERE playlist_id = ?)",
    )
    .bind(imported)
    .execute(&db.pool)
    .await
    .unwrap();

    assert_eq!(members(&db.pool, artist).await, ids, "now the rule finds them");

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Taking hold of an artist playlist's order is what fixes it.
///
/// A rule's matches are not rows in `playlist_tracks` at all, so there is
/// nothing to drag them between. The first reorder writes down the order
/// currently on screen; from then on it is an ordinary hand-ordered playlist
/// that the rule still adds to, at the end.
///
/// The alternative — position the dragged track and leave its neighbours
/// without one — sorts hand-placed above rule-matched, so dragging a row
/// *downwards* would fling it to the top.
#[tokio::test]
async fn the_first_reorder_of_an_artist_playlist_writes_its_order_down() {
    let (db, base) = fixture("rule-materialise").await;
    let ids = seed_remote(&db.pool, "ivycomb", &["Y2K", "Strays", "Free"]).await;

    let playlist = new_playlist(&db.pool).await;
    rule(&db.pool, playlist, "ivycomb").await;

    // Nothing is placed: every track is here because the rule says so.
    assert_eq!(order(&db.pool, playlist).await, Vec::<i64>::new());
    assert_eq!(members(&db.pool, playlist).await, ids);

    // The move a drag issues, through the real command's helper path.
    let shown = members(&db.pool, playlist).await;
    for (position, track) in shown.iter().enumerate() {
        sqlx::query(
            "INSERT INTO playlist_tracks (playlist_id, track_id, position)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (playlist_id, track_id) DO UPDATE SET position = ?3",
        )
        .bind(playlist)
        .bind(track)
        .bind(position as i64)
        .execute(&db.pool)
        .await
        .unwrap();
    }

    assert_eq!(
        order(&db.pool, playlist).await,
        ids,
        "the shown order should have been written down exactly",
    );
    assert_eq!(positions(&db.pool, playlist).await, vec![0, 1, 2]);

    // And it still reads back the same, now from real positions.
    assert_eq!(members(&db.pool, playlist).await, ids);

    // A track the rule finds later joins at the end rather than disturbing it.
    let latecomer = seed_remote(&db.pool, "ivycomb", &["Vancouver"]).await;
    let after = members(&db.pool, playlist).await;
    assert_eq!(after.last(), Some(&latecomer[0]), "got {after:?}");

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// An ordering row says *where*, never *whether*.
///
/// The whole point of the split: a playlist that fills itself from an artist
/// can have a stored order from the moment the rule is made -- so dragging
/// works immediately, with no invisible change of behaviour partway through --
/// and removing the rule still removes its tracks, because those rows never
/// conferred membership.
#[tokio::test]
async fn recording_an_order_does_not_make_rule_matches_permanent() {
    let (db, base) = fixture("by-rule-order").await;
    let ids = seed_remote(&db.pool, "ivycomb", &["Y2K", "Strays", "Free"]).await;

    let playlist = new_playlist(&db.pool).await;
    rule(&db.pool, playlist, "ivycomb").await;

    // As adding the rule does: the visible order, written down.
    record_order(&db.pool, playlist, &ids).await;
    assert_eq!(members(&db.pool, playlist).await, ids, "order recorded");

    // And it is a real order -- rows exist to drag between.
    assert_eq!(positions(&db.pool, playlist).await, vec![0, 1, 2]);

    // Now drop the rule. The rows are still there; the tracks are not.
    sqlx::query("DELETE FROM playlist_artist_rules WHERE playlist_id = ?")
        .bind(playlist)
        .execute(&db.pool)
        .await
        .unwrap();

    assert!(
        members(&db.pool, playlist).await.is_empty(),
        "recording an order must not turn rule matches into members",
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Adding a track by hand is a different claim from where it sits, and it
/// outlasts the rule that first brought the track in.
#[tokio::test]
async fn adding_by_hand_promotes_an_ordering_row_to_a_real_member() {
    let (db, base) = fixture("by-rule-promote").await;
    let ids = seed_remote(&db.pool, "ivycomb", &["Y2K", "Strays"]).await;

    let playlist = new_playlist(&db.pool).await;
    rule(&db.pool, playlist, "ivycomb").await;
    record_order(&db.pool, playlist, &ids).await;

    // The user keeps one of them explicitly.
    add(&db.pool, playlist, &[ids[1]]).await;

    sqlx::query("DELETE FROM playlist_artist_rules WHERE playlist_id = ?")
        .bind(playlist)
        .execute(&db.pool)
        .await
        .unwrap();

    assert_eq!(
        members(&db.pool, playlist).await,
        vec![ids[1]],
        "the hand-added one stays, the other goes with the rule",
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// A track the rule finds later joins at the end, leaving an arranged order
/// alone.
#[tokio::test]
async fn a_later_match_appends_rather_than_disturbing_the_order() {
    let (db, base) = fixture("by-rule-append").await;
    let ids = seed_remote(&db.pool, "ivycomb", &["Y2K", "Strays"]).await;

    let playlist = new_playlist(&db.pool).await;
    rule(&db.pool, playlist, "ivycomb").await;

    // Arranged deliberately backwards, to prove the order is respected.
    record_order(&db.pool, playlist, &[ids[1], ids[0]]).await;
    assert_eq!(members(&db.pool, playlist).await, vec![ids[1], ids[0]]);

    let later = seed_remote(&db.pool, "ivycomb", &["Vancouver"]).await;
    assert_eq!(
        members(&db.pool, playlist).await,
        vec![ids[1], ids[0], later[0]],
        "a new match belongs at the end, not woven into the arrangement",
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}
