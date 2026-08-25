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

#[derive(Deserialize, Clone)]
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
    sort_by: Option<String>,
    sort_dir: Option<String>,
}

/// Which facet dimension (if any) a query should leave out of its own WHERE
/// clause — a facet must ignore its own selection so choosing one value
/// doesn't hide the sibling values you could switch to (standard faceted
/// search), while still respecting every other active filter, including
/// folder scope.
#[derive(Default, Clone, Copy)]
struct FacetExclude {
    categories: bool,
    sound_types: bool,
    folder: bool,
}

/// Builds the shared `WHERE` conditions (and bound params) used by search and
/// every facet-count query, so folder/category/sound-type/search-text scoping
/// stays consistent across all of them instead of drifting apart.
fn build_conditions(
    filters: &SearchFilters,
    exclude: FacetExclude,
) -> (bool, Vec<String>, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut conditions: Vec<String> = Vec::new();
    let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut needs_fts = false;

    if let Some(text) = filters.text.as_deref().filter(|t| !t.trim().is_empty()) {
        let fts_query = sanitize_fts_query(text);
        if !fts_query.is_empty() {
            needs_fts = true;
            conditions.push("files_fts MATCH ?".to_string());
            params_vec.push(Box::new(fts_query));
        }
    }
    if !exclude.folder {
        if let Some(root) = filters.root_path.as_deref().filter(|s| !s.is_empty()) {
            conditions.push("f.root_path = ?".to_string());
            params_vec.push(Box::new(root.to_string()));
        }
        if let Some(folder) = filters.folder_path.as_deref().filter(|s| !s.is_empty()) {
            conditions.push("(f.folder_path = ? OR f.folder_path LIKE ?)".to_string());
            params_vec.push(Box::new(folder.to_string()));
            params_vec.push(Box::new(format!("{folder}/%")));
        }
    }
    if !exclude.categories {
        if let Some(categories) = filters.categories.as_ref().filter(|c| !c.is_empty()) {
            let placeholders = vec!["?"; categories.len()].join(",");
            conditions.push(format!("f.parent_folder IN ({placeholders})"));
            for c in categories {
                params_vec.push(Box::new(c.clone()));
            }
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
    if !exclude.sound_types {
        if let Some(sound_types) = filters.sound_types.as_ref().filter(|c| !c.is_empty()) {
            let clauses = vec!["f.dsp_tags LIKE ?"; sound_types.len()].join(" OR ");
            conditions.push(format!("({clauses})"));
            for t in sound_types {
                params_vec.push(Box::new(format!("%,{t},%")));
            }
        }
    }

    (needs_fts, conditions, params_vec)
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

/// Maps a frontend-chosen sort column to a safe, hardcoded ORDER BY
/// fragment — never interpolate the raw string, so this can't become a SQL
/// injection vector.
fn sort_clause(sort_by: Option<&str>, sort_dir: Option<&str>) -> &'static str {
    let desc = matches!(sort_dir, Some(d) if d.eq_ignore_ascii_case("desc"));
    match sort_by {
        Some("name") if desc => "f.filename DESC",
        Some("name") => "f.filename ASC",
        Some("folder") if desc => "f.folder_path DESC, f.filename ASC",
        Some("folder") => "f.folder_path ASC, f.filename ASC",
        Some("duration") if desc => "f.duration_secs DESC, f.filename ASC",
        Some("duration") => "f.duration_secs ASC, f.filename ASC",
        Some("type") if desc => "f.dsp_tags DESC, f.filename ASC",
        Some("type") => "f.dsp_tags ASC, f.filename ASC",
        _ => "f.parent_folder ASC, f.filename ASC",
    }
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
    let (needs_fts, conditions, mut params_vec) = build_conditions(&filters, FacetExclude::default());
    if needs_fts {
        sql.push_str(" JOIN files_fts ON files_fts.rowid = f.id");
    }

    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY ");
    sql.push_str(sort_clause(filters.sort_by.as_deref(), filters.sort_dir.as_deref()));
    sql.push_str(" LIMIT ? OFFSET ?");
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

#[derive(Serialize)]
pub struct NamedCount {
    name: String,
    count: i64,
}

/// Sound types are counted in Rust rather than SQL because `dsp_tags` packs
/// multiple comma-separated labels into one column (`,impact,bright,`), so a
/// plain `GROUP BY` can't split them out.
#[tauri::command]
pub fn list_sound_types(state: State<AppState>, filters: SearchFilters) -> Result<Vec<NamedCount>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let (needs_fts, conditions, params_vec) = build_conditions(
        &filters,
        FacetExclude {
            sound_types: true,
            ..Default::default()
        },
    );

    let mut sql = String::from(
        "SELECT f.dsp_tags FROM files f WHERE f.dsp_tags IS NOT NULL AND f.dsp_tags != ''",
    );
    if needs_fts {
        sql = sql.replace("FROM files f", "FROM files f JOIN files_fts ON files_fts.rowid = f.id");
    }
    if !conditions.is_empty() {
        sql.push_str(" AND ");
        sql.push_str(&conditions.join(" AND "));
    }

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;

    let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for r in rows.flatten() {
        for tok in r.trim_matches(',').split(',') {
            if !tok.is_empty() {
                *counts.entry(tok.to_string()).or_insert(0) += 1;
            }
        }
    }
    Ok(counts
        .into_iter()
        .map(|(name, count)| NamedCount { name, count })
        .collect())
}

#[tauri::command]
pub fn list_categories(state: State<AppState>, filters: SearchFilters) -> Result<Vec<NamedCount>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let (needs_fts, conditions, params_vec) = build_conditions(
        &filters,
        FacetExclude {
            categories: true,
            ..Default::default()
        },
    );

    let mut sql = String::from(
        "SELECT f.parent_folder, COUNT(*) FROM files f WHERE f.parent_folder IS NOT NULL AND f.parent_folder != ''",
    );
    if needs_fts {
        sql = sql.replace("FROM files f", "FROM files f JOIN files_fts ON files_fts.rowid = f.id");
    }
    if !conditions.is_empty() {
        sql.push_str(" AND ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" GROUP BY f.parent_folder ORDER BY f.parent_folder");

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(NamedCount {
                name: row.get(0)?,
                count: row.get(1)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.flatten().collect())
}

#[derive(Serialize)]
pub struct FolderEntry {
    root_path: String,
    folder_path: String,
    count: i64,
}

/// Returns every folder that exists in the library (unfiltered, so the tree
/// itself never collapses out from under you as you navigate), each
/// annotated with how many files directly in it match the *other* active
/// filters (text/categories/sound types/duration/favorites — everything
/// except folder scope itself). The frontend sums each node's own count with
/// its descendants' to get the cumulative total shown next to a folder.
#[tauri::command]
pub fn list_folder_tree(state: State<AppState>, filters: SearchFilters) -> Result<Vec<FolderEntry>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;

    let mut all_stmt = conn
        .prepare("SELECT DISTINCT root_path, folder_path FROM files ORDER BY root_path, folder_path")
        .map_err(|e| e.to_string())?;
    let all_entries: Vec<(String, String)> = all_stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();

    let (needs_fts, conditions, params_vec) = build_conditions(
        &filters,
        FacetExclude {
            folder: true,
            ..Default::default()
        },
    );
    let mut sql = String::from("SELECT f.root_path, f.folder_path, COUNT(*) FROM files f");
    if needs_fts {
        sql.push_str(" JOIN files_fts ON files_fts.rowid = f.id");
    }
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" GROUP BY f.root_path, f.folder_path");

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
    let count_rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })
        .map_err(|e| e.to_string())?;
    let mut counts: std::collections::HashMap<(String, String), i64> = std::collections::HashMap::new();
    for r in count_rows.flatten() {
        counts.insert((r.0, r.1), r.2);
    }

    Ok(all_entries
        .into_iter()
        .map(|(root_path, folder_path)| {
            let count = counts
                .get(&(root_path.clone(), folder_path.clone()))
                .copied()
                .unwrap_or(0);
            FolderEntry {
                root_path,
                folder_path,
                count,
            }
        })
        .collect())
}
