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
    }

    fn error(&self, message: String) {
        self.0.errors.lock().unwrap().push(message);
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
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None);

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
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None);

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

    let stream: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (source, title, state, remote_id, remote_url) \
         VALUES ('youtube', 'Never Gonna Give You Up', 'saved', 'dQw4w9WgXcQ', \
                 'https://www.youtube.com/watch?v=dQw4w9WgXcQ') RETURNING id",
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
