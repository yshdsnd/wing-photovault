use chrono::Utc;
use rusqlite::{params, Connection};

use crate::error::DbResult;

const MIGRATIONS: &[(i64, &str, &str)] = &[(1, "v1_initial_schema", V1_INITIAL_SCHEMA)];

const V1_INITIAL_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS storages (
    id              TEXT PRIMARY KEY,
    root_path       TEXT NOT NULL,
    media_type      TEXT NOT NULL,
    tier            TEXT NOT NULL,
    volume_id       TEXT,
    last_seen_at    TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS assets (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    captured_at         TEXT,
    captured_subsec     TEXT,
    camera_make         TEXT,
    camera_model        TEXT,
    camera_serial       TEXT,
    lens_model_raw      TEXT,
    lens_model          TEXT,
    focal_length_mm     REAL,
    focal_length_35mm   REAL,
    f_number            REAL,
    shutter_speed_s     REAL,
    shutter_speed_str   TEXT,
    iso                 INTEGER,
    exposure_bias       REAL,
    width               INTEGER,
    height              INTEGER,
    orientation         INTEGER,
    gps_lat             REAL,
    gps_lon             REAL,
    duration_ms         INTEGER,
    fps                 REAL,
    rating              INTEGER NOT NULL DEFAULT 0,
    pick_status         TEXT NOT NULL DEFAULT 'unreviewed',
    is_favorite         INTEGER NOT NULL DEFAULT 0,
    hot_pinned          INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS contents (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    hash_algo       TEXT NOT NULL,
    hash_hex        TEXT NOT NULL,
    byte_size       INTEGER NOT NULL,
    partial_hash    TEXT,
    media_kind      TEXT NOT NULL,
    format          TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    UNIQUE(hash_algo, hash_hex)
);

CREATE TABLE IF NOT EXISTS content_metadata (
    content_id        INTEGER PRIMARY KEY REFERENCES contents(id) ON DELETE CASCADE,
    extractor         TEXT NOT NULL,
    captured_at       TEXT,
    captured_subsec   TEXT,
    camera_make       TEXT,
    camera_model      TEXT,
    camera_serial     TEXT,
    lens_model_raw    TEXT,
    lens_model        TEXT,
    focal_length_mm   REAL,
    focal_length_35mm REAL,
    f_number          REAL,
    shutter_speed_s   REAL,
    shutter_speed_str TEXT,
    iso               INTEGER,
    exposure_bias     REAL,
    width             INTEGER,
    height            INTEGER,
    orientation       INTEGER,
    gps_lat           REAL,
    gps_lon           REAL,
    duration_ms       INTEGER,
    fps               REAL,
    raw_json          TEXT,
    extracted_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS scan_runs (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    storage_id      TEXT REFERENCES storages(id),
    started_at      TEXT NOT NULL,
    finished_at     TEXT,
    mode            TEXT NOT NULL,
    status          TEXT NOT NULL,
    files_scanned   INTEGER NOT NULL DEFAULT 0,
    files_added     INTEGER NOT NULL DEFAULT 0,
    files_updated   INTEGER NOT NULL DEFAULT 0,
    files_skipped   INTEGER NOT NULL DEFAULT 0,
    files_missing   INTEGER NOT NULL DEFAULT 0,
    bytes_hashed    INTEGER NOT NULL DEFAULT 0,
    error_count     INTEGER NOT NULL DEFAULT 0,
    message         TEXT
);

CREATE TABLE IF NOT EXISTS files (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    storage_id          TEXT NOT NULL REFERENCES storages(id),
    rel_path            TEXT NOT NULL,
    file_name           TEXT NOT NULL,
    file_stem           TEXT NOT NULL,
    ext                 TEXT NOT NULL,
    byte_size           INTEGER NOT NULL,
    mtime_ns            INTEGER NOT NULL,
    ctime_ns            INTEGER,
    content_id          INTEGER REFERENCES contents(id),
    asset_id            INTEGER REFERENCES assets(id),
    role                TEXT NOT NULL DEFAULT 'original',
    presence            TEXT NOT NULL DEFAULT 'present',
    last_seen_scan_id   INTEGER REFERENCES scan_runs(id),
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    UNIQUE(storage_id, rel_path)
);

CREATE INDEX IF NOT EXISTS idx_files_storage_presence ON files(storage_id, presence);
CREATE INDEX IF NOT EXISTS idx_files_content_id ON files(content_id);
CREATE INDEX IF NOT EXISTS idx_files_asset_id ON files(asset_id);
CREATE INDEX IF NOT EXISTS idx_files_stem_ext ON files(file_stem, ext);
CREATE INDEX IF NOT EXISTS idx_contents_size ON contents(byte_size);
CREATE INDEX IF NOT EXISTS idx_contents_format ON contents(format);
CREATE INDEX IF NOT EXISTS idx_assets_captured_at ON assets(captured_at);
CREATE INDEX IF NOT EXISTS idx_assets_camera_lens ON assets(camera_model, lens_model);
CREATE INDEX IF NOT EXISTS idx_assets_rating_pick ON assets(rating, pick_status);
"#;

pub fn run_migrations(conn: &mut Connection) -> DbResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version     INTEGER PRIMARY KEY,
            name        TEXT NOT NULL,
            applied_at  TEXT NOT NULL
        );",
    )?;

    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |r| r.get(0),
    )?;

    for &(version, name, sql) in MIGRATIONS {
        if version > current {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                params![version, name, Utc::now().to_rfc3339()],
            )?;
            tx.commit()?;
        }
    }

    Ok(())
}
