use chrono::Utc;
use rusqlite::{params, Connection};

use crate::error::DbResult;

const MIGRATIONS: &[(i64, &str, &str)] = &[
    (1, "v1_initial_schema", V1_INITIAL_SCHEMA),
    (
        2,
        "v2_multihost_permissions_and_verification",
        V2_MULTIHOST_PERMISSIONS_AND_VERIFICATION,
    ),
    (
        3,
        "v3_storage_presence_stem_index",
        V3_STORAGE_PRESENCE_STEM_INDEX,
    ),
    (4, "v4_operation_journal", V4_OPERATION_JOURNAL),
];

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

const V2_MULTIHOST_PERMISSIONS_AND_VERIFICATION: &str = r#"
CREATE TABLE IF NOT EXISTS hosts (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    os              TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS storage_mounts (
    storage_id      TEXT NOT NULL REFERENCES storages(id) ON DELETE CASCADE,
    host_id         TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    root_path       TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    PRIMARY KEY (storage_id, host_id)
);

ALTER TABLE storages ADD COLUMN access_mode TEXT NOT NULL DEFAULT 'managed';
ALTER TABLE storages ADD COLUMN backup_mode TEXT NOT NULL DEFAULT 'none';
ALTER TABLE storages ADD COLUMN status TEXT NOT NULL DEFAULT 'online';
ALTER TABLE storages ADD COLUMN failure_domain TEXT;

ALTER TABLE assets ADD COLUMN public_id TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS idx_assets_public_id ON assets(public_id);

ALTER TABLE files ADD COLUMN last_seen_at TEXT;
ALTER TABLE files ADD COLUMN last_verified_at TEXT;

-- Separate storage offline state from file-level presence
UPDATE files SET presence = 'present' WHERE presence = 'offline';

CREATE INDEX IF NOT EXISTS idx_assets_captured_id ON assets(captured_at DESC, id ASC);
CREATE INDEX IF NOT EXISTS idx_files_last_verified ON files(storage_id, last_verified_at);
"#;

const V3_STORAGE_PRESENCE_STEM_INDEX: &str = r#"
-- Composite index so in-batch sidecar parent lookup and RAW+JPEG stem lookup
-- use O(log N) B-tree probes instead of scanning all present files in a storage.
CREATE INDEX IF NOT EXISTS idx_files_storage_presence_stem
    ON files(storage_id, presence, file_stem);
"#;

const V4_OPERATION_JOURNAL: &str = r#"
-- Phase 2: Durable state-machine journal for copy/ingest/move/quarantine/restore/verify operations.
CREATE TABLE IF NOT EXISTS operation_journal (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    op_group_id     TEXT NOT NULL,
    op_type         TEXT NOT NULL,
    state           TEXT NOT NULL,
    host_id         TEXT REFERENCES hosts(id),
    asset_id        INTEGER REFERENCES assets(id),
    file_id         INTEGER REFERENCES files(id),
    src_storage_id  TEXT REFERENCES storages(id),
    src_rel_path    TEXT,
    dst_storage_id  TEXT REFERENCES storages(id),
    dst_rel_path    TEXT,
    expected_algo   TEXT,
    expected_hash   TEXT,
    actual_hash     TEXT,
    byte_size       INTEGER,
    error_message   TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_op_journal_group ON operation_journal(op_group_id);
CREATE INDEX IF NOT EXISTS idx_op_journal_state ON operation_journal(state, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_op_journal_type_created ON operation_journal(op_type, created_at DESC);
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
