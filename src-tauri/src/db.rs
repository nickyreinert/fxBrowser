use rusqlite::Connection;
use std::path::PathBuf;

pub fn db_path(app_data_dir: &std::path::Path) -> PathBuf {
    app_data_dir.join("fxbrowser.sqlite3")
}

pub fn open(path: &std::path::Path) -> rusqlite::Result<Connection> {
    std::fs::create_dir_all(path.parent().unwrap()).ok();
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    init_schema(&conn)?;
    migrate(&conn);
    Ok(conn)
}

/// Best-effort additive migrations for columns introduced after the initial
/// schema. `ALTER TABLE ADD COLUMN` errors (already exists) are ignored.
fn migrate(conn: &Connection) {
    conn.execute(
        "ALTER TABLE files ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0",
        [],
    )
    .ok();
    // Defaults to 0 so every pre-existing row is treated as older than any
    // real ANALYSIS_VERSION and gets reprocessed on its next rescan.
    conn.execute(
        "ALTER TABLE files ADD COLUMN analyzed_version INTEGER NOT NULL DEFAULT 0",
        [],
    )
    .ok();
    // Deterministic DSP-derived labels (impact/whoosh/drone/tonal/...), kept
    // separate from `tags` (filename tokens + sidecar metadata) so the two
    // don't drown each other out in search/filtering.
    conn.execute("ALTER TABLE files ADD COLUMN dsp_tags TEXT", [])
        .ok();
}

fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS scan_state (
            root_path TEXT PRIMARY KEY,
            label TEXT,
            status TEXT NOT NULL DEFAULT 'idle',
            last_scanned_at INTEGER,
            total_files INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY,
            path TEXT UNIQUE NOT NULL,
            root_path TEXT NOT NULL,
            filename TEXT NOT NULL,
            ext TEXT,
            parent_folder TEXT,
            folder_path TEXT,
            duration_secs REAL,
            samplerate INTEGER,
            channels INTEGER,
            bitrate INTEGER,
            filesize INTEGER,
            mtime INTEGER,
            description TEXT,
            tags TEXT,
            sidecar_source TEXT,
            FOREIGN KEY (root_path) REFERENCES scan_state(root_path) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_files_root ON files(root_path);
        CREATE INDEX IF NOT EXISTS idx_files_parent ON files(parent_folder);
        CREATE INDEX IF NOT EXISTS idx_files_duration ON files(duration_secs);

        CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(
            filename, parent_folder, folder_path, description, tags,
            content='files', content_rowid='id'
        );

        CREATE TRIGGER IF NOT EXISTS files_ai AFTER INSERT ON files BEGIN
            INSERT INTO files_fts(rowid, filename, parent_folder, folder_path, description, tags)
            VALUES (new.id, new.filename, new.parent_folder, new.folder_path, new.description, new.tags);
        END;

        CREATE TRIGGER IF NOT EXISTS files_ad AFTER DELETE ON files BEGIN
            INSERT INTO files_fts(files_fts, rowid, filename, parent_folder, folder_path, description, tags)
            VALUES ('delete', old.id, old.filename, old.parent_folder, old.folder_path, old.description, old.tags);
        END;

        CREATE TRIGGER IF NOT EXISTS files_au AFTER UPDATE ON files BEGIN
            INSERT INTO files_fts(files_fts, rowid, filename, parent_folder, folder_path, description, tags)
            VALUES ('delete', old.id, old.filename, old.parent_folder, old.folder_path, old.description, old.tags);
            INSERT INTO files_fts(rowid, filename, parent_folder, folder_path, description, tags)
            VALUES (new.id, new.filename, new.parent_folder, new.folder_path, new.description, new.tags);
        END;
        "#,
    )?;
    Ok(())
}
