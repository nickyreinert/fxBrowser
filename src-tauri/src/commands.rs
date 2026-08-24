use crate::indexer;
use crate::waveform;
use crate::AppState;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

#[tauri::command]
pub fn play_file(state: State<AppState>, path: String, loop_playback: bool) -> Result<(), String> {
    state.player.play(path, loop_playback);
    Ok(())
}

#[tauri::command]
pub fn stop_playback(state: State<AppState>) -> Result<(), String> {
    state.player.stop();
    Ok(())
}

#[tauri::command]
pub fn get_playback_level(state: State<AppState>) -> f32 {
    state.player.current_level()
}

#[tauri::command]
pub fn get_playback_spectrum(state: State<AppState>, bars: usize) -> Vec<f32> {
    state.player.current_spectrum(bars)
}

#[tauri::command]
pub fn seek_playback(state: State<AppState>, secs: f64) -> Result<(), String> {
    state.player.seek(secs);
    Ok(())
}

#[tauri::command]
pub fn toggle_favorite(state: State<AppState>, id: i64) -> Result<bool, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE files SET favorite = 1 - favorite WHERE id = ?1",
        params![id],
    )
    .map_err(|e| e.to_string())?;
    let favorite: i64 = conn
        .query_row("SELECT favorite FROM files WHERE id = ?1", params![id], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    Ok(favorite != 0)
}

#[derive(Serialize)]
pub struct DurationBounds {
    min: f64,
    max: f64,
}

#[tauri::command]
pub fn get_duration_bounds(state: State<AppState>) -> Result<DurationBounds, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let (min, max): (Option<f64>, Option<f64>) = conn
        .query_row(
            "SELECT MIN(duration_secs), MAX(duration_secs) FROM files WHERE duration_secs IS NOT NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or((None, None));
    Ok(DurationBounds {
        min: min.unwrap_or(0.0).max(0.0),
        max: max.unwrap_or(60.0).max(1.0),
    })
}

#[derive(Serialize)]
pub struct WaveformResponse {
    channels: Vec<Vec<(f32, f32)>>,
}

#[tauri::command]
pub fn get_waveform(path: String, buckets: Option<u32>) -> Result<WaveformResponse, String> {
    let buckets = buckets.unwrap_or(400) as usize;
    let channels = waveform::compute_peaks(std::path::Path::new(&path), buckets)
        .ok_or_else(|| "failed to decode audio for waveform".to_string())?;
    Ok(WaveformResponse { channels })
}

#[derive(Serialize)]
pub struct RootInfo {
    root_path: String,
    label: Option<String>,
    status: String,
    last_scanned_at: Option<i64>,
    total_files: i64,
}

#[tauri::command]
pub fn list_roots(state: State<AppState>) -> Result<Vec<RootInfo>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("SELECT root_path, label, status, last_scanned_at, total_files FROM scan_state ORDER BY root_path")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(RootInfo {
                root_path: row.get(0)?,
                label: row.get(1)?,
                status: row.get(2)?,
                last_scanned_at: row.get(3)?,
                total_files: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.flatten().collect())
}

#[tauri::command]
pub fn add_root(app: AppHandle, state: State<AppState>, path: String) -> Result<(), String> {
    if !std::path::Path::new(&path).is_dir() {
        return Err(format!("{path} is not a directory"));
    }
    {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR IGNORE INTO scan_state (root_path, label, status, total_files) VALUES (?1, ?1, 'idle', 0)",
            params![path],
        )
        .map_err(|e| e.to_string())?;
    }
    let db_path = state.db_path.clone();
    std::thread::spawn(move || indexer::run_index(app, db_path, path));
    Ok(())
}

#[tauri::command]
pub fn rescan_root(app: AppHandle, state: State<AppState>, path: String) -> Result<(), String> {
    let db_path = state.db_path.clone();
    std::thread::spawn(move || indexer::run_index(app, db_path, path));
    Ok(())
}

#[tauri::command]
pub fn remove_root(state: State<AppState>, path: String) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    indexer::remove_root(&conn, &path).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
pub struct SearchFilters {
    text: Option<String>,
    root_path: Option<String>,
    folder_path: Option<String>,
    categories: Option<Vec<String>>,
    min_secs: Option<f64>,
    max_secs: Option<f64>,
    favorites_only: Option<bool>,
    sound_types: Option<Vec<String>>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Serialize)]
pub struct FileRow {
    id: i64,
    path: String,
    root_path: String,
    filename: String,
    ext: Option<String>,
    parent_folder: Option<String>,
    folder_path: Option<String>,
    duration_secs: Option<f64>,
    description: Option<String>,
    tags: Option<String>,
    dsp_tags: Option<String>,
    favorite: bool,
}

fn sanitize_fts_query(text: &str) -> String {
    text.split_whitespace()
        .map(|tok| {
            let cleaned: String = tok.chars().filter(|c| c.is_alphanumeric()).collect();
            format!("\"{cleaned}\"*")
        })
        .filter(|t| t.len() > 3)
        .collect::<Vec<_>>()
        .join(" ")
}

#[tauri::command]
pub fn search_files(state: State<AppState>, filters: SearchFilters) -> Result<Vec<FileRow>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let limit = filters.limit.unwrap_or(200).clamp(1, 1000);
    let offset = filters.offset.unwrap_or(0).max(0);

    let mut sql = String::from(
        "SELECT f.id, f.path, f.root_path, f.filename, f.ext, f.parent_folder, f.folder_path, f.duration_secs, f.description, f.tags, f.dsp_tags, f.favorite
         FROM files f",
    );
    let mut conditions: Vec<String> = Vec::new();
    let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(text) = filters.text.as_deref().filter(|t| !t.trim().is_empty()) {
        let fts_query = sanitize_fts_query(text);
        if !fts_query.is_empty() {
            sql.push_str(" JOIN files_fts ON files_fts.rowid = f.id");
            conditions.push("files_fts MATCH ?".to_string());
            params_vec.push(Box::new(fts_query));
        }
    }
    if let Some(root) = filters.root_path.filter(|s| !s.is_empty()) {
        conditions.push("f.root_path = ?".to_string());
        params_vec.push(Box::new(root));
    }
    if let Some(folder) = filters.folder_path.filter(|s| !s.is_empty()) {
        conditions.push("(f.folder_path = ? OR f.folder_path LIKE ?)".to_string());
        params_vec.push(Box::new(folder.clone()));
        params_vec.push(Box::new(format!("{folder}/%")));
    }
    if let Some(categories) = filters.categories.filter(|c| !c.is_empty()) {
        let placeholders = vec!["?"; categories.len()].join(",");
        conditions.push(format!("f.parent_folder IN ({placeholders})"));
        for c in categories {
            params_vec.push(Box::new(c));
        }
    }
    if let Some(min) = filters.min_secs {
        conditions.push("f.duration_secs >= ?".to_string());
        params_vec.push(Box::new(min));
    }
    if let Some(max) = filters.max_secs {
        conditions.push("f.duration_secs <= ?".to_string());
        params_vec.push(Box::new(max));
    }
    if filters.favorites_only.unwrap_or(false) {
        conditions.push("f.favorite = 1".to_string());
    }
    if let Some(sound_types) = filters.sound_types.filter(|c| !c.is_empty()) {
        let clauses = vec!["f.dsp_tags LIKE ?"; sound_types.len()].join(" OR ");
        conditions.push(format!("({clauses})"));
        for t in sound_types {
            params_vec.push(Box::new(format!("%,{t},%")));
        }
    }

    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY f.parent_folder, f.filename LIMIT ? OFFSET ?");
    params_vec.push(Box::new(limit));
    params_vec.push(Box::new(offset));

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(FileRow {
                id: row.get(0)?,
                path: row.get(1)?,
                root_path: row.get(2)?,
                filename: row.get(3)?,
                ext: row.get(4)?,
                parent_folder: row.get(5)?,
                folder_path: row.get(6)?,
                duration_secs: row.get(7)?,
                description: row.get(8)?,
                tags: row.get(9)?,
                dsp_tags: row.get(10)?,
                favorite: row.get::<_, i64>(11)? != 0,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.flatten().collect())
}

#[tauri::command]
pub fn list_sound_types(state: State<AppState>) -> Result<Vec<String>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT dsp_tags FROM files WHERE dsp_tags IS NOT NULL AND dsp_tags != ''")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut set = std::collections::BTreeSet::new();
    for r in rows.flatten() {
        for tok in r.trim_matches(',').split(',') {
            if !tok.is_empty() {
                set.insert(tok.to_string());
            }
        }
    }
    Ok(set.into_iter().collect())
}

#[tauri::command]
pub fn list_categories(state: State<AppState>) -> Result<Vec<String>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT parent_folder FROM files WHERE parent_folder IS NOT NULL AND parent_folder != '' ORDER BY parent_folder")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    Ok(rows.flatten().collect())
}

#[derive(Serialize)]
pub struct FolderEntry {
    root_path: String,
    folder_path: String,
}

#[tauri::command]
pub fn list_folder_tree(state: State<AppState>) -> Result<Vec<FolderEntry>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT root_path, folder_path FROM files ORDER BY root_path, folder_path")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(FolderEntry {
                root_path: row.get(0)?,
                folder_path: row.get(1)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.flatten().collect())
}
