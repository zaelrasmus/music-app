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

#[derive(Default)]
struct Captured {
    progress: Mutex<Vec<f64>>,
    /// Background cache fills, as (track, title). `None` is the finish.
    caching: Mutex<Vec<(i64, Option<String>)>>,
    /// Progress again, but keeping which track each tick was about.
    ticks: Mutex<Vec<(Option<i64>, f64)>>,
    states: Mutex<Vec<PlayerStatus>>,
    errors: Mutex<Vec<String>>,
    queues: Mutex<Vec<QueueState>>,
}

/// Newtype so the impl is local (orphan rule).
#[derive(Clone, Default)]
struct Recorder(Arc<Captured>);

impl PlayerEvents for Recorder {
    fn state(&self, status: PlayerStatus) {
        self.0.states.lock().unwrap().push(status);
    }

    fn progress(&self, progress: PlayerProgress) {
        self.0.progress.lock().unwrap().push(progress.position_secs);
        self.0
            .ticks
            .lock()
            .unwrap()
            .push((progress.track_id, progress.position_secs));
    }

    fn error(&self, message: String) {
        self.0.errors.lock().unwrap().push(message);
    }

    fn caching(&self, track_id: i64, title: Option<String>) {
        self.0.caching.lock().unwrap().push((track_id, title));
    }

    fn queue(&self, queue: QueueState) {
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
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None, None);

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
        Some(std::path::PathBuf::from("ffmpeg")),
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
        Some(std::path::PathBuf::from("ffmpeg")),
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
        Some(std::path::PathBuf::from("ffmpeg")),
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
        Some(std::path::PathBuf::from("ffmpeg")),
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
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None, None);

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
        Some(std::path::PathBuf::from("ffmpeg")),
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
        Some(std::path::PathBuf::from("ffmpeg")),
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
        Some(std::path::PathBuf::from("ffmpeg")),
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
        Some(std::path::PathBuf::from("ffmpeg")),
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
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None, None);

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
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None, None);

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
        Some(std::path::PathBuf::from("ffmpeg")),
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
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None, None);

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
    // The tool paths are never reached: the fetch they would run happens after
    // the announcement, and fails immediately with nothing at these paths.
    let handle = player::spawn(
        recorder.clone(),
        db.pool.clone(),
        Some(base.join("no-such-ffmpeg.exe")),
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
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None, None);

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
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None, None);

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
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None, None);

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
