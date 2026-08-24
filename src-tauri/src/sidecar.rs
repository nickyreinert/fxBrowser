use calamine::{open_workbook_auto, Data, Reader};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct SidecarMeta {
    pub description: Option<String>,
    pub tags: Option<String>,
    pub source: String,
}

/// Keys are lowercased filenames and lowercased filename-stems (without extension),
/// so a lookup can try either form.
pub type SidecarIndex = HashMap<String, SidecarMeta>;

const FILENAME_HEADERS: &[&str] = &["filename", "file name", "file", "name", "sample", "asset"];
const DESC_HEADERS: &[&str] = &[
    "description",
    "desc",
    "notes",
    "note",
    "summary",
    "comment",
];
const TAGS_HEADERS: &[&str] = &["tags", "keywords", "category", "categories", "genre"];

fn header_index(headers: &[String], candidates: &[&str]) -> Option<usize> {
    headers.iter().position(|h| {
        let h = h.trim().to_lowercase();
        candidates.contains(&h.as_str())
    })
}

fn insert_entry(index: &mut SidecarIndex, filename: &str, meta: SidecarMeta) {
    if filename.trim().is_empty() {
        return;
    }
    let lower = filename.trim().to_lowercase();
    let stem = Path::new(&lower)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&lower)
        .to_string();
    index.entry(lower).or_insert_with(|| meta.clone());
    index.entry(stem).or_insert(meta);
}

/// Scan a single directory (non-recursive) for CSV/TXT/XLSX sidecar metadata files
/// and build a filename -> {description, tags} lookup for that directory.
pub fn scan_folder_sidecars(folder: &Path) -> SidecarIndex {
    let mut index = SidecarIndex::new();
    let entries = match std::fs::read_dir(folder) {
        Ok(e) => e,
        Err(_) => return index,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let source = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("")
            .to_string();

        match ext.as_str() {
            "csv" => parse_csv(&path, &source, &mut index),
            "xlsx" | "xls" | "xlsm" => parse_xlsx(&path, &source, &mut index),
            "txt" => parse_txt(&path, &source, &mut index),
            _ => {}
        }
    }

    index
}

fn parse_csv(path: &Path, source: &str, index: &mut SidecarIndex) {
    let mut reader = match csv::ReaderBuilder::new().flexible(true).from_path(path) {
        Ok(r) => r,
        Err(_) => return,
    };
    let headers: Vec<String> = match reader.headers() {
        Ok(h) => h.iter().map(|s| s.to_string()).collect(),
        Err(_) => return,
    };
    let filename_idx = header_index(&headers, FILENAME_HEADERS).unwrap_or(0);
    let desc_idx = header_index(&headers, DESC_HEADERS);
    let tags_idx = header_index(&headers, TAGS_HEADERS);

    for record in reader.records().flatten() {
        let filename = record.get(filename_idx).unwrap_or("").to_string();
        let description = desc_idx.and_then(|i| record.get(i)).map(|s| s.to_string());
        let tags = tags_idx.and_then(|i| record.get(i)).map(|s| s.to_string());
        insert_entry(
            index,
            &filename,
            SidecarMeta {
                description,
                tags,
                source: source.to_string(),
            },
        );
    }
}

fn parse_xlsx(path: &Path, source: &str, index: &mut SidecarIndex) {
    let mut workbook = match open_workbook_auto(path) {
        Ok(w) => w,
        Err(_) => return,
    };
    let sheet_name = match workbook.sheet_names().first().cloned() {
        Some(n) => n,
        None => return,
    };
    let range = match workbook.worksheet_range(&sheet_name) {
        Ok(r) => r,
        Err(_) => return,
    };
    let mut rows = range.rows();
    let header_row = match rows.next() {
        Some(r) => r,
        None => return,
    };
    let headers: Vec<String> = header_row.iter().map(data_to_string).collect();
    let filename_idx = header_index(&headers, FILENAME_HEADERS).unwrap_or(0);
    let desc_idx = header_index(&headers, DESC_HEADERS);
    let tags_idx = header_index(&headers, TAGS_HEADERS);

    for row in rows {
        let filename = row
            .get(filename_idx)
            .map(data_to_string)
            .unwrap_or_default();
        let description = desc_idx.and_then(|i| row.get(i)).map(data_to_string);
        let tags = tags_idx.and_then(|i| row.get(i)).map(data_to_string);
        insert_entry(
            index,
            &filename,
            SidecarMeta {
                description,
                tags,
                source: source.to_string(),
            },
        );
    }
}

fn data_to_string(d: &Data) -> String {
    match d {
        Data::String(s) => s.clone(),
        Data::Float(f) => f.to_string(),
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

/// Best-effort TXT parsing: one entry per line, splitting on the first of
/// " - ", ":", ",", or "\t" once a known audio extension is spotted in the
/// leading token.
fn parse_txt(path: &Path, source: &str, index: &mut SidecarIndex) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };
    const AUDIO_EXT: &[&str] = &["wav", "mp3", "aiff", "aif", "flac", "ogg", "m4a"];
    const DELIMS: &[&str] = &[" - ", ":", "\t", ","];

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut split: Option<(&str, &str)> = None;
        for delim in DELIMS {
            if let Some((left, right)) = line.split_once(delim) {
                let left_ext = Path::new(left.trim())
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if AUDIO_EXT.contains(&left_ext.as_str()) {
                    split = Some((left.trim(), right.trim()));
                    break;
                }
            }
        }
        if let Some((filename, description)) = split {
            insert_entry(
                index,
                filename,
                SidecarMeta {
                    description: Some(description.to_string()),
                    tags: None,
                    source: source.to_string(),
                },
            );
        }
    }
}
