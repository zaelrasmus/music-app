//! The two-tier queue, end to end through the real coordinator.
//!
//! `queue.rs` proves the priority rule in isolation. What it cannot prove is
//! that the coordinator actually *asks* — that both the Next command and the
//! engine's end-of-track report route through the same call, and that the
//! queue panel payload reflects what will really play. A wiring mistake there
//! is invisible to the unit tests and obvious to the user.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use music_app_lib::player::{
    self, PlayerCommand, PlayerEvents, PlayerProgress, PlayerStatus, QueueState,
};

#[derive(Default)]
struct Captured {
    states: Mutex<Vec<PlayerStatus>>,
    errors: Mutex<Vec<String>>,
    queues: Mutex<Vec<QueueState>>,
}

#[derive(Clone, Default)]
struct Recorder(Arc<Captured>);

impl PlayerEvents for Recorder {
    fn state(&self, status: PlayerStatus) {
        self.0.states.lock().unwrap().push(status);
    }

    fn progress(&self, _progress: PlayerProgress) {}

    fn error(&self, message: String) {
        self.0.errors.lock().unwrap().push(message);
    }

    fn queue(&self, queue: QueueState) {
        self.0.queues.lock().unwrap().push(queue);
    }
}

impl Recorder {
    /// The order tracks actually became current, with repeats collapsed.
    fn played(&self) -> Vec<i64> {
        let mut order: Vec<i64> = Vec::new();
        for status in self.0.states.lock().unwrap().iter() {
            if let Some(id) = status.track_id {
                if order.last() != Some(&id) {
                    order.push(id);
                }
            }
        }
        order
    }

    fn no_audio_device(&self) -> bool {
        self.0
            .errors
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.contains("No audio output device"))
    }

    fn latest_queue(&self) -> QueueState {
        self.0
            .queues
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("the coordinator emits a queue state after every command")
    }
}

/// A real PCM WAV of `millis`, short so a whole queue drains inside a test.
fn write_wav(path: &std::path::Path, millis: u32) {
    use std::io::Write;

    let samples = 44100 * millis / 1000;
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

/// Four playable local tracks named A..D, returned as ids in that order.
///
/// `millis` is per track. Tests that assert on *ordering* want them short
/// enough to drain quickly; tests that assert on a *snapshot* want them long
/// enough that nothing advances underneath the assertion.
async fn fixture(
    base: &std::path::Path,
    millis: u32,
) -> (music_app_lib::db::Db, Vec<i64>) {
    let db = music_app_lib::db::init(&base.join("data")).await.unwrap();

    let mut ids = Vec::new();
    for name in ["A", "B", "C", "D"] {
        let wav = base.join(format!("{name}.wav"));
        write_wav(&wav, millis);

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

    (db, ids)
}

#[tokio::test]
async fn a_queued_track_interrupts_the_context_then_the_context_resumes() {
    let base = std::env::temp_dir().join("music-app-two-tier-priority");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let (db, ids) = fixture(&base, 400).await;
    let (a, b, c, d) = (ids[0], ids[1], ids[2], ids[3]);

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None, None);

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![a, b, c],
            start_index: 0,
            context_name: Some("Test context".into()),
        })
        .unwrap();

    // Queue D while A is playing. It must land between A and B.
    handle.send(PlayerCommand::AddToQueue(d)).unwrap();

    // Four 400ms tracks, plus up to a 200ms poll interval to notice each end.
    tokio::time::sleep(Duration::from_secs(5)).await;

    if recorder.no_audio_device() {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    assert_eq!(
        recorder.played(),
        vec![a, d, b, c],
        "the queued track should play after A and before the rest of the context"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn changing_context_keeps_the_queue_and_the_panel_shows_it() {
    let base = std::env::temp_dir().join("music-app-two-tier-context-change");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let (db, ids) = fixture(&base, 3000).await;
    let (a, b, c, d) = (ids[0], ids[1], ids[2], ids[3]);

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None, None);

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![a, b],
            start_index: 0,
            context_name: Some("First".into()),
        })
        .unwrap();
    handle.send(PlayerCommand::AddToQueue(d)).unwrap();

    // The subtle one: switching playlists must not discard what the user
    // interposed.
    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![c],
            start_index: 0,
            context_name: Some("Second".into()),
        })
        .unwrap();

    tokio::time::sleep(Duration::from_millis(800)).await;

    let queue = recorder.latest_queue();

    assert_eq!(
        queue.manual.iter().map(|e| e.track_id).collect::<Vec<_>>(),
        vec![d],
        "the manual queue must survive a context change"
    );
    assert_eq!(
        queue.manual[0].title, "D",
        "the panel hydrates titles itself, so a just-saved track still shows"
    );
    assert!(
        queue.manual[0].entry_id.is_some(),
        "manual rows must be addressable, or removal targets the wrong one"
    );
    assert_eq!(queue.context_name.as_deref(), Some("Second"));

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn queueing_with_nothing_playing_starts_playback() {
    let base = std::env::temp_dir().join("music-app-two-tier-autostart");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let (db, ids) = fixture(&base, 400).await;

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None, None);

    // No context at all: nothing would ever pick this up without the
    // start-if-idle rule, and "Add to queue" would look broken on a fresh
    // launch.
    handle.send(PlayerCommand::AddToQueue(ids[0])).unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    if recorder.no_audio_device() {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    assert_eq!(recorder.played(), vec![ids[0]]);

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}

/// A missing file must not swallow the tracks queued behind it.
///
/// The skip-on-failure loop advances the queue itself, so if it stepped the
/// context directly it would consume the manual entry as collateral.
#[tokio::test]
async fn skipping_an_unplayable_track_still_honours_the_queue() {
    let base = std::env::temp_dir().join("music-app-two-tier-skip");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let (db, ids) = fixture(&base, 1000).await;

    // Point A at a file that is not there, exactly as a deleted file would.
    let gone = base.join("gone.wav");
    sqlx::query("UPDATE tracks SET local_path = ? WHERE id = ?")
        .bind(gone.to_str().unwrap())
        .bind(ids[0])
        .execute(&db.pool)
        .await
        .unwrap();

    let recorder = Recorder::default();
    let handle = player::spawn(recorder.clone(), db.pool.clone(), None, None, None);

    handle
        .send(PlayerCommand::PlayQueue {
            track_ids: vec![ids[0], ids[1]],
            start_index: 0,
            context_name: None,
        })
        .unwrap();
    handle.send(PlayerCommand::AddToQueue(ids[3])).unwrap();

    tokio::time::sleep(Duration::from_secs(3)).await;

    if recorder.no_audio_device() {
        eprintln!("SKIP: no audio device on this machine");
        return;
    }

    let played = recorder.played();
    assert!(
        played.contains(&ids[3]),
        "the queued track was consumed while skipping a dead one: {played:?}"
    );

    db.pool.close().await;
    let _ = std::fs::remove_dir_all(&base);
}
