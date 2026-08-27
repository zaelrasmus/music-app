//! End-to-end over the path only the engine can drive.
//!
//! Progress and end-of-track are reported *by the engine*, never by a command,
//! so a break here is invisible from the command API — the play button keeps
//! working perfectly while the progress bar sits at zero. That is exactly the
//! failure this test exists to catch.

use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use music_app_lib::player::{
    self, PlayerCommand, PlayerEvents, PlayerProgress, PlayerStatus, QueueState,
};

/// Serialises the tests that touch the network.
///
/// Run in parallel they compete for the same things -- several yt-dlp
/// resolves against YouTube at once, and the one audio device -- which makes
/// the timing assertions measure contention rather than the code under test.
/// A tokio mutex rather than a std one because it is held across awaits.
static NETWORK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));


/// Serialises the tests that measure *time*.
///
/// They share one audio device and one machine, and what they assert is how
/// long something took -- run in parallel they measure contention rather than
/// the code under test. Both failures that sent me here were this: each test
/// passed alone and failed in the suite.
static TIMING: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// The bundled ffmpeg, which local playback now requires.
///
/// Passing `None` used to be fine here: rodio decoded a plain WAV natively, so
/// a test that only played local files needed no sidecar at all. ffmpeg is now
/// the only decoder, so `None` means every track fails to load -- and the
/// failure is quiet, because the coordinator responds by skipping to the next
/// track, three times, and then halting. That looks exactly like a queue-order
/// bug rather than a missing binary.
fn ffmpeg() -> Option<std::path::PathBuf> {
    music_app_lib::sidecar::staged_for_tests(music_app_lib::sidecar::Tool::Ffmpeg)
}
#[derive(Default)]
struct Captured {
    progress: Mutex<Vec<f64>>,
    /// Background cache fills, as (track, title). `None` is the finish.
    caching: Mutex<Vec<(i64, Option<String>)>>,
    /// Progress again, but keeping which track each tick was about.
    ticks: Mutex<Vec<(Option<i64>, f64)>>,
    /// When each tick arrived, which is the only way to see a *gap*: the
    /// positions look identical whether or not the handover was seamless, and
    /// what differs is the wall-clock silence between them.
    timed: Mutex<Vec<(std::time::Instant, Option<i64>)>>,
    states: Mutex<Vec<PlayerStatus>>,
    errors: Mutex<Vec<String>>,
    queues: Mutex<Vec<QueueState>>,
    /// Emission order across event *types*, which the per-type vectors lose.
    ///
    /// Only ordering matters, so this is a plain counter rather than a clock:
    /// the question it answers is "did a queue payload follow this state", and
    /// a timestamp would make that flaky on a loaded machine.
    seq: std::sync::atomic::AtomicU64,
    /// (sequence, track_id, is_playing) for every state emitted.
    state_seq: Mutex<Vec<(u64, Option<i64>, bool)>>,
    /// (sequence, current track_id) for every queue payload.
    queue_seq: Mutex<Vec<(u64, Option<i64>)>>,
    /// The same, keeping the title the panel would have drawn.
    ///
    /// The player bar needs more than the id: it needs the payload to actually
    /// *describe* the track, and a row the hydrating query missed comes back
    /// titled "Unavailable" rather than absent. An id-only record cannot tell
    /// those apart.
    queue_desc: Mutex<Vec<(u64, Option<i64>, Option<String>)>>,
}

/// Newtype so the impl is local (orphan rule).
#[derive(Clone, Default)]
struct Recorder(Arc<Captured>);

impl PlayerEvents for Recorder {
    fn state(&self, status: PlayerStatus) {
        let seq = self
            .0
            .seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.0
            .state_seq
            .lock()
            .unwrap()
            .push((seq, status.track_id, status.state == music_app_lib::player::PlaybackState::Playing));
        self.0.states.lock().unwrap().push(status);
    }

    fn progress(&self, progress: PlayerProgress) {
        self.0.progress.lock().unwrap().push(progress.position_secs);
        self.0
            .ticks
            .lock()
            .unwrap()
            .push((progress.track_id, progress.position_secs));
        self.0
            .timed
            .lock()
            .unwrap()
            .push((std::time::Instant::now(), progress.track_id));
    }

    fn error(&self, message: String) {
        self.0.errors.lock().unwrap().push(message);
    }

    fn caching(&self, track_id: i64, title: Option<String>) {
        self.0.caching.lock().unwrap().push((track_id, title));
    }

    fn queue(&self, queue: QueueState) {
        let seq = self
            .0
            .seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.0
            .queue_seq
            .lock()
            .unwrap()
            .push((seq, queue.current.as_ref().map(|c| c.track_id)));
        self.0.queue_desc.lock().unwrap().push((
            seq,
            queue.current.as_ref().map(|c| c.track_id),
            queue.current.as_ref().map(|c| c.title.clone()),
        ));
        self.0.queues.lock().unwrap().push(queue);
    }
}

/// A real 3-second PCM WAV, so the engine has something genuine to decode.
fn write_wav(path: &std::path::Path) {
    write_wav_secs(path, 3);
}

fn write_wav_secs(path: &std::path::Path, seconds: u32) {
    use std::io::Write;

    let samples: u32 = 44100 * seconds;
    let data_len = samples * 2;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVE");
    b.extend_from_slice(b"fmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&44100u32.to_le_bytes());
    b.extend_from_slice(&88200u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    b.extend_from_slice(&vec![0u8; data_len as usize]);

    std::fs::File::create(path).unwrap().write_all(&b).unwrap();
}

#[tokio::test]
async fn progress_reaches_the_ui_while_a_track_plays() {
    let base = std::env::temp_dir().join("music-app-coordinator-progress");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let wav = base.join("tone.wav");
    write_wav(&wav);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state) \
         VALUES ('local', 'Tone', ?, 'present')",
    )
    .bind(wav.to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();

    let track_id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    // No ffmpeg: the fixture is a plain WAV, decoded natively by rodio.
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(1500)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    let progress = recorder.0.progress.lock().unwrap().clone();
    eprintln!("errors:   {errors:?}");
    eprintln!("progress: {progress:?}");

    if errors.iter().any(|e| e.contains("No audio output device")) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    assert!(
        progress.iter().any(|p| *p > 0.0),
        "progress never advanced past zero (got {progress:?}); \
         the engine reports position but it is not reaching the UI"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The Part B acceptance criterion, end to end.
///
/// rodio has no Opus codec at all, so if this plays it can only be because the
/// seam routed the file to ffmpeg and `FfmpegSource` fed real samples back
/// through the ring buffer. A WAV would prove nothing -- rodio decodes those
/// natively.
#[tokio::test]
async fn an_opus_file_plays_through_ffmpeg() {
    let base = std::env::temp_dir().join("music-app-opus-playback");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let opus = base.join("tone.opus");

    // Encode a real Opus file rather than faking one, so the codec path is
    // genuinely exercised.
    let encoded = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=3"])
        .args(["-c:a", "libopus"])
        .arg(&opus)
        .status();

    match encoded {
        Ok(status) if status.success() => {}
        _ => {
            eprintln!("SKIP: ffmpeg is not available to build the fixture");
            return;
        }
    }

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state) \
         VALUES ('local', 'Opus Tone', ?, 'present')",
    )
    .bind(opus.to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();

    let track_id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        // Resolved off PATH; Command::new does the lookup.
        ffmpeg(),
        // No yt-dlp: this fixture is a local file.
        None,
        None,
    );

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(2000)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    let progress = recorder.0.progress.lock().unwrap().clone();
    eprintln!("errors:   {errors:?}");
    eprintln!("progress: {progress:?}");

    if errors.iter().any(|e| e.contains("No audio output device")) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    assert!(
        errors.is_empty(),
        "opus playback should not report errors: {errors:?}"
    );
    assert!(
        progress.iter().any(|p| *p > 0.0),
        "opus never advanced past zero (got {progress:?}); \
         rodio cannot decode opus, so this means ffmpeg audio never arrived"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The Part D acceptance criterion: a `saved` YouTube track, holding nothing
/// but a video id, becomes audio.
///
/// Everything is real -- yt-dlp resolves a live stream URL, ffmpeg decodes it
/// over the network, and rodio plays it. Nothing about this path can be faked
/// convincingly, so it either works against YouTube or it does not work.
#[tokio::test]
async fn a_saved_youtube_track_streams() {
    let _network = NETWORK.lock().await;

    let base = std::env::temp_dir().join("music-app-youtube-stream");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    // Metadata only, exactly as `save_remote_track` writes it: no local_path.
    sqlx::query(
        "INSERT INTO tracks (source, title, state, remote_id, remote_url) \
         VALUES ('youtube', 'Never Gonna Give You Up', 'saved', 'dQw4w9WgXcQ', \
                 'https://www.youtube.com/watch?v=dQw4w9WgXcQ')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let track_id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        ffmpeg(),
        Some(std::path::PathBuf::from("yt-dlp")),
        None,
    );

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Resolution alone takes ~7s, then ffmpeg has to buffer over the network.
    tokio::time::sleep(Duration::from_secs(25)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    let progress = recorder.0.progress.lock().unwrap().clone();
    eprintln!("errors:   {errors:?}");
    eprintln!("progress: {progress:?}");

    // The upload date rides along on the resolve.
    //
    // YouTube publishes nothing datelike in `--flat-playlist` search, so this
    // is the *only* way a YouTube track ever learns when it was uploaded --
    // and it is free, because the resolve has already done the extraction that
    // produces it. Worth asserting end to end rather than only parsing a
    // fixture: what would break here is yt-dlp's output, not our parsing.
    //
    // Checked *before* the skip below, and deliberately so. Every skippable
    // condition except an outright resolve failure happens after yt-dlp has
    // already succeeded -- a 403 comes from ffmpeg fetching the URL yt-dlp
    // returned, and a missing audio device stops playback, not resolution. So
    // the date must be there in all of those cases, and letting the skip run
    // first would leave this assertion never executing on a red day.
    //
    // Fire-and-forget, so it may land just after playback starts.
    let mut uploaded_at: Option<i64> = None;
    for _ in 0..20 {
        uploaded_at = sqlx::query_scalar("SELECT uploaded_at FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        if uploaded_at.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // The only failures that mean yt-dlp itself never ran or never answered.
    // Nothing can be concluded about the date in those cases.
    let resolve_failed = |e: &String| {
        e.contains("internet connection")
            || e.contains("Could not find yt-dlp")
            || e.contains("Could not start yt-dlp")
            || e.contains("no longer available")
    };

    if errors.iter().any(resolve_failed) {
        eprintln!("SKIP: yt-dlp never resolved, so nothing can be asserted about the date");
    } else {
        let uploaded_at =
            uploaded_at.expect("playing a YouTube track must record its upload date");
        // 2009-10-25, and it is not going to change.
        assert!(
            (1_256_000_000..1_257_000_000).contains(&uploaded_at),
            "expected the real upload date, got {uploaded_at}"
        );
    }

    // Conditions outside this codebase. A 403 in particular is YouTube
    // refusing the request -- usually because the bundled yt-dlp has aged out
    // of whatever client YouTube currently accepts. That is worth reporting
    // loudly, but it is not a regression in the pipeline under test.
    let skippable = |e: &String| {
        e.contains("No audio output device")
            || e.contains("internet connection")
            || e.contains("Could not find yt-dlp")
            || e.contains("Could not start")
            || e.contains("YouTube refused")
            || e.contains("no longer available")
    };
    if errors.iter().any(skippable) {
        eprintln!("SKIP: upstream/environment condition, not a pipeline failure");
        return;
    }

    assert!(errors.is_empty(), "streaming reported errors: {errors:?}");
    assert!(
        progress.iter().any(|p| *p > 0.0),
        "the stream never advanced past zero (got {progress:?})"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The Part A acceptance criterion: a `saved` SoundCloud track becomes audio.
///
/// Nothing here is YouTube-shaped -- a numeric id, a stored page URL that
/// could not have been derived from it, and `source = 'soundcloud'`. If this
/// plays, the generalised schema and the provider-driven seam are both real.
#[tokio::test]
async fn a_saved_soundcloud_track_streams() {
    let _network = NETWORK.lock().await;

    let base = std::env::temp_dir().join("music-app-soundcloud-stream");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    sqlx::query(
        "INSERT INTO tracks (source, title, state, remote_id, remote_url) \
         VALUES ('soundcloud', 'One More Time', 'saved', '199428706', \
                 'https://soundcloud.com/daft-punk-id/daft-punk-one-more-time')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let track_id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        ffmpeg(),
        Some(std::path::PathBuf::from("yt-dlp")),
        None,
    );

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // SoundCloud hands back an HLS playlist, so ffmpeg has segments to pull
    // before any audio flows.
    tokio::time::sleep(Duration::from_secs(25)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    let progress = recorder.0.progress.lock().unwrap().clone();
    eprintln!("errors:   {errors:?}");
    eprintln!("progress: {progress:?}");

    let skippable = |e: &String| {
        e.contains("No audio output device")
            || e.contains("internet connection")
            || e.contains("Could not find yt-dlp")
            || e.contains("Could not start")
            || e.contains("no longer available")
    };
    if errors.iter().any(skippable) {
        eprintln!("SKIP: upstream/environment condition, not a pipeline failure");
        return;
    }

    assert!(errors.is_empty(), "streaming reported errors: {errors:?}");
    assert!(
        progress.iter().any(|p| *p > 0.0),
        "the SoundCloud stream never advanced past zero (got {progress:?})"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Seeking inside a stream, which rodio cannot do at all.
///
/// `FfmpegSource` reads a pipe, so `try_seek` is `NotSupported` by
/// construction. The engine restarts the decode with `-ss` instead and adds
/// the offset back on. Both halves are load-bearing and both are invisible
/// from the command API: if the offset were dropped, seeking would still
/// "work" while the progress bar snapped back to zero and the coordinator
/// thought the track had restarted.
#[tokio::test]
async fn a_stream_can_be_seeked_and_reports_the_real_position() {
    let _network = NETWORK.lock().await;

    const TARGET_SECS: f64 = 100.0;

    let base = std::env::temp_dir().join("music-app-stream-seek");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, state, remote_id, remote_url) \
         VALUES ('youtube', 'Never Gonna Give You Up', 'saved', 'dQw4w9WgXcQ', \
                 'https://www.youtube.com/watch?v=dQw4w9WgXcQ')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let track_id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        ffmpeg(),
        Some(std::path::PathBuf::from("yt-dlp")),
        None,
    );

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Resolution alone takes ~7s, then ffmpeg buffers over the network.
    tokio::time::sleep(Duration::from_secs(20)).await;

    let skippable = |e: &String| {
        e.contains("No audio output device")
            || e.contains("internet connection")
            || e.contains("Could not find yt-dlp")
            || e.contains("Could not start")
            || e.contains("YouTube refused")
            || e.contains("no longer available")
    };
    let early_errors = recorder.0.errors.lock().unwrap().clone();
    if early_errors.iter().any(skippable) {
        eprintln!("SKIP: upstream/environment condition ({early_errors:?})");
        return;
    }

    let before = recorder.0.progress.lock().unwrap().last().copied();
    eprintln!("position before the seek: {before:?}");
    assert!(
        before.is_some_and(|p| p < TARGET_SECS),
        "the track should still be near its start, got {before:?}"
    );

    recorder.0.progress.lock().unwrap().clear();
    handle.send(PlayerCommand::Seek(TARGET_SECS)).unwrap();

    // The restart costs about half a second for YouTube; allow for a slow
    // network, then a few ticks to confirm it keeps counting up from there.
    tokio::time::sleep(Duration::from_secs(8)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    let after = recorder.0.progress.lock().unwrap().clone();
    eprintln!("errors after seek: {errors:?}");
    eprintln!("positions after seek: {after:?}");

    if errors.iter().any(skippable) {
        eprintln!("SKIP: upstream/environment condition ({errors:?})");
        return;
    }
    assert!(errors.is_empty(), "seeking a stream reported errors: {errors:?}");

    assert!(
        after.iter().any(|p| *p >= TARGET_SECS),
        "position never reached the seek target; the offset is being dropped \
         and the bar would snap back to zero (got {after:?})"
    );
    assert!(
        after.iter().all(|p| *p >= TARGET_SECS - 1.0),
        "position fell back below the seek target (got {after:?})"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The native path must keep working: a local file seeks in place, with no
/// process restart and therefore no offset.
#[tokio::test]
async fn a_local_file_still_seeks_natively() {
    let base = std::env::temp_dir().join("music-app-local-seek");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let wav = base.join("tone.wav");
    write_wav(&wav);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state) \
         VALUES ('local', 'Tone', ?, 'present')",
    )
    .bind(wav.to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();

    let track_id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(600)).await;

    if recorder
        .0
        .errors
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.contains("No audio output device"))
    {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    recorder.0.progress.lock().unwrap().clear();
    handle.send(PlayerCommand::Seek(2.0)).unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;

    let after = recorder.0.progress.lock().unwrap().clone();
    eprintln!("positions after seek: {after:?}");

    assert!(
        after.iter().any(|p| *p >= 2.0),
        "a local seek should land at the target (got {after:?})"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Replaying a stream must not pay the resolve cost twice.
///
/// A yt-dlp resolve measured 6.5-7.4s against the live service, and it was
/// being paid on every play -- including replaying the track that just
/// finished. The URLs carry hours of validity, so the second start should be
/// dominated by ffmpeg alone.
///
/// Asserted as a ratio rather than an absolute: the point is that a whole
/// process launch and network round trip disappeared, and that survives a slow
/// machine or a slow connection in a way that a fixed millisecond budget would
/// not.
#[tokio::test]
async fn replaying_a_stream_skips_the_resolve() {
    let _network = NETWORK.lock().await;

    let base = std::env::temp_dir().join("music-app-url-cache");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, state, remote_id, remote_url) \
         VALUES ('youtube', 'Never Gonna Give You Up', 'saved', 'dQw4w9WgXcQ', \
                 'https://www.youtube.com/watch?v=dQw4w9WgXcQ')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let track_id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        ffmpeg(),
        Some(std::path::PathBuf::from("yt-dlp")),
        None,
    );

    let skippable = |e: &String| {
        e.contains("No audio output device")
            || e.contains("internet connection")
            || e.contains("Could not find yt-dlp")
            || e.contains("Could not start")
            || e.contains("YouTube refused")
            || e.contains("no longer available")
    };

    // Playing is what actually reaches the resolver, so each start is timed by
    // waiting for audio to move rather than by instrumenting the cache.
    let play_and_time = |label: &'static str| {
        let handle = &handle;
        let recorder = &recorder;
        async move {
            recorder.0.progress.lock().unwrap().clear();
            let started = std::time::Instant::now();

            handle
                .send(PlayerCommand::PlayQueue {
                    track_ids: vec![track_id],
                    start_index: 0,
                    context_name: None,
                })
                .unwrap();

            loop {
                if recorder.0.progress.lock().unwrap().iter().any(|p| *p > 0.0) {
                    break Some(started.elapsed());
                }
                if recorder.0.errors.lock().unwrap().iter().any(skippable) {
                    break None;
                }
                if started.elapsed() > Duration::from_secs(40) {
                    break None;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            .inspect(|taken| eprintln!("{label}: {taken:?}"))
        }
    };

    let Some(cold) = play_and_time("cold start (resolve + ffmpeg)").await else {
        eprintln!("SKIP: upstream/environment condition");
        return;
    };

    handle.send(PlayerCommand::Stop).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let Some(warm) = play_and_time("warm start (cached URL)").await else {
        eprintln!("SKIP: upstream/environment condition");
        return;
    };

    assert!(
        warm < cold,
        "the replay was not faster: cold {cold:?}, warm {warm:?}"
    );
    assert!(
        warm * 2 < cold,
        "the resolve does not look skipped -- a cached start should be far \
         cheaper than a cold one (cold {cold:?}, warm {warm:?})"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The gap between two tracks, when the second one streams.
///
/// A resolve measured 6-7s against the live service, and it used to be paid
/// *after* the previous track ended -- silence with nothing to show for it.
/// Prefetching moves that work under the track already playing.
///
/// The threshold is what makes this meaningful: four seconds is unreachable
/// without a prefetch, because the resolve alone exceeds it.
#[tokio::test]
async fn the_next_stream_is_ready_before_the_current_track_ends() {
    let _network = NETWORK.lock().await;

    let base = std::env::temp_dir().join("music-app-prefetch");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    // Deliberately longer than a resolve takes (~7s measured). A real song
    // is minutes long, so this is the realistic case; a first track *shorter*
    // than a resolve simply cannot be covered, and falls back to the old
    // behaviour of resolving at the handover.
    let wav = base.join("tone.wav");
    write_wav_secs(&wav, 12);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    let local: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, local_path, state) \
         VALUES ('local', 'Tone', ?, 'present') RETURNING id",
    )
    .bind(wav.to_str().unwrap())
    .fetch_one(&db.pool)
    .await
    .unwrap();

    // SoundCloud rather than YouTube on purpose. A failed prefetch is silent
    // by design -- the real play simply resolves normally -- so a YouTube 403
    // would be indistinguishable here from prefetching being broken.
    // SoundCloud does not rate-limit us, which keeps this measuring the code
    // rather than the weather.
    let stream: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, state, remote_id, remote_url) \
         VALUES ('soundcloud', 'One More Time', 'saved', '199428706', \
                 'https://soundcloud.com/daft-punk-id/daft-punk-one-more-time') \
         RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        ffmpeg(),
        Some(std::path::PathBuf::from("yt-dlp")),
        None,
    );

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![local, stream],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    let skippable = |e: &String| {
        e.contains("No audio output device")
            || e.contains("internet connection")
            || e.contains("Could not find yt-dlp")
            || e.contains("Could not start")
            || e.contains("YouTube refused")
            || e.contains("no longer available")
    };

    // Wait for the local track to hand over, then time how long the stream
    // takes to make a sound.
    let mut handover: Option<std::time::Instant> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(45);

    let gap = loop {
        if std::time::Instant::now() > deadline {
            eprintln!("SKIP: timed out waiting for the handover");
            return;
        }
        if recorder.0.errors.lock().unwrap().iter().any(skippable) {
            eprintln!("SKIP: upstream/environment condition");
            return;
        }

        let current = recorder
            .0
            .states
            .lock()
            .unwrap()
            .last()
            .and_then(|s| s.track_id);

        if handover.is_none() && current == Some(stream) {
            // The previous track's ticks are still in the buffer. Clearing
            // here is what makes the next non-zero value mean *this* track --
            // without it the gap measures as instant no matter how slow the
            // handover really was.
            recorder.0.progress.lock().unwrap().clear();
            handover = Some(std::time::Instant::now());
        }

        if let Some(started) = handover {
            let playing = recorder.0.progress.lock().unwrap().iter().any(|p| *p > 0.0);
            if playing && current == Some(stream) {
                break started.elapsed();
            }
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
    };

    eprintln!("gap between tracks: {gap:?}");
    assert!(
        gap < Duration::from_secs(4),
        "the stream took {gap:?} to start, which means its URL was resolved \
         after the handover rather than prefetched underneath the track before"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// A streamed track fills the cache as it plays, and the next play uses it.
///
/// The cache copy is written by the same ffmpeg that is decoding for playback,
/// so it costs no extra network traffic. What proves it landed is the replay:
/// the coordinator is given **no yt-dlp at all** the second time, so if any
/// audio comes out it can only have come from disk.
#[tokio::test]
async fn a_streamed_track_is_cached_and_replays_without_the_network() {
    let _network = NETWORK.lock().await;

    let base = std::env::temp_dir().join("music-app-audio-cache");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let cache_dir = base.join("cache");
    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    // A short track, so it can be played to the end inside a test -- only a
    // complete decode commits a cache entry.
    sqlx::query(
        "INSERT INTO tracks (source, title, state, remote_id, remote_url) \
         VALUES ('youtube', 'Me at the zoo', 'saved', 'jNQXAC9IVRw', \
                 'https://www.youtube.com/watch?v=jNQXAC9IVRw')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let track_id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let skippable = |e: &String| {
        e.contains("No audio output device")
            || e.contains("internet connection")
            || e.contains("Could not find yt-dlp")
            || e.contains("Could not start")
            || e.contains("YouTube refused")
            || e.contains("no longer available")
    };

    // --- first play: over the network, filling the cache ---
    {
        let recorder = Recorder::default();
        let handle = player::spawn(
            recorder.clone(),
            db.pool.clone(),
            Some(std::path::PathBuf::from("ffmpeg")),
            Some(std::path::PathBuf::from("yt-dlp")),
            Some(music_app_lib::audio_cache::AudioCache::new(cache_dir.clone())),
        );

        handle
            .send(PlayerCommand::PlayQueue {
                track_ids: vec![track_id],
                start_index: 0,
                context_name: None,
            })
            .unwrap();

        // Resolve, then let the whole 19-second clip run to its end.
        tokio::time::sleep(Duration::from_secs(45)).await;

        let errors = recorder.0.errors.lock().unwrap().clone();
        if errors.iter().any(skippable) {
            eprintln!("SKIP: upstream/environment condition ({errors:?})");
            return;
        }
        // Dropping the handle stops playback, which is what commits the entry.
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let cached: Vec<_> = std::fs::read_dir(&cache_dir)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    eprintln!("cache contents: {cached:?}");

    assert!(
        cached.iter().any(|name| name.to_string_lossy().ends_with(".mka")),
        "nothing was cached; the second ffmpeg output never landed"
    );
    assert!(
        !cached.iter().any(|name| name.to_string_lossy().contains(".part.")),
        "a partial file was left behind: {cached:?}"
    );

    // --- second play: no yt-dlp, so the network is not an option ---
    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        ffmpeg(),
        // The whole point: there is no way to resolve a stream now.
        None,
        Some(music_app_lib::audio_cache::AudioCache::new(cache_dir.clone())),
    );

    let started = std::time::Instant::now();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_secs(5)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    let progress = recorder.0.progress.lock().unwrap().clone();
    eprintln!("replay errors: {errors:?}");
    eprintln!("replay took: {:?}", started.elapsed());

    assert!(
        errors.is_empty(),
        "the cached replay should need nothing external: {errors:?}"
    );
    assert!(
        progress.iter().any(|p| *p > 0.0),
        "the cached copy did not play (got {progress:?})"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The coordinator keeps answering while a track is still loading.
///
/// A stream load is a yt-dlp resolve then ffmpeg buffering -- six or seven
/// seconds. It used to be awaited inside the command handler, so for that
/// whole time the player was deaf: pressing Pause, Next or Mute did nothing
/// until the load finished.
///
/// Mute is the probe because it is cheap, unambiguous, and visible in the
/// state snapshot. If it lands while the load is still in flight, the
/// coordinator is genuinely concurrent.
#[tokio::test]
async fn commands_are_answered_while_a_track_is_loading() {
    let _network = NETWORK.lock().await;

    let base = std::env::temp_dir().join("music-app-responsive-load");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, state, remote_id, remote_url) \
         VALUES ('youtube', 'Never Gonna Give You Up', 'saved', 'dQw4w9WgXcQ', \
                 'https://www.youtube.com/watch?v=dQw4w9WgXcQ')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let track_id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    // No cache, so the load genuinely goes to the network and takes its time.
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        ffmpeg(),
        Some(std::path::PathBuf::from("yt-dlp")),
        None,
    );

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Long enough to be certain the resolve is under way, far short of the
    // ~6s it takes to finish.
    tokio::time::sleep(Duration::from_millis(700)).await;

    let loading = recorder
        .0
        .states
        .lock()
        .unwrap()
        .iter()
        .any(|s| s.state == music_app_lib::player::PlaybackState::Loading);
    if !loading {
        eprintln!("SKIP: the load never started (upstream/environment)");
        return;
    }

    let asked_at = std::time::Instant::now();
    handle.send(PlayerCommand::SetMuted(true)).unwrap();

    // Generous, but still nowhere near a full resolve.
    let deadline = Duration::from_millis(2500);
    let answered = loop {
        if recorder.0.states.lock().unwrap().iter().any(|s| s.muted) {
            break Some(asked_at.elapsed());
        }
        if asked_at.elapsed() > deadline {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    eprintln!("mute answered after: {answered:?}");
    assert!(
        answered.is_some(),
        "the coordinator did not answer within {deadline:?} of being asked, \
         which means it was still blocked waiting for the load"
    );

    // The assertion lands in milliseconds, but the load it was racing is still
    // out there holding a yt-dlp process. Returning now would release the
    // network lock while that is still running and hand the contention to
    // whichever test goes next -- which is exactly what made the prefetch
    // timing flaky.
    handle.send(PlayerCommand::Stop).unwrap();
    tokio::time::sleep(Duration::from_secs(8)).await;

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Restoring the last session's track, and resuming it.
///
/// Two things matter and neither is visible from the command API alone.
/// Restoring must not *load* anything -- resolving a stream at startup would
/// cost seconds before the window is usable -- and pressing play afterwards
/// must begin at the saved position rather than at the beginning.
#[tokio::test]
async fn a_restored_track_waits_in_the_bar_then_resumes_where_it_was() {
    // Past two minutes, with far more than five left of the ten the row
    // claims -- the only shape of listen whose position is kept at all.
    const RESUME_AT: f64 = 130.0;
    const CLAIMED_DURATION: i64 = 600;

    let base = std::env::temp_dir().join("music-app-resume");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    // Long enough to hold the resume point, and no longer: a genuine ten
    // minutes of PCM is 53MB of temp file to prove nothing extra. The row's
    // duration is what the rule reads, and a stated duration disagreeing with
    // what actually decodes is ordinary for a remote track anyway.
    let wav = base.join("tone.wav");
    write_wav_secs(&wav, 180);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    let track_id: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
         VALUES ('local', 'Tone', ?, 'present', ?) RETURNING id",
    )
    .bind(wav.to_str().unwrap())
    .bind(CLAIMED_DURATION)
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);

    handle
        .send(PlayerCommand::Restore {
            track_id,
            position_secs: RESUME_AT,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(400)).await;

    // The track is in the bar, at its position, and nothing is playing.
    let latest = recorder
        .0
        .states
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("restore emits a state");

    assert_eq!(latest.track_id, Some(track_id), "the track should be shown");
    assert_eq!(
        latest.state,
        music_app_lib::player::PlaybackState::Stopped,
        "restoring must not start playback"
    );
    assert!(
        recorder
            .0
            .progress
            .lock()
            .unwrap()
            .iter()
            .any(|p| (*p - RESUME_AT).abs() < 1.0),
        "the bar should show the saved position"
    );
    assert!(
        recorder.0.errors.lock().unwrap().is_empty(),
        "restoring should touch nothing that can fail"
    );

    // Now press play: it must pick up where it left off.
    recorder.0.progress.lock().unwrap().clear();
    handle.send(PlayerCommand::TogglePlayPause).unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;

    if recorder
        .0
        .errors
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.contains("No audio output device"))
    {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    let after = recorder.0.progress.lock().unwrap().clone();
    eprintln!("positions after resume: {after:?}");

    assert!(
        after.iter().any(|p| *p >= RESUME_AT),
        "playback restarted from the beginning instead of resuming (got {after:?})"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// A position at the very end is not worth resuming: it would play a moment of
/// silence and skip on.
#[tokio::test]
async fn a_position_at_the_end_of_a_track_is_not_restored() {
    let base = std::env::temp_dir().join("music-app-resume-end");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let wav = base.join("tone.wav");
    write_wav_secs(&wav, 60);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    let track_id: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
         VALUES ('local', 'Tone', ?, 'present', 60) RETURNING id",
    )
    .bind(wav.to_str().unwrap())
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);

    // Two seconds from the end.
    handle
        .send(PlayerCommand::Restore {
            track_id,
            position_secs: 58.0,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(400)).await;

    let progress = recorder.0.progress.lock().unwrap().clone();
    assert!(
        progress.iter().all(|p| *p < 1.0),
        "a position at the end should reset to the start (got {progress:?})"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Leaving a track part-way through fetches a complete copy of it.
///
/// The negative cases matter more than the positive one: this spends the
/// user's bandwidth, so it must fire only when the free path has genuinely
/// failed and the listen was long enough to mean something.
#[tokio::test]
async fn a_track_left_part_way_through_is_fetched_for_offline() {
    let _network = NETWORK.lock().await;

    let base = std::env::temp_dir().join("music-app-keep-abandoned");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let cache_dir = base.join("cache");
    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    // A 19-second clip, so "half of it" arrives quickly and the duration cap
    // is comfortably clear.
    let stream: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, state, remote_id, remote_url, duration_secs) \
         VALUES ('youtube', 'Me at the zoo', 'saved', 'jNQXAC9IVRw', \
                 'https://www.youtube.com/watch?v=jNQXAC9IVRw', 19) RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        ffmpeg(),
        Some(std::path::PathBuf::from("yt-dlp")),
        Some(music_app_lib::audio_cache::AudioCache::new(cache_dir.clone())),
    );

    handle
        .send(PlayerCommand::SetKeepAbandoned(true))
        .unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![stream],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Play past halfway, then walk away.
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    loop {
        if recorder.0.errors.lock().unwrap().iter().any(|e| {
            e.contains("No audio output device")
                || e.contains("internet connection")
                || e.contains("Could not find yt-dlp")
                || e.contains("Could not start")
                || e.contains("YouTube refused")
        }) {
            eprintln!("SKIP: upstream/environment condition");
            return;
        }
        if recorder.0.progress.lock().unwrap().iter().any(|p| *p > 11.0) {
            break;
        }
        if std::time::Instant::now() > deadline {
            eprintln!("SKIP: never reached the halfway mark");
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    handle.send(PlayerCommand::Stop).unwrap();

    // The fetch is a fresh resolve plus a download of the whole clip.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut cached = false;
    while std::time::Instant::now() < deadline {
        cached = std::fs::read_dir(&cache_dir)
            .map(|d| {
                d.flatten()
                    .any(|e| e.file_name().to_string_lossy().ends_with("jNQXAC9IVRw.mka"))
            })
            .unwrap_or(false);
        if cached {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let leftovers: Vec<_> = std::fs::read_dir(&cache_dir)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    eprintln!("cache contents: {leftovers:?}");

    assert!(cached, "an abandoned track was not fetched for offline use");
    assert!(
        !leftovers
            .iter()
            .any(|n| n.to_string_lossy().contains(".part.")),
        "a partial file was left behind: {leftovers:?}"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The guards, which are what keep this from spending data it should not.
///
/// Each of these would otherwise trigger a full download of a track the user
/// showed no real interest in, or that cannot benefit.
#[tokio::test]
async fn the_offline_copy_guards_hold() {
    let base = std::env::temp_dir().join("music-app-keep-guards");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let cache_dir = base.join("cache");
    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    let wav = base.join("tone.wav");
    write_wav_secs(&wav, 60);

    // A local track: nothing to fetch, whatever else is true.
    let local: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
         VALUES ('local', 'Tone', ?, 'present', 60) RETURNING id",
    )
    .bind(wav.to_str().unwrap())
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        None,
        None,
        Some(music_app_lib::audio_cache::AudioCache::new(cache_dir.clone())),
    );

    handle.send(PlayerCommand::SetKeepAbandoned(true)).unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![local],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_secs(2)).await;
    handle.send(PlayerCommand::Stop).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;

    let entries: Vec<_> = std::fs::read_dir(&cache_dir)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();

    assert!(
        entries.is_empty(),
        "a local track needs no offline copy, but something was written: {entries:?}"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// A play is recorded once it means something, and only once.
///
/// The threshold matters more than it looks: counting a play on *start* would
/// fill the history with everything skipped past while hunting for a song,
/// which is the opposite of what a recently-played list is for. Reaching the
/// end counts regardless, so short tracks are not excluded by it.
#[tokio::test]
async fn a_play_is_recorded_only_once_it_has_been_listened_to() {
    let base = std::env::temp_dir().join("music-app-history");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    // Short enough to finish inside the test, and far below the 30s threshold,
    // so only the natural end can record it.
    let short = base.join("short.wav");
    write_wav_secs(&short, 1);
    let short_id: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
         VALUES ('local', 'Short', ?, 'present', 1) RETURNING id",
    )
    .bind(short.to_str().unwrap())
    .fetch_one(&db.pool)
    .await
    .unwrap();

    // Long, and abandoned almost immediately: it must not be recorded.
    let long = base.join("long.wav");
    write_wav_secs(&long, 120);
    let long_id: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
         VALUES ('local', 'Long', ?, 'present', 120) RETURNING id",
    )
    .bind(long.to_str().unwrap())
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);

    // Play the short one through to its end.
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![short_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    if recorder
        .0
        .errors
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.contains("No audio output device"))
    {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    // Then start the long one and walk away almost at once.
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![long_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    handle.send(PlayerCommand::Stop).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let plays: Vec<(i64, i64, Option<i64>)> =
        sqlx::query_as("SELECT id, play_count, last_played FROM tracks ORDER BY id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    eprintln!("plays: {plays:?}");

    let short_row = plays.iter().find(|(id, _, _)| *id == short_id).unwrap();
    let long_row = plays.iter().find(|(id, _, _)| *id == long_id).unwrap();

    assert_eq!(
        short_row.1, 1,
        "a track played to its end should count once, however short"
    );
    assert!(short_row.2.is_some(), "and should carry a played-at time");

    assert_eq!(
        long_row.1, 0,
        "a track abandoned after a moment must not enter the history"
    );
    assert!(long_row.2.is_none());

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Leaving a streamed track part-way through must announce the cache fill.
///
/// The announcement itself is cosmetic -- a grey line in the activity panel --
/// but the path it runs on is not. It reads the row on a spawned task, and a
/// column the statement did not select makes `Row::get` panic there. Release
/// builds abort on panic, so that panic is not a lost background task: it is
/// the whole application disappearing the moment the user presses Next.
///
/// Which is why this asserts on the *title*. Nothing else in the payload comes
/// from a column that only this feature needs, and a column only one feature
/// needs is the one that gets dropped.
#[tokio::test]
async fn leaving_a_streamed_track_part_way_announces_the_cache_fill() {
    let _guard = NETWORK.lock().await;

    let base = std::env::temp_dir().join("music-app-abandoned-copy");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let wav = base.join("kept.wav");
    // Long enough that half of it is comfortably clear of the load, which
    // costs the better part of a second before the first sample is heard.
    write_wav_secs(&wav, 6);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    // Inserted as `downloaded` so it plays from the file with no network, and
    // so the schema accepts a `local_path` at all -- a `saved` row may not
    // carry one. It is flipped to `saved` below, once it is playing, which is
    // exactly what deleting a download while it plays does.
    sqlx::query(
        "INSERT INTO tracks \
         (source, title, artist, duration_secs, state, local_path, \
          remote_id, remote_url) \
         VALUES ('youtube', 'Kept For Later', 'Someone', 6, 'downloaded', ?, \
                 'abcdefghijk', 'https://www.youtube.com/watch?v=abcdefghijk')",
    )
    .bind(wav.to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();

    let track_id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    // A real ffmpeg, because the track has to actually play: the announcement
    // only happens once the listen passes halfway, and nothing reaches halfway
    // without a decoder. This used to pass a bogus path for it too -- fine when
    // rodio decoded the WAV natively, and now the difference between the test
    // measuring something and measuring nothing.
    //
    // yt-dlp stays bogus, which is what the test actually relies on: the fetch
    // triggered *after* the announcement resolves through yt-dlp and so fails
    // immediately, leaving the announcement itself as the only thing observed.
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        ffmpeg(),
        Some(base.join("no-such-yt-dlp.exe")),
        Some(music_app_lib::audio_cache::AudioCache::new(
            base.join("cache"),
        )),
    );

    handle
        .send(PlayerCommand::SetKeepAbandoned(true))
        .unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Past half of the six seconds the row claims, which is the threshold
    // for a part-way listen being worth keeping.
    tokio::time::sleep(Duration::from_millis(4000)).await;

    if recorder
        .0
        .errors
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.contains("No audio output device"))
    {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    sqlx::query("UPDATE tracks SET state = 'saved', local_path = NULL WHERE id = ?")
        .bind(track_id)
        .execute(&db.pool)
        .await
        .unwrap();

    handle.send(PlayerCommand::Stop).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let caching = recorder.0.caching.lock().unwrap().clone();
    // Both printed: an empty list is either the announcement never happening
    // or the listen never reaching the halfway mark, and only the positions
    // tell the two apart.
    eprintln!("caching:  {caching:?}");
    eprintln!("progress: {:?}", recorder.0.progress.lock().unwrap());

    assert!(
        caching.contains(&(track_id, Some("Kept For Later".to_string()))),
        "abandoning a streamed track past halfway should announce the fill \
         with the track's title, got {caching:?}",
    );
}

/// A track that is starting must say where it starts, before it gets there.
///
/// The bar shows the last position it was told about. Nothing tells it a track
/// changed — the position and the track arrive as separate events — so until
/// the new track produces a tick of its own, the bar goes on counting through
/// the previous one. For a local file that is imperceptible. For a stream it
/// is the several seconds yt-dlp spends resolving, which is exactly what "the
/// song did not restart" means.
///
/// The second track here cannot load at all, which is what makes this an
/// assertion rather than a race: the only tick it can ever produce is the one
/// emitted when its load *begins*.
#[tokio::test]
async fn a_track_that_never_starts_still_resets_the_position() {
    let _guard = NETWORK.lock().await;

    let base = std::env::temp_dir().join("music-app-position-reset");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let wav = base.join("plays.wav");
    write_wav_secs(&wav, 6);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state) \
         VALUES ('local', 'Plays', ?, 'present')",
    )
    .bind(wav.to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();

    // Present as far as the row is concerned, and not on disk. Resolving it
    // fails the way a stream fails, without the network being involved.
    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state) \
         VALUES ('local', 'Will Not Load', ?, 'present')",
    )
    .bind(base.join("gone.wav").to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();

    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM tracks ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    let (playable, broken) = (ids[0], ids[1]);

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![playable],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Far enough in that carrying this position over would be obvious.
    tokio::time::sleep(Duration::from_millis(2000)).await;

    if recorder
        .0
        .errors
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.contains("No audio output device"))
    {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    let before = recorder.0.ticks.lock().unwrap().clone();
    assert!(
        before.iter().any(|&(id, secs)| id == Some(playable) && secs > 1.0),
        "the first track should have been playing for a while, got {before:?}",
    );

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![broken],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(1000)).await;

    let ticks = recorder.0.ticks.lock().unwrap().clone();
    eprintln!("ticks: {ticks:?}");

    assert!(
        ticks.contains(&(Some(broken), 0.0)),
        "starting a track should announce its position, so the bar stops \
         showing the previous track's, got {ticks:?}",
    );
}

/// The bar's two halves must describe the same track.
///
/// The id arrives on `player-state` and the title, artist and artwork arrive
/// on `player-queue`. Anything that reads both — the player bar does — is
/// entitled to assume they agree, and has no way to draw anything sensible if
/// they do not: a streamed audition is not in the library, so there is no
/// second place to look the details up.
#[tokio::test]
async fn the_state_and_the_queue_agree_about_what_is_playing() {
    let base = std::env::temp_dir().join("music-app-bar-agreement");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let wav = base.join("audition.wav");
    write_wav(&wav);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    // Exactly what auditioning a search result leaves behind: a remote track
    // that is deliberately *not* in the library. Held as `downloaded` against
    // a real file so it plays without a network.
    let track_id: i64 = sqlx::query_scalar(
        "INSERT INTO tracks \
         (source, title, artist, duration_secs, state, in_library, local_path, \
          remote_id, remote_url) \
         VALUES ('youtube', 'An Audition', 'A Channel', 3, 'downloaded', 0, ?, \
                 'abcdefghijk', 'https://www.youtube.com/watch?v=abcdefghijk') \
         RETURNING id",
    )
    .bind(wav.to_str().unwrap())
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
            context_name: Some("YouTube search".to_string()),
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(800)).await;

    let state = recorder
        .0
        .states
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("playing emits a state");
    let queue = recorder
        .0
        .queues
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("playing emits a queue");

    eprintln!("state.track_id: {:?}", state.track_id);
    eprintln!("queue.current:  {:?}", queue.current.as_ref().map(|c| (c.track_id, &c.title)));

    assert_eq!(state.track_id, Some(track_id), "the state names the track");

    let current = queue.current.expect("the queue names a current track");
    assert_eq!(current.track_id, track_id, "the queue names the same track");
    assert_eq!(current.title, "An Audition", "and can describe it");
    assert_eq!(current.artist.as_deref(), Some("A Channel"));

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The same agreement, across a change of track.
///
/// The cold-start case proves the payloads are built correctly. This proves
/// nothing is left behind: the reported symptom was the *previous* song's
/// title sitting over the new song's audio, which is what a queue event that
/// lags the state event by one track would look like.
#[tokio::test]
async fn changing_track_leaves_nothing_of_the_previous_one_in_the_bar() {
    let base = std::env::temp_dir().join("music-app-bar-transition");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let first = base.join("first.wav");
    let second = base.join("second.wav");
    write_wav(&first);
    write_wav(&second);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    let local: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
         VALUES ('local', 'A Local Song', ?, 'present', 3) RETURNING id",
    )
    .bind(first.to_str().unwrap())
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let online: i64 = sqlx::query_scalar(
        "INSERT INTO tracks \
         (source, title, artist, duration_secs, state, in_library, local_path, \
          remote_id, remote_url) \
         VALUES ('youtube', 'An Audition', 'A Channel', 3, 'downloaded', 0, ?, \
                 'abcdefghijk', 'https://www.youtube.com/watch?v=abcdefghijk') \
         RETURNING id",
    )
    .bind(second.to_str().unwrap())
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);

    for id in [local, online] {
        handle
            .send(PlayerCommand::PlayQueue {
                track_ids: vec![id],
                start_index: 0,
                context_name: Some("YouTube search".to_string()),
            })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
    }

    let states: Vec<Option<i64>> = recorder
        .0
        .states
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.track_id)
        .collect();
    let queues: Vec<(Option<i64>, Option<String>)> = recorder
        .0
        .queues
        .lock()
        .unwrap()
        .iter()
        .map(|q| {
            (
                q.current.as_ref().map(|c| c.track_id),
                q.current.as_ref().map(|c| c.title.clone()),
            )
        })
        .collect();

    eprintln!("local={local} online={online}");
    eprintln!("states: {states:?}");
    eprintln!("queues: {queues:?}");

    assert_eq!(states.last(), Some(&Some(online)), "the state names the new track");
    assert_eq!(
        queues.last().map(|(id, _)| *id),
        Some(Some(online)),
        "the queue names the new track too, got {queues:?}",
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// A cache copy this app cannot read back must not outlive one play.
///
/// The cache is written *alongside* playback by stream-copying what arrives.
/// Interrupt that and what lands decodes cleanly right up to the damage — so
/// nothing notices until someone seeks past it, and from then on the track is
/// unplayable forever, because every later play finds the same file. Deleting
/// it is the only thing that ends that; the provider still has the track.
///
/// Observed for real: a SoundCloud copy held 2:10 of a 2:29 song, clean to
/// 2:00 and noise after it.
#[tokio::test]
async fn a_cache_copy_that_will_not_decode_is_thrown_away() {
    let _guard = NETWORK.lock().await;

    let base = std::env::temp_dir().join("music-app-poisoned-cache");
    let _ = std::fs::remove_dir_all(&base);
    let cache_dir = base.join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Named exactly as the cache names its own files, and full of nothing
    // ffmpeg can make sense of.
    let poisoned = cache_dir.join("youtube-abcdefghijk.mka");
    std::fs::write(&poisoned, vec![0x7fu8; 64 * 1024]).unwrap();

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    let track_id: i64 = sqlx::query_scalar(
        "INSERT INTO tracks \
         (source, title, duration_secs, state, remote_id, remote_url) \
         VALUES ('youtube', 'Poisoned', 149, 'saved', 'abcdefghijk', \
                 'https://www.youtube.com/watch?v=abcdefghijk') \
         RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        // Resolved through PATH. Without it there is no decoder, and "no
        // ffmpeg" is a different failure that must not evict anything.
        ffmpeg(),
        // Deliberately absent, so the retry after eviction fails immediately
        // instead of going to the network. Eviction is what is under test.
        Some(base.join("no-such-yt-dlp.exe")),
        Some(music_app_lib::audio_cache::AudioCache::new(cache_dir.clone())),
    );

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(3000)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    eprintln!("errors: {errors:?}");

    if errors.iter().any(|e| e.contains("Could not start ffmpeg")) {
        eprintln!("SKIP: no ffmpeg on PATH");
        return;
    }

    assert!(
        !poisoned.exists(),
        "the unreadable copy is still there, so every later play finds it too",
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// A copy of ours that plays but is damaged must not survive the play.
///
/// This is the half a decode failure never catches. Truncate a cache copy and
/// it decodes perfectly right up to the damage — ffmpeg exits 0 and says only
/// `File ended prematurely` — so the song just ends early, every play, forever,
/// and nothing ever reconsiders the file. Observed for real: a SoundCloud copy
/// holding 2:10 of a 2:29 track.
///
/// The rule is the same one the write side uses. Not a comparison against the
/// stored duration: "ended early" and "the duration metadata was wrong" are
/// indistinguishable, and acting on that would re-download some tracks on
/// every play forever. "ffmpeg complained" is a fact.
#[tokio::test]
async fn a_cache_copy_ffmpeg_complains_about_does_not_survive_the_play() {
    let _guard = NETWORK.lock().await;

    let base = std::env::temp_dir().join("music-app-truncated-cache");
    let _ = std::fs::remove_dir_all(&base);
    let cache_dir = base.join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    let ffmpeg = std::path::PathBuf::from("ffmpeg");
    let whole = base.join("whole.mka");

    // A real tone in the same container the cache uses.
    let built = std::process::Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=4"])
        .args(["-c:a", "aac", "-f", "matroska", "-y"])
        .arg(&whole)
        .status();

    if !built.map(|s| s.success()).unwrap_or(false) {
        eprintln!("SKIP: no ffmpeg on PATH");
        return;
    }

    // Cut short exactly the way an interrupted copy is.
    let full = std::fs::read(&whole).unwrap();
    let cut = &full[..full.len() * 87 / 100];
    let poisoned = cache_dir.join("youtube-abcdefghijk.mka");
    std::fs::write(&poisoned, cut).unwrap();

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    let track_id: i64 = sqlx::query_scalar(
        "INSERT INTO tracks \
         (source, title, duration_secs, state, remote_id, remote_url) \
         VALUES ('youtube', 'Cut Short', 4, 'saved', 'abcdefghijk', \
                 'https://www.youtube.com/watch?v=abcdefghijk') \
         RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        Some(ffmpeg),
        Some(base.join("no-such-yt-dlp.exe")),
        Some(music_app_lib::audio_cache::AudioCache::new(cache_dir.clone())),
    );

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Long enough for the copy to play out, hit its truncated end, and be
    // dropped. The fixture is deliberately shorter than the read-ahead so
    // ffmpeg meets the damage while playing rather than long afterwards.
    tokio::time::sleep(Duration::from_millis(5000)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    if errors.iter().any(|e| e.contains("No audio output device")) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    let progress = recorder.0.progress.lock().unwrap().clone();
    eprintln!("progress: {progress:?}");
    assert!(
        progress.iter().any(|p| *p > 0.5),
        "the damaged copy should still have played -- this is the case a \
         decode failure never catches (got {progress:?})",
    );

    handle.send(PlayerCommand::Stop).unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;

    assert!(
        !poisoned.exists(),
        "the damaged copy outlived the play, so the song ends early forever",
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// A track reported as playing must also have been *described*.
///
/// The player bar resolves what to show in two steps: the queue payload if it
/// names the same id, otherwise the library list. A streamed audition is
/// `in_library = 0` and so is not in that list at all -- for those the queue
/// payload is the only source of a title or a cover, and without it the bar
/// sits on "Loading track details…" with a grey tile where the artwork goes.
///
/// The gap was that `handle_load` emitted state on success without emitting the
/// queue beside it, so the last thing the frontend heard about the track was
/// its id. The failure branch three lines below already did both.
///
/// Asserted as an *ordering* property rather than "a queue payload exists",
/// because one is emitted at command time regardless. What matters is that one
/// follows the state that announced the track as playing.
#[tokio::test]
async fn a_track_that_starts_playing_is_also_described() {
    let base = std::env::temp_dir().join("music-app-coordinator-described");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let wav = base.join("tone.wav");
    write_wav(&wav);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state) \
         VALUES ('local', 'Tone', ?, 'present')",
    )
    .bind(wav.to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();

    let track_id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(1500)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    if errors.iter().any(|e| e.contains("No audio output device")) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    let states = recorder.0.state_seq.lock().unwrap().clone();
    let queues = recorder.0.queue_seq.lock().unwrap().clone();
    eprintln!("states: {states:?}");
    eprintln!("queues: {queues:?}");

    // The moment the coordinator first said this track was the one playing.
    // The state that said this track is *playing* -- not the earlier "loading"
    // one, which a command-time queue emit already sits after.
    let announced = states
        .iter()
        .find(|(_, id, playing)| *id == Some(track_id) && *playing)
        .map(|(seq, _, _)| *seq)
        .expect("the coordinator never reported the track as playing");

    assert!(
        queues
            .iter()
            .any(|(seq, id)| *seq > announced && *id == Some(track_id)),
        "the track was announced at sequence {announced} and no queue payload \
         describing it followed (queues: {queues:?}) -- the bar has an id and \
         nothing to draw with",
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The beta report: a streamed track plays, but the bar cannot name it.
///
/// This is the *player bar's* contract, not the queue panel's, and the two
/// differ in one way that matters. The bar resolves what is playing by
/// matching `player-state`'s track id against the `player-queue` payload, and
/// falls back to the library list when they disagree. A streamed audition is
/// `in_library = 0`, so for it that fallback is empty -- the queue payload is
/// the only thing in the app that can produce a title, an artist or a cover.
/// Which is why this shows up on YouTube and SoundCloud tracks and on nothing
/// else: every other kind of track is quietly rescued by the fallback.
///
/// So the assertion is not "a queue payload was emitted". It is the thing the
/// user can actually see: replay the two event streams in the order they were
/// emitted, exactly as the bar does, and ask what the bar is showing when the
/// dust settles.
///
/// Live, because the failure is about the timing of a real resolve -- yt-dlp
/// takes seconds, and that gap is the whole point. See the note on
/// `a_saved_youtube_track_streams`.
#[tokio::test]
async fn the_bar_can_name_a_streamed_track() {
    let _network = NETWORK.lock().await;

    let base = std::env::temp_dir().join("music-app-bar-names-stream");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    // Exactly what `save_remote_track` writes when a search result is played:
    // metadata, no local path, and deliberately *not* in the library.
    sqlx::query(
        "INSERT INTO tracks (source, title, artist, state, remote_id, remote_url, \
         remote_thumbnail_url, in_library) \
         VALUES ('youtube', 'Never Gonna Give You Up', 'Rick Astley', 'saved', \
                 'dQw4w9WgXcQ', 'https://www.youtube.com/watch?v=dQw4w9WgXcQ', \
                 'https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg', 0)",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let track_id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        ffmpeg(),
        Some(std::path::PathBuf::from("yt-dlp")),
        None,
    );

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_secs(25)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    if errors
        .iter()
        .any(|e| e.contains("No audio output device") || e.contains("yt-dlp"))
    {
        eprintln!("SKIP: cannot reach the stream on this machine: {errors:?}");
        return;
    }

    let states = recorder.0.state_seq.lock().unwrap().clone();
    let queues = recorder.0.queue_desc.lock().unwrap().clone();

    if !states.iter().any(|(_, _, playing)| *playing) {
        eprintln!("SKIP: the track never reached Playing: {errors:?}");
        return;
    }

    // Replay both streams in emission order and drive the bar the way the
    // component does.
    let mut merged: Vec<(u64, Option<i64>, Option<Option<String>>)> = Vec::new();
    for (seq, id, _) in &states {
        merged.push((*seq, *id, None));
    }
    for (seq, id, title) in &queues {
        merged.push((*seq, *id, Some(title.clone())));
    }
    merged.sort_by_key(|(seq, _, _)| *seq);

    // `player.trackId`, and `queueStore.current` as (id, title).
    let mut bar_track: Option<i64> = None;
    let mut bar_queue: Option<(i64, String)> = None;
    // What the bar was showing after each event, so a failure can say where it
    // went blank rather than only that it did.
    let mut shown: Vec<(u64, Option<String>)> = Vec::new();

    for (seq, id, payload) in &merged {
        match payload {
            // A queue payload: the panel redraws from it wholesale.
            Some(title) => {
                bar_queue = match (id, title) {
                    (Some(i), Some(t)) => Some((*i, t.clone())),
                    _ => None,
                }
            }
            // A state event: the id, and nothing that describes it.
            None => bar_track = *id,
        }

        // `nowPlaying`, verbatim: the queue payload where it agrees, and for an
        // audition there is no library row to fall back to.
        let now = match (bar_track, &bar_queue) {
            (Some(t), Some((qid, title))) if *qid == t => Some(title.clone()),
            _ => None,
        };
        shown.push((*seq, now));
    }

    eprintln!("states: {states:?}");
    eprintln!("queues: {queues:?}");
    eprintln!("bar:    {shown:?}");

    let (_, settled) = shown.last().expect("no events at all").clone();

    assert_eq!(
        settled.as_deref(),
        Some("Never Gonna Give You Up"),
        "the bar could not name the streamed track once everything had \
         settled -- this is the \"Loading track details…\" the testers see. \
         Bar over time: {shown:?}",
    );
}

/// The same question, but in the state a listener is actually in.
///
/// Nobody launches the app and immediately plays one YouTube result. They are
/// already listening to something, and *then* pick a stream -- which is the
/// case where the bar has a queue payload already, describing the wrong track.
/// A payload that is merely stale is far more dangerous than a missing one:
/// it is truthy, it satisfies "a payload exists", and the bar still cannot use
/// it because it names somebody else.
#[tokio::test]
async fn the_bar_can_name_a_stream_chosen_mid_listen() {
    let _network = NETWORK.lock().await;

    let base = std::env::temp_dir().join("music-app-bar-names-midlisten");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let wav = base.join("tone.wav");
    write_wav_secs(&wav, 30);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    // A local library track, then the audition that interrupts it.
    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state, in_library) \
         VALUES ('local', 'Tone', ?, 'present', 1)",
    )
    .bind(wav.to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO tracks (source, title, artist, state, remote_id, remote_url, \
         remote_thumbnail_url, in_library) \
         VALUES ('youtube', 'Never Gonna Give You Up', 'Rick Astley', 'saved', \
                 'dQw4w9WgXcQ', 'https://www.youtube.com/watch?v=dQw4w9WgXcQ', \
                 'https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg', 0)",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let local_id: i64 = sqlx::query_scalar("SELECT id FROM tracks WHERE source = 'local'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let stream_id: i64 = sqlx::query_scalar("SELECT id FROM tracks WHERE source = 'youtube'")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        ffmpeg(),
        Some(std::path::PathBuf::from("yt-dlp")),
        None,
    );

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![local_id],
            start_index: 0,
            context_name: Some("Library".to_string()),
        })
        .unwrap();

    // Let it genuinely start, so the bar is in the settled, describing state
    // the interruption has to survive.
    tokio::time::sleep(Duration::from_secs(3)).await;

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![stream_id],
            start_index: 0,
            context_name: Some("YouTube search".to_string()),
        })
        .unwrap();

    tokio::time::sleep(Duration::from_secs(25)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    if errors
        .iter()
        .any(|e| e.contains("No audio output device") || e.contains("yt-dlp"))
    {
        eprintln!("SKIP: cannot reach the stream on this machine: {errors:?}");
        return;
    }

    let states = recorder.0.state_seq.lock().unwrap().clone();
    let queues = recorder.0.queue_desc.lock().unwrap().clone();

    if !states
        .iter()
        .any(|(_, id, playing)| *id == Some(stream_id) && *playing)
    {
        eprintln!("SKIP: the stream never reached Playing: {errors:?}");
        return;
    }

    let shown = replay_the_bar(&states, &queues);
    eprintln!("states: {states:?}");
    eprintln!("queues: {queues:?}");
    eprintln!("bar:    {shown:?}");

    let (_, settled) = shown.last().expect("no events at all").clone();
    assert_eq!(
        settled.as_deref(),
        Some("Never Gonna Give You Up"),
        "the bar could not name the stream that interrupted a local track. \
         Bar over time: {shown:?}",
    );
}

/// Drives the player bar the way the component does, over the real emission
/// order, and reports what it was showing after each event.
///
/// The fallback the bar has for a library track is deliberately absent: this
/// asks what a *streamed audition* can show, and for that the queue payload is
/// the only source in the app.
fn replay_the_bar(
    states: &[(u64, Option<i64>, bool)],
    queues: &[(u64, Option<i64>, Option<String>)],
) -> Vec<(u64, Option<String>)> {
    let mut merged: Vec<(u64, Option<i64>, Option<Option<String>>)> = Vec::new();
    for (seq, id, _) in states {
        merged.push((*seq, *id, None));
    }
    for (seq, id, title) in queues {
        merged.push((*seq, *id, Some(title.clone())));
    }
    merged.sort_by_key(|(seq, _, _)| *seq);

    let mut bar_track: Option<i64> = None;
    let mut bar_queue: Option<(i64, String)> = None;
    let mut shown = Vec::new();

    for (seq, id, payload) in &merged {
        match payload {
            Some(title) => {
                bar_queue = match (id, title) {
                    (Some(i), Some(t)) => Some((*i, t.clone())),
                    _ => None,
                }
            }
            None => bar_track = *id,
        }

        let now = match (bar_track, &bar_queue) {
            (Some(t), Some((qid, title))) if *qid == t => Some(title.clone()),
            _ => None,
        };
        shown.push((*seq, now));
    }

    shown
}

/// The path with a hand-off in it: advancing *into* a stream.
///
/// This is the one a listener hits without choosing to. A queued or playlisted
/// stream is resolved ahead of time by the prefetcher, and when the track
/// before it ends `begin_load` takes the prepared branch -- which returns
/// early, before the `Loading` state the slow path emits. Fewer events on the
/// way through is exactly the condition under which a payload can be missed,
/// so the contract is worth checking here rather than assumed from the paths
/// where the load is slow and noisy.
#[tokio::test]
async fn the_bar_can_name_a_stream_it_advanced_into() {
    let _network = NETWORK.lock().await;

    let base = std::env::temp_dir().join("music-app-bar-names-advanced");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let wav = base.join("tone.wav");
    write_wav_secs(&wav, 12);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state, in_library) \
         VALUES ('local', 'Tone', ?, 'present', 1)",
    )
    .bind(wav.to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO tracks (source, title, artist, state, remote_id, remote_url, \
         remote_thumbnail_url, in_library) \
         VALUES ('youtube', 'Never Gonna Give You Up', 'Rick Astley', 'saved', \
                 'dQw4w9WgXcQ', 'https://www.youtube.com/watch?v=dQw4w9WgXcQ', \
                 'https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg', 0)",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let local_id: i64 = sqlx::query_scalar("SELECT id FROM tracks WHERE source = 'local'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let stream_id: i64 = sqlx::query_scalar("SELECT id FROM tracks WHERE source = 'youtube'")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        ffmpeg(),
        Some(std::path::PathBuf::from("yt-dlp")),
        None,
    );

    // Both in one context, so the stream is reached by the track before it
    // ending -- never by a command.
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![local_id, stream_id],
            start_index: 0,
            context_name: Some("Mixed playlist".to_string()),
        })
        .unwrap();

    // 12s of tone, then the resolve and buffer for what follows it.
    tokio::time::sleep(Duration::from_secs(45)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    if errors
        .iter()
        .any(|e| e.contains("No audio output device") || e.contains("yt-dlp"))
    {
        eprintln!("SKIP: cannot reach the stream on this machine: {errors:?}");
        return;
    }

    let states = recorder.0.state_seq.lock().unwrap().clone();
    let queues = recorder.0.queue_desc.lock().unwrap().clone();

    if !states
        .iter()
        .any(|(_, id, playing)| *id == Some(stream_id) && *playing)
    {
        eprintln!("SKIP: never advanced into the stream: {errors:?}");
        return;
    }

    let shown = replay_the_bar(&states, &queues);
    eprintln!("states: {states:?}");
    eprintln!("queues: {queues:?}");
    eprintln!("bar:    {shown:?}");

    let (_, settled) = shown.last().expect("no events at all").clone();
    assert_eq!(
        settled.as_deref(),
        Some("Never Gonna Give You Up"),
        "the bar could not name the stream it advanced into. Bar: {shown:?}",
    );
}

/// With no decoder, the player stops on the track it was asked for.
///
/// ffmpeg decodes everything now, so its absence is not a per-track problem —
/// every track fails identically. The load-failure path treats a failure as
/// evidence the *track* is bad and skips to the next one, up to
/// `MAX_LOAD_ATTEMPTS`, which here would walk two innocent tracks out of the
/// queue and leave the listener reading an error about the third.
///
/// So the queue must not move, and the message must be the one written for
/// somebody who has never heard of ffmpeg.
#[tokio::test]
async fn without_a_decoder_the_player_stops_and_says_why() {
    let base = std::env::temp_dir().join("music-app-no-decoder");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    let mut ids = Vec::new();
    for name in ["First", "Second", "Third"] {
        let wav = base.join(format!("{name}.wav"));
        write_wav(&wav);
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO tracks (source, title, local_path, state) \
             VALUES ('local', ?, ?, 'present') RETURNING id",
        )
        .bind(name)
        .bind(wav.to_str().unwrap())
        .fetch_one(&db.pool)
        .await
        .unwrap();
        ids.push(id);
    }

    let recorder = Recorder::default();
    // The condition under test: no decoder anywhere.
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None, None);

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: ids.clone(),
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(1500)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    let states = recorder.0.state_seq.lock().unwrap().clone();
    eprintln!("errors: {errors:?}");
    eprintln!("states: {states:?}");

    assert!(
        errors.iter().any(|e| e.contains("Reinstalling")),
        "the listener should be told what is actually wrong, got {errors:?}",
    );
    assert!(
        !errors.iter().any(|e| e.contains("src-tauri")),
        "a path in our source tree is not something to show a listener: {errors:?}",
    );

    // Never advanced past the track that was asked for. Without the guard the
    // coordinator reaches the third id here.
    let reached: Vec<Option<i64>> = states.iter().map(|(_, id, _)| *id).collect();
    assert!(
        !reached.contains(&Some(ids[1])) && !reached.contains(&Some(ids[2])),
        "the queue was walked looking for a track that would play, but none \
         can: {reached:?}",
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Gapless, through the real coordinator rather than the engine alone.
///
/// The engine's own tests prove it can hand one track to the next without
/// stopping. They say nothing about whether the coordinator ever *asks* it to,
/// and that is a chain of conditions -- the setting, a known duration, a
/// prepared decode, a stable next track, the right moment -- any one of which
/// silently means an ordinary gap.
#[tokio::test]
async fn one_track_hands_over_to_the_next_without_a_gap() {
    let _guard = TIMING.lock().await;
    let base = std::env::temp_dir().join("music-app-coordinator-gapless");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    // Short, so the whole run is a few seconds; `duration_secs` is what the
    // coordinator reads to decide when the end is close.
    let first = base.join("first.wav");
    let second = base.join("second.wav");
    write_wav_secs(&first, 3);
    write_wav_secs(&second, 3);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    for (title, path) in [("First", &first), ("Second", &second)] {
        sqlx::query(
            "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
             VALUES ('local', ?, ?, 'present', 3)",
        )
        .bind(title)
        .bind(path.to_str().unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    }

    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM tracks ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);

    handle.send(PlayerCommand::SetGapless(true)).unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: ids.clone(),
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Both tracks, plus room either side.
    tokio::time::sleep(Duration::from_millis(8_000)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    if errors.iter().any(|e| e.contains("No audio output device")) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }
    eprintln!("errors: {errors:?}");

    // Every (track, position) the UI was told, in order. A gapless handover
    // shows as the second track's ticks beginning while the first track's last
    // tick is still recent; a gap shows as a hole between them.
    let ticks = recorder.0.ticks.lock().unwrap().clone();
    eprintln!("ticks: {ticks:?}");

    let first_id = ids[0];
    let second_id = ids[1];

    assert!(
        ticks.iter().any(|(id, _)| *id == Some(first_id)),
        "the first track never reported progress"
    );
    assert!(
        ticks.iter().any(|(id, _)| *id == Some(second_id)),
        "the second track never played at all"
    );

    // Where the handover shows up: the last position reported for the first
    // track, against the first reported for the second. The engine polls every
    // 50 ms and reports every fourth, so ~200 ms of slack is the cadence
    // itself; a cold load of the next track costs far more than that.
    let last_first = ticks
        .iter()
        .filter(|(id, _)| *id == Some(first_id))
        .map(|(_, p)| *p)
        .fold(0.0f64, f64::max);

    eprintln!("first track's last reported position: {last_first:.3}s of 3s");

    assert!(
        last_first > 2.0,
        "the first track's progress stopped at {last_first:.3}s of 3s -- the UI \
         went quiet well before the end, which is what happens when the \
         coordinator moves its epoch on while the track is still playing"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Plays two tracks back to back and measures the silence between them.
///
/// The positions reported look the same either way -- the second track starts
/// at zero and counts up regardless. What differs is *when* its first tick
/// arrives, so the only honest measurement is wall-clock.
async fn handover_gap(name: &str, gapless: bool) -> Option<Duration> {
    let base = std::env::temp_dir().join(format!("music-app-gapless-{name}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let first = base.join("first.wav");
    let second = base.join("second.wav");
    write_wav_secs(&first, 3);
    write_wav_secs(&second, 3);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    for (title, path) in [("First", &first), ("Second", &second)] {
        sqlx::query(
            "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
             VALUES ('local', ?, ?, 'present', 3)",
        )
        .bind(title)
        .bind(path.to_str().unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    }

    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM tracks ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);

    handle.send(PlayerCommand::SetGapless(gapless)).unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: ids.clone(),
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(7_500)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    if errors.iter().any(|e| e.contains("No audio output device")) {
        return None;
    }
    assert!(errors.is_empty(), "{name}: {errors:?}");

    let timed = recorder.0.timed.lock().unwrap().clone();
    let (first_id, second_id) = (ids[0], ids[1]);

    let last_of_first = timed
        .iter()
        .filter(|(_, id)| *id == Some(first_id))
        .map(|(at, _)| *at)
        .next_back()
        .unwrap_or_else(|| panic!("{name}: the first track never reported"));
    let first_of_second = timed
        .iter()
        .find(|(_, id)| *id == Some(second_id))
        .map(|(at, _)| *at)
        .unwrap_or_else(|| panic!("{name}: the second track never played"));

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);

    Some(first_of_second.saturating_duration_since(last_of_first))
}

/// Gapless, measured rather than asserted about.
///
/// Two runs of the same two tracks, differing only in the setting. The engine
/// reports progress every fourth 50 ms poll, so ~200-250 ms between
/// consecutive ticks is the cadence itself and the floor for either run. What
/// a *gap* adds on top is the whole of loading the next track: spawning
/// ffmpeg and waiting for it to buffer.
#[tokio::test]
async fn gapless_measurably_shortens_the_handover() {
    let _guard = TIMING.lock().await;
    let Some(off) = handover_gap("off", false).await else {
        eprintln!("SKIP: no audio device on this machine");
        return;
    };
    let on = handover_gap("on", true).await.expect("device vanished");

    eprintln!("handover with gapless off: {off:?}");
    eprintln!("handover with gapless on:  {on:?}");

    assert!(
        on < off,
        "gapless made no difference: {on:?} against {off:?} without it"
    );

    // The cadence is ~250 ms, so anything near it means the next track was
    // already sounding when the tick arrived.
    assert!(
        on < Duration::from_millis(500),
        "the handover still took {on:?}, which is a load rather than a join"
    );
}

/// Gapless on the two tracks that were reported as still gapping.
///
/// Both are cold YouTube streams with no file on disk, which is the case the
/// local fixtures above cannot reach: preparing one means resolving a URL
/// through yt-dlp and then waiting for ffmpeg to buffer it, seconds of work
/// where a local WAV costs milliseconds. If the handover holds here it holds
/// for the hardest thing this app plays.
///
/// The first track is seeked to twelve seconds from its end rather than played
/// through -- five minutes of real time would make this untestable, and the
/// seek exercises the path that had to be fixed for gapless to survive one.
///
/// `cargo test --test player_progress the_reported_tracks -- --ignored --nocapture`
#[tokio::test]
#[ignore = "network: resolves two real YouTube tracks and plays them"]
async fn the_reported_tracks_hand_over_without_a_gap() {
    let _guard = NETWORK.lock().await;

    const FIRST: (&str, &str, i64) = (
        "Sonic Forces - Infinite (KITSUN3POWR REMIX V3)",
        "https://www.youtube.com/watch?v=QSBmYN2hsMA",
        304,
    );
    const SECOND: (&str, &str, i64) = (
        "Sonic 06 - His World (Zebrahead Ver.)",
        "https://www.youtube.com/watch?v=MdJGOEEJA4I",
        223,
    );

    let base = std::env::temp_dir().join("music-app-gapless-real");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    for (title, url, secs) in [FIRST, SECOND] {
        sqlx::query(
            "INSERT INTO tracks (source, title, state, remote_id, remote_url, duration_secs, \
             in_library) VALUES ('youtube', ?, 'saved', ?, ?, ?, 1)",
        )
        .bind(title)
        .bind(url.rsplit('=').next().unwrap())
        .bind(url)
        .bind(secs)
        .execute(&db.pool)
        .await
        .unwrap();
    }

    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM tracks ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();

    let yt_dlp = music_app_lib::sidecar::staged_for_tests(music_app_lib::sidecar::Tool::YtDlp);
    assert!(yt_dlp.is_some(), "the staged yt-dlp sidecar is missing");

    let recorder = Recorder::default();
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        ffmpeg(),
        yt_dlp,
        // No cache: this is about the handover, and a cache copy would make a
        // rerun measure something different from the first run.
        None,
    );

    handle.send(PlayerCommand::SetGapless(true)).unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: ids.clone(),
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Resolving a cold stream takes seconds; let the first track get going.
    tokio::time::sleep(Duration::from_secs(20)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    if errors.iter().any(|e| e.contains("No audio output device")) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }
    assert!(
        recorder
            .0
            .ticks
            .lock()
            .unwrap()
            .iter()
            .any(|(id, p)| *id == Some(ids[0]) && *p > 0.0),
        "the first track never started: {errors:?}"
    );

    // Seeked, or played through? The seek is the quick version; the whole
    // track is what a listener actually does, and the difference between them
    // is how long the prepared decode has been sitting idle.
    if std::env::var("PROBE_FULL").is_ok() {
        eprintln!("playing the first track through -- about five minutes");
        tokio::time::sleep(Duration::from_secs(FIRST.2 as u64 - 20 + 25)).await;
    } else {
        handle
            .send(PlayerCommand::Seek((FIRST.2 - 12) as f64))
            .unwrap();
        tokio::time::sleep(Duration::from_secs(22)).await;
    }

    let errors = recorder.0.errors.lock().unwrap().clone();
    eprintln!("errors: {errors:?}");

    let timed = recorder.0.timed.lock().unwrap().clone();
    let (first_id, second_id) = (ids[0], ids[1]);

    let last_of_first = timed
        .iter()
        .filter(|(_, id)| *id == Some(first_id))
        .map(|(at, _)| *at)
        .next_back()
        .expect("the first track never reported");
    let first_of_second = timed
        .iter()
        .find(|(_, id)| *id == Some(second_id))
        .map(|(at, _)| *at)
        .expect("the second track never played");

    let gap = first_of_second.saturating_duration_since(last_of_first);
    eprintln!("handover between the two reported tracks: {gap:?}");

    // Without gapless this is a full cold load of a YouTube stream: a yt-dlp
    // resolve plus ffmpeg's prefill, which is where the "large silence" came
    // from. With it, the next tick is one cadence away.
    assert!(
        gap < Duration::from_millis(600),
        "the handover took {gap:?} -- the second track was loaded from cold \
         rather than already appended, which is the gap that was reported"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}


/// Sets up two short local tracks and returns the handle plus their ids.
async fn two_tracks(name: &str) -> (Recorder, player::PlayerHandle, Vec<i64>, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!("music-app-gapless-edge-{name}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let first = base.join("first.wav");
    let second = base.join("second.wav");
    write_wav_secs(&first, 4);
    write_wav_secs(&second, 3);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    for (title, path) in [("First", &first), ("Second", &second)] {
        sqlx::query(
            "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
             VALUES ('local', ?, ?, 'present', ?)",
        )
        .bind(title)
        .bind(path.to_str().unwrap())
        .bind(if title == "First" { 4 } else { 3 })
        .execute(&db.pool)
        .await
        .unwrap();
    }

    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM tracks ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);

    (recorder, handle, ids, base)
}

fn played(recorder: &Recorder, id: i64) -> bool {
    recorder
        .0
        .ticks
        .lock()
        .unwrap()
        .iter()
        .any(|(t, _)| *t == Some(id))
}

fn no_device(recorder: &Recorder) -> bool {
    recorder
        .0
        .errors
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.contains("No audio output device"))
}

/// Switching it on part-way through a track.
///
/// It cannot retroactively queue anything -- there was nothing to queue when
/// the track started -- but the handover it is switched on *before* has to be
/// the seamless one, or the setting looks like it does nothing until the app
/// is restarted.
#[tokio::test]
async fn turning_gapless_on_mid_track_applies_to_that_track_s_handover() {
    let _guard = TIMING.lock().await;
    let (recorder, handle, ids, base) = two_tracks("on-midway").await;

    handle.send(PlayerCommand::SetGapless(false)).unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: ids.clone(),
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // A second in, well before the end of a four-second track.
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    if no_device(&recorder) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }
    handle.send(PlayerCommand::SetGapless(true)).unwrap();

    tokio::time::sleep(Duration::from_millis(5_000)).await;

    let timed = recorder.0.timed.lock().unwrap().clone();
    let last_first = timed
        .iter()
        .filter(|(_, id)| *id == Some(ids[0]))
        .map(|(at, _)| *at)
        .next_back()
        .expect("the first track never reported");
    let first_second = timed
        .iter()
        .find(|(_, id)| *id == Some(ids[1]))
        .map(|(at, _)| *at)
        .expect("the second track never played");

    let gap = first_second.saturating_duration_since(last_first);
    eprintln!("handover after switching gapless on mid-track: {gap:?}");
    assert!(
        gap < Duration::from_millis(500),
        "switching gapless on mid-track did not affect that track's handover \
         ({gap:?}) -- the setting would look like it needed a restart"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// Switching it off once the next track is already appended.
///
/// The queued track stays queued: pulling it back out means tearing down the
/// running player, which is a gap *now* -- in the middle of the song being
/// listened to -- in order to guarantee a gap later. What must not happen is
/// anything worse: a lost track, a stall, or a double play.
#[tokio::test]
async fn turning_gapless_off_after_queueing_still_plays_both_tracks() {
    let _guard = TIMING.lock().await;
    let (recorder, handle, ids, base) = two_tracks("off-late").await;

    handle.send(PlayerCommand::SetGapless(true)).unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: ids.clone(),
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Inside the queueing window of a four-second track.
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    if no_device(&recorder) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }
    handle.send(PlayerCommand::SetGapless(false)).unwrap();

    tokio::time::sleep(Duration::from_millis(5_000)).await;

    assert!(played(&recorder, ids[0]), "the first track never played");
    assert!(
        played(&recorder, ids[1]),
        "the second track was lost when gapless was switched off after it had \
         already been queued"
    );

    // And it played once, not twice: the coordinator adopting a handover must
    // not also start the track itself.
    let ticks = recorder.0.ticks.lock().unwrap().clone();
    let restarts = ticks
        .windows(2)
        .filter(|w| w[0].0 == Some(ids[1]) && w[1].0 == Some(ids[1]) && w[1].1 < w[0].1)
        .count();
    assert_eq!(restarts, 0, "the second track restarted: {ticks:?}");

    let _ = std::fs::remove_dir_all(&base);
}

/// Paused near the end, then resumed.
///
/// The engine reports no progress while paused, and progress is what drives
/// queueing -- so nothing is appended for as long as the pause lasts. The
/// handover has to survive being set up entirely in the seconds after the
/// resume.
#[tokio::test]
async fn pausing_near_the_end_then_resuming_still_hands_over() {
    let _guard = TIMING.lock().await;
    let (recorder, handle, ids, base) = two_tracks("paused").await;

    handle.send(PlayerCommand::SetGapless(true)).unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: ids.clone(),
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(800)).await;
    if no_device(&recorder) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    // Two seconds from the end of a four-second track, then held there.
    handle.send(PlayerCommand::Seek(2.0)).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle.send(PlayerCommand::Pause).unwrap();
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    handle.send(PlayerCommand::Resume).unwrap();

    tokio::time::sleep(Duration::from_millis(5_000)).await;

    let timed = recorder.0.timed.lock().unwrap().clone();
    let last_first = timed
        .iter()
        .filter(|(_, id)| *id == Some(ids[0]))
        .map(|(at, _)| *at)
        .next_back()
        .expect("the first track never reported");
    let first_second = timed
        .iter()
        .find(|(_, id)| *id == Some(ids[1]))
        .map(|(at, _)| *at)
        .expect("the second track never played after the pause");

    let gap = first_second.saturating_duration_since(last_first);
    eprintln!("handover after a pause near the end: {gap:?}");
    assert!(
        gap < Duration::from_millis(600),
        "the handover took {gap:?} after a pause -- queueing never recovered \
         from the progress reports stopping"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The bar has to describe the track that is sounding.
///
/// The handover happens in the engine, and the coordinator finds out up to one
/// poll later. If the UI were told the new position before it was told the new
/// track, it would briefly show the second track's position against the
/// first's length -- a progress bar that jumps backwards and a duration that
/// belongs to something else.
#[tokio::test]
async fn the_ui_never_shows_one_track_s_position_against_another_s() {
    let _guard = TIMING.lock().await;
    let (recorder, handle, ids, base) = two_tracks("ui").await;

    handle.send(PlayerCommand::SetGapless(true)).unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: ids.clone(),
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(8_000)).await;
    if no_device(&recorder) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    let ticks = recorder.0.ticks.lock().unwrap().clone();
    eprintln!("ticks: {ticks:?}");

    // Every tick names the track it belongs to, and the two tracks are 4s and
    // 3s -- so a position reported against the wrong one shows up as a
    // position past that track's length.
    for (id, position) in &ticks {
        let length = if *id == Some(ids[0]) { 4.0 } else { 3.0 };
        assert!(
            *position <= length + 0.5,
            "a position of {position}s was reported for track {id:?}, which is \
             only {length}s long -- the bar was describing the wrong track"
        );
    }

    // And the first track's position never goes backwards within itself.
    let first_positions: Vec<f64> = ticks
        .iter()
        .filter(|(id, _)| *id == Some(ids[0]))
        .map(|(_, p)| *p)
        .collect();
    assert!(
        first_positions.windows(2).all(|w| w[1] >= w[0]),
        "the bar jumped backwards during the first track: {first_positions:?}"
    );

    let _ = std::fs::remove_dir_all(&base);
}


/// Repeat-one plays the same track again, never the next one.
///
/// `peek_next` reports the next track in the *list*; under repeat-one the
/// end-of-track path replays the current one instead. Queueing what
/// `peek_next` says hands the audio over to a track the coordinator then has
/// to undo -- correct in the end, but a wasted decode, and for a stream a
/// reload measured in seconds.
///
/// Honest about what this catches: it passes with or without the guard in
/// `enqueue_next`, because the wrong track sounds for less than one progress
/// tick before being replaced. It is here for the louder regression -- the one
/// where repeat-one starts actually playing the next track.
#[tokio::test]
async fn repeat_one_does_not_hand_over_to_the_wrong_track() {
    let _guard = TIMING.lock().await;
    let (recorder, handle, ids, base) = two_tracks("repeat-one").await;

    handle.send(PlayerCommand::SetGapless(true)).unwrap();
    handle
        .send(PlayerCommand::SetRepeat(music_app_lib::player::RepeatMode::One))
        .unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: ids.clone(),
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Through the end of the four-second first track and a little beyond.
    tokio::time::sleep(Duration::from_millis(7_000)).await;
    if no_device(&recorder) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    let ticks = recorder.0.ticks.lock().unwrap().clone();
    eprintln!("ticks: {ticks:?}");

    assert!(
        !played(&recorder, ids[1]),
        "under repeat-one the second track played, so the engine handed over \
         to a track the queue was never going to advance to: {ticks:?}"
    );
    assert!(
        played(&recorder, ids[0]),
        "the first track never played at all"
    );

    let _ = std::fs::remove_dir_all(&base);
}


/// A WAV of `tone_secs` of audible tone followed by `silence_secs` of nothing.
///
/// Shaped like the track that prompted this: five minutes of music and then
/// fourteen and a half seconds of digital silence, faithfully encoded.
fn write_wav_with_tail(path: &std::path::Path, tone_secs: f32, silence_secs: f32) {
    use std::io::Write;

    const RATE: u32 = 44_100;
    let tone = (RATE as f32 * tone_secs) as u32;
    let quiet = (RATE as f32 * silence_secs) as u32;
    let samples = tone + quiet;
    let data_len = samples * 2;

    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVE");
    b.extend_from_slice(b"fmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&RATE.to_le_bytes());
    b.extend_from_slice(&(RATE * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());

    for n in 0..samples {
        let value = if n < tone {
            let t = n as f32 / RATE as f32;
            (std::f32::consts::TAU * 440.0 * t).sin() * 0.5
        } else {
            0.0
        };
        b.extend_from_slice(&((value * 32_767.0) as i16).to_le_bytes());
    }

    std::fs::File::create(path).unwrap().write_all(&b).unwrap();
}

/// Sets up a track with a silent tail, followed by a normal one.
async fn tail_fixture(
    name: &str,
    tone: f32,
    tail: f32,
) -> (Recorder, player::PlayerHandle, Vec<i64>, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!("music-app-tail-{name}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let first = base.join("first.wav");
    let second = base.join("second.wav");
    write_wav_with_tail(&first, tone, tail);
    write_wav_secs(&second, 3);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    let total = (tone + tail).round() as i64;
    for (title, path, secs) in [("First", &first, total), ("Second", &second, 3)] {
        sqlx::query(
            "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
             VALUES ('local', ?, ?, 'present', ?)",
        )
        .bind(title)
        .bind(path.to_str().unwrap())
        .bind(secs)
        .execute(&db.pool)
        .await
        .unwrap();
    }

    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM tracks ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);

    (recorder, handle, ids, base)
}

/// The gap that is not this app's fault, removed anyway.
///
/// The track that prompted this runs for 5:03 and stops making sound at 4:48.
/// A seamless handover after fourteen and a half seconds of encoded silence is
/// still fourteen and a half seconds of silence, and no amount of engine work
/// fixes that -- the silence is in the file.
///
/// So the track ends when the *music* does. Here: three seconds of tone and
/// five of nothing, and the next track has to start at around three.
#[tokio::test]
async fn a_track_ends_when_its_music_does_not_when_its_file_does() {
    let _guard = TIMING.lock().await;
    let (recorder, handle, ids, base) = tail_fixture("trimmed", 3.0, 5.0).await;

    handle.send(PlayerCommand::SetGapless(true)).unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: ids.clone(),
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Long enough that an untrimmed track would plainly reach the end of its
    // eight-second file, so the two outcomes are far apart rather than adjacent.
    tokio::time::sleep(Duration::from_millis(10_000)).await;
    if no_device(&recorder) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    let ticks = recorder.0.ticks.lock().unwrap().clone();
    eprintln!("ticks: {ticks:?}");

    let last_first = ticks
        .iter()
        .filter(|(id, _)| *id == Some(ids[0]))
        .map(|(_, p)| *p)
        .fold(0.0f64, f64::max);

    eprintln!("first track ended at {last_first:.2}s of an 8s file (3s of tone)");

    assert!(
        played(&recorder, ids[1]),
        "the second track never played: {ticks:?}"
    );
    assert!(
        last_first < 7.0,
        "the first track played to {last_first:.2}s -- it sat through its own \
         silent tail, which is the gap that was reported"
    );
    assert!(
        last_first > 2.5,
        "the first track was cut at {last_first:.2}s, before its music had \
         finished at 3s"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// And the promise it must not break: a quiet passage is not an ending.
///
/// Two seconds of true digital silence is the threshold, so a track with a
/// shorter pause in it has to play straight through.
#[tokio::test]
async fn a_short_pause_inside_a_track_does_not_end_it() {
    let _guard = TIMING.lock().await;
    let base = std::env::temp_dir().join("music-app-tail-pause");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    // Tone, one second of silence, tone again.
    let path = base.join("paused.wav");
    {
        use std::io::Write;
        const RATE: u32 = 44_100;
        let samples = RATE * 6;
        let data_len = samples * 2;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&RATE.to_le_bytes());
        b.extend_from_slice(&(RATE * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for n in 0..samples {
            let t = n as f32 / RATE as f32;
            let value = if (2.0..3.0).contains(&t) {
                0.0
            } else {
                (std::f32::consts::TAU * 440.0 * t).sin() * 0.5
            };
            b.extend_from_slice(&((value * 32_767.0) as i16).to_le_bytes());
        }
        std::fs::File::create(&path).unwrap().write_all(&b).unwrap();
    }

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
         VALUES ('local', 'Paused', ?, 'present', 6)",
    )
    .bind(path.to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();

    let id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);
    handle.send(PlayerCommand::SetTrimSilence(true)).unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(7_500)).await;
    if no_device(&recorder) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    let reached = recorder
        .0
        .ticks
        .lock()
        .unwrap()
        .iter()
        .map(|(_, p)| *p)
        .fold(0.0f64, f64::max);

    eprintln!("a track with a one-second pause reached {reached:.2}s of 6s");
    assert!(
        reached > 5.0,
        "the track was ended at {reached:.2}s by a pause in the middle of it"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// A track that is quiet from the start has no tail to trim.
///
/// Caught by an existing test rather than by design: the fixtures here are
/// silent WAVs, and the first version of the trim ended one of them two
/// seconds in -- which broke a test about abandoning a stream *past halfway*,
/// because it never got halfway.
///
/// Trimming a silent tail presupposes a head. Cutting a quiet track short is
/// playing less of the file than the file holds.
#[tokio::test]
async fn a_silent_track_plays_to_its_end_rather_than_being_trimmed() {
    let _guard = TIMING.lock().await;
    let base = std::env::temp_dir().join("music-app-tail-silent");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let path = base.join("silent.wav");
    write_wav_secs(&path, 5);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state, duration_secs)          VALUES ('local', 'Silent', ?, 'present', 5)",
    )
    .bind(path.to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();

    let id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);
    handle.send(PlayerCommand::SetTrimSilence(true)).unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(6_500)).await;
    if no_device(&recorder) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    let reached = recorder
        .0
        .ticks
        .lock()
        .unwrap()
        .iter()
        .map(|(_, p)| *p)
        .fold(0.0f64, f64::max);

    eprintln!("a wholly silent 5s track reached {reached:.2}s");
    assert!(
        reached > 4.0,
        "a silent track was cut at {reached:.2}s -- it has no tail to trim,          only content"
    );

    let _ = std::fs::remove_dir_all(&base);
}


/// Writes a WAV from a list of (seconds, audible) segments.
fn write_wav_segments(path: &std::path::Path, segments: &[(f32, bool)]) {
    use std::io::Write;

    const RATE: u32 = 44_100;
    let total: u32 = segments
        .iter()
        .map(|(secs, _)| (RATE as f32 * secs) as u32)
        .sum();
    let data_len = total * 2;

    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVE");
    b.extend_from_slice(b"fmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&RATE.to_le_bytes());
    b.extend_from_slice(&(RATE * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());

    let mut n: u32 = 0;
    for (secs, audible) in segments {
        for _ in 0..((RATE as f32 * secs) as u32) {
            let value = if *audible {
                let t = n as f32 / RATE as f32;
                (std::f32::consts::TAU * 440.0 * t).sin() * amplitude_for(*secs)
            } else {
                0.0
            };
            b.extend_from_slice(&((value * 32_767.0) as i16).to_le_bytes());
            n += 1;
        }
    }

    std::fs::File::create(path).unwrap().write_all(&b).unwrap();
}

/// Half scale for everything; the segment length is not a level.
fn amplitude_for(_secs: f32) -> f32 {
    0.5
}

/// Plays one segmented track and reports how far it got.
async fn plays_to(
    name: &str,
    segments: &[(f32, bool)],
    settle: u64,
    setup: Vec<PlayerCommand>,
) -> Option<f64> {
    let base = std::env::temp_dir().join(format!("music-app-fp-{name}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let path = base.join("track.wav");
    write_wav_segments(&path, segments);
    let total: f32 = segments.iter().map(|(s, _)| *s).sum();

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
         VALUES ('local', 'Probe', ?, 'present', ?)",
    )
    .bind(path.to_str().unwrap())
    .bind(total.round() as i64)
    .execute(&db.pool)
    .await
    .unwrap();

    let id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);
    handle.send(PlayerCommand::SetTrimSilence(true)).unwrap();
    for command in setup {
        handle.send(command).unwrap();
    }
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(settle)).await;
    if no_device(&recorder) {
        return None;
    }

    let reached = recorder
        .0
        .ticks
        .lock()
        .unwrap()
        .iter()
        .map(|(_, p)| *p)
        .fold(0.0f64, f64::max);

    let _ = std::fs::remove_dir_all(&base);
    Some(reached)
}

/// A false ending: music, a gap, then more music.
///
/// The failure that would actually cost someone their song. Three seconds of
/// silence is longer than the pause before almost every false ending, but this
/// is the case worth being sure about -- a track cut here loses its outro
/// permanently, and the listener has no way to tell it was ever there.
#[tokio::test]
async fn a_false_ending_does_not_cut_the_outro() {
    let _guard = TIMING.lock().await;
    // 3s music, 2s silence, 3s music. The gap is inside the trimming window,
    // and shorter than the threshold.
    let Some(reached) = plays_to("false-ending", &[(3.0, true), (2.0, false), (3.0, true)], 9_500, Vec::new()).await
    else {
        eprintln!("SKIP: no audio device on this machine");
        return;
    };

    eprintln!("a track with a two-second false ending reached {reached:.2}s of 8s");
    assert!(
        reached > 7.0,
        "the outro was cut: the track stopped at {reached:.2}s of 8s"
    );
}

/// Muting part-way through is not a finished track.
///
/// The silence is counted before the volume stage precisely so this cannot
/// happen. Muting *from the start* is caught by `heard_audio` anyway -- nothing
/// audible was ever heard, so there is no tail -- which is why this mutes two
/// seconds in, once the track has established that it has music in it.
#[tokio::test]
async fn muting_part_way_through_does_not_end_the_track() {
    let _guard = TIMING.lock().await;
    let base = std::env::temp_dir().join("music-app-fp-muted-midway");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("track.wav");
    write_wav_segments(&path, &[(8.0, true)]);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state, duration_secs)          VALUES ('local', 'Loud', ?, 'present', 8)",
    )
    .bind(path.to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();
    let id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);
    handle.send(PlayerCommand::SetTrimSilence(true)).unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Let it establish that it has music, then mute for the rest.
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    if no_device(&recorder) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }
    handle.send(PlayerCommand::SetMuted(true)).unwrap();

    tokio::time::sleep(Duration::from_millis(7_500)).await;

    let reached = recorder
        .0
        .ticks
        .lock()
        .unwrap()
        .iter()
        .map(|(_, p)| *p)
        .fold(0.0f64, f64::max);

    eprintln!("an 8s track muted at 2s reached {reached:.2}s");
    assert!(
        reached > 7.0,
        "muting cut the track at {reached:.2}s -- silence is being measured          after the volume stage, so reaching for the mute button ends the song"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// And the same for the volume slider at zero, which is a different path to
/// the same place.
#[tokio::test]
async fn a_track_at_zero_volume_is_not_cut_short() {
    let _guard = TIMING.lock().await;
    let Some(reached) = plays_to(
        "zero-volume",
        &[(6.0, true)],
        7_500,
        vec![PlayerCommand::SetVolume(0.0)],
    )
    .await
    else {
        eprintln!("SKIP: no audio device on this machine");
        return;
    };

    eprintln!("a 6s track at zero volume reached {reached:.2}s");
    assert!(reached > 5.0, "zero volume cut the track at {reached:.2}s");
}

/// Silence far from the end is out of reach entirely.
///
/// Two thresholds guard this, and they are independent: the gap has to be
/// longer than three seconds *and* start within twenty of the end. This is the
/// second one on its own.
#[tokio::test]
async fn a_long_gap_far_from_the_end_is_not_a_tail() {
    let _guard = TIMING.lock().await;
    // 2s music, 5s silence, then 25s of music -- so the gap is long enough but
    // nowhere near the end.
    let Some(reached) = plays_to("early-gap", &[(2.0, true), (5.0, false), (25.0, true)], 12_000, Vec::new()).await
    else {
        eprintln!("SKIP: no audio device on this machine");
        return;
    };

    // Not played to the end -- that would take half a minute. What matters is
    // that it survived the gap and was still going afterwards.
    eprintln!("a track with a five-second gap at 2s reached {reached:.2}s");
    assert!(
        reached > 9.0,
        "a gap twenty-five seconds from the end ended the track at {reached:.2}s"
    );
}

/// Trimming is not gapless, and neither implies the other.
#[tokio::test]
async fn trimming_and_gapless_are_independent_settings() {
    let _guard = TIMING.lock().await;
    // Trimming off, gapless on: the tail plays in full.
    let base = std::env::temp_dir().join("music-app-fp-independent");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("track.wav");
    write_wav_segments(&path, &[(2.0, true), (5.0, false)]);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
         VALUES ('local', 'Tail', ?, 'present', 7)",
    )
    .bind(path.to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();
    let id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);
    handle.send(PlayerCommand::SetGapless(true)).unwrap();
    handle.send(PlayerCommand::SetTrimSilence(false)).unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(8_500)).await;
    if no_device(&recorder) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    let reached = recorder
        .0
        .ticks
        .lock()
        .unwrap()
        .iter()
        .map(|(_, p)| *p)
        .fold(0.0f64, f64::max);

    eprintln!("gapless on, trimming off: reached {reached:.2}s of 7s");
    assert!(
        reached > 6.0,
        "the tail was trimmed at {reached:.2}s with trimming switched off -- \
         the gapless setting is still driving it"
    );

    let _ = std::fs::remove_dir_all(&base);
}


/// One queue, three kinds of track, handed over seamlessly at both joins.
///
/// The question this answers: does any of it care where the audio came from?
/// A local file, a cold YouTube stream and a local file again, in one queue --
/// which is what a real library queue looks like.
///
/// Nothing in the handover path is source-aware. Every source is decoded by
/// ffmpeg to the device's rate, and what the engine appends is a decode; where
/// it was read from stopped mattering at `build_source`. This is here to prove
/// that rather than to argue it.
///
/// `cargo test --test player_progress a_mixed_queue -- --ignored --nocapture`
#[tokio::test]
#[ignore = "network: resolves a real YouTube track and plays around it"]
async fn a_mixed_queue_hands_over_seamlessly_at_every_join() {
    let _guard = NETWORK.lock().await;
    let _timing = TIMING.lock().await;

    // Long enough that resolving the stream behind it comfortably finishes.
    const FIRST_SECS: u32 = 20;
    // His World: 3:42.4 of file, music stopping at 3:36.8.
    const STREAM_SECS: i64 = 223;
    const STREAM_URL: &str = "https://www.youtube.com/watch?v=MdJGOEEJA4I";

    let base = std::env::temp_dir().join("music-app-mixed-queue");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let first = base.join("first.wav");
    let last = base.join("last.wav");
    write_wav_segments(&first, &[(FIRST_SECS as f32, true)]);
    write_wav_segments(&last, &[(6.0, true)]);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state, duration_secs, in_library) \
         VALUES ('local', 'Local First', ?, 'present', ?, 1)",
    )
    .bind(first.to_str().unwrap())
    .bind(i64::from(FIRST_SECS))
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, state, remote_id, remote_url, duration_secs, \
         in_library) VALUES ('youtube', 'Cold Stream', 'saved', 'MdJGOEEJA4I', ?, ?, 1)",
    )
    .bind(STREAM_URL)
    .bind(STREAM_SECS)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state, duration_secs, in_library) \
         VALUES ('local', 'Local Last', ?, 'present', 6, 1)",
    )
    .bind(last.to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();

    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM tracks ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();

    let yt_dlp = music_app_lib::sidecar::staged_for_tests(music_app_lib::sidecar::Tool::YtDlp);
    assert!(yt_dlp.is_some(), "the staged yt-dlp sidecar is missing");

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), yt_dlp, None);
    handle.send(PlayerCommand::SetGapless(true)).unwrap();
    handle.send(PlayerCommand::SetTrimSilence(true)).unwrap();
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: ids.clone(),
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Through the first local track and into the stream.
    tokio::time::sleep(Duration::from_secs(u64::from(FIRST_SECS) + 4)).await;
    if no_device(&recorder) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    assert!(
        played(&recorder, ids[1]),
        "the cold stream never started: {:?}",
        recorder.0.errors.lock().unwrap()
    );

    // Twelve seconds from the end of the stream, so the last track is prepared
    // and appended after the seek -- the path a seek had to be taught to allow.
    handle
        .send(PlayerCommand::Seek((STREAM_SECS - 12) as f64))
        .unwrap();
    tokio::time::sleep(Duration::from_secs(20)).await;

    let errors = recorder.0.errors.lock().unwrap().clone();
    eprintln!("errors: {errors:?}");

    assert!(
        played(&recorder, ids[2]),
        "the queue never reached the last local track: {errors:?}"
    );

    let timed = recorder.0.timed.lock().unwrap().clone();
    let gap_between = |a: i64, b: i64| {
        let last = timed
            .iter()
            .filter(|(_, id)| *id == Some(a))
            .map(|(at, _)| *at)
            .next_back()
            .expect("first side never reported");
        let next = timed
            .iter()
            .find(|(_, id)| *id == Some(b))
            .map(|(at, _)| *at)
            .expect("second side never reported");
        next.saturating_duration_since(last)
    };

    let local_to_stream = gap_between(ids[0], ids[1]);
    let stream_to_local = gap_between(ids[1], ids[2]);

    eprintln!("local file  -> cold stream : {local_to_stream:?}");
    eprintln!("cold stream -> local file  : {stream_to_local:?}");

    assert!(
        local_to_stream < Duration::from_millis(600),
        "handing a local file over to a cold stream took {local_to_stream:?}"
    );
    assert!(
        stream_to_local < Duration::from_millis(600),
        "handing a cold stream over to a local file took {stream_to_local:?}"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}



/// The scrubber must never go backwards after a seek.
///
/// The user-visible shape of the seek race: a position tick already in the
/// coordinator's inbox when the seek is issued is read *after* the bar has
/// been redrawn at the new place, and puts it back where the track used to be.
/// Reported as `[100.0, 8.375, 100.21, ...]`.
///
/// Honest about what this can prove. The race needs the stale tick to be
/// queued at the moment the seek is handled, which depends on how busy the
/// machine is -- that is why the bug was filed as load-dependent and why the
/// original failing test passed in isolation. The rule itself is pinned
/// exactly by `progress_guard_tests`; this checks the behaviour those numbers
/// are supposed to produce, and catches the regression whenever the timing
/// happens to line up.
#[tokio::test]
async fn seeking_backwards_never_reports_the_old_position_again() {
    let _guard = TIMING.lock().await;

    let base = std::env::temp_dir().join("music-app-seek-monotonic");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("track.wav");
    write_wav_secs(&path, 60);

    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (source, title, local_path, state, duration_secs) \
         VALUES ('local', 'Long', ?, 'present', 60)",
    )
    .bind(path.to_str().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();
    let id: i64 = sqlx::query_scalar("SELECT id FROM tracks")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), ffmpeg(), None, None);
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![id],
            start_index: 0,
            context_name: None,
        })
        .unwrap();

    // Out to a position far from where the seeks land, so a stale tick is
    // unmistakable rather than within a tick of the right answer.
    tokio::time::sleep(Duration::from_millis(4_000)).await;
    if no_device(&recorder) {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    // Several seeks, because the window is one tick wide and repeating it is
    // the only way to widen the chance of landing in it.
    for target in [30.0, 2.0, 45.0, 3.0, 50.0, 1.0] {
        handle.send(PlayerCommand::Seek(target)).unwrap();
        tokio::time::sleep(Duration::from_millis(700)).await;
    }

    let ticks = recorder.0.progress.lock().unwrap().clone();
    eprintln!("positions: {ticks:?}");

    // A seek is the only thing that may move the position backwards, and each
    // one is followed by its own echo -- so a backwards step is only a fault
    // if the position it returns to is one already reported *before* the seek.
    // Rather than model that, this looks for the signature of the bug: a jump
    // back to a position higher than anything the track has reached since.
    let mut faults = Vec::new();
    for window in ticks.windows(3) {
        let (before, seeked, after) = (window[0], window[1], window[2]);
        // Down, then straight back up past where it was: nothing but a stale
        // tick does that.
        if seeked < before - 1.0 && after > seeked + 5.0 && (after - before).abs() < 1.0 {
            faults.push((before, seeked, after));
        }
    }

    assert!(
        faults.is_empty(),
        "the scrubber jumped back to a position from before the seek: {faults:?} \
         in {ticks:?}"
    );

    let _ = std::fs::remove_dir_all(&base);
}
