//! End-to-end over the path only the engine can drive.
//!
//! Progress and end-of-track are reported *by the engine*, never by a command,
//! so a break here is invisible from the command API — the play button keeps
//! working perfectly while the progress bar sits at zero. That is exactly the
//! failure this test exists to catch.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use music_app_lib::player::{self, PlayerCommand, PlayerEvents, PlayerProgress, PlayerStatus};

#[derive(Default)]
struct Captured {
    progress: Mutex<Vec<f64>>,
    states: Mutex<Vec<PlayerStatus>>,
    errors: Mutex<Vec<String>>,
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
}

/// A real 3-second PCM WAV, so the engine has something genuine to decode.
fn write_wav(path: &std::path::Path) {
    use std::io::Write;

    let samples: u32 = 44100 * 3;
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
    let handle = player::spawn(recorder.clone(), db.pool.clone());

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![track_id],
            start_index: 0,
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
