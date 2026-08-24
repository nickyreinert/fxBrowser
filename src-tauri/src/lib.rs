mod commands;
mod db;
mod dsp;
mod indexer;
mod player;
mod sidecar;
mod waveform;

use player::Player;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

pub struct AppState {
    pub db: Mutex<Connection>,
    pub db_path: PathBuf,
    pub player: Player,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_drag::init())
        .setup(|app| {
            let app_data_dir = app
                .path()
                .app_data_dir()
                .expect("could not resolve app data dir");
            let db_path = db::db_path(&app_data_dir);
            let conn = db::open(&db_path).expect("failed to open sqlite db");

            let existing_roots: Vec<String> = {
                let mut stmt = conn.prepare("SELECT root_path FROM scan_state").unwrap();
                let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
                rows.flatten().collect()
            };

            app.manage(AppState {
                db: Mutex::new(conn),
                db_path: db_path.clone(),
                player: Player::spawn(),
            });

            let handle = app.handle().clone();
            for root in existing_roots {
                let handle = handle.clone();
                let db_path = db_path.clone();
                std::thread::spawn(move || indexer::run_index(handle, db_path, root));
            }

            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("FXBrowser")
                .inner_size(1280.0, 800.0)
                .min_inner_size(900.0, 600.0)
                .build()
                .expect("failed to build main window");

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::play_file,
            commands::stop_playback,
            commands::get_playback_level,
            commands::get_waveform,
            commands::toggle_favorite,
            commands::get_duration_bounds,
            commands::list_sound_types,
            commands::list_roots,
            commands::add_root,
            commands::rescan_root,
            commands::remove_root,
            commands::search_files,
            commands::list_categories,
            commands::list_folder_tree,
        ])
        .run(tauri::generate_context!())
        .expect("error while running fxbrowser");
}
