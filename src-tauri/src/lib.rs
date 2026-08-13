// TODO 1: Scan directories and store the directories scanned for the user with WalkDir. The user can select various directories and put a button for reescan. The audio files in these directories should be showed.
//


#[tauri::command]
pub async fn scan_directories() -> Result

// TODO 2: When the user can scan directories and the app obtain the audio files, scan the audio files and extract the metadata with lofty
//
//
// TODO 3: Store the metadata of the audio files in the database with SQlite
//
//
// TODO 4: Fetch the audio files from the database and display them in the UI. Should be updated if the directorie are re-scanned or if more directories are added
//
// TODO 5: Enter with the player with the audioFiles
//
//
//

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
