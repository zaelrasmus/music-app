pub mod audio_cache;
mod collections;
mod covers;
pub mod db;
mod download;
mod downloads;
mod engine;
mod library;
mod playable;
pub mod playlists;
pub mod player;
mod providers;
mod queue;
mod scanner;
mod search;
mod sidecar;
mod soundcloud;
mod stream_urls;
mod tags;
mod transcode;
mod tracks;
mod updater;
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
// DONE: cover art -- a content-addressed store fed by embedded tag pictures,
//       provider thumbnails and user-picked playlist images, served to the
//       webview over the asset protocol scoped to that one directory.
//
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

            // The download queue and the one worker that drains it. Managed
            // before the worker starts, because the worker reads the queue
            // back out of managed state rather than keeping its own copy.
            let (queue, doorbell) = downloads::Downloads::new();
            app.manage(queue);

            // yt-dlp updates itself by rewriting its own executable, which the
            // install directory does not permit. Staging a copy in app data is
            // what makes updating possible at all, and doing it *before* the
            // resolves below is what makes the path stable: from here on every
            // caller gets the app-data copy, so a binary replaced underneath is
            // used by the next spawn rather than after a restart.
            //
            // Failure is deliberately ignored: `resolve` still finds the
            // bundled copy, and a stale yt-dlp beats an app that will not open.
            //
            // ffmpeg is not staged. It does not break when YouTube changes, and
            // it is 110 MB of copying to no purpose.
            let staged = matches!(
                sidecar::seed(app.handle(), sidecar::Tool::YtDlp),
                Ok(sidecar::Seed::Refreshed)
            );

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

            // Disposable copies of streamed audio, so replaying or seeking
            // backwards does not fetch the same bytes twice.
            // One instance, shared: the settings commands and the player must
            // agree about the limit, or changing it would only take effect on
            // the next launch.
            let audio_cache = audio_cache::AudioCache::new(data_dir.join("cache").join("audio"));
            app.manage(audio_cache.clone());

            // Cover art. Not under `cache/`: entries here are cheap to
            // rebuild but are referenced by id from the database, so clearing
            // them behind the app is a visible loss rather than a free one.
            let cover_store = covers::CoverStore::new(data_dir.join("covers"));
            app.manage(cover_store.clone());

            // Drains the download queue, one track at a time, for as long as
            // the app runs. Started here because it needs the pool and the
            // cover store, and both are about to be moved elsewhere.
            downloads::spawn_worker(
                app.handle().clone(),
                pool.clone(),
                cover_store.clone(),
                doorbell,
            );

            // Reclaim covers nothing points at any more, once, in the
            // background.
            //
            // The sweep already runs after every scan, but a library of
            // streamed tracks may go months without one -- and the migration
            // that stopped auditions from keeping artwork has just released a
            // batch of files that no row references. Waiting for a rescan to
            // collect them would mean the fix does nothing for the people who
            // already have the problem.
            //
            // Safe to run here specifically because nothing fetches a cover at
            // startup: the only writers are keeping a track and finishing a
            // download, both of which need a user.
            {
                let pool = pool.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = scanner::sweep_covers(&pool, &cover_store).await;
                });
            }

            app.manage(player::spawn(
                app.handle().clone(),
                pool,
                ffmpeg,
                yt_dlp,
                Some(audio_cache),
            ));

            // Keeps yt-dlp current. Managed before it is started so the first
            // status event has somewhere to land, and started last so that
            // nothing above it waits on a network call.
            //
            // `staged` carries one fact the updater cannot work out for
            // itself: that the binary underneath it was written moments ago
            // by an install or an upgrade, which is worth a check of its own
            // rather than a place in the daily rota.
            app.manage(updater::Updater::new());
            updater::spawn(app.handle(), staged);

            // The window is created hidden so that the first frame the user
            // sees is already themed and laid out -- an undecorated window
            // otherwise flashes as a bare white rectangle while the frontend
            // boots, and it has no title bar to explain what it is.
            //
            // The frontend reveals it as soon as the layout mounts. This is the
            // safety net for when it never does: a bundle that fails to load
            // would otherwise leave no window at all, which is indistinguishable
            // from the app not starting. Showing late is a bad first frame;
            // never showing is a bug report with nothing to look at.
            if let Some(window) = app.get_webview_window("main") {
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    // A no-op if the frontend got there first.
                    let _ = window.show();
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            library::add_library_folder,
            library::list_library_folders,
            library::remove_library_folder,
            tracks::list_tracks,
            tracks::recently_played,
            tracks::rescan_library,
            tracks::update_track_metadata,
            tracks::set_in_library,
            playlists::create_playlist,
            playlists::rename_playlist,
            playlists::delete_playlist,
            playlists::list_playlists,
            playlists::get_playlist,
            playlists::add_tracks_to_playlist,
            playlists::remove_track_from_playlist,
            playlists::reorder_playlist_track,
            playlists::list_library_artists,
            playlists::add_playlist_artist_rule,
            playlists::remove_playlist_artist_rule,
            tags::assign_tag,
            tags::remove_tag_from_track,
            tags::list_tags,
            tags::list_track_tags,
            tags::rename_tag,
            tags::set_tag_color,
            tags::list_tag_colors,
            covers::cover_dir,
            covers::set_playlist_cover,
            covers::clear_playlist_cover,
            tags::delete_tag,
            search::query_library,
            search::group_tracks_by_artist,
            downloads::download_track,
            downloads::download_playlist,
            downloads::download_activity,
            downloads::cancel_download,
            downloads::clear_finished_downloads,
            download::delete_download,
            player::play_queue,
            player::play_next,
            player::add_to_queue,
            player::remove_from_queue,
            player::play_queued_entry,
            player::play_upcoming,
            player::reorder_queue,
            player::clear_queue,
            player::request_queue_state,
            player::restore_playback,
            player::set_keep_abandoned,
            player::toggle_play_pause,
            player::next_track,
            player::previous_track,
            player::stop,
            player::set_volume,
            player::set_muted,
            player::set_repeat,
            player::set_shuffle,
            player::seek,
            audio_cache::audio_cache_stats,
            audio_cache::set_audio_cache_limit,
            audio_cache::clear_audio_cache,
            audio_cache::cached_track_ids,
            providers::list_providers,
            youtube::search_provider,
            youtube::save_remote_track,
            youtube::save_remote_tracks,
            collections::search_collections,
            collections::expand_collection,
            collections::max_expanded_tracks,
            playlists::import_playlist,
            updater::yt_dlp_status,
            updater::update_yt_dlp,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
