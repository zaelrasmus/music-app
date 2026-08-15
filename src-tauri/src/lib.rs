pub mod db;
mod download;
mod engine;
mod library;
mod playable;
mod playlists;
pub mod player;
mod providers;
mod queue;
mod scanner;
mod search;
mod sidecar;
mod stream_urls;
mod tags;
mod transcode;
mod tracks;
mod youtube;

use tauri::Manager;

// DONE: library folders -- add / list / remove, persisted in SQLite.
// DONE: scan pipeline -- walkdir + lofty populate `tracks`, mtime-skip on
//       rescan, vanished files marked 'missing' rather than deleted.
// DONE: minimal player -- rodio on a dedicated audio thread, driven by a
//       command channel. Source resolution goes through `playable.rs`.
// DONE: queue, repeat, shuffle, volume/mute. The engine stays dumb; every
//       policy decision lives in the coordinator, which is the sole owner of
//       the queue.
// DONE: seek + progress ticks + the real player bar.
// DONE: playlists, tags, group-by-artist, FTS search, in-playlist filtering.
// DONE: two-tier queue -- a consumed manual queue that outranks the context
//       queue. Shuffle and repeat act on the context alone.
//
// TODO: cover art extraction into `tracks.cover_path` (column already exists).
// TODO: removing tracks from the context preview (the permutation makes this
//       more work than it looks; the preview is read-only for now).

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;

            // Blocking here, rather than spawning, makes "the database is
            // ready" an invariant the frontend can rely on -- otherwise the
            // window can load and call a command before the pool is managed.
            let db = tauri::async_runtime::block_on(db::init(&data_dir))?;
            let pool = db.pool.clone();
            app.manage(db);
            app.manage(scanner::ScanLock::new());
            app.manage(download::DownloadLock::new());

            // Coordinator (queue, repeat, shuffle, volume) plus the dumb audio
            // engine it drives. The engine thread owns the output device for
            // the whole process.
            // Optional on purpose: without ffmpeg the app still plays every
            // format rodio handles natively, and only Opus files report a
            // clear "ffmpeg not found" instead of failing obscurely.
            let ffmpeg = sidecar::resolve(app.handle(), sidecar::Tool::Ffmpeg)
                .ok()
                .map(|found| found.path);

            // Resolved once: the coordinator needs it to turn a saved YouTube
            // track into a playable stream.
            let yt_dlp = sidecar::resolve(app.handle(), sidecar::Tool::YtDlp)
                .ok()
                .map(|found| found.path);

            app.manage(player::spawn(app.handle().clone(), pool, ffmpeg, yt_dlp));

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            library::add_library_folder,
            library::list_library_folders,
            library::remove_library_folder,
            tracks::list_tracks,
            tracks::rescan_library,
            tracks::update_track_metadata,
            playlists::create_playlist,
            playlists::rename_playlist,
            playlists::delete_playlist,
            playlists::list_playlists,
            playlists::get_playlist,
            playlists::add_tracks_to_playlist,
            playlists::remove_track_from_playlist,
            playlists::reorder_playlist_track,
            tags::assign_tag,
            tags::remove_tag_from_track,
            tags::list_tags,
            tags::list_track_tags,
            tags::rename_tag,
            tags::delete_tag,
            search::query_library,
            search::group_tracks_by_artist,
            download::download_track,
            download::delete_download,
            player::play_queue,
            player::play_next,
            player::add_to_queue,
            player::remove_from_queue,
            player::reorder_queue,
            player::clear_queue,
            player::request_queue_state,
            player::toggle_play_pause,
            player::next_track,
            player::previous_track,
            player::stop,
            player::set_volume,
            player::set_muted,
            player::set_repeat,
            player::set_shuffle,
            player::seek,
            providers::list_providers,
            youtube::search_provider,
            youtube::save_remote_track,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
