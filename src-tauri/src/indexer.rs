use crate::db;
use crate::dsp;
use crate::sidecar::{self, SidecarIndex};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

const AUDIO_EXT: &[&str] = &["wav", "mp3", "aiff", "aif", "flac", "ogg", "m4a"];

/// Bump whenever indexing/analysis logic changes in a way that should cause
/// already-indexed files to be reprocessed on their next rescan, even though
/// their mtime/size haven't changed (e.g. adding the DSP tag classifier).
const ANALYSIS_VERSION: i64 = 3;

#[derive(Serialize, Clone)]
struct IndexProgress {
    root_path: String,
    processed: usize,
    total: usize,
    current_file: String,
}

#[derive(Serialize, Clone)]
struct IndexComplete {
    root_path: String,
    total_files: usize,
}

#[derive(Deserialize, Default)]
struct FfprobeFormat {
    duration: Option<String>,
    bit_rate: Option<String>,
}

#[derive(Deserialize, Default)]
struct FfprobeStream {
    sample_rate: Option<String>,
    channels: Option<i64>,
}

#[derive(Deserialize, Default)]
struct FfprobeOutput {
    #[serde(default)]
    format: FfprobeFormat,
    #[serde(default)]
    streams: Vec<FfprobeStream>,
}

struct Probed {
    duration_secs: Option<f64>,
    samplerate: Option<i64>,
    channels: Option<i64>,
    bitrate: Option<i64>,
}

fn probe_audio(path: &Path) -> Probed {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_entries",
            "format=duration,bit_rate:stream=sample_rate,channels",
            "-select_streams",
            "a:0",
        ])
        .arg(path)
        .output();

    let Ok(output) = output else {
        return Probed {
            duration_secs: None,
            samplerate: None,
            channels: None,
            bitrate: None,
        };
    };

    let parsed: FfprobeOutput =
        serde_json::from_slice(&output.stdout).unwrap_or_default();
    let stream = parsed.streams.first();

    Probed {
        duration_secs: parsed.format.duration.and_then(|d| d.parse().ok()),
        bitrate: parsed.format.bit_rate.and_then(|b| b.parse().ok()),
        samplerate: stream.and_then(|s| s.sample_rate.clone()).and_then(|s| s.parse().ok()),
        channels: stream.and_then(|s| s.channels),
    }
}

fn infer_tags_from_filename(stem: &str) -> Vec<String> {
    stem.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2 && !t.chars().all(|c| c.is_ascii_digit()))
        .map(|t| t.to_lowercase())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXT.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Runs synchronously on a background thread spawned by the caller.
pub fn run_index(app: AppHandle, db_path: PathBuf, root_path: String) {
    let conn = match db::open(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to open db for indexing: {e}");
            return;
        }
    };

    conn.execute(
        "INSERT INTO scan_state (root_path, label, status, total_files) VALUES (?1, ?1, 'scanning', 0)
         ON CONFLICT(root_path) DO UPDATE SET status = 'scanning'",
        params![root_path],
    )
    .ok();

    let root = PathBuf::from(&root_path);
    let mut audio_files: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() && is_audio_file(entry.path()) {
            audio_files.push(entry.into_path());
        }
    }

    let total = audio_files.len();
    app.emit(
        "index-progress",
        IndexProgress {
            root_path: root_path.clone(),
            processed: 0,
            total,
            current_file: String::new(),
        },
    )
    .ok();

    // Existing rows for this root, keyed by path, to skip re-probing unchanged files
    // that were already analyzed by the current ANALYSIS_VERSION.
    let mut existing: HashMap<String, (Option<i64>, Option<i64>, i64)> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT path, mtime, filesize, analyzed_version FROM files WHERE root_path = ?1")
            .unwrap();
        let rows = stmt
            .query_map(params![root_path], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .unwrap();
        for r in rows.flatten() {
            existing.insert(r.0, (r.1, r.2, r.3));
        }
    }

    let mut sidecar_cache: HashMap<PathBuf, std::rc::Rc<SidecarIndex>> = HashMap::new();
    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut last_emit = std::time::Instant::now();

    for (i, path) in audio_files.iter().enumerate() {
        let path_str = path.to_string_lossy().to_string();
        seen_paths.insert(path_str.clone());

        let metadata = std::fs::metadata(path).ok();
        let mtime = metadata
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        let filesize = metadata.as_ref().map(|m| m.len() as i64);

        let unchanged = existing
            .get(&path_str)
            .map(|(m, s, v)| *m == mtime && *s == filesize && *v >= ANALYSIS_VERSION)
            .unwrap_or(false);

        if !unchanged {
            let parent_dir = path.parent().unwrap_or(&root).to_path_buf();
            let sidecar_index = sidecar_cache
                .entry(parent_dir.clone())
                .or_insert_with(|| std::rc::Rc::new(sidecar::scan_folder_sidecars(&parent_dir)))
                .clone();

            let filename = path
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("")
                .to_string();
            let stem = path
                .file_stem()
                .and_then(|f| f.to_str())
                .unwrap_or("")
                .to_string();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            let parent_folder = parent_dir
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("")
                .to_string();
            let folder_path = parent_dir
                .strip_prefix(&root)
                .unwrap_or(&parent_dir)
                .to_string_lossy()
                .to_string();

            let sidecar_meta = sidecar_index
                .get(&filename.to_lowercase())
                .or_else(|| sidecar_index.get(&stem.to_lowercase()));

            let probed = probe_audio(path);

            let mut tags = infer_tags_from_filename(&stem);
            if let Some(m) = sidecar_meta {
                if let Some(t) = &m.tags {
                    tags.extend(t.split(|c| c == ',' || c == ';').map(|s| s.trim().to_lowercase()));
                }
            }
            tags.sort();
            tags.dedup();
            let tags_str = tags.join(",");

            // Kept in its own column (comma-padded for safe `LIKE '%,x,%'`
            // membership checks) rather than merged into `tags`, so DSP
            // labels stay independently filterable instead of drowning in
            // filename/sidecar tags.
            let dsp_tags = dsp::classify_file(path, probed.duration_secs.unwrap_or(0.0));
            let dsp_tags_str = if dsp_tags.is_empty() {
                None
            } else {
                Some(format!(",{},", dsp_tags.join(",")))
            };

            let description = sidecar_meta.and_then(|m| m.description.clone());
            let sidecar_source = sidecar_meta.map(|m| m.source.clone());

            conn.execute(
                "INSERT INTO files (path, root_path, filename, ext, parent_folder, folder_path,
                    duration_secs, samplerate, channels, bitrate, filesize, mtime,
                    description, tags, sidecar_source, analyzed_version, dsp_tags)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
                 ON CONFLICT(path) DO UPDATE SET
                    filename=excluded.filename, ext=excluded.ext,
                    parent_folder=excluded.parent_folder, folder_path=excluded.folder_path,
                    duration_secs=excluded.duration_secs, samplerate=excluded.samplerate,
                    channels=excluded.channels, bitrate=excluded.bitrate,
                    filesize=excluded.filesize, mtime=excluded.mtime,
                    description=excluded.description, tags=excluded.tags,
                    sidecar_source=excluded.sidecar_source, analyzed_version=excluded.analyzed_version,
                    dsp_tags=excluded.dsp_tags",
                params![
                    path_str,
                    root_path,
                    filename,
                    ext,
                    parent_folder,
                    folder_path,
                    probed.duration_secs,
                    probed.samplerate,
                    probed.channels,
                    probed.bitrate,
                    filesize,
                    mtime,
                    description,
                    tags_str,
                    sidecar_source,
                    ANALYSIS_VERSION,
                    dsp_tags_str,
                ],
            )
            .ok();
        }

        if last_emit.elapsed().as_millis() >= 200 || i + 1 == total {
            app.emit(
                "index-progress",
                IndexProgress {
                    root_path: root_path.clone(),
                    processed: i + 1,
                    total,
                    current_file: path_str,
                },
            )
            .ok();
            last_emit = std::time::Instant::now();
        }
    }

    prune_stale(&conn, &root_path, &seen_paths);

    conn.execute(
        "UPDATE scan_state SET status = 'idle', last_scanned_at = ?2, total_files = ?3 WHERE root_path = ?1",
        params![root_path, now_secs(), total as i64],
    )
    .ok();

    app.emit(
        "index-complete",
        IndexComplete {
            root_path: root_path.clone(),
            total_files: total,
        },
    )
    .ok();
}

fn prune_stale(conn: &Connection, root_path: &str, seen: &HashSet<String>) {
    let mut stmt = conn
        .prepare("SELECT path FROM files WHERE root_path = ?1")
        .unwrap();
    let stale: Vec<String> = stmt
        .query_map(params![root_path], |row| row.get::<_, String>(0))
        .unwrap()
        .flatten()
        .filter(|p| !seen.contains(p))
        .collect();
    for path in stale {
        conn.execute("DELETE FROM files WHERE path = ?1", params![path])
            .ok();
    }
}

pub fn remove_root(conn: &Connection, root_path: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM files WHERE root_path = ?1", params![root_path])?;
    conn.execute("DELETE FROM scan_state WHERE root_path = ?1", params![root_path])?;
    Ok(())
}
