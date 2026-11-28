use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use uuid::Uuid;
use wing_photovault_core::config::StorageConfig;
use wing_photovault_core::metadata::{resolve_lens_alias, split_lens_extender};
use wing_photovault_core::model::{
    sidecar_parent_stem, ExtractedMetadata, FileRole, HashAlgorithm, MediaKind, ScanManifestRecord,
    StorageStatus,
};

use crate::error::DbResult;
use crate::migrations::run_migrations;

pub struct Database {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct ExistingFileSig {
    pub file_id: i64,
    pub byte_size: u64,
    pub mtime_ns: i64,
    pub content_id: Option<i64>,
    pub asset_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct UpsertFileInput {
    pub storage_id: String,
    pub rel_path: String,
    pub file_name: String,
    pub file_stem: String,
    pub ext: String,
    pub byte_size: u64,
    pub mtime_ns: i64,
    pub ctime_ns: Option<i64>,
    pub media_kind: MediaKind,
    pub format: String,
    pub role: FileRole,
    pub hash_algo: HashAlgorithm,
    pub hash_hex: String,
    pub partial_hash: Option<String>,
    pub metadata: Option<ExtractedMetadata>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PairReport {
    pub pairs_merged: u64,
    pub ambiguous_skipped: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SidecarLinkReport {
    pub linked: u64,
    pub unmatched_or_ambiguous: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanRunSummary {
    pub scan_run_id: i64,
    pub storage_id: String,
    pub mode: String,
    pub status: String,
    pub files_scanned: u64,
    pub files_added: u64,
    pub files_updated: u64,
    pub files_skipped: u64,
    pub files_missing: u64,
    pub bytes_hashed: u64,
    pub error_count: u64,
    pub raw_jpeg_merged: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageStatsRow {
    pub storage_id: String,
    pub root_path: String,
    pub media_type: String,
    pub tier: String,
    pub access_mode: String,
    pub backup_mode: String,
    pub status: String,
    pub failure_domain: Option<String>,
    pub present_files: i64,
    pub missing_files: i64,
    pub total_bytes: i64,
    pub last_seen_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub schema_version: i64,
    pub total_assets: i64,
    pub total_contents: i64,
    pub total_present_files: i64,
    pub total_missing_files: i64,
    pub total_present_bytes: i64,
    pub storages: Vec<StorageStatsRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BucketCount {
    pub label: String,
    pub count: i64,
    pub total_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatsReport {
    pub total_assets: i64,
    pub total_contents: i64,
    pub total_present_files: i64,
    pub total_present_bytes: i64,
    pub backed_up_contents: i64,
    pub by_format: Vec<BucketCount>,
    pub by_camera: Vec<BucketCount>,
    pub by_lens: Vec<BucketCount>,
    pub by_storage: Vec<StorageStatsRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct YearStatsRow {
    pub year: String,
    pub assets: i64,
    pub active_days: i64,
    pub top_camera: Option<String>,
    pub top_lens: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GearTimelineRow {
    pub name: String,
    pub assets: i64,
    pub active_days: i64,
    pub first_captured: Option<String>,
    pub last_captured: Option<String>,
    pub top_partner: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GearHistoryReport {
    pub cameras: Vec<GearTimelineRow>,
    pub lenses: Vec<GearTimelineRow>,
}

#[derive(Debug, Clone, Default)]
pub struct StatsFilter {
    /// Filter assets to a specific 4-digit capture year (e.g. `"2025"`).
    pub year: Option<String>,
    /// Case-insensitive substring filter on `assets.camera_model`.
    pub camera: Option<String>,
    /// Case-insensitive substring filter on `assets.lens_model`.
    pub lens: Option<String>,
    /// Split lens + teleconverter strings so both parent lens and extender are credited.
    pub split_extenders: bool,
}

impl StatsFilter {
    pub fn has_asset_filter(&self) -> bool {
        self.year.is_some() || self.camera.is_some() || self.lens.is_some()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FocalBucketRow {
    pub label: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FocalExactRow {
    pub focal_mm: i64,
    pub count: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FocalDistributionReport {
    pub total_with_focal: i64,
    pub min_mm: Option<f64>,
    pub max_mm: Option<f64>,
    pub avg_mm: Option<f64>,
    pub buckets: Vec<FocalBucketRow>,
    pub top_exact_mm: Vec<FocalExactRow>,
}

#[derive(Debug, Clone, Default)]
pub struct DuplicateFilter {
    /// Restrict detection to duplicates within a single storage ID.
    pub storage_id: Option<String>,
    /// Also include within-storage duplicates found inside `read_only`/`backup`/`external` storages.
    /// Cross-tier intentional backups (e.g. 1 Hot + 1 Backup) are NEVER reported as duplicates.
    pub include_backups: bool,
    /// Include sidecar files (`.xmp`, `.thm`); defaults to `false`.
    pub include_sidecars: bool,
    /// Only report groups whose single-file size is >= `min_bytes`.
    pub min_bytes: u64,
    /// Maximum number of groups to return (`None` or `Some(0)` = unlimited).
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateFileRow {
    pub file_id: i64,
    pub asset_id: Option<i64>,
    pub storage_id: String,
    pub rel_path: String,
    pub tier: String,
    pub access_mode: String,
    pub backup_mode: String,
    pub role: String,
    pub protected: bool,
    /// Advisory role: `"keep"`, `"duplicate"`, or `"protected"`.
    pub recommendation: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateGroup {
    pub content_id: i64,
    pub hash_algo: String,
    pub hash_hex: String,
    pub format: String,
    pub byte_size: i64,
    pub total_copies: usize,
    pub actionable_copies: usize,
    pub protected_copies: usize,
    pub reclaimable_bytes: i64,
    pub redundant_bytes: i64,
    pub files: Vec<DuplicateFileRow>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DuplicateReport {
    pub total_groups: usize,
    pub returned_groups: usize,
    pub actionable_groups: usize,
    pub protected_only_groups: usize,
    pub total_redundant_files: usize,
    pub actionable_redundant_files: usize,
    pub total_redundant_bytes: i64,
    pub reclaimable_bytes: i64,
    pub groups: Vec<DuplicateGroup>,
}

/// Opaque-style keyset pagination cursor for `find_page`, matching
/// `ORDER BY COALESCE(a.captured_at, '') DESC, f.id ASC`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindCursor {
    pub captured_at: String,
    pub file_id: i64,
}

impl FindCursor {
    pub fn encode(&self) -> String {
        format!("v1:{}:{}", self.file_id, self.captured_at)
    }

    pub fn decode(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        let rest = trimmed
            .strip_prefix("v1:")
            .ok_or_else(|| "invalid cursor format: expected 'v1:<file_id>:<captured_at>'".to_string())?;
        let (id_str, captured_at) = rest
            .split_once(':')
            .ok_or_else(|| "invalid cursor format: missing separator".to_string())?;
        let file_id: i64 = id_str
            .parse()
            .map_err(|_| "invalid cursor file_id: expected positive integer".to_string())?;
        if file_id <= 0 {
            return Err("invalid cursor file_id: must be > 0".to_string());
        }
        if captured_at.len() > 64 || captured_at.chars().any(|c| c.is_control()) {
            return Err("invalid cursor captured_at value".to_string());
        }
        Ok(Self {
            captured_at: captured_at.to_string(),
            file_id,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct FindFilter {
    pub storage_id: Option<String>,
    pub format: Option<String>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub min_focal_mm: Option<f64>,
    pub max_focal_mm: Option<f64>,
    pub min_iso: Option<i64>,
    pub max_iso: Option<i64>,
    pub min_f_number: Option<f64>,
    pub max_f_number: Option<f64>,
    pub path_contains: Option<String>,
    pub asset_id: Option<i64>,
    pub unbacked_only: bool,
    pub limit: Option<i64>,
    pub cursor: Option<FindCursor>,
}

#[derive(Debug, Clone)]
pub struct CopyCandidateRow {
    pub file_id: i64,
    pub asset_id: Option<i64>,
    pub content_id: Option<i64>,
    pub storage_id: String,
    pub storage_tier: String,
    pub rel_path: String,
    pub role: String,
    pub format: String,
    pub byte_size: i64,
    pub hash_algo: String,
    pub hash_hex: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FindPage {
    pub items: Vec<FindRow>,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FindRow {
    pub file_id: i64,
    pub asset_id: Option<i64>,
    pub asset_public_id: Option<String>,
    pub storage_id: String,
    pub rel_path: String,
    pub format: String,
    pub byte_size: i64,
    pub captured_at: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f64>,
    pub f_number: Option<f64>,
    pub shutter_speed_str: Option<String>,
    pub iso: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LensAliasChange {
    pub raw_lens: String,
    pub previous_lens: Option<String>,
    pub normalized_lens: Option<String>,
    pub assets_affected: u64,
    pub contents_affected: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NormalizeLensesReport {
    pub dry_run: bool,
    pub aliases_configured: usize,
    pub assets_updated: u64,
    pub contents_updated: u64,
    pub changes: Vec<LensAliasChange>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SeedBenchmarkReport {
    pub files_created: u64,
    pub assets_created: u64,
    pub contents_created: u64,
}

impl Database {
    pub fn open(path: &Path) -> DbResult<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let mut conn = Connection::open(path)?;
        configure_connection(&conn)?;
        run_migrations(&mut conn)?;
        Ok(Self { conn })
    }

    /// Open an existing database strictly in read-only mode (guarantees zero writes for `--dry-run`).
    pub fn open_read_only(path: &Path) -> DbResult<Self> {
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags)?;
        conn.execute_batch(
            "PRAGMA query_only = ON;
             PRAGMA busy_timeout = 5000;",
        )?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> DbResult<Self> {
        let mut conn = Connection::open_in_memory()?;
        configure_connection(&conn)?;
        run_migrations(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn upsert_host(&self, host_id: &str, name: &str, os: &str) -> DbResult<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO hosts (id, name, os, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name,
               os = excluded.os,
               updated_at = excluded.updated_at",
            params![host_id, name, os, now],
        )?;
        Ok(())
    }

    pub fn upsert_storage(
        &self,
        storage: &StorageConfig,
        host_id: &str,
        status: StorageStatus,
    ) -> DbResult<()> {
        let now = Utc::now().to_rfc3339();
        let last_seen = if status == StorageStatus::Online {
            Some(now.as_str())
        } else {
            None
        };
        let root_str = storage.root.to_string_lossy();
        self.conn.execute(
            "INSERT INTO storages (
                id, root_path, media_type, tier, access_mode, backup_mode,
                status, failure_domain, last_seen_at, created_at, updated_at
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
             ON CONFLICT(id) DO UPDATE SET
               root_path = excluded.root_path,
               media_type = excluded.media_type,
               tier = excluded.tier,
               access_mode = excluded.access_mode,
               backup_mode = excluded.backup_mode,
               status = excluded.status,
               failure_domain = COALESCE(excluded.failure_domain, storages.failure_domain),
               last_seen_at = COALESCE(excluded.last_seen_at, storages.last_seen_at),
               updated_at = excluded.updated_at",
            params![
                storage.id,
                root_str,
                storage.media.as_str(),
                storage.tier.as_str(),
                storage.access_mode.as_str(),
                storage.backup_mode.as_str(),
                status.as_str(),
                storage.failure_domain,
                last_seen,
                now
            ],
        )?;

        self.conn.execute(
            "INSERT INTO storage_mounts (storage_id, host_id, root_path, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(storage_id, host_id) DO UPDATE SET
               root_path = excluded.root_path,
               updated_at = excluded.updated_at",
            params![storage.id, host_id, root_str, now],
        )?;
        Ok(())
    }

    pub fn start_scan_run(&self, storage_id: &str, mode: &str) -> DbResult<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO scan_runs (storage_id, started_at, mode, status) VALUES (?1, ?2, ?3, 'running')",
            params![storage_id, now, mode],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_scan_run(&self, summary: &ScanRunSummary, message: Option<&str>) -> DbResult<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE scan_runs SET
                finished_at = ?1,
                status = ?2,
                files_scanned = ?3,
                files_added = ?4,
                files_updated = ?5,
                files_skipped = ?6,
                files_missing = ?7,
                bytes_hashed = ?8,
                error_count = ?9,
                message = ?10
             WHERE id = ?11",
            params![
                now,
                summary.status,
                summary.files_scanned as i64,
                summary.files_added as i64,
                summary.files_updated as i64,
                summary.files_skipped as i64,
                summary.files_missing as i64,
                summary.bytes_hashed as i64,
                summary.error_count as i64,
                message,
                summary.scan_run_id,
            ],
        )?;
        Ok(())
    }

    pub fn load_storage_file_index(
        &self,
        storage_id: &str,
    ) -> DbResult<HashMap<String, ExistingFileSig>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, rel_path, byte_size, mtime_ns, content_id, asset_id
             FROM files
             WHERE storage_id = ?1",
        )?;
        let rows = stmt.query_map(params![storage_id], |r| {
            let rel_path: String = r.get(1)?;
            let sig = ExistingFileSig {
                file_id: r.get(0)?,
                byte_size: r.get::<_, i64>(2)? as u64,
                mtime_ns: r.get(3)?,
                content_id: r.get(4)?,
                asset_id: r.get(5)?,
            };
            Ok((rel_path, sig))
        })?;

        let mut map = HashMap::new();
        for row in rows {
            let (k, v) = row?;
            map.insert(k, v);
        }
        Ok(map)
    }

    /// Updates existence check timestamp (`last_seen_at`) WITHOUT overwriting `last_verified_at`.
    pub fn touch_unchanged_files(&mut self, file_ids: &[i64], scan_run_id: i64) -> DbResult<()> {
        if file_ids.is_empty() {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE files SET
                    presence = 'present',
                    last_seen_scan_id = ?1,
                    last_seen_at = ?2,
                    updated_at = ?2
                 WHERE id = ?3",
            )?;
            for &fid in file_ids {
                stmt.execute(params![scan_run_id, now, fid])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_scanned_batch(
        &mut self,
        items: &[UpsertFileInput],
        scan_run_id: i64,
    ) -> DbResult<(u64, u64)> {
        if items.is_empty() {
            return Ok((0, 0));
        }

        let now = Utc::now().to_rfc3339();
        let tx = self.conn.transaction()?;
        let mut added = 0u64;
        let mut updated = 0u64;

        for item in items {
            // 1. Upsert content by (hash_algo, hash_hex)
            tx.execute(
                "INSERT INTO contents (hash_algo, hash_hex, byte_size, partial_hash, media_kind, format, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(hash_algo, hash_hex) DO NOTHING",
                params![
                    item.hash_algo.as_str(),
                    item.hash_hex,
                    item.byte_size as i64,
                    item.partial_hash,
                    item.media_kind.as_str(),
                    item.format,
                    now,
                ],
            )?;

            let content_id: i64 = tx.query_row(
                "SELECT id FROM contents WHERE hash_algo = ?1 AND hash_hex = ?2",
                params![item.hash_algo.as_str(), item.hash_hex],
                |r| r.get(0),
            )?;

            // 2. Upsert content_metadata if available
            if let Some(meta) = item.metadata.as_ref() {
                tx.execute(
                    "INSERT INTO content_metadata (
                        content_id, extractor, captured_at, captured_subsec,
                        camera_make, camera_model, camera_serial,
                        lens_model_raw, lens_model,
                        focal_length_mm, focal_length_35mm, f_number,
                        shutter_speed_s, shutter_speed_str, iso, exposure_bias,
                        width, height, orientation, gps_lat, gps_lon,
                        duration_ms, fps, raw_json, extracted_at
                     ) VALUES (
                        ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                        ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25
                     )
                     ON CONFLICT(content_id) DO UPDATE SET
                        extractor = excluded.extractor,
                        captured_at = COALESCE(excluded.captured_at, content_metadata.captured_at),
                        captured_subsec = COALESCE(excluded.captured_subsec, content_metadata.captured_subsec),
                        camera_make = COALESCE(excluded.camera_make, content_metadata.camera_make),
                        camera_model = COALESCE(excluded.camera_model, content_metadata.camera_model),
                        camera_serial = COALESCE(excluded.camera_serial, content_metadata.camera_serial),
                        lens_model_raw = COALESCE(excluded.lens_model_raw, content_metadata.lens_model_raw),
                        lens_model = COALESCE(excluded.lens_model, content_metadata.lens_model),
                        focal_length_mm = COALESCE(excluded.focal_length_mm, content_metadata.focal_length_mm),
                        focal_length_35mm = COALESCE(excluded.focal_length_35mm, content_metadata.focal_length_35mm),
                        f_number = COALESCE(excluded.f_number, content_metadata.f_number),
                        shutter_speed_s = COALESCE(excluded.shutter_speed_s, content_metadata.shutter_speed_s),
                        shutter_speed_str = COALESCE(excluded.shutter_speed_str, content_metadata.shutter_speed_str),
                        iso = COALESCE(excluded.iso, content_metadata.iso),
                        exposure_bias = COALESCE(excluded.exposure_bias, content_metadata.exposure_bias),
                        width = COALESCE(excluded.width, content_metadata.width),
                        height = COALESCE(excluded.height, content_metadata.height),
                        orientation = COALESCE(excluded.orientation, content_metadata.orientation),
                        gps_lat = COALESCE(excluded.gps_lat, content_metadata.gps_lat),
                        gps_lon = COALESCE(excluded.gps_lon, content_metadata.gps_lon),
                        duration_ms = COALESCE(excluded.duration_ms, content_metadata.duration_ms),
                        fps = COALESCE(excluded.fps, content_metadata.fps),
                        raw_json = COALESCE(excluded.raw_json, content_metadata.raw_json),
                        extracted_at = excluded.extracted_at",
                    params![
                        content_id,
                        meta.extractor,
                        meta.captured_at,
                        meta.captured_subsec,
                        meta.camera_make,
                        meta.camera_model,
                        meta.camera_serial,
                        meta.lens_model_raw,
                        meta.lens_model,
                        meta.focal_length_mm,
                        meta.focal_length_35mm,
                        meta.f_number,
                        meta.shutter_speed_s,
                        meta.shutter_speed_str,
                        meta.iso,
                        meta.exposure_bias,
                        meta.width,
                        meta.height,
                        meta.orientation,
                        meta.gps_lat,
                        meta.gps_lon,
                        meta.duration_ms,
                        meta.fps,
                        meta.raw_json,
                        now,
                    ],
                )?;
            }

            // 3. Resolve asset_id:
            //    - Sidecars (`.xmp`, `.thm`) NEVER create standalone logical assets; they link
            //      to an unambiguous parent asset in the same directory if already present (or stay
            //      NULL until `link_sidecar_files` runs at the end of the scan).
            //    - Media files reuse an existing asset for the same content (e.g. NAS backup) or
            //      perform O(1) indexed in-batch RAW+JPEG pairing.
            let asset_id: Option<i64> = if item.role == FileRole::Sidecar {
                find_in_batch_sidecar_parent_asset(&tx, item)?
            } else {
                let mut existing_asset_id: Option<i64> = tx
                    .query_row(
                        "SELECT asset_id FROM files WHERE content_id = ?1 AND asset_id IS NOT NULL LIMIT 1",
                        params![content_id],
                        |r| r.get(0),
                    )
                    .optional()?;

                if existing_asset_id.is_none() {
                    existing_asset_id = find_in_batch_raw_jpeg_asset(&tx, item)?;
                }

                let resolved = if let Some(aid) = existing_asset_id {
                    if let Some(meta) = item.metadata.as_ref() {
                        tx.execute(
                            "UPDATE assets SET
                                captured_at = COALESCE(?1, captured_at),
                                captured_subsec = COALESCE(?2, captured_subsec),
                                camera_make = COALESCE(?3, camera_make),
                                camera_model = COALESCE(?4, camera_model),
                                camera_serial = COALESCE(?5, camera_serial),
                                lens_model_raw = COALESCE(?6, lens_model_raw),
                                lens_model = COALESCE(?7, lens_model),
                                focal_length_mm = COALESCE(?8, focal_length_mm),
                                focal_length_35mm = COALESCE(?9, focal_length_35mm),
                                f_number = COALESCE(?10, f_number),
                                shutter_speed_s = COALESCE(?11, shutter_speed_s),
                                shutter_speed_str = COALESCE(?12, shutter_speed_str),
                                iso = COALESCE(?13, iso),
                                exposure_bias = COALESCE(?14, exposure_bias),
                                width = COALESCE(?15, width),
                                height = COALESCE(?16, height),
                                orientation = COALESCE(?17, orientation),
                                gps_lat = COALESCE(?18, gps_lat),
                                gps_lon = COALESCE(?19, gps_lon),
                                duration_ms = COALESCE(?20, duration_ms),
                                fps = COALESCE(?21, fps),
                                updated_at = ?22
                             WHERE id = ?23",
                            params![
                                meta.captured_at.as_deref(),
                                meta.captured_subsec.as_deref(),
                                meta.camera_make.as_deref(),
                                meta.camera_model.as_deref(),
                                meta.camera_serial.as_deref(),
                                meta.lens_model_raw.as_deref(),
                                meta.lens_model.as_deref(),
                                meta.focal_length_mm,
                                meta.focal_length_35mm,
                                meta.f_number,
                                meta.shutter_speed_s,
                                meta.shutter_speed_str.as_deref(),
                                meta.iso,
                                meta.exposure_bias,
                                meta.width,
                                meta.height,
                                meta.orientation,
                                meta.gps_lat,
                                meta.gps_lon,
                                meta.duration_ms,
                                meta.fps,
                                now,
                                aid,
                            ],
                        )?;
                    }
                    aid
                } else {
                    let public_id = Uuid::now_v7().to_string();
                    let meta = item.metadata.as_ref();
                    tx.execute(
                        "INSERT INTO assets (
                            public_id, captured_at, captured_subsec, camera_make, camera_model,
                            camera_serial, lens_model_raw, lens_model, focal_length_mm,
                            focal_length_35mm, f_number, shutter_speed_s, shutter_speed_str,
                            iso, exposure_bias, width, height, orientation, gps_lat, gps_lon,
                            duration_ms, fps, created_at, updated_at
                         ) VALUES (
                            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                            ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?23
                         )",
                        params![
                            public_id,
                            meta.and_then(|m| m.captured_at.as_deref()),
                            meta.and_then(|m| m.captured_subsec.as_deref()),
                            meta.and_then(|m| m.camera_make.as_deref()),
                            meta.and_then(|m| m.camera_model.as_deref()),
                            meta.and_then(|m| m.camera_serial.as_deref()),
                            meta.and_then(|m| m.lens_model_raw.as_deref()),
                            meta.and_then(|m| m.lens_model.as_deref()),
                            meta.and_then(|m| m.focal_length_mm),
                            meta.and_then(|m| m.focal_length_35mm),
                            meta.and_then(|m| m.f_number),
                            meta.and_then(|m| m.shutter_speed_s),
                            meta.and_then(|m| m.shutter_speed_str.as_deref()),
                            meta.and_then(|m| m.iso),
                            meta.and_then(|m| m.exposure_bias),
                            meta.and_then(|m| m.width),
                            meta.and_then(|m| m.height),
                            meta.and_then(|m| m.orientation),
                            meta.and_then(|m| m.gps_lat),
                            meta.and_then(|m| m.gps_lon),
                            meta.and_then(|m| m.duration_ms),
                            meta.and_then(|m| m.fps),
                            now,
                        ],
                    )?;
                    tx.last_insert_rowid()
                };
                Some(resolved)
            };

            // 4. Upsert physical file record (sets both last_seen_at and last_verified_at since content was hashed)
            let existed: Option<i64> = tx
                .query_row(
                    "SELECT id FROM files WHERE storage_id = ?1 AND rel_path = ?2",
                    params![item.storage_id, item.rel_path],
                    |r| r.get(0),
                )
                .optional()?;

            if existed.is_some() {
                tx.execute(
                    "UPDATE files SET
                        file_name = ?1,
                        file_stem = ?2,
                        ext = ?3,
                        byte_size = ?4,
                        mtime_ns = ?5,
                        ctime_ns = ?6,
                        content_id = ?7,
                        asset_id = COALESCE(asset_id, ?8),
                        role = ?9,
                        presence = 'present',
                        last_seen_scan_id = ?10,
                        last_seen_at = ?11,
                        last_verified_at = ?11,
                        updated_at = ?11
                     WHERE storage_id = ?12 AND rel_path = ?13",
                    params![
                        item.file_name,
                        item.file_stem,
                        item.ext,
                        item.byte_size as i64,
                        item.mtime_ns,
                        item.ctime_ns,
                        content_id,
                        asset_id,
                        item.role.as_str(),
                        scan_run_id,
                        now,
                        item.storage_id,
                        item.rel_path,
                    ],
                )?;
                updated += 1;
            } else {
                tx.execute(
                    "INSERT INTO files (
                        storage_id, rel_path, file_name, file_stem, ext,
                        byte_size, mtime_ns, ctime_ns, content_id, asset_id,
                        role, presence, last_seen_scan_id, last_seen_at, last_verified_at,
                        created_at, updated_at
                     ) VALUES (
                        ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'present', ?12, ?13, ?13, ?13, ?13
                     )",
                    params![
                        item.storage_id,
                        item.rel_path,
                        item.file_name,
                        item.file_stem,
                        item.ext,
                        item.byte_size as i64,
                        item.mtime_ns,
                        item.ctime_ns,
                        content_id,
                        asset_id,
                        item.role.as_str(),
                        scan_run_id,
                        now,
                    ],
                )?;
                added += 1;
            }
        }

        tx.commit()?;
        Ok((added, updated))
    }

    pub fn mark_unseen_as_missing(&self, storage_id: &str, scan_run_id: i64) -> DbResult<u64> {
        let now = Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "UPDATE files SET presence = 'missing', updated_at = ?1
             WHERE storage_id = ?2 AND presence = 'present'
               AND (last_seen_scan_id IS NULL OR last_seen_scan_id != ?3)",
            params![now, storage_id, scan_run_id],
        )?;
        Ok(n as u64)
    }

    /// Pair high-confidence RAW + JPEG/HEIF files within a storage into a single logical Asset.
    ///
    /// Rules (Fail Closed / never merge when ambiguous):
    /// - Same directory OR sibling `RAW` / `JPEG` (`JPG`) directories under the same parent.
    /// - Case-insensitive `file_stem` match.
    /// - Both files have non-null identical `captured_at`.
    /// - If both have `camera_model`, `camera_serial`, or `captured_subsec`, they must match.
    /// - Exactly 1 RAW asset and 1 Image asset must match for a given bucket; otherwise skipped.
    pub fn pair_raw_jpeg_assets(&mut self, storage_id: &str) -> DbResult<PairReport> {
        #[derive(Debug, Clone)]
        struct Candidate {
            asset_id: i64,
            media_kind: String,
            rel_path: String,
            file_stem: String,
            captured_at: Option<String>,
            captured_subsec: Option<String>,
            camera_model: Option<String>,
            camera_serial: Option<String>,
            priority_score: i64,
        }

        let candidates: Vec<Candidate> = {
            let mut stmt = self.conn.prepare(
                "SELECT f.asset_id, c.media_kind, f.rel_path, f.file_stem,
                        a.captured_at, a.captured_subsec, a.camera_model, a.camera_serial,
                        (CASE WHEN a.rating > 0 OR a.pick_status != 'unreviewed' OR a.is_favorite != 0 OR a.hot_pinned != 0 THEN 1 ELSE 0 END)
                 FROM files f
                 JOIN contents c ON c.id = f.content_id
                 JOIN assets a ON a.id = f.asset_id
                 WHERE f.storage_id = ?1
                   AND f.presence = 'present'
                   AND c.media_kind IN ('raw', 'image')",
            )?;
            let rows = stmt.query_map(params![storage_id], |r| {
                Ok(Candidate {
                    asset_id: r.get(0)?,
                    media_kind: r.get(1)?,
                    rel_path: r.get(2)?,
                    file_stem: r.get(3)?,
                    captured_at: r.get(4)?,
                    captured_subsec: r.get(5)?,
                    camera_model: r.get(6)?,
                    camera_serial: r.get(7)?,
                    priority_score: r.get(8)?,
                })
            })?;
            let mut list = Vec::new();
            for row in rows {
                list.push(row?);
            }
            list
        };

        // Group by (pairing_folder_key, lowercase_stem)
        let mut buckets: HashMap<(String, String), (Vec<Candidate>, Vec<Candidate>)> = HashMap::new();
        for cand in candidates {
            let folder_key = pairing_folder_key(&cand.rel_path, &cand.media_kind);
            let stem_key = cand.file_stem.to_ascii_lowercase();
            let entry = buckets.entry((folder_key, stem_key)).or_default();
            if cand.media_kind == "raw" {
                entry.0.push(cand);
            } else {
                entry.1.push(cand);
            }
        }

        let mut report = PairReport::default();
        let mut merges: Vec<(i64, i64)> = Vec::new(); // (keep_asset_id, absorb_asset_id)

        for (_key, (raws, images)) in buckets {
            if raws.is_empty() || images.is_empty() {
                continue;
            }
            // Must be unambiguous 1-to-1 within this folder+stem bucket
            if raws.len() != 1 || images.len() != 1 {
                report.ambiguous_skipped += 1;
                continue;
            }
            let r = &raws[0];
            let i = &images[0];
            if r.asset_id == i.asset_id {
                continue; // Already merged
            }

            // Require non-null identical captured_at
            let (Some(r_dt), Some(i_dt)) = (&r.captured_at, &i.captured_at) else {
                report.ambiguous_skipped += 1;
                continue;
            };
            if r_dt != i_dt {
                report.ambiguous_skipped += 1;
                continue;
            }

            // If both sides have camera_model / camera_serial / captured_subsec, they must match
            if !opt_eq_if_both(&r.camera_model, &i.camera_model)
                || !opt_eq_if_both(&r.camera_serial, &i.camera_serial)
                || !opt_eq_if_both(&r.captured_subsec, &i.captured_subsec)
            {
                report.ambiguous_skipped += 1;
                continue;
            }

            // Prefer asset with user curation (rating/pick/favorite/pin), then smaller id for determinism
            let (keep, absorb) = if r.priority_score > i.priority_score
                || (r.priority_score == i.priority_score && r.asset_id < i.asset_id)
            {
                (r.asset_id, i.asset_id)
            } else {
                (i.asset_id, r.asset_id)
            };
            merges.push((keep, absorb));
        }

        if merges.is_empty() {
            return Ok(report);
        }

        let now = Utc::now().to_rfc3339();
        let tx = self.conn.transaction()?;
        for (keep, absorb) in merges {
            // Backfill any NULL metadata on the surviving asset from the absorbed asset
            tx.execute(
                "UPDATE assets SET
                    captured_at = COALESCE(captured_at, (SELECT captured_at FROM assets WHERE id = ?2)),
                    captured_subsec = COALESCE(captured_subsec, (SELECT captured_subsec FROM assets WHERE id = ?2)),
                    camera_make = COALESCE(camera_make, (SELECT camera_make FROM assets WHERE id = ?2)),
                    camera_model = COALESCE(camera_model, (SELECT camera_model FROM assets WHERE id = ?2)),
                    camera_serial = COALESCE(camera_serial, (SELECT camera_serial FROM assets WHERE id = ?2)),
                    lens_model_raw = COALESCE(lens_model_raw, (SELECT lens_model_raw FROM assets WHERE id = ?2)),
                    lens_model = COALESCE(lens_model, (SELECT lens_model FROM assets WHERE id = ?2)),
                    focal_length_mm = COALESCE(focal_length_mm, (SELECT focal_length_mm FROM assets WHERE id = ?2)),
                    focal_length_35mm = COALESCE(focal_length_35mm, (SELECT focal_length_35mm FROM assets WHERE id = ?2)),
                    f_number = COALESCE(f_number, (SELECT f_number FROM assets WHERE id = ?2)),
                    shutter_speed_s = COALESCE(shutter_speed_s, (SELECT shutter_speed_s FROM assets WHERE id = ?2)),
                    shutter_speed_str = COALESCE(shutter_speed_str, (SELECT shutter_speed_str FROM assets WHERE id = ?2)),
                    iso = COALESCE(iso, (SELECT iso FROM assets WHERE id = ?2)),
                    exposure_bias = COALESCE(exposure_bias, (SELECT exposure_bias FROM assets WHERE id = ?2)),
                    width = COALESCE(width, (SELECT width FROM assets WHERE id = ?2)),
                    height = COALESCE(height, (SELECT height FROM assets WHERE id = ?2)),
                    orientation = COALESCE(orientation, (SELECT orientation FROM assets WHERE id = ?2)),
                    gps_lat = COALESCE(gps_lat, (SELECT gps_lat FROM assets WHERE id = ?2)),
                    gps_lon = COALESCE(gps_lon, (SELECT gps_lon FROM assets WHERE id = ?2)),
                    updated_at = ?3
                 WHERE id = ?1",
                params![keep, absorb, now],
            )?;

            // Re-point all files (across all storages, including NAS backups) from absorb -> keep
            tx.execute(
                "UPDATE files SET asset_id = ?1, updated_at = ?3 WHERE asset_id = ?2",
                params![keep, absorb, now],
            )?;

            // Delete the absorbed asset only if no files still reference it
            tx.execute(
                "DELETE FROM assets WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM files WHERE asset_id = ?1)",
                params![absorb],
            )?;

            report.pairs_merged += 1;
        }
        tx.commit()?;

        Ok(report)
    }

    /// Link unattached sidecar files (`.xmp`, `.thm`) within a storage to their parent logical Asset
    /// in the same directory.
    ///
    /// Rules (Fail Closed):
    /// - Same directory (case-insensitive on Windows).
    /// - Case-insensitive match between `sidecar_parent_stem(sidecar.file_name)` and parent `file_stem`.
    /// - Only links when all matching parent files resolve to exactly ONE distinct `asset_id`
    ///   (e.g. a single RAW/Video or a paired RAW+JPEG sharing the same `asset_id`).
    /// - Orphaned or ambiguous sidecars remain `asset_id = NULL` (never creating bogus Assets).
    pub fn link_sidecar_files(&mut self, storage_id: &str) -> DbResult<SidecarLinkReport> {
        let unlinked: Vec<(i64, String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, rel_path, file_name
                 FROM files
                 WHERE storage_id = ?1
                   AND presence = 'present'
                   AND role = 'sidecar'
                   AND asset_id IS NULL",
            )?;
            let rows = stmt.query_map(params![storage_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };

        if unlinked.is_empty() {
            return Ok(SidecarLinkReport::default());
        }

        // Index parent assets by (dir_key, lowercase_stem) -> Option<i64> (None = ambiguous)
        let mut parent_map: HashMap<(String, String), Option<i64>> = HashMap::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT rel_path, file_stem, asset_id
                 FROM files
                 WHERE storage_id = ?1
                   AND presence = 'present'
                   AND role != 'sidecar'
                   AND asset_id IS NOT NULL",
            )?;
            let rows = stmt.query_map(params![storage_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (rel_path, stem, aid) = row?;
                let key = (dir_key(&rel_path), stem.to_ascii_lowercase());
                parent_map
                    .entry(key)
                    .and_modify(|existing| {
                        if *existing != Some(aid) {
                            *existing = None; // Ambiguous: multiple distinct parent assets
                        }
                    })
                    .or_insert(Some(aid));
            }
        }

        let mut updates: Vec<(i64, i64)> = Vec::new(); // (file_id, parent_asset_id)
        let mut report = SidecarLinkReport::default();

        for (file_id, rel_path, file_name) in unlinked {
            let Some(parent_stem) = sidecar_parent_stem(&file_name) else {
                report.unmatched_or_ambiguous += 1;
                continue;
            };
            let key = (dir_key(&rel_path), parent_stem.to_ascii_lowercase());
            match parent_map.get(&key) {
                Some(Some(parent_aid)) => updates.push((file_id, *parent_aid)),
                _ => report.unmatched_or_ambiguous += 1,
            }
        }

        if updates.is_empty() {
            return Ok(report);
        }

        let now = Utc::now().to_rfc3339();
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE files SET asset_id = ?1, updated_at = ?2 WHERE id = ?3 AND asset_id IS NULL",
            )?;
            for (file_id, parent_aid) in updates {
                let n = stmt.execute(params![parent_aid, now, file_id])?;
                report.linked += n as u64;
            }
        }
        tx.commit()?;

        Ok(report)
    }

    /// Stream all present files of a storage (with hash + metadata) to a callback as `ScanManifestRecord`s.
    /// Avoids loading the entire catalog into memory (32-bit and million-file safe).
    pub fn stream_storage_manifest<F>(
        &self,
        storage_id: &str,
        mut on_record: F,
    ) -> DbResult<u64>
    where
        F: FnMut(ScanManifestRecord) -> std::io::Result<()>,
    {
        let mut stmt = self.conn.prepare(
            "SELECT f.storage_id, f.rel_path, f.file_name, f.file_stem, f.ext,
                    f.byte_size, f.mtime_ns, f.ctime_ns,
                    c.media_kind, c.format, c.hash_algo, c.hash_hex, c.partial_hash,
                    m.extractor, m.captured_at, m.captured_subsec,
                    m.camera_make, m.camera_model, m.camera_serial,
                    m.lens_model_raw, m.lens_model,
                    m.focal_length_mm, m.focal_length_35mm, m.f_number,
                    m.shutter_speed_s, m.shutter_speed_str, m.iso, m.exposure_bias,
                    m.width, m.height, m.orientation, m.gps_lat, m.gps_lon,
                    m.duration_ms, m.fps
             FROM files f
             JOIN contents c ON c.id = f.content_id
             LEFT JOIN content_metadata m ON m.content_id = c.id
             WHERE f.storage_id = ?1 AND f.presence = 'present'
             ORDER BY f.id ASC",
        )?;

        let mut rows = stmt.query(params![storage_id])?;
        let mut count = 0u64;

        while let Some(r) = rows.next()? {
            let kind_str: String = r.get(8)?;
            let media_kind = match kind_str.as_str() {
                "image" => MediaKind::Image,
                "raw" => MediaKind::Raw,
                "video" => MediaKind::Video,
                _ => MediaKind::Other,
            };
            let algo_str: String = r.get(10)?;
            let hash_algo = match algo_str.as_str() {
                "sha256" => HashAlgorithm::Sha256,
                _ => HashAlgorithm::Blake3,
            };
            let extractor: Option<String> = r.get(13)?;
            let metadata = extractor.map(|ext_name| ExtractedMetadata {
                extractor: ext_name,
                captured_at: r.get(14).unwrap_or(None),
                captured_subsec: r.get(15).unwrap_or(None),
                camera_make: r.get(16).unwrap_or(None),
                camera_model: r.get(17).unwrap_or(None),
                camera_serial: r.get(18).unwrap_or(None),
                lens_model_raw: r.get(19).unwrap_or(None),
                lens_model: r.get(20).unwrap_or(None),
                focal_length_mm: r.get(21).unwrap_or(None),
                focal_length_35mm: r.get(22).unwrap_or(None),
                f_number: r.get(23).unwrap_or(None),
                shutter_speed_s: r.get(24).unwrap_or(None),
                shutter_speed_str: r.get(25).unwrap_or(None),
                iso: r.get(26).unwrap_or(None),
                exposure_bias: r.get(27).unwrap_or(None),
                width: r.get(28).unwrap_or(None),
                height: r.get(29).unwrap_or(None),
                orientation: r.get(30).unwrap_or(None),
                gps_lat: r.get(31).unwrap_or(None),
                gps_lon: r.get(32).unwrap_or(None),
                duration_ms: r.get(33).unwrap_or(None),
                fps: r.get(34).unwrap_or(None),
                raw_json: None,
            });

            let rec = ScanManifestRecord {
                storage_id: r.get(0)?,
                rel_path: r.get(1)?,
                file_name: r.get(2)?,
                file_stem: r.get(3)?,
                ext: r.get(4)?,
                byte_size: r.get::<_, i64>(5)? as u64,
                mtime_ns: r.get(6)?,
                ctime_ns: r.get(7)?,
                media_kind,
                format: r.get(9)?,
                hash_algo,
                hash_hex: r.get(11)?,
                partial_hash: r.get(12)?,
                metadata,
            };

            on_record(rec)?;
            count += 1;
        }

        Ok(count)
    }

    pub fn status(&self) -> DbResult<StatusReport> {
        let schema_version: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |r| r.get(0),
        )?;
        let total_assets: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM assets", [], |r| r.get(0))?;
        let total_contents: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM contents", [], |r| r.get(0))?;
        let total_present_files: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE presence = 'present'",
            [],
            |r| r.get(0),
        )?;
        let total_missing_files: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE presence = 'missing'",
            [],
            |r| r.get(0),
        )?;
        let total_present_bytes: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(byte_size), 0) FROM files WHERE presence = 'present'",
            [],
            |r| r.get(0),
        )?;
        let storages = self.storage_stats()?;

        Ok(StatusReport {
            schema_version,
            total_assets,
            total_contents,
            total_present_files,
            total_missing_files,
            total_present_bytes,
            storages,
        })
    }

    pub fn stats(&self) -> DbResult<StatsReport> {
        self.stats_with_options(false)
    }

    pub fn stats_with_options(&self, split_extenders: bool) -> DbResult<StatsReport> {
        self.stats_filtered(&StatsFilter {
            split_extenders,
            ..Default::default()
        })
    }

    pub fn stats_filtered(&self, filter: &StatsFilter) -> DbResult<StatsReport> {
        let st = self.status()?;
        let has_filter = filter.has_asset_filter();

        let (
            total_assets,
            total_contents,
            total_present_files,
            total_present_bytes,
            backed_up_contents,
            by_format,
        ) = if !has_filter {
            let backed_up: i64 = self.conn.query_row(
                "SELECT COUNT(DISTINCT f.content_id)
                 FROM files f
                 JOIN storages s ON s.id = f.storage_id
                 WHERE f.presence = 'present' AND f.content_id IS NOT NULL AND s.tier = 'backup'",
                [],
                |r| r.get(0),
            )?;
            let fmt_buckets = self.query_buckets(
                "SELECT COALESCE(c.format, 'unknown'), COUNT(*), COALESCE(SUM(f.byte_size), 0)
                 FROM files f
                 LEFT JOIN contents c ON c.id = f.content_id
                 WHERE f.presence = 'present'
                 GROUP BY COALESCE(c.format, 'unknown')
                 ORDER BY COUNT(*) DESC",
            )?;
            (
                st.total_assets,
                st.total_contents,
                st.total_present_files,
                st.total_present_bytes,
                backed_up,
                fmt_buckets,
            )
        } else {
            let mut w = String::from("WHERE 1=1");
            let mut b: Vec<rusqlite::types::Value> = Vec::new();
            append_stats_asset_where("a", filter, &mut w, &mut b);

            let total_assets: i64 = self.conn.query_row(
                &format!("SELECT COUNT(*) FROM assets a {w}"),
                rusqlite::params_from_iter(b.clone()),
                |r| r.get(0),
            )?;
            let (total_contents, total_present_files, total_present_bytes): (i64, i64, i64) =
                self.conn.query_row(
                    &format!(
                        "SELECT COUNT(DISTINCT f.content_id), COUNT(*), COALESCE(SUM(f.byte_size), 0)
                         FROM files f
                         JOIN assets a ON a.id = f.asset_id
                         {w} AND f.presence = 'present'"
                    ),
                    rusqlite::params_from_iter(b.clone()),
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
            let backed_up: i64 = self.conn.query_row(
                &format!(
                    "SELECT COUNT(DISTINCT f.content_id)
                     FROM files f
                     JOIN assets a ON a.id = f.asset_id
                     JOIN storages s ON s.id = f.storage_id
                     {w} AND f.presence = 'present' AND f.content_id IS NOT NULL AND s.tier = 'backup'"
                ),
                rusqlite::params_from_iter(b.clone()),
                |r| r.get(0),
            )?;
            let fmt_buckets = self.query_buckets_with_params(
                &format!(
                    "SELECT COALESCE(c.format, 'unknown'), COUNT(*), COALESCE(SUM(f.byte_size), 0)
                     FROM files f
                     JOIN assets a ON a.id = f.asset_id
                     LEFT JOIN contents c ON c.id = f.content_id
                     {w} AND f.presence = 'present'
                     GROUP BY COALESCE(c.format, 'unknown')
                     ORDER BY COUNT(*) DESC"
                ),
                b,
            )?;
            (
                total_assets,
                total_contents,
                total_present_files,
                total_present_bytes,
                backed_up,
                fmt_buckets,
            )
        };

        let mut asset_where = String::from("WHERE 1=1");
        let mut asset_binds: Vec<rusqlite::types::Value> = Vec::new();
        append_stats_asset_where("a", filter, &mut asset_where, &mut asset_binds);

        let by_camera = self.query_buckets_with_params(
            &format!(
                "SELECT COALESCE(a.camera_model, '(unknown)'), COUNT(DISTINCT a.id), 0
                 FROM assets a
                 {asset_where}
                 GROUP BY COALESCE(a.camera_model, '(unknown)')
                 ORDER BY COUNT(DISTINCT a.id) DESC
                 LIMIT 30"
            ),
            asset_binds.clone(),
        )?;

        let by_lens = if !filter.split_extenders {
            self.query_buckets_with_params(
                &format!(
                    "SELECT COALESCE(a.lens_model, '(unknown)'), COUNT(DISTINCT a.id), 0
                     FROM assets a
                     {asset_where}
                     GROUP BY COALESCE(a.lens_model, '(unknown)')
                     ORDER BY COUNT(DISTINCT a.id) DESC
                     LIMIT 30"
                ),
                asset_binds,
            )?
        } else {
            let raw_buckets = self.query_buckets_with_params(
                &format!(
                    "SELECT COALESCE(a.lens_model, '(unknown)'), COUNT(DISTINCT a.id), 0
                     FROM assets a
                     {asset_where}
                     GROUP BY COALESCE(a.lens_model, '(unknown)')"
                ),
                asset_binds,
            )?;
            let mut merged: HashMap<String, i64> = HashMap::new();
            for b in raw_buckets {
                if b.label == "(unknown)" {
                    *merged.entry(b.label).or_insert(0) += b.count;
                } else {
                    let (parent, ext) = split_lens_extender(&b.label);
                    *merged.entry(parent).or_insert(0) += b.count;
                    if let Some(ext_name) = ext {
                        *merged.entry(ext_name).or_insert(0) += b.count;
                    }
                }
            }
            let mut rows: Vec<BucketCount> = merged
                .into_iter()
                .map(|(label, count)| BucketCount {
                    label,
                    count,
                    total_bytes: 0,
                })
                .collect();
            rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
            rows.truncate(30);
            rows
        };

        Ok(StatsReport {
            total_assets,
            total_contents,
            total_present_files,
            total_present_bytes,
            backed_up_contents,
            by_format,
            by_camera,
            by_lens,
            by_storage: st.storages,
        })
    }

    /// Yearly asset counts, active shooting days, and top camera/lens per year.
    /// Malformed EXIF timestamps (e.g. `0000-...`) or NULL are grouped into `(unknown)`.
    pub fn stats_by_year(&self) -> DbResult<Vec<YearStatsRow>> {
        self.stats_by_year_with_options(false)
    }

    pub fn stats_by_year_with_options(&self, split_extenders: bool) -> DbResult<Vec<YearStatsRow>> {
        self.stats_by_year_filtered(&StatsFilter {
            split_extenders,
            ..Default::default()
        })
    }

    pub fn stats_by_year_filtered(&self, filter: &StatsFilter) -> DbResult<Vec<YearStatsRow>> {
        const YEAR_EXPR: &str = "CASE WHEN a.captured_at IS NOT NULL AND length(a.captured_at) >= 10 \
             AND substr(a.captured_at, 1, 4) >= '1970' AND substr(a.captured_at, 1, 4) <= '2999' \
             THEN substr(a.captured_at, 1, 4) ELSE '(unknown)' END";
        const DAY_EXPR: &str = "CASE WHEN a.captured_at IS NOT NULL AND length(a.captured_at) >= 10 \
             AND substr(a.captured_at, 1, 4) >= '1970' AND substr(a.captured_at, 1, 4) <= '2999' \
             THEN substr(a.captured_at, 1, 10) ELSE NULL END";

        let mut asset_where = String::from("WHERE 1=1");
        let mut asset_binds: Vec<rusqlite::types::Value> = Vec::new();
        append_stats_asset_where("a", filter, &mut asset_where, &mut asset_binds);

        let mut base_rows: Vec<(String, i64, i64)> = Vec::new();
        {
            let sql = format!(
                "SELECT {YEAR_EXPR} AS yr, COUNT(*), COUNT(DISTINCT {DAY_EXPR})
                 FROM assets a
                 {asset_where}
                 GROUP BY yr
                 ORDER BY CASE WHEN yr = '(unknown)' THEN 1 ELSE 0 END, yr ASC"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(asset_binds.clone()), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            for row in rows {
                base_rows.push(row?);
            }
        }

        let mut top_cam_by_year: HashMap<String, (String, i64)> = HashMap::new();
        {
            let sql = format!(
                "SELECT {YEAR_EXPR} AS yr, a.camera_model, COUNT(*)
                 FROM assets a
                 {asset_where} AND a.camera_model IS NOT NULL
                 GROUP BY yr, a.camera_model"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(asset_binds.clone()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
            })?;
            for row in rows {
                let (yr, cam, cnt) = row?;
                let replace = match top_cam_by_year.get(&yr) {
                    None => true,
                    Some((prev_name, prev_cnt)) => {
                        cnt > *prev_cnt || (cnt == *prev_cnt && cam < *prev_name)
                    }
                };
                if replace {
                    top_cam_by_year.insert(yr, (cam, cnt));
                }
            }
        }

        let mut top_lens_by_year: HashMap<String, (String, i64)> = HashMap::new();
        {
            let sql = format!(
                "SELECT {YEAR_EXPR} AS yr, a.lens_model, COUNT(*)
                 FROM assets a
                 {asset_where} AND a.lens_model IS NOT NULL
                 GROUP BY yr, a.lens_model"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(asset_binds), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
            })?;
            if !filter.split_extenders {
                for row in rows {
                    let (yr, lens, cnt) = row?;
                    let replace = match top_lens_by_year.get(&yr) {
                        None => true,
                        Some((prev_name, prev_cnt)) => {
                            cnt > *prev_cnt || (cnt == *prev_cnt && lens < *prev_name)
                        }
                    };
                    if replace {
                        top_lens_by_year.insert(yr, (lens, cnt));
                    }
                }
            } else {
                let mut counts: HashMap<(String, String), i64> = HashMap::new();
                for row in rows {
                    let (yr, lens, cnt) = row?;
                    let (parent, _) = split_lens_extender(&lens);
                    *counts.entry((yr, parent)).or_insert(0) += cnt;
                }
                for ((yr, lens), cnt) in counts {
                    let replace = match top_lens_by_year.get(&yr) {
                        None => true,
                        Some((prev_name, prev_cnt)) => {
                            cnt > *prev_cnt || (cnt == *prev_cnt && lens < *prev_name)
                        }
                    };
                    if replace {
                        top_lens_by_year.insert(yr, (lens, cnt));
                    }
                }
            }
        }

        Ok(base_rows
            .into_iter()
            .map(|(year, assets, active_days)| {
                let top_camera = top_cam_by_year.remove(&year).map(|(name, _)| name);
                let top_lens = top_lens_by_year.remove(&year).map(|(name, _)| name);
                YearStatsRow {
                    year,
                    assets,
                    active_days,
                    top_camera,
                    top_lens,
                }
            })
            .collect())
    }

    /// Chronological camera and lens usage timeline (`first_captured`..`last_captured`,
    /// total assets, active shooting days, and most frequent partner lens/camera).
    pub fn gear_history(&self) -> DbResult<GearHistoryReport> {
        self.gear_history_with_options(false)
    }

    pub fn gear_history_with_options(&self, split_extenders: bool) -> DbResult<GearHistoryReport> {
        self.gear_history_filtered(&StatsFilter {
            split_extenders,
            ..Default::default()
        })
    }

    pub fn gear_history_filtered(&self, filter: &StatsFilter) -> DbResult<GearHistoryReport> {
        const VALID_DAY: &str = "CASE WHEN a.captured_at IS NOT NULL AND length(a.captured_at) >= 10 \
             AND substr(a.captured_at, 1, 4) >= '1970' AND substr(a.captured_at, 1, 4) <= '2999' \
             THEN substr(a.captured_at, 1, 10) ELSE NULL END";

        let mut asset_where = String::from("WHERE 1=1");
        let mut asset_binds: Vec<rusqlite::types::Value> = Vec::new();
        append_stats_asset_where("a", filter, &mut asset_where, &mut asset_binds);

        // Compute top partner lens per camera and top partner camera per lens in one pass.
        let mut top_lens_for_cam: HashMap<String, (String, i64)> = HashMap::new();
        let mut top_cam_for_lens: HashMap<String, (String, i64)> = HashMap::new();
        {
            let sql = format!(
                "SELECT a.camera_model, a.lens_model, COUNT(*)
                 FROM assets a
                 {asset_where} AND a.camera_model IS NOT NULL AND a.lens_model IS NOT NULL
                 GROUP BY a.camera_model, a.lens_model"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(asset_binds.clone()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
            })?;
            if !filter.split_extenders {
                for row in rows {
                    let (cam, lens, cnt) = row?;
                    let update_cam = match top_lens_for_cam.get(&cam) {
                        None => true,
                        Some((prev_name, prev_cnt)) => {
                            cnt > *prev_cnt || (cnt == *prev_cnt && lens < *prev_name)
                        }
                    };
                    if update_cam {
                        top_lens_for_cam.insert(cam.clone(), (lens.clone(), cnt));
                    }
                    let update_lens = match top_cam_for_lens.get(&lens) {
                        None => true,
                        Some((prev_name, prev_cnt)) => {
                            cnt > *prev_cnt || (cnt == *prev_cnt && cam < *prev_name)
                        }
                    };
                    if update_lens {
                        top_cam_for_lens.insert(lens, (cam, cnt));
                    }
                }
            } else {
                let mut cam_parent_counts: HashMap<(String, String), i64> = HashMap::new();
                let mut lens_cam_counts: HashMap<(String, String), i64> = HashMap::new();
                for row in rows {
                    let (cam, lens, cnt) = row?;
                    let (parent, ext) = split_lens_extender(&lens);
                    *cam_parent_counts
                        .entry((cam.clone(), parent.clone()))
                        .or_insert(0) += cnt;
                    *lens_cam_counts.entry((parent, cam.clone())).or_insert(0) += cnt;
                    if let Some(ext_name) = ext {
                        *lens_cam_counts.entry((ext_name, cam)).or_insert(0) += cnt;
                    }
                }
                for ((cam, lens), cnt) in cam_parent_counts {
                    let update_cam = match top_lens_for_cam.get(&cam) {
                        None => true,
                        Some((prev_name, prev_cnt)) => {
                            cnt > *prev_cnt || (cnt == *prev_cnt && lens < *prev_name)
                        }
                    };
                    if update_cam {
                        top_lens_for_cam.insert(cam, (lens, cnt));
                    }
                }
                for ((lens, cam), cnt) in lens_cam_counts {
                    let update_lens = match top_cam_for_lens.get(&lens) {
                        None => true,
                        Some((prev_name, prev_cnt)) => {
                            cnt > *prev_cnt || (cnt == *prev_cnt && cam < *prev_name)
                        }
                    };
                    if update_lens {
                        top_cam_for_lens.insert(lens, (cam, cnt));
                    }
                }
            }
        }

        let mut cameras = Vec::new();
        {
            let sql = format!(
                "SELECT a.camera_model, COUNT(*), COUNT(DISTINCT {VALID_DAY}),
                        MIN({VALID_DAY}), MAX({VALID_DAY})
                 FROM assets a
                 {asset_where} AND a.camera_model IS NOT NULL
                 GROUP BY a.camera_model
                 ORDER BY CASE WHEN MIN({VALID_DAY}) IS NULL THEN 1 ELSE 0 END,
                          MIN({VALID_DAY}) ASC, COUNT(*) DESC, a.camera_model ASC"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(asset_binds.clone()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?;
            for row in rows {
                let (name, assets, active_days, first_captured, last_captured) = row?;
                let top_partner = top_lens_for_cam.remove(&name).map(|(p, _)| p);
                cameras.push(GearTimelineRow {
                    name,
                    assets,
                    active_days,
                    first_captured,
                    last_captured,
                    top_partner,
                });
            }
        }

        let mut lenses = Vec::new();
        if !filter.split_extenders {
            let sql = format!(
                "SELECT a.lens_model, COUNT(*), COUNT(DISTINCT {VALID_DAY}),
                        MIN({VALID_DAY}), MAX({VALID_DAY})
                 FROM assets a
                 {asset_where} AND a.lens_model IS NOT NULL
                 GROUP BY a.lens_model
                 ORDER BY CASE WHEN MIN({VALID_DAY}) IS NULL THEN 1 ELSE 0 END,
                          MIN({VALID_DAY}) ASC, COUNT(*) DESC, a.lens_model ASC"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(asset_binds), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?;
            for row in rows {
                let (name, assets, active_days, first_captured, last_captured) = row?;
                let top_partner = top_cam_for_lens.remove(&name).map(|(p, _)| p);
                lenses.push(GearTimelineRow {
                    name,
                    assets,
                    active_days,
                    first_captured,
                    last_captured,
                    top_partner,
                });
            }
        } else {
            #[derive(Default)]
            struct SplitLensAgg {
                assets: i64,
                days: HashSet<String>,
                first: Option<String>,
                last: Option<String>,
            }
            impl SplitLensAgg {
                fn add(&mut self, day: Option<&str>, cnt: i64) {
                    self.assets += cnt;
                    if let Some(d) = day {
                        self.days.insert(d.to_string());
                        if self.first.as_deref().is_none_or(|f| d < f) {
                            self.first = Some(d.to_string());
                        }
                        if self.last.as_deref().is_none_or(|l| d > l) {
                            self.last = Some(d.to_string());
                        }
                    }
                }
            }

            let sql = format!(
                "SELECT a.lens_model, {VALID_DAY} AS d, COUNT(*)
                 FROM assets a
                 {asset_where} AND a.lens_model IS NOT NULL
                 GROUP BY a.lens_model, d"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(asset_binds), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            let mut aggs: HashMap<String, SplitLensAgg> = HashMap::new();
            for row in rows {
                let (lens_raw, day_opt, cnt) = row?;
                let (parent, ext) = split_lens_extender(&lens_raw);
                aggs.entry(parent).or_default().add(day_opt.as_deref(), cnt);
                if let Some(ext_name) = ext {
                    aggs.entry(ext_name)
                        .or_default()
                        .add(day_opt.as_deref(), cnt);
                }
            }

            for (name, agg) in aggs {
                let top_partner = top_cam_for_lens.remove(&name).map(|(p, _)| p);
                lenses.push(GearTimelineRow {
                    name,
                    assets: agg.assets,
                    active_days: agg.days.len() as i64,
                    first_captured: agg.first,
                    last_captured: agg.last,
                    top_partner,
                });
            }
            lenses.sort_by(|a, b| {
                a.first_captured
                    .is_none()
                    .cmp(&b.first_captured.is_none())
                    .then_with(|| a.first_captured.cmp(&b.first_captured))
                    .then_with(|| b.assets.cmp(&a.assets))
                    .then_with(|| a.name.cmp(&b.name))
            });
        }

        Ok(GearHistoryReport { cameras, lenses })
    }

    /// Focal-length distribution histogram (range buckets + top exact rounded mm values).
    pub fn focal_distribution(&self, filter: &StatsFilter) -> DbResult<FocalDistributionReport> {
        let mut asset_where = String::from("WHERE a.focal_length_mm IS NOT NULL AND a.focal_length_mm > 0");
        let mut asset_binds: Vec<rusqlite::types::Value> = Vec::new();
        append_stats_asset_where("a", filter, &mut asset_where, &mut asset_binds);

        let (total_with_focal, min_mm, max_mm, avg_mm): (i64, Option<f64>, Option<f64>, Option<f64>) =
            self.conn.query_row(
                &format!(
                    "SELECT COUNT(*), MIN(a.focal_length_mm), MAX(a.focal_length_mm), AVG(a.focal_length_mm)
                     FROM assets a
                     {asset_where}"
                ),
                rusqlite::params_from_iter(asset_binds.clone()),
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;

        const BUCKET_DEFS: [(&str, i64, i64); 11] = [
            ("< 24mm", 0, 23),
            ("24-35mm", 24, 35),
            ("36-70mm", 36, 70),
            ("71-135mm", 71, 135),
            ("136-200mm", 136, 200),
            ("201-300mm", 201, 300),
            ("301-400mm", 301, 400),
            ("401-500mm", 401, 500),
            ("501-600mm", 501, 600),
            ("601-800mm", 601, 800),
            ("> 800mm", 801, i64::MAX),
        ];
        let mut bucket_counts = [0i64; 11];
        let mut exact_rows: Vec<FocalExactRow> = Vec::new();

        {
            let sql = format!(
                "SELECT CAST(ROUND(a.focal_length_mm) AS INTEGER) AS mm, COUNT(*)
                 FROM assets a
                 {asset_where}
                 GROUP BY mm"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(asset_binds), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (mm, cnt) = row?;
                for (idx, &(_, lo, hi)) in BUCKET_DEFS.iter().enumerate() {
                    if mm >= lo && mm <= hi {
                        bucket_counts[idx] += cnt;
                        break;
                    }
                }
                exact_rows.push(FocalExactRow {
                    focal_mm: mm,
                    count: cnt,
                });
            }
        }

        exact_rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.focal_mm.cmp(&b.focal_mm)));
        exact_rows.truncate(12);

        let buckets = BUCKET_DEFS
            .iter()
            .enumerate()
            .map(|(idx, &(label, _, _))| FocalBucketRow {
                label: label.to_string(),
                count: bucket_counts[idx],
            })
            .collect();

        Ok(FocalDistributionReport {
            total_with_focal,
            min_mm,
            max_mm,
            avg_mm,
            buckets,
            top_exact_mm: exact_rows,
        })
    }

    /// Read-only duplicate content detection respecting backup & read-only protection invariants.
    ///
    /// Rules (Fail Closed / never treat intentional backups as duplicates):
    /// - A `content_id` with 1 copy on a managed storage and 1 copy on a backup storage is an
    ///   intentional backup, NOT a duplicate.
    /// - Actionable duplicates occur when >= 2 present copies exist on managed non-backup storages
    ///   (`access_mode = 'managed' AND tier != 'backup' AND backup_mode != 'external'`).
    /// - Within-storage duplicates inside `read_only`/`backup`/`external` storages are only shown
    ///   when `filter.include_backups` is `true` or `filter.storage_id` explicitly targets that
    ///   storage, and their files are always marked `protected = true` with `reclaimable_bytes = 0`.
    pub fn find_duplicates(&self, filter: &DuplicateFilter) -> DbResult<DuplicateReport> {
        let mut sql = String::from(
            "SELECT f.content_id, c.hash_algo, c.hash_hex, c.format, c.byte_size,
                    f.id, f.asset_id, f.storage_id, f.rel_path,
                    s.tier, s.access_mode, s.backup_mode, f.role
             FROM files f
             JOIN contents c ON c.id = f.content_id
             JOIN storages s ON s.id = f.storage_id
             WHERE f.presence = 'present'
               AND f.content_id IS NOT NULL
               AND f.content_id IN (
                   SELECT f2.content_id
                   FROM files f2
                   WHERE f2.presence = 'present'
                     AND f2.content_id IS NOT NULL",
        );

        let mut bind_values: Vec<rusqlite::types::Value> = Vec::new();
        if !filter.include_sidecars {
            sql.push_str(" AND f2.role != 'sidecar'");
        }
        if let Some(sid) = &filter.storage_id {
            sql.push_str(" AND f2.storage_id = ?");
            bind_values.push(sid.clone().into());
        }
        sql.push_str(" GROUP BY f2.content_id HAVING COUNT(*) > 1)");

        if !filter.include_sidecars {
            sql.push_str(" AND f.role != 'sidecar'");
        }
        if let Some(sid) = &filter.storage_id {
            sql.push_str(" AND f.storage_id = ?");
            bind_values.push(sid.clone().into());
        }
        if filter.min_bytes > 0 {
            sql.push_str(" AND c.byte_size >= ?");
            bind_values.push((filter.min_bytes.min(i64::MAX as u64) as i64).into());
        }
        sql.push_str(" ORDER BY f.content_id ASC, f.storage_id ASC, f.rel_path ASC, f.id ASC");

        #[derive(Clone)]
        struct RawRow {
            content_id: i64,
            hash_algo: String,
            hash_hex: String,
            format: String,
            byte_size: i64,
            file_id: i64,
            asset_id: Option<i64>,
            storage_id: String,
            rel_path: String,
            tier: String,
            access_mode: String,
            backup_mode: String,
            role: String,
        }

        let mut grouped: BTreeMap<i64, Vec<RawRow>> = BTreeMap::new();
        {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(bind_values), |r| {
                Ok(RawRow {
                    content_id: r.get(0)?,
                    hash_algo: r.get(1)?,
                    hash_hex: r.get(2)?,
                    format: r.get(3)?,
                    byte_size: r.get(4)?,
                    file_id: r.get(5)?,
                    asset_id: r.get(6)?,
                    storage_id: r.get(7)?,
                    rel_path: r.get(8)?,
                    tier: r.get(9)?,
                    access_mode: r.get(10)?,
                    backup_mode: r.get(11)?,
                    role: r.get(12)?,
                })
            })?;
            for row in rows {
                let r = row?;
                grouped.entry(r.content_id).or_default().push(r);
            }
        }

        let include_protected_dups = filter.include_backups || filter.storage_id.is_some();
        let mut groups: Vec<DuplicateGroup> = Vec::new();

        for (content_id, raw_files) in grouped {
            if raw_files.len() < 2 {
                continue;
            }
            let mut managed_rows: Vec<&RawRow> = Vec::new();
            let mut protected_by_storage: HashMap<&str, Vec<&RawRow>> = HashMap::new();

            for rf in &raw_files {
                let is_protected = rf.access_mode == "read_only"
                    || rf.tier == "backup"
                    || rf.backup_mode == "external";
                if is_protected {
                    protected_by_storage
                        .entry(rf.storage_id.as_str())
                        .or_default()
                        .push(rf);
                } else {
                    managed_rows.push(rf);
                }
            }

            let has_actionable = managed_rows.len() >= 2;
            let has_protected_internal_dup =
                protected_by_storage.values().any(|v| v.len() >= 2);

            // Intentional cross-tier backups (e.g. 1 managed + 1 backup, or 1 per distinct backup)
            // are NEVER duplicates.
            if !has_actionable && !(include_protected_dups && has_protected_internal_dup) {
                continue;
            }

            // Deterministic advisory keeper selection among managed copies:
            // prefer non-hot (archive) slightly? Keep simple & predictable:
            // shortest rel_path first, then lexicographical rel_path, then smallest file_id.
            managed_rows.sort_by(|a, b| {
                a.rel_path
                    .len()
                    .cmp(&b.rel_path.len())
                    .then_with(|| a.rel_path.cmp(&b.rel_path))
                    .then_with(|| a.file_id.cmp(&b.file_id))
            });

            let mut files: Vec<DuplicateFileRow> = Vec::new();
            let mut actionable_redundant: usize = 0;
            let mut protected_redundant: usize = 0;

            if has_actionable {
                for (idx, rf) in managed_rows.iter().enumerate() {
                    let rec = if idx == 0 { "keep" } else { "duplicate" };
                    if idx > 0 {
                        actionable_redundant += 1;
                    }
                    files.push(DuplicateFileRow {
                        file_id: rf.file_id,
                        asset_id: rf.asset_id,
                        storage_id: rf.storage_id.clone(),
                        rel_path: rf.rel_path.clone(),
                        tier: rf.tier.clone(),
                        access_mode: rf.access_mode.clone(),
                        backup_mode: rf.backup_mode.clone(),
                        role: rf.role.clone(),
                        protected: false,
                        recommendation: rec.to_string(),
                    });
                }
            }

            if include_protected_dups && has_protected_internal_dup {
                let mut sids: Vec<&str> = protected_by_storage.keys().copied().collect();
                sids.sort_unstable();
                for sid in sids {
                    let list = &protected_by_storage[sid];
                    if list.len() < 2 {
                        continue;
                    }
                    protected_redundant += list.len() - 1;
                    for rf in list {
                        files.push(DuplicateFileRow {
                            file_id: rf.file_id,
                            asset_id: rf.asset_id,
                            storage_id: rf.storage_id.clone(),
                            rel_path: rf.rel_path.clone(),
                            tier: rf.tier.clone(),
                            access_mode: rf.access_mode.clone(),
                            backup_mode: rf.backup_mode.clone(),
                            role: rf.role.clone(),
                            protected: true,
                            recommendation: "protected".to_string(),
                        });
                    }
                }
            }

            let meta = &raw_files[0];
            let total_copies = files.len();
            let actionable_copies = if has_actionable { managed_rows.len() } else { 0 };
            let protected_copies = total_copies.saturating_sub(actionable_copies);
            let reclaimable_bytes = meta.byte_size.saturating_mul(actionable_redundant as i64);
            let redundant_bytes = meta
                .byte_size
                .saturating_mul((actionable_redundant + protected_redundant) as i64);

            groups.push(DuplicateGroup {
                content_id,
                hash_algo: meta.hash_algo.clone(),
                hash_hex: meta.hash_hex.clone(),
                format: meta.format.clone(),
                byte_size: meta.byte_size,
                total_copies,
                actionable_copies,
                protected_copies,
                reclaimable_bytes,
                redundant_bytes,
                files,
            });
        }

        groups.sort_by(|a, b| {
            b.reclaimable_bytes
                .cmp(&a.reclaimable_bytes)
                .then_with(|| b.redundant_bytes.cmp(&a.redundant_bytes))
                .then_with(|| b.byte_size.cmp(&a.byte_size))
                .then_with(|| a.content_id.cmp(&b.content_id))
        });

        let total_groups = groups.len();
        let actionable_groups = groups.iter().filter(|g| g.actionable_copies >= 2).count();
        let protected_only_groups = total_groups.saturating_sub(actionable_groups);
        let actionable_redundant_files: usize = groups
            .iter()
            .map(|g| g.actionable_copies.saturating_sub(1))
            .sum();
        let total_redundant_files: usize = groups
            .iter()
            .map(|g| {
                let mut r = g.actionable_copies.saturating_sub(1);
                if g.protected_copies >= 2 {
                    let mut by_s: HashMap<&str, usize> = HashMap::new();
                    for f in &g.files {
                        if f.protected {
                            *by_s.entry(f.storage_id.as_str()).or_insert(0) += 1;
                        }
                    }
                    r += by_s.values().map(|c| c.saturating_sub(1)).sum::<usize>();
                }
                r
            })
            .sum();
        let reclaimable_bytes: i64 = groups.iter().map(|g| g.reclaimable_bytes).sum();
        let total_redundant_bytes: i64 = groups.iter().map(|g| g.redundant_bytes).sum();

        if let Some(limit) = filter.limit {
            if limit > 0 && groups.len() > limit {
                groups.truncate(limit);
            }
        }
        let returned_groups = groups.len();

        Ok(DuplicateReport {
            total_groups,
            returned_groups,
            actionable_groups,
            protected_only_groups,
            total_redundant_files,
            actionable_redundant_files,
            total_redundant_bytes,
            reclaimable_bytes,
            groups,
        })
    }

    pub fn find(&self, filter: &FindFilter) -> DbResult<Vec<FindRow>> {
        Ok(self.find_page(filter)?.items)
    }

    /// Keyset-paginated search over present files ordered by
    /// `COALESCE(a.captured_at, '') DESC, f.id ASC`.
    /// Uses `LIMIT + 1` probing to determine `has_more` and `next_cursor` without `OFFSET`.
    pub fn find_page(&self, filter: &FindFilter) -> DbResult<FindPage> {
        let mut sql = String::from(
            "SELECT f.id, f.asset_id, a.public_id, f.storage_id, f.rel_path,
                    COALESCE(c.format, f.ext), f.byte_size,
                    a.captured_at, a.camera_model, a.lens_model,
                    a.focal_length_mm, a.f_number, a.shutter_speed_str, a.iso,
                    a.duration_ms, a.fps
             FROM files f
             LEFT JOIN contents c ON c.id = f.content_id
             LEFT JOIN assets a ON a.id = f.asset_id
             WHERE f.presence = 'present'",
        );
        let mut bind_values: Vec<rusqlite::types::Value> = Vec::new();

        if let Some(s) = &filter.storage_id {
            sql.push_str(" AND f.storage_id = ?");
            bind_values.push(s.clone().into());
        }
        if let Some(fmt) = &filter.format {
            sql.push_str(" AND LOWER(COALESCE(c.format, '')) = LOWER(?)");
            bind_values.push(fmt.clone().into());
        }
        if let Some(cam) = &filter.camera {
            sql.push_str(" AND LOWER(COALESCE(a.camera_model, '')) LIKE LOWER(?)");
            bind_values.push(format!("%{cam}%").into());
        }
        if let Some(lens) = &filter.lens {
            sql.push_str(" AND LOWER(COALESCE(a.lens_model, '')) LIKE LOWER(?)");
            bind_values.push(format!("%{lens}%").into());
        }
        if let Some(from) = &filter.from_date {
            sql.push_str(" AND a.captured_at >= ?");
            bind_values.push(from.clone().into());
        }
        if let Some(to) = &filter.to_date {
            sql.push_str(" AND a.captured_at <= ?");
            bind_values.push(to.clone().into());
        }
        if let Some(min_fl) = filter.min_focal_mm {
            sql.push_str(" AND a.focal_length_mm >= ?");
            bind_values.push(min_fl.into());
        }
        if let Some(max_fl) = filter.max_focal_mm {
            sql.push_str(" AND a.focal_length_mm <= ?");
            bind_values.push(max_fl.into());
        }
        if let Some(min_iso) = filter.min_iso {
            sql.push_str(" AND a.iso >= ?");
            bind_values.push(min_iso.into());
        }
        if let Some(max_iso) = filter.max_iso {
            sql.push_str(" AND a.iso <= ?");
            bind_values.push(max_iso.into());
        }
        if let Some(min_f) = filter.min_f_number {
            sql.push_str(" AND a.f_number >= ?");
            bind_values.push(min_f.into());
        }
        if let Some(max_f) = filter.max_f_number {
            sql.push_str(" AND a.f_number <= ?");
            bind_values.push(max_f.into());
        }
        if let Some(path_sub) = &filter.path_contains {
            sql.push_str(" AND LOWER(f.rel_path) LIKE LOWER(?)");
            bind_values.push(format!("%{path_sub}%").into());
        }
        if let Some(aid) = filter.asset_id {
            sql.push_str(" AND f.asset_id = ?");
            bind_values.push(aid.into());
        }
        if filter.unbacked_only {
            sql.push_str(
                " AND f.content_id IS NOT NULL
                  AND NOT EXISTS (
                      SELECT 1 FROM files fb
                      JOIN storages sb ON sb.id = fb.storage_id
                      WHERE fb.content_id = f.content_id
                        AND fb.presence = 'present'
                        AND sb.tier = 'backup'
                  )",
            );
        }
        if let Some(cursor) = &filter.cursor {
            sql.push_str(
                " AND (COALESCE(a.captured_at, '') < ? OR (COALESCE(a.captured_at, '') = ? AND f.id > ?))",
            );
            bind_values.push(cursor.captured_at.clone().into());
            bind_values.push(cursor.captured_at.clone().into());
            bind_values.push(cursor.file_id.into());
        }

        sql.push_str(" ORDER BY COALESCE(a.captured_at, '') DESC, f.id ASC");

        let effective_limit = filter.limit.map(|l| l.max(1));
        if let Some(limit) = effective_limit {
            sql.push_str(" LIMIT ?");
            bind_values.push(limit.saturating_add(1).into());
        }

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bind_values), |r| {
            Ok(FindRow {
                file_id: r.get(0)?,
                asset_id: r.get(1)?,
                asset_public_id: r.get(2)?,
                storage_id: r.get(3)?,
                rel_path: r.get(4)?,
                format: r.get(5)?,
                byte_size: r.get(6)?,
                captured_at: r.get(7)?,
                camera_model: r.get(8)?,
                lens_model: r.get(9)?,
                focal_length_mm: r.get(10)?,
                f_number: r.get(11)?,
                shutter_speed_str: r.get(12)?,
                iso: r.get(13)?,
                duration_ms: r.get(14)?,
                fps: r.get(15)?,
            })
        })?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }

        let has_more = match effective_limit {
            Some(limit) if (items.len() as i64) > limit => {
                items.truncate(limit as usize);
                true
            }
            _ => false,
        };

        let next_cursor = if has_more {
            items.last().map(|last| {
                FindCursor {
                    captured_at: last.captured_at.clone().unwrap_or_default(),
                    file_id: last.file_id,
                }
                .encode()
            })
        } else {
            None
        };

        Ok(FindPage {
            items,
            has_more,
            next_cursor,
        })
    }

    /// Resolve concrete present file copies (with content hashes) for a set of matched `FindRow`s.
    /// When `with_companions` is `true`, also expands each matched `asset_id` to include paired
    /// RAW/JPEG files and linked `.xmp`/`.thm` sidecars across all storages.
    pub fn resolve_copy_candidates(
        &self,
        matched_rows: &[FindRow],
        with_companions: bool,
    ) -> DbResult<Vec<CopyCandidateRow>> {
        if matched_rows.is_empty() {
            return Ok(Vec::new());
        }

        let mut seen_file_ids: HashSet<i64> = HashSet::new();
        let mut results: Vec<CopyCandidateRow> = Vec::new();

        let mut query_by_file_id = self.conn.prepare(
            "SELECT f.id, f.asset_id, f.content_id, f.storage_id, s.tier,
                    f.rel_path, f.role, COALESCE(c.format, f.ext), f.byte_size,
                    COALESCE(c.hash_algo, 'blake3'), c.hash_hex
             FROM files f
             JOIN storages s ON s.id = f.storage_id
             LEFT JOIN contents c ON c.id = f.content_id
             WHERE f.id = ?1 AND f.presence = 'present'",
        )?;
        let mut query_by_asset_id = self.conn.prepare(
            "SELECT f.id, f.asset_id, f.content_id, f.storage_id, s.tier,
                    f.rel_path, f.role, COALESCE(c.format, f.ext), f.byte_size,
                    COALESCE(c.hash_algo, 'blake3'), c.hash_hex
             FROM files f
             JOIN storages s ON s.id = f.storage_id
             LEFT JOIN contents c ON c.id = f.content_id
             WHERE f.asset_id = ?1 AND f.presence = 'present'
             ORDER BY CASE s.tier WHEN 'hot' THEN 0 WHEN 'archive' THEN 1 WHEN 'backup' THEN 2 ELSE 3 END,
                      f.id ASC",
        )?;

        let mut seen_assets: HashSet<i64> = HashSet::new();
        for row in matched_rows {
            if with_companions {
                if let Some(aid) = row.asset_id {
                    if seen_assets.insert(aid) {
                        let mapped = query_by_asset_id.query_map(params![aid], |r| {
                            Ok(CopyCandidateRow {
                                file_id: r.get(0)?,
                                asset_id: r.get(1)?,
                                content_id: r.get(2)?,
                                storage_id: r.get(3)?,
                                storage_tier: r.get(4)?,
                                rel_path: r.get(5)?,
                                role: r.get(6)?,
                                format: r.get(7)?,
                                byte_size: r.get(8)?,
                                hash_algo: r.get(9)?,
                                hash_hex: r.get(10)?,
                            })
                        })?;
                        for item in mapped {
                            let c = item?;
                            if seen_file_ids.insert(c.file_id) {
                                results.push(c);
                            }
                        }
                    }
                    continue;
                }
            }

            if seen_file_ids.insert(row.file_id) {
                if let Some(c) = query_by_file_id
                    .query_row(params![row.file_id], |r| {
                        Ok(CopyCandidateRow {
                            file_id: r.get(0)?,
                            asset_id: r.get(1)?,
                            content_id: r.get(2)?,
                            storage_id: r.get(3)?,
                            storage_tier: r.get(4)?,
                            rel_path: r.get(5)?,
                            role: r.get(6)?,
                            format: r.get(7)?,
                            byte_size: r.get(8)?,
                            hash_algo: r.get(9)?,
                            hash_hex: r.get(10)?,
                        })
                    })
                    .optional()?
                {
                    results.push(c);
                }
            }
        }

        Ok(results)
    }

    /// Re-apply `[exif.lens_aliases]` to existing `assets` and `content_metadata` rows
    /// using each row's original `lens_model_raw`, without rescanning files.
    ///
    /// - Idempotent and reversible: removing an alias reverts `lens_model` back to trimmed `lens_model_raw`.
    /// - Preserves `lens_model_raw`, Asset IDs, ratings, tags, and physical files.
    /// - When `dry_run` is `true`, performs read-only aggregation and never mutates the DB.
    pub fn reapply_lens_aliases(
        &mut self,
        aliases: &HashMap<String, String>,
        dry_run: bool,
    ) -> DbResult<NormalizeLensesReport> {
        #[derive(PartialEq, Eq, PartialOrd, Ord)]
        struct ChangeKey {
            raw: String,
            previous: Option<String>,
            normalized: Option<String>,
        }

        let mut change_buckets: BTreeMap<ChangeKey, (u64, u64)> = BTreeMap::new();
        let mut asset_updates: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut content_updates: BTreeMap<String, Option<String>> = BTreeMap::new();

        {
            let mut stmt = self.conn.prepare(
                "SELECT lens_model_raw, lens_model, COUNT(*)
                 FROM assets
                 WHERE lens_model_raw IS NOT NULL
                 GROUP BY lens_model_raw, lens_model",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (raw, current, cnt) = row?;
                let expected = resolve_lens_alias(&raw, aliases);
                if current != expected {
                    asset_updates.insert(raw.clone(), expected.clone());
                    let key = ChangeKey {
                        raw,
                        previous: current,
                        normalized: expected,
                    };
                    let entry = change_buckets.entry(key).or_insert((0, 0));
                    entry.0 += cnt.max(0) as u64;
                }
            }
        }

        {
            let mut stmt = self.conn.prepare(
                "SELECT lens_model_raw, lens_model, COUNT(*)
                 FROM content_metadata
                 WHERE lens_model_raw IS NOT NULL
                 GROUP BY lens_model_raw, lens_model",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (raw, current, cnt) = row?;
                let expected = resolve_lens_alias(&raw, aliases);
                if current != expected {
                    content_updates.insert(raw.clone(), expected.clone());
                    let key = ChangeKey {
                        raw,
                        previous: current,
                        normalized: expected,
                    };
                    let entry = change_buckets.entry(key).or_insert((0, 0));
                    entry.1 += cnt.max(0) as u64;
                }
            }
        }

        let mut changes: Vec<LensAliasChange> = change_buckets
            .into_iter()
            .map(|(key, (assets_affected, contents_affected))| LensAliasChange {
                raw_lens: key.raw,
                previous_lens: key.previous,
                normalized_lens: key.normalized,
                assets_affected,
                contents_affected,
            })
            .collect();
        changes.sort_by(|a, b| {
            b.assets_affected
                .cmp(&a.assets_affected)
                .then_with(|| b.contents_affected.cmp(&a.contents_affected))
                .then_with(|| a.raw_lens.cmp(&b.raw_lens))
        });

        let (assets_updated, contents_updated) = if dry_run {
            let a_total = changes.iter().map(|c| c.assets_affected).sum();
            let c_total = changes.iter().map(|c| c.contents_affected).sum();
            (a_total, c_total)
        } else if asset_updates.is_empty() && content_updates.is_empty() {
            (0, 0)
        } else {
            let now = Utc::now().to_rfc3339();
            let tx = self.conn.transaction()?;
            let mut a_total = 0u64;
            let mut c_total = 0u64;

            if !asset_updates.is_empty() {
                let mut stmt = tx.prepare_cached(
                    "UPDATE assets
                     SET lens_model = ?1, updated_at = ?2
                     WHERE lens_model_raw = ?3 AND lens_model IS NOT ?1",
                )?;
                for (raw, expected) in &asset_updates {
                    let n = stmt.execute(params![expected.as_deref(), now, raw])?;
                    a_total += n as u64;
                }
            }

            if !content_updates.is_empty() {
                let mut stmt = tx.prepare_cached(
                    "UPDATE content_metadata
                     SET lens_model = ?1
                     WHERE lens_model_raw = ?2 AND lens_model IS NOT ?1",
                )?;
                for (raw, expected) in &content_updates {
                    let n = stmt.execute(params![expected.as_deref(), raw])?;
                    c_total += n as u64;
                }
            }

            tx.commit()?;
            (a_total, c_total)
        };

        Ok(NormalizeLensesReport {
            dry_run,
            aliases_configured: aliases.len(),
            assets_updated,
            contents_updated,
            changes,
        })
    }

    /// Populate a fresh temporary benchmark database with `target_files` realistic synthetic
    /// RAW+JPEG records across two storages (Hot NVMe + External Backup NAS) using standard
    /// durable WAL batch transactions (never mutates real photo assets).
    pub fn seed_synthetic_benchmark(&mut self, target_files: usize) -> DbResult<SeedBenchmarkReport> {
        use std::path::PathBuf;
        use wing_photovault_core::model::{AccessMode, BackupMode, MediaType, StorageTier};

        if target_files == 0 {
            return Ok(SeedBenchmarkReport::default());
        }

        self.upsert_host("bench-host", "Benchmark Host", "synthetic")?;
        let hot = StorageConfig {
            id: "ssd_hot_bench".into(),
            root: PathBuf::from("/bench/hot"),
            media: MediaType::Nvme,
            tier: StorageTier::Hot,
            access_mode: AccessMode::Managed,
            backup_mode: BackupMode::None,
            failure_domain: Some("bench-nvme".into()),
            exclude_paths: vec![],
        };
        let nas = StorageConfig {
            id: "nas_backup_bench".into(),
            root: PathBuf::from("/bench/nas"),
            media: MediaType::Hdd,
            tier: StorageTier::Backup,
            access_mode: AccessMode::ReadOnly,
            backup_mode: BackupMode::External,
            failure_domain: Some("bench-nas".into()),
            exclude_paths: vec![],
        };
        self.upsert_storage(&hot, "bench-host", StorageStatus::Online)?;
        self.upsert_storage(&nas, "bench-host", StorageStatus::Online)?;
        let run_hot = self.start_scan_run(&hot.id, "benchmark-seed")?;
        let run_nas = self.start_scan_run(&nas.id, "benchmark-seed")?;

        const CAMERAS: [&str; 8] = [
            "Canon EOS R5m2",
            "Canon EOS 7D Mark II",
            "Canon EOS R5",
            "Canon EOS 90D",
            "Canon EOS R7",
            "Canon EOS 6D Mark II",
            "Canon EOS 10D",
            "Canon EOS 20D",
        ];
        const LENSES: [&str; 8] = [
            "RF100-500mm F4.5-7.1 L IS USM",
            "EF100-400mm f/4.5-5.6L IS II USM",
            "RF200-800mm F6.3-9 IS USM",
            "EF24-105mm f/4L IS USM",
            "Unknown 100-400mm",
            "60-600mm F4.5-6.3 DG OS HSM | Sports 018",
            "Unknown 28-75mm",
            "RF24-105mm F4 L IS USM",
        ];

        let now = Utc::now().to_rfc3339();
        let batch_assets = 5_000usize;
        let total_assets = target_files.div_ceil(2);
        let mut files_created = 0u64;
        let mut assets_created = 0u64;
        let mut contents_created = 0u64;

        let mut asset_idx = 0usize;
        while asset_idx < total_assets {
            let chunk_end = (asset_idx + batch_assets).min(total_assets);
            let tx = self.conn.transaction()?;
            {
                let mut ins_asset = tx.prepare_cached(
                    "INSERT INTO assets (
                        public_id, captured_at, camera_make, camera_model,
                        lens_model_raw, lens_model, focal_length_mm, f_number,
                        shutter_speed_s, shutter_speed_str, iso, created_at, updated_at
                     ) VALUES (?1, ?2, 'Canon', ?3, ?4, ?4, ?5, 5.6, 0.0005, '1/2000', ?6, ?7, ?7)",
                )?;
                let mut ins_content = tx.prepare_cached(
                    "INSERT INTO contents (
                        hash_algo, hash_hex, byte_size, media_kind, format, created_at
                     ) VALUES ('blake3', ?1, ?2, ?3, ?4, ?5)",
                )?;
                let mut ins_meta = tx.prepare_cached(
                    "INSERT INTO content_metadata (
                        content_id, extractor, captured_at, camera_make, camera_model,
                        lens_model_raw, lens_model, focal_length_mm, f_number,
                        shutter_speed_s, shutter_speed_str, iso, extracted_at
                     ) VALUES (?1, 'synthetic', ?2, 'Canon', ?3, ?4, ?4, ?5, 5.6, 0.0005, '1/2000', ?6, ?7)",
                )?;
                let mut ins_file = tx.prepare_cached(
                    "INSERT INTO files (
                        storage_id, rel_path, file_name, file_stem, ext,
                        byte_size, mtime_ns, content_id, asset_id, role,
                        presence, last_seen_scan_id, last_seen_at, last_verified_at,
                        created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'present', ?11, ?12, ?12, ?12, ?12)",
                )?;

                for i in asset_idx..chunk_end {
                    let year = 2003 + (i % 24);
                    let month = 1 + ((i / 24) % 12);
                    let day = 1 + ((i / 288) % 28);
                    let sec_of_day = (i * 17) % 86_400;
                    let hour = sec_of_day / 3600;
                    let minute = (sec_of_day % 3600) / 60;
                    let second = sec_of_day % 60;
                    let captured_at = format!(
                        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"
                    );
                    let cam = CAMERAS[i % CAMERAS.len()];
                    let lens = LENSES[(i / 3) % LENSES.len()];
                    let focal = 100.0 + ((i % 7) as f64) * 100.0;
                    let iso = 100i64 * (1 << (i % 6));
                    let public_id = Uuid::now_v7().to_string();

                    ins_asset.execute(params![public_id, captured_at, cam, lens, focal, iso, now])?;
                    let asset_id = tx.last_insert_rowid();
                    assets_created += 1;

                    let (storage_id, role, scan_id) = if i % 2 == 0 {
                        (hot.id.as_str(), "original", run_hot)
                    } else {
                        (nas.id.as_str(), "backup", run_nas)
                    };

                    // File 1: RAW (cr3 / cr2)
                    if (files_created as usize) < target_files {
                        let (ext, fmt, size) = if year >= 2019 {
                            (".cr3", "cr3", 45_000_000i64)
                        } else {
                            (".cr2", "cr2", 24_000_000i64)
                        };
                        let hash_hex = format!("{i:032x}00000000000000000000000000000001");
                        ins_content.execute(params![hash_hex, size, "raw", fmt, now])?;
                        let content_id = tx.last_insert_rowid();
                        contents_created += 1;
                        ins_meta.execute(params![
                            content_id,
                            captured_at,
                            cam,
                            lens,
                            focal,
                            iso,
                            now
                        ])?;

                        let stem = format!("IMG_{i:06}");
                        let file_name = format!("{stem}{ext}");
                        let rel_path = format!("{year:04}/{year:04}_{month:02}_{day:02}/{file_name}");
                        ins_file.execute(params![
                            storage_id,
                            rel_path,
                            file_name,
                            stem,
                            ext,
                            size,
                            1_700_000_000i64 + i as i64,
                            content_id,
                            asset_id,
                            role,
                            scan_id,
                            now
                        ])?;
                        files_created += 1;
                    }

                    // File 2: Companion JPEG
                    if (files_created as usize) < target_files {
                        let size = 8_500_000i64;
                        let hash_hex = format!("{i:032x}00000000000000000000000000000002");
                        ins_content.execute(params![hash_hex, size, "image", "jpeg", now])?;
                        let content_id = tx.last_insert_rowid();
                        contents_created += 1;
                        ins_meta.execute(params![
                            content_id,
                            captured_at,
                            cam,
                            lens,
                            focal,
                            iso,
                            now
                        ])?;

                        let stem = format!("IMG_{i:06}");
                        let file_name = format!("{stem}.JPG");
                        let rel_path = format!("{year:04}/{year:04}_{month:02}_{day:02}/{file_name}");
                        ins_file.execute(params![
                            storage_id,
                            rel_path,
                            file_name,
                            stem,
                            ".jpg",
                            size,
                            1_700_000_000i64 + i as i64,
                            content_id,
                            asset_id,
                            role,
                            scan_id,
                            now
                        ])?;
                        files_created += 1;
                    }
                }
            }
            tx.commit()?;
            asset_idx = chunk_end;
        }

        Ok(SeedBenchmarkReport {
            files_created,
            assets_created,
            contents_created,
        })
    }

    fn storage_stats(&self) -> DbResult<Vec<StorageStatsRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.root_path, s.media_type, s.tier,
                    s.access_mode, s.backup_mode, s.status, s.failure_domain,
                    COALESCE(SUM(CASE WHEN f.presence = 'present' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN f.presence = 'missing' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN f.presence = 'present' THEN f.byte_size ELSE 0 END), 0),
                    s.last_seen_at
             FROM storages s
             LEFT JOIN files f ON f.storage_id = s.id
             GROUP BY s.id
             ORDER BY s.id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(StorageStatsRow {
                storage_id: r.get(0)?,
                root_path: r.get(1)?,
                media_type: r.get(2)?,
                tier: r.get(3)?,
                access_mode: r.get(4)?,
                backup_mode: r.get(5)?,
                status: r.get(6)?,
                failure_domain: r.get(7)?,
                present_files: r.get(8)?,
                missing_files: r.get(9)?,
                total_bytes: r.get(10)?,
                last_seen_at: r.get(11)?,
            })
        })?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    fn query_buckets(&self, sql: &str) -> DbResult<Vec<BucketCount>> {
        self.query_buckets_with_params(sql, Vec::new())
    }

    fn query_buckets_with_params(
        &self,
        sql: &str,
        bind_values: Vec<rusqlite::types::Value>,
    ) -> DbResult<Vec<BucketCount>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bind_values), |r| {
            Ok(BucketCount {
                label: r.get(0)?,
                count: r.get(1)?,
                total_bytes: r.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

fn opt_eq_if_both(a: &Option<String>, b: &Option<String>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => x == y,
        _ => true,
    }
}

/// Fast O(1) indexed lookup for a confident RAW<->JPEG partner already present in the DB
/// (including earlier items in the same `upsert_scanned_batch` transaction).
/// Returns `Some(asset_id)` only when exactly one unambiguous partner matches.
fn find_in_batch_raw_jpeg_asset(
    tx: &rusqlite::Transaction<'_>,
    item: &UpsertFileInput,
) -> DbResult<Option<i64>> {
    let partner_kind = match item.media_kind {
        MediaKind::Raw => "image",
        MediaKind::Image => "raw",
        _ => return Ok(None),
    };
    let Some(meta) = item.metadata.as_ref() else {
        return Ok(None);
    };
    let Some(captured_at) = meta.captured_at.as_deref() else {
        return Ok(None);
    };

    let stem_upper = item.file_stem.to_ascii_uppercase();
    let stem_lower = item.file_stem.to_ascii_lowercase();
    let target_folder = pairing_folder_key(&item.rel_path, item.media_kind.as_str());

    let mut stmt = tx.prepare_cached(
        "SELECT f.asset_id, f.rel_path, a.camera_model, a.camera_serial, a.captured_subsec
         FROM files f
         JOIN contents c ON c.id = f.content_id
         JOIN assets a ON a.id = f.asset_id
         WHERE f.file_stem IN (?1, ?2, ?3)
           AND f.storage_id = ?4
           AND f.presence = 'present'
           AND c.media_kind = ?5
           AND a.captured_at = ?6",
    )?;

    let rows = stmt.query_map(
        params![
            item.file_stem,
            stem_upper,
            stem_lower,
            item.storage_id,
            partner_kind,
            captured_at,
        ],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        },
    )?;

    let mut matched_asset_id: Option<i64> = None;
    for row in rows {
        let (aid, rel_path, cam, serial, subsec) = row?;
        if pairing_folder_key(&rel_path, partner_kind) != target_folder {
            continue;
        }
        if !opt_eq_if_both(&meta.camera_model, &cam)
            || !opt_eq_if_both(&meta.camera_serial, &serial)
            || !opt_eq_if_both(&meta.captured_subsec, &subsec)
        {
            continue;
        }
        match matched_asset_id {
            None => matched_asset_id = Some(aid),
            Some(prev) if prev == aid => {}
            Some(_) => return Ok(None), // Ambiguous: multiple distinct partner assets
        }
    }

    Ok(matched_asset_id)
}

/// Fast O(1) indexed lookup for a parent asset in the same directory when ingesting a sidecar.
/// Returns `Some(asset_id)` only if exactly one distinct parent asset matches (Fail Closed).
fn find_in_batch_sidecar_parent_asset(
    tx: &rusqlite::Transaction<'_>,
    item: &UpsertFileInput,
) -> DbResult<Option<i64>> {
    let Some(parent_stem) = sidecar_parent_stem(&item.file_name) else {
        return Ok(None);
    };
    let stem_upper = parent_stem.to_ascii_uppercase();
    let stem_lower = parent_stem.to_ascii_lowercase();
    let target_dir = dir_key(&item.rel_path);

    let mut stmt = tx.prepare_cached(
        "SELECT asset_id, rel_path
         FROM files
         WHERE file_stem IN (?1, ?2, ?3)
           AND storage_id = ?4
           AND presence = 'present'
           AND role != 'sidecar'
           AND asset_id IS NOT NULL",
    )?;
    let rows = stmt.query_map(
        params![parent_stem, stem_upper, stem_lower, item.storage_id],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
    )?;

    let mut matched: Option<i64> = None;
    for row in rows {
        let (aid, rel_path) = row?;
        if dir_key(&rel_path) != target_dir {
            continue;
        }
        match matched {
            None => matched = Some(aid),
            Some(prev) if prev == aid => {}
            Some(_) => return Ok(None), // Ambiguous
        }
    }
    Ok(matched)
}

fn append_stats_asset_where(
    alias: &str,
    filter: &StatsFilter,
    sql: &mut String,
    binds: &mut Vec<rusqlite::types::Value>,
) {
    if let Some(yr) = &filter.year {
        sql.push_str(&format!(" AND substr({alias}.captured_at, 1, 4) = ?"));
        binds.push(yr.clone().into());
    }
    if let Some(cam) = &filter.camera {
        sql.push_str(&format!(
            " AND LOWER(COALESCE({alias}.camera_model, '')) LIKE LOWER(?)"
        ));
        binds.push(format!("%{cam}%").into());
    }
    if let Some(lens) = &filter.lens {
        sql.push_str(&format!(
            " AND LOWER(COALESCE({alias}.lens_model, '')) LIKE LOWER(?)"
        ));
        binds.push(format!("%{lens}%").into());
    }
}

fn dir_key(rel_path: &str) -> String {
    let dir = match rel_path.rfind('/') {
        Some(idx) => &rel_path[..idx],
        None => "",
    };
    if cfg!(windows) {
        dir.to_ascii_lowercase()
    } else {
        dir.to_string()
    }
}

/// Normalize parent directory for RAW+JPEG pairing.
/// Supports both same-directory pairs (`2026/2026_11_24/IMG_0001.{CR3,JPG}`)
/// and legacy sibling `RAW/` + `JPEG/` (`JPG/`) folders (`2015/08_10/RAW/IMG_0001.CR2` + `2015/08_10/JPEG/IMG_0001.JPG`).
fn pairing_folder_key(rel_path: &str, media_kind: &str) -> String {
    let dir = match rel_path.rfind('/') {
        Some(idx) => &rel_path[..idx],
        None => "",
    };
    let (parent, leaf) = match dir.rfind('/') {
        Some(idx) => (&dir[..idx], &dir[idx + 1..]),
        None => ("", dir),
    };
    let leaf_lower = leaf.to_ascii_lowercase();
    let is_split_dir = (media_kind == "raw" && leaf_lower == "raw")
        || (media_kind == "image" && (leaf_lower == "jpeg" || leaf_lower == "jpg"));

    let chosen = if is_split_dir { parent } else { dir };
    if cfg!(windows) {
        chosen.to_ascii_lowercase()
    } else {
        chosen.to_string()
    }
}

fn configure_connection(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA temp_store = MEMORY;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use wing_photovault_core::model::{AccessMode, BackupMode, MediaType, StorageTier};

    #[test]
    fn shares_content_and_asset_across_hot_and_backup_storages() {
        let mut db = Database::open_in_memory().unwrap();
        db.upsert_host("main-pc", "Main PC", "windows").unwrap();
        db.upsert_host("eagle", "Eagle NAS", "freebsd").unwrap();

        let hot = StorageConfig {
            id: "ssd_hot_g".into(),
            root: PathBuf::from(r"G:\Photos"),
            media: MediaType::Nvme,
            tier: StorageTier::Hot,
            access_mode: AccessMode::Managed,
            backup_mode: BackupMode::None,
            failure_domain: Some("pc-nvme-g".into()),
            exclude_paths: vec![],
        };
        let nas_win = StorageConfig {
            id: "nas_backup_eagle".into(),
            root: PathBuf::from(r"\\eagle\photos"),
            media: MediaType::Nas,
            tier: StorageTier::Backup,
            access_mode: AccessMode::ReadOnly,
            backup_mode: BackupMode::External,
            failure_domain: Some("nas-eagle".into()),
            exclude_paths: vec![],
        };
        let mut nas_bsd = nas_win.clone();
        nas_bsd.root = PathBuf::from("/tank/photos");

        db.upsert_storage(&hot, "main-pc", StorageStatus::Online)
            .unwrap();
        db.upsert_storage(&nas_win, "main-pc", StorageStatus::Online)
            .unwrap();
        // Same storage mounted on FreeBSD host under POSIX path
        db.upsert_storage(&nas_bsd, "eagle", StorageStatus::Online)
            .unwrap();

        let mount_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM storage_mounts WHERE storage_id = 'nas_backup_eagle'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(mount_count, 2);

        let run1 = db.start_scan_run(&hot.id, "incremental").unwrap();
        let run2 = db.start_scan_run(&nas_win.id, "incremental").unwrap();

        let item_hot = UpsertFileInput {
            storage_id: hot.id.clone(),
            rel_path: "2026/2026_11_24/100EOSR5/IMG_0001.CR3".into(),
            file_name: "IMG_0001.CR3".into(),
            file_stem: "IMG_0001".into(),
            ext: ".cr3".into(),
            byte_size: 42_000_000,
            mtime_ns: 1_700_000_000,
            ctime_ns: None,
            media_kind: MediaKind::Raw,
            format: "cr3".into(),
            role: FileRole::Original,
            hash_algo: HashAlgorithm::Blake3,
            hash_hex: "abcdef1234567890".into(),
            partial_hash: None,
            metadata: Some(ExtractedMetadata {
                extractor: "native-cr3".into(),
                camera_model: Some("Canon EOS R5".into()),
                lens_model: Some("RF200-800mm F6.3-9 IS USM".into()),
                captured_at: Some("2026-11-24T10:00:00".into()),
                ..Default::default()
            }),
        };

        let mut item_nas = item_hot.clone();
        item_nas.storage_id = nas_win.id.clone();
        item_nas.rel_path = "2026/11_24/RAW/IMG_0001.CR3".into();
        item_nas.role = FileRole::Backup;

        db.upsert_scanned_batch(&[item_hot], run1).unwrap();
        db.upsert_scanned_batch(&[item_nas], run2).unwrap();

        let stats = db.stats().unwrap();
        assert_eq!(stats.total_present_files, 2);
        assert_eq!(stats.total_contents, 1);
        assert_eq!(stats.total_assets, 1);
        assert_eq!(stats.backed_up_contents, 1);

        let rows = db.find(&FindFilter::default()).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].asset_public_id.is_some());
        assert_eq!(rows[0].asset_public_id, rows[1].asset_public_id);
    }

    #[test]
    fn touch_unchanged_preserves_last_verified_at() {
        let mut db = Database::open_in_memory().unwrap();
        db.upsert_host("main-pc", "Main PC", "windows").unwrap();
        let hot = StorageConfig {
            id: "ssd_hot_g".into(),
            root: PathBuf::from(r"G:\Photos"),
            media: MediaType::Nvme,
            tier: StorageTier::Hot,
            access_mode: AccessMode::Managed,
            backup_mode: BackupMode::None,
            failure_domain: None,
            exclude_paths: vec![],
        };
        db.upsert_storage(&hot, "main-pc", StorageStatus::Online)
            .unwrap();
        let run1 = db.start_scan_run(&hot.id, "incremental").unwrap();

        let item = UpsertFileInput {
            storage_id: hot.id.clone(),
            rel_path: "a.jpg".into(),
            file_name: "a.jpg".into(),
            file_stem: "a".into(),
            ext: ".jpg".into(),
            byte_size: 100,
            mtime_ns: 1000,
            ctime_ns: None,
            media_kind: MediaKind::Image,
            format: "jpeg".into(),
            role: FileRole::Original,
            hash_algo: HashAlgorithm::Blake3,
            hash_hex: "00112233".into(),
            partial_hash: None,
            metadata: None,
        };
        db.upsert_scanned_batch(&[item], run1).unwrap();

        let (fid, verified_before): (i64, String) = db
            .conn
            .query_row(
                "SELECT id, last_verified_at FROM files WHERE rel_path = 'a.jpg'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();

        let run2 = db.start_scan_run(&hot.id, "incremental").unwrap();
        db.touch_unchanged_files(&[fid], run2).unwrap();

        let verified_after: String = db
            .conn
            .query_row(
                "SELECT last_verified_at FROM files WHERE id = ?1",
                params![fid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(verified_before, verified_after);
    }

    #[test]
    fn pairs_same_dir_and_sibling_raw_jpeg_and_rejects_mismatches() {
        let mut db = Database::open_in_memory().unwrap();
        db.upsert_host("main-pc", "Main PC", "windows").unwrap();
        let hot = StorageConfig {
            id: "ssd_hot_g".into(),
            root: PathBuf::from(r"G:\Photos"),
            media: MediaType::Nvme,
            tier: StorageTier::Hot,
            access_mode: AccessMode::Managed,
            backup_mode: BackupMode::None,
            failure_domain: None,
            exclude_paths: vec![],
        };
        db.upsert_storage(&hot, "main-pc", StorageStatus::Online)
            .unwrap();
        let run1 = db.start_scan_run(&hot.id, "incremental").unwrap();

        let make_item = |rel_path: &str,
                         stem: &str,
                         ext: &str,
                         kind: MediaKind,
                         fmt: &str,
                         hash: &str,
                         dt: Option<&str>,
                         cam: Option<&str>,
                         lens: Option<&str>| UpsertFileInput {
            storage_id: hot.id.clone(),
            rel_path: rel_path.into(),
            file_name: format!("{stem}{ext}"),
            file_stem: stem.into(),
            ext: ext.into(),
            byte_size: 1000,
            mtime_ns: 1000,
            ctime_ns: None,
            media_kind: kind,
            format: fmt.into(),
            role: FileRole::Original,
            hash_algo: HashAlgorithm::Blake3,
            hash_hex: hash.into(),
            partial_hash: None,
            metadata: dt.map(|d| ExtractedMetadata {
                extractor: "native".into(),
                captured_at: Some(d.into()),
                camera_model: cam.map(String::from),
                lens_model: lens.map(String::from),
                ..Default::default()
            }),
        };

        let items = vec![
            // Pair 1: same directory (CR3 missing lens, JPG has lens -> backfill)
            make_item(
                "2026/2026_11_22/1M5A1060.CR3",
                "1M5A1060",
                ".cr3",
                MediaKind::Raw,
                "cr3",
                "h1",
                Some("2026-11-22T11:58:45"),
                Some("Canon EOS R5m2"),
                None,
            ),
            make_item(
                "2026/2026_11_22/1M5A1060.JPG",
                "1M5A1060",
                ".jpg",
                MediaKind::Image,
                "jpeg",
                "h2",
                Some("2026-11-22T11:58:45"),
                Some("Canon EOS R5m2"),
                Some("RF200-800mm F6.3-9 IS USM"),
            ),
            // Pair 2: legacy sibling RAW/ and JPEG/ directories
            make_item(
                "2015/08_10/RAW/IMG_0002.CR2",
                "IMG_0002",
                ".cr2",
                MediaKind::Raw,
                "cr2",
                "h3",
                Some("2015-08-10T09:00:00"),
                Some("Canon EOS 7D Mark II"),
                Some("EF100-400mm"),
            ),
            make_item(
                "2015/08_10/JPEG/IMG_0002.JPG",
                "IMG_0002",
                ".jpg",
                MediaKind::Image,
                "jpeg",
                "h4",
                Some("2015-08-10T09:00:00"),
                Some("Canon EOS 7D Mark II"),
                Some("EF100-400mm"),
            ),
            // Mismatch 1: different camera models -> must NOT pair
            make_item(
                "2026/2026_11_22/IMG_9999.CR3",
                "IMG_9999",
                ".cr3",
                MediaKind::Raw,
                "cr3",
                "h5",
                Some("2026-11-22T12:00:00"),
                Some("Canon EOS R5"),
                None,
            ),
            make_item(
                "2026/2026_11_22/IMG_9999.JPG",
                "IMG_9999",
                ".jpg",
                MediaKind::Image,
                "jpeg",
                "h6",
                Some("2026-11-22T12:00:00"),
                Some("Canon EOS R6"),
                None,
            ),
            // Mismatch 2: missing EXIF date on CRW -> must NOT pair
            make_item(
                "2004/2004_05_01/CRW_0010.CRW",
                "CRW_0010",
                ".crw",
                MediaKind::Raw,
                "crw",
                "h7",
                None,
                None,
                None,
            ),
            make_item(
                "2004/2004_05_01/CRW_0010.JPG",
                "CRW_0010",
                ".jpg",
                MediaKind::Image,
                "jpeg",
                "h8",
                Some("2004-05-01T10:00:00"),
                Some("Canon EOS 10D"),
                None,
            ),
        ];

        db.upsert_scanned_batch(&items, run1).unwrap();
        // In-batch pairing already merged Pair 1 (same dir) and Pair 2 (sibling RAW/JPEG)
        // while keeping the 2 mismatches separate: 8 files -> 6 logical assets immediately!
        let st_immediate = db.stats().unwrap();
        assert_eq!(st_immediate.total_present_files, 8);
        assert_eq!(st_immediate.total_contents, 8);
        assert_eq!(st_immediate.total_assets, 6);

        // Post-scan sweep is idempotent and reports 0 additional merges
        let rep = db.pair_raw_jpeg_assets(&hot.id).unwrap();
        assert_eq!(rep.pairs_merged, 0);
        assert_eq!(rep.ambiguous_skipped, 2);

        let st = db.stats().unwrap();
        assert_eq!(st.total_present_files, 8);
        assert_eq!(st.total_contents, 8); // RAW and JPEG remain distinct contents
        assert_eq!(st.total_assets, 6); // 8 - 2 merged = 6 logical assets

        // Verify lens backfill on Pair 1
        let pair1_rows = db
            .find(&FindFilter {
                lens: Some("RF200-800".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(pair1_rows.len(), 2);
        assert_eq!(pair1_rows[0].asset_id, pair1_rows[1].asset_id);

        // Idempotent second run
        let rep2 = db.pair_raw_jpeg_assets(&hot.id).unwrap();
        assert_eq!(rep2.pairs_merged, 0);
    }

    #[test]
    fn reapply_lens_aliases_supports_dry_run_apply_idempotency_and_revert() {
        let mut db = Database::open_in_memory().unwrap();
        db.upsert_host("eagle", "Eagle NAS", "freebsd").unwrap();
        let nas = StorageConfig {
            id: "nas_backup_eagle".into(),
            root: PathBuf::from("/home/yoshi/Pictures"),
            media: MediaType::Hdd,
            tier: StorageTier::Backup,
            access_mode: AccessMode::ReadOnly,
            backup_mode: BackupMode::External,
            failure_domain: Some("nas-eagle".into()),
            exclude_paths: vec![],
        };
        db.upsert_storage(&nas, "eagle", StorageStatus::Online)
            .unwrap();
        let run1 = db.start_scan_run(&nas.id, "incremental").unwrap();

        let items = vec![
            UpsertFileInput {
                storage_id: nas.id.clone(),
                rel_path: "2007/IMG_0001.CR2".into(),
                file_name: "IMG_0001.CR2".into(),
                file_stem: "IMG_0001".into(),
                ext: ".cr2".into(),
                byte_size: 10_000,
                mtime_ns: 100,
                ctime_ns: None,
                media_kind: MediaKind::Raw,
                format: "cr2".into(),
                role: FileRole::Backup,
                hash_algo: HashAlgorithm::Blake3,
                hash_hex: "aaa111".into(),
                partial_hash: None,
                metadata: Some(ExtractedMetadata {
                    extractor: "exiftool-stay-open".into(),
                    captured_at: Some("2007-05-01T10:00:00".into()),
                    camera_model: Some("Canon EOS 20D".into()),
                    lens_model_raw: Some("Unknown 100-400mm".into()),
                    lens_model: Some("Unknown 100-400mm".into()),
                    ..Default::default()
                }),
            },
            UpsertFileInput {
                storage_id: nas.id.clone(),
                rel_path: "2014/IMG_0002.CR2".into(),
                file_name: "IMG_0002.CR2".into(),
                file_stem: "IMG_0002".into(),
                ext: ".cr2".into(),
                byte_size: 20_000,
                mtime_ns: 200,
                ctime_ns: None,
                media_kind: MediaKind::Raw,
                format: "cr2".into(),
                role: FileRole::Backup,
                hash_algo: HashAlgorithm::Blake3,
                hash_hex: "bbb222".into(),
                partial_hash: None,
                metadata: Some(ExtractedMetadata {
                    extractor: "native".into(),
                    captured_at: Some("2014-06-01T10:00:00".into()),
                    camera_model: Some("Canon EOS 6D".into()),
                    lens_model_raw: Some("15-30mm".into()),
                    lens_model: Some("15-30mm".into()),
                    ..Default::default()
                }),
            },
        ];
        db.upsert_scanned_batch(&items, run1).unwrap();

        let public_ids_before: Vec<Option<String>> = db
            .find(&FindFilter::default())
            .unwrap()
            .into_iter()
            .map(|r| r.asset_public_id)
            .collect();

        let mut aliases = HashMap::new();
        aliases.insert(
            "Unknown 100-400mm".to_string(),
            "EF100-400mm f/4.5-5.6L IS USM".to_string(),
        );
        aliases.insert(
            "15-30mm".to_string(),
            "SIGMA 15-30mm F3.5-4.5 EX DG ASPHERICAL".to_string(),
        );

        // 1. Dry-run reports planned changes without mutating DB
        let dry = db.reapply_lens_aliases(&aliases, true).unwrap();
        assert!(dry.dry_run);
        assert_eq!(dry.assets_updated, 2);
        assert_eq!(dry.contents_updated, 2);
        assert_eq!(dry.changes.len(), 2);
        assert_eq!(
            db.find(&FindFilter {
                lens: Some("SIGMA 15-30mm".into()),
                ..Default::default()
            })
            .unwrap()
            .len(),
            0
        );

        // 2. Real apply updates both assets and content_metadata while preserving raw & public_id
        let applied = db.reapply_lens_aliases(&aliases, false).unwrap();
        assert!(!applied.dry_run);
        assert_eq!(applied.assets_updated, 2);
        assert_eq!(applied.contents_updated, 2);

        let public_ids_after: Vec<Option<String>> = db
            .find(&FindFilter::default())
            .unwrap()
            .into_iter()
            .map(|r| r.asset_public_id)
            .collect();
        assert_eq!(public_ids_before, public_ids_after);

        let sigma_rows = db
            .find(&FindFilter {
                lens: Some("SIGMA 15-30mm".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(sigma_rows.len(), 1);
        assert_eq!(
            sigma_rows[0].lens_model.as_deref(),
            Some("SIGMA 15-30mm F3.5-4.5 EX DG ASPHERICAL")
        );

        // Verify content_metadata preserves lens_model_raw and updates lens_model
        let (cm_raw, cm_norm): (String, String) = db
            .conn
            .query_row(
                "SELECT lens_model_raw, lens_model FROM content_metadata WHERE lens_model_raw = '15-30mm'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(cm_raw, "15-30mm");
        assert_eq!(cm_norm, "SIGMA 15-30mm F3.5-4.5 EX DG ASPHERICAL");

        // 3. Second run with same aliases is a no-op (idempotent)
        let again = db.reapply_lens_aliases(&aliases, false).unwrap();
        assert_eq!(again.assets_updated, 0);
        assert_eq!(again.contents_updated, 0);
        assert!(again.changes.is_empty());

        // 4. Removing an alias reverts lens_model back to lens_model_raw
        aliases.remove("15-30mm");
        let reverted = db.reapply_lens_aliases(&aliases, false).unwrap();
        assert_eq!(reverted.assets_updated, 1);
        assert_eq!(reverted.contents_updated, 1);
        assert_eq!(reverted.changes[0].raw_lens, "15-30mm");
        assert_eq!(
            reverted.changes[0].normalized_lens.as_deref(),
            Some("15-30mm")
        );
    }

    #[test]
    fn computes_yearly_stats_and_chronological_gear_history_ignoring_malformed_dates() {
        let mut db = Database::open_in_memory().unwrap();
        db.upsert_host("eagle", "Eagle NAS", "freebsd").unwrap();
        let nas = StorageConfig {
            id: "nas_backup_eagle".into(),
            root: PathBuf::from("/home/yoshi/Pictures"),
            media: MediaType::Hdd,
            tier: StorageTier::Backup,
            access_mode: AccessMode::ReadOnly,
            backup_mode: BackupMode::External,
            failure_domain: Some("nas-eagle".into()),
            exclude_paths: vec![],
        };
        db.upsert_storage(&nas, "eagle", StorageStatus::Online)
            .unwrap();
        let run1 = db.start_scan_run(&nas.id, "incremental").unwrap();

        let make = |rel: &str, hash: &str, dt: Option<&str>, cam: Option<&str>, lens: Option<&str>| {
            UpsertFileInput {
                storage_id: nas.id.clone(),
                rel_path: rel.into(),
                file_name: rel.into(),
                file_stem: rel.into(),
                ext: ".cr3".into(),
                byte_size: 1000,
                mtime_ns: 100,
                ctime_ns: None,
                media_kind: MediaKind::Raw,
                format: "cr3".into(),
                role: FileRole::Backup,
                hash_algo: HashAlgorithm::Blake3,
                hash_hex: hash.into(),
                partial_hash: None,
                metadata: Some(ExtractedMetadata {
                    extractor: "native".into(),
                    captured_at: dt.map(String::from),
                    camera_model: cam.map(String::from),
                    lens_model_raw: lens.map(String::from),
                    lens_model: lens.map(String::from),
                    ..Default::default()
                }),
            }
        };

        let items = vec![
            // 2004: EOS 10D (2 shots on 2 days)
            make(
                "2004/a.crw",
                "h1",
                Some("2004-05-06T09:00:00"),
                Some("Canon EOS 10D"),
                Some("EF100-400mm f/4.5-5.6L IS USM"),
            ),
            make(
                "2004/b.crw",
                "h2",
                Some("2004-09-19T12:00:00"),
                Some("Canon EOS 10D"),
                Some("EF100-400mm f/4.5-5.6L IS USM"),
            ),
            // 2025: EOS R5m2 (2 shots on 1 day + 1 shot on another day)
            make(
                "2025/c.cr3",
                "h3",
                Some("2025-01-10T08:00:00"),
                Some("Canon EOS R5m2"),
                Some("RF200-800mm F6.3-9 IS USM"),
            ),
            make(
                "2025/d.cr3",
                "h4",
                Some("2025-01-10T08:01:00"),
                Some("Canon EOS R5m2"),
                Some("RF200-800mm F6.3-9 IS USM"),
            ),
            make(
                "2025/e.cr3",
                "h5",
                Some("2025-11-20T15:00:00"),
                Some("Canon EOS R5m2"),
                Some("RF100-500mm F4.5-7.1 L IS USM"),
            ),
            // Malformed EXIF date 0000-00-00 on R5m2 must NOT pollute first_captured!
            make(
                "2025/bad.cr3",
                "h6",
                Some("0000-00-00T00:00:00"),
                Some("Canon EOS R5m2"),
                Some("RF200-800mm F6.3-9 IS USM"),
            ),
        ];
        db.upsert_scanned_batch(&items, run1).unwrap();

        let yearly = db.stats_by_year().unwrap();
        assert_eq!(yearly.len(), 3);
        assert_eq!(yearly[0].year, "2004");
        assert_eq!(yearly[0].assets, 2);
        assert_eq!(yearly[0].active_days, 2);
        assert_eq!(yearly[0].top_camera.as_deref(), Some("Canon EOS 10D"));

        assert_eq!(yearly[1].year, "2025");
        assert_eq!(yearly[1].assets, 3);
        assert_eq!(yearly[1].active_days, 2);
        assert_eq!(yearly[1].top_camera.as_deref(), Some("Canon EOS R5m2"));
        assert_eq!(
            yearly[1].top_lens.as_deref(),
            Some("RF200-800mm F6.3-9 IS USM")
        );

        assert_eq!(yearly[2].year, "(unknown)");
        assert_eq!(yearly[2].assets, 1);

        let gh = db.gear_history().unwrap();
        assert_eq!(gh.cameras.len(), 2);
        assert_eq!(gh.cameras[0].name, "Canon EOS 10D");
        assert_eq!(gh.cameras[0].first_captured.as_deref(), Some("2004-05-06"));
        assert_eq!(gh.cameras[0].last_captured.as_deref(), Some("2004-09-19"));
        assert_eq!(
            gh.cameras[0].top_partner.as_deref(),
            Some("EF100-400mm f/4.5-5.6L IS USM")
        );

        assert_eq!(gh.cameras[1].name, "Canon EOS R5m2");
        // Must ignore 0000-00-00 and start at 2025-01-10
        assert_eq!(gh.cameras[1].first_captured.as_deref(), Some("2025-01-10"));
        assert_eq!(gh.cameras[1].last_captured.as_deref(), Some("2025-11-20"));
        assert_eq!(gh.cameras[1].assets, 4);
        assert_eq!(gh.cameras[1].active_days, 2);
        assert_eq!(
            gh.cameras[1].top_partner.as_deref(),
            Some("RF200-800mm F6.3-9 IS USM")
        );

        assert_eq!(gh.lenses.len(), 3);
        assert_eq!(gh.lenses[0].name, "EF100-400mm f/4.5-5.6L IS USM");
        assert_eq!(gh.lenses[0].top_partner.as_deref(), Some("Canon EOS 10D"));
    }

    #[test]
    fn keyset_pagination_traverses_ties_and_nulls_without_duplicates_or_gaps() {
        let mut db = Database::open_in_memory().unwrap();
        db.upsert_host("pc", "PC", "windows").unwrap();
        let st = StorageConfig {
            id: "ssd_hot".into(),
            root: PathBuf::from(r"G:\Photos"),
            media: MediaType::Nvme,
            tier: StorageTier::Hot,
            access_mode: AccessMode::Managed,
            backup_mode: BackupMode::None,
            failure_domain: None,
            exclude_paths: vec![],
        };
        db.upsert_storage(&st, "pc", StorageStatus::Online).unwrap();
        let run1 = db.start_scan_run(&st.id, "incremental").unwrap();

        let make = |rel: &str, hash: &str, dt: Option<&str>| UpsertFileInput {
            storage_id: st.id.clone(),
            rel_path: rel.into(),
            file_name: rel.into(),
            file_stem: rel.into(),
            ext: ".jpg".into(),
            byte_size: 100,
            mtime_ns: 100,
            ctime_ns: None,
            media_kind: MediaKind::Image,
            format: "jpeg".into(),
            role: FileRole::Original,
            hash_algo: HashAlgorithm::Blake3,
            hash_hex: hash.into(),
            partial_hash: None,
            metadata: dt.map(|d| ExtractedMetadata {
                extractor: "native".into(),
                captured_at: Some(d.into()),
                ..Default::default()
            }),
        };

        // 5 files: 2 burst ties at 2026-11-24, 1 older at 2025-01-01, 2 NULL timestamps
        let items = vec![
            make("burst_a.jpg", "h1", Some("2026-11-24T10:00:00")),
            make("burst_b.jpg", "h2", Some("2026-11-24T10:00:00")),
            make("older.jpg", "h3", Some("2025-01-01T08:00:00")),
            make("null_a.jpg", "h4", None),
            make("null_b.jpg", "h5", None),
        ];
        db.upsert_scanned_batch(&items, run1).unwrap();

        let mut collected = Vec::new();
        let mut cursor: Option<FindCursor> = None;
        let mut pages = 0;

        loop {
            let page = db
                .find_page(&FindFilter {
                    limit: Some(2),
                    cursor: cursor.clone(),
                    ..Default::default()
                })
                .unwrap();
            pages += 1;
            collected.extend(page.items.into_iter().map(|r| r.rel_path));
            if !page.has_more {
                assert!(page.next_cursor.is_none());
                break;
            }
            let raw_cursor = page.next_cursor.expect("has_more must return next_cursor");
            cursor = Some(FindCursor::decode(&raw_cursor).unwrap());
        }

        assert_eq!(pages, 3);
        assert_eq!(
            collected,
            vec![
                "burst_a.jpg",
                "burst_b.jpg",
                "older.jpg",
                "null_a.jpg",
                "null_b.jpg"
            ]
        );

        // Fail Closed cursor decoding validation
        assert!(FindCursor::decode("invalid").is_err());
        assert!(FindCursor::decode("v1:0:2026-11-24").is_err());
        assert!(FindCursor::decode("v1:-5:2026-11-24").is_err());
        assert!(FindCursor::decode("v1:1:bad\nctrl").is_err());
    }

    #[test]
    fn links_sidecars_to_parent_assets_without_inflating_asset_count() {
        let mut db = Database::open_in_memory().unwrap();
        db.upsert_host("pc", "PC", "windows").unwrap();
        let st = StorageConfig {
            id: "ssd_hot".into(),
            root: PathBuf::from(r"G:\Photos"),
            media: MediaType::Nvme,
            tier: StorageTier::Hot,
            access_mode: AccessMode::Managed,
            backup_mode: BackupMode::None,
            failure_domain: None,
            exclude_paths: vec![],
        };
        db.upsert_storage(&st, "pc", StorageStatus::Online).unwrap();
        let run1 = db.start_scan_run(&st.id, "incremental").unwrap();

        let make = |rel: &str,
                    name: &str,
                    stem: &str,
                    ext: &str,
                    kind: MediaKind,
                    fmt: &str,
                    role: FileRole,
                    hash: &str| UpsertFileInput {
            storage_id: st.id.clone(),
            rel_path: rel.into(),
            file_name: name.into(),
            file_stem: stem.into(),
            ext: ext.into(),
            byte_size: 100,
            mtime_ns: 100,
            ctime_ns: None,
            media_kind: kind,
            format: fmt.into(),
            role,
            hash_algo: HashAlgorithm::Blake3,
            hash_hex: hash.into(),
            partial_hash: None,
            metadata: if role == FileRole::Sidecar {
                None
            } else {
                Some(ExtractedMetadata {
                    extractor: "native".into(),
                    captured_at: Some("2026-11-24T10:00:00".into()),
                    camera_model: Some("Canon EOS R5m2".into()),
                    duration_ms: if kind == MediaKind::Video {
                        Some(12_340)
                    } else {
                        None
                    },
                    fps: if kind == MediaKind::Video {
                        Some(59.94)
                    } else {
                        None
                    },
                    ..Default::default()
                })
            },
        };

        // Sidecar ingested BEFORE its parent CR3 (tests post-scan link_sidecar_files)
        // plus double-extension `.CR3.xmp`, CRW `.THM`, a video, and an orphan `.xmp`.
        let items = vec![
            make(
                "2026/IMG_0001.xmp",
                "IMG_0001.xmp",
                "IMG_0001",
                ".xmp",
                MediaKind::Other,
                "xmp",
                FileRole::Sidecar,
                "s1",
            ),
            make(
                "2026/IMG_0001.CR3",
                "IMG_0001.CR3",
                "IMG_0001",
                ".cr3",
                MediaKind::Raw,
                "cr3",
                FileRole::Original,
                "r1",
            ),
            make(
                "2026/IMG_0001.CR3.xmp",
                "IMG_0001.CR3.xmp",
                "IMG_0001.CR3",
                ".xmp",
                MediaKind::Other,
                "xmp",
                FileRole::Sidecar,
                "s2",
            ),
            make(
                "2004/CRW_0010.CRW",
                "CRW_0010.CRW",
                "CRW_0010",
                ".crw",
                MediaKind::Raw,
                "crw",
                FileRole::Original,
                "r2",
            ),
            make(
                "2004/CRW_0010.THM",
                "CRW_0010.THM",
                "CRW_0010",
                ".thm",
                MediaKind::Other,
                "thm",
                FileRole::Sidecar,
                "s3",
            ),
            make(
                "2026/MVI_0001.MP4",
                "MVI_0001.MP4",
                "MVI_0001",
                ".mp4",
                MediaKind::Video,
                "mp4",
                FileRole::Original,
                "v1",
            ),
            make(
                "2026/ORPHAN.xmp",
                "ORPHAN.xmp",
                "ORPHAN",
                ".xmp",
                MediaKind::Other,
                "xmp",
                FileRole::Sidecar,
                "s4",
            ),
        ];
        db.upsert_scanned_batch(&items, run1).unwrap();

        // Sidecars must NEVER create standalone Assets: only CR3, CRW, MP4 = 3 logical assets
        let st_before = db.stats().unwrap();
        assert_eq!(st_before.total_assets, 3);
        assert_eq!(st_before.total_present_files, 7);

        // Post-scan link links IMG_0001.xmp (which arrived before CR3);
        // IMG_0001.CR3.xmp and CRW_0010.THM were already linked in-batch; ORPHAN.xmp stays NULL.
        let link_rep = db.link_sidecar_files(&st.id).unwrap();
        assert_eq!(link_rep.linked, 1);
        assert_eq!(link_rep.unmatched_or_ambiguous, 1);

        let cr3_aid: i64 = db
            .conn
            .query_row(
                "SELECT asset_id FROM files WHERE rel_path = '2026/IMG_0001.CR3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let xmp1_aid: Option<i64> = db
            .conn
            .query_row(
                "SELECT asset_id FROM files WHERE rel_path = '2026/IMG_0001.xmp'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let xmp2_aid: Option<i64> = db
            .conn
            .query_row(
                "SELECT asset_id FROM files WHERE rel_path = '2026/IMG_0001.CR3.xmp'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let orphan_aid: Option<i64> = db
            .conn
            .query_row(
                "SELECT asset_id FROM files WHERE rel_path = '2026/ORPHAN.xmp'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(xmp1_aid, Some(cr3_aid));
        assert_eq!(xmp2_aid, Some(cr3_aid));
        assert_eq!(orphan_aid, None);

        // Video find row carries duration_ms & fps
        let videos = db
            .find(&FindFilter {
                format: Some("mp4".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(videos.len(), 1);
        assert_eq!(videos[0].duration_ms, Some(12_340));
        assert_eq!(videos[0].fps, Some(59.94));
    }

    #[test]
    fn stats_and_gear_history_split_extenders_counts_parent_and_extender() {
        let mut db = Database::open_in_memory().unwrap();
        db.upsert_host("pc", "PC", "windows").unwrap();
        let st = StorageConfig {
            id: "ssd_hot".into(),
            root: PathBuf::from(r"G:\Photos"),
            media: MediaType::Nvme,
            tier: StorageTier::Hot,
            access_mode: AccessMode::Managed,
            backup_mode: BackupMode::None,
            failure_domain: None,
            exclude_paths: vec![],
        };
        db.upsert_storage(&st, "pc", StorageStatus::Online).unwrap();
        let run1 = db.start_scan_run(&st.id, "incremental").unwrap();

        let make = |name: &str, hash: &str, captured: &str, cam: &str, lens: &str| UpsertFileInput {
            storage_id: st.id.clone(),
            rel_path: format!("2025/{name}"),
            file_name: name.into(),
            file_stem: name.trim_end_matches(".CR3").into(),
            ext: ".cr3".into(),
            byte_size: 1000,
            mtime_ns: 100,
            ctime_ns: None,
            media_kind: MediaKind::Raw,
            format: "cr3".into(),
            role: FileRole::Original,
            hash_algo: HashAlgorithm::Blake3,
            hash_hex: hash.into(),
            partial_hash: None,
            metadata: Some(ExtractedMetadata {
                extractor: "native-cr3".into(),
                captured_at: Some(captured.into()),
                camera_model: Some(cam.into()),
                lens_model_raw: Some(lens.into()),
                lens_model: Some(lens.into()),
                ..Default::default()
            }),
        };

        let items = vec![
            // Same day (2025-05-01): bare RF100-500 + RF100-500 with RF1.4x (active_days should deduplicate to 1!)
            make(
                "IMG_1.CR3",
                "h1",
                "2025-05-01T10:00:00",
                "Canon EOS R5m2",
                "RF100-500mm F4.5-7.1 L IS USM",
            ),
            make(
                "IMG_2.CR3",
                "h2",
                "2025-05-01T11:00:00",
                "Canon EOS R5m2",
                "RF100-500mm F4.5-7.1 L IS USM + EXTENDER RF1.4x",
            ),
            // Next day: RF200-800 with RF1.4x
            make(
                "IMG_3.CR3",
                "h3",
                "2025-05-02T09:00:00",
                "Canon EOS R5",
                "RF200-800mm F6.3-9 IS USM + EXTENDER RF1.4x",
            ),
            // EF lens with shorthand +1.4x III
            make(
                "IMG_4.CR3",
                "h4",
                "2025-05-03T09:00:00",
                "Canon EOS 7D Mark II",
                "EF100-400mm f/4.5-5.6L IS II USM +1.4x III",
            ),
        ];
        db.upsert_scanned_batch(&items, run1).unwrap();

        // Default (non-split) keeps combined lens names distinct
        let plain = db.stats().unwrap();
        assert_eq!(plain.by_lens.len(), 4);

        // Split mode credits both parent lens and extender
        let split = db.stats_with_options(true).unwrap();
        let count_of = |label: &str| {
            split
                .by_lens
                .iter()
                .find(|b| b.label == label)
                .map(|b| b.count)
                .unwrap_or(0)
        };
        assert_eq!(count_of("RF100-500mm F4.5-7.1 L IS USM"), 2);
        assert_eq!(count_of("EXTENDER RF1.4x"), 2);
        assert_eq!(count_of("RF200-800mm F6.3-9 IS USM"), 1);
        assert_eq!(count_of("EF100-400mm f/4.5-5.6L IS II USM"), 1);
        assert_eq!(count_of("EXTENDER EF1.4x III"), 1);

        // Year stats picks merged parent lens as top_lens for 2025
        let yearly = db.stats_by_year_with_options(true).unwrap();
        assert_eq!(yearly.len(), 1);
        assert_eq!(
            yearly[0].top_lens.as_deref(),
            Some("RF100-500mm F4.5-7.1 L IS USM")
        );

        // Gear history deduplicates active_days for RF100-500 (still 1 day, not 2!)
        // and spans EXTENDER RF1.4x across both days (2025-05-01..2025-05-02, 2 days)
        let gh = db.gear_history_with_options(true).unwrap();
        let rf100 = gh
            .lenses
            .iter()
            .find(|l| l.name == "RF100-500mm F4.5-7.1 L IS USM")
            .unwrap();
        assert_eq!(rf100.assets, 2);
        assert_eq!(rf100.active_days, 1);
        assert_eq!(rf100.top_partner.as_deref(), Some("Canon EOS R5m2"));

        let ext14 = gh
            .lenses
            .iter()
            .find(|l| l.name == "EXTENDER RF1.4x")
            .unwrap();
        assert_eq!(ext14.assets, 2);
        assert_eq!(ext14.active_days, 2);
        assert_eq!(ext14.first_captured.as_deref(), Some("2025-05-01"));
        assert_eq!(ext14.last_captured.as_deref(), Some("2025-05-02"));
    }

    #[test]
    fn sidecar_parent_lookup_uses_storage_presence_stem_index() {
        let db = Database::open_in_memory().unwrap();
        let mut stmt = db
            .conn
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT asset_id, rel_path
                 FROM files
                 WHERE file_stem IN (?1, ?2, ?3)
                   AND storage_id = ?4
                   AND presence = 'present'
                   AND role != 'sidecar'
                   AND asset_id IS NOT NULL",
            )
            .unwrap();
        let details: Vec<String> = stmt
            .query_map(params!["IMG_0001", "IMG_0001", "img_0001", "nas"], |r| {
                r.get::<_, String>(3)
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let plan = details.join("\n");
        assert!(
            plan.contains("idx_files_storage_presence_stem"),
            "expected idx_files_storage_presence_stem in query plan, got:\n{plan}"
        );
    }

    #[test]
    fn find_duplicates_protects_backups_and_detects_managed_and_internal_duplicates() {
        let mut db = Database::open_in_memory().unwrap();
        db.upsert_host("pc", "PC", "windows").unwrap();
        let hot = StorageConfig {
            id: "ssd_hot".into(),
            root: PathBuf::from(r"G:\Photos"),
            media: MediaType::Nvme,
            tier: StorageTier::Hot,
            access_mode: AccessMode::Managed,
            backup_mode: BackupMode::None,
            failure_domain: Some("pc-nvme".into()),
            exclude_paths: vec![],
        };
        let nas = StorageConfig {
            id: "nas_backup".into(),
            root: PathBuf::from(r"\\eagle\photos"),
            media: MediaType::Nas,
            tier: StorageTier::Backup,
            access_mode: AccessMode::ReadOnly,
            backup_mode: BackupMode::External,
            failure_domain: Some("nas-eagle".into()),
            exclude_paths: vec![],
        };
        db.upsert_storage(&hot, "pc", StorageStatus::Online).unwrap();
        db.upsert_storage(&nas, "pc", StorageStatus::Online).unwrap();
        let run_hot = db.start_scan_run(&hot.id, "incremental").unwrap();
        let run_nas = db.start_scan_run(&nas.id, "incremental").unwrap();

        let make = |sid: &str, rel: &str, hash: &str, size: u64, role: FileRole| UpsertFileInput {
            storage_id: sid.into(),
            rel_path: rel.into(),
            file_name: rel.rsplit('/').next().unwrap_or(rel).into(),
            file_stem: "IMG".into(),
            ext: if role == FileRole::Sidecar {
                ".xmp".into()
            } else {
                ".cr3".into()
            },
            byte_size: size,
            mtime_ns: 100,
            ctime_ns: None,
            media_kind: if role == FileRole::Sidecar {
                MediaKind::Other
            } else {
                MediaKind::Raw
            },
            format: if role == FileRole::Sidecar {
                "xmp".into()
            } else {
                "cr3".into()
            },
            role,
            hash_algo: HashAlgorithm::Blake3,
            hash_hex: hash.into(),
            partial_hash: None,
            metadata: None,
        };

        // 1) Intentional Hot + Backup pair (hash_backup_ok): MUST NEVER be flagged as duplicate
        // 2) Managed duplicate (hash_managed_dup): 2 copies on ssd_hot (50 MB each)
        // 3) Backup-internal duplicate (hash_nas_dup): 2 copies inside read-only nas_backup (20 MB each)
        // 4) Sidecar duplicate (hash_xmp_dup): excluded by default
        db.upsert_scanned_batch(
            &[
                make("ssd_hot", "2026/IMG_OK.CR3", "hash_backup_ok", 10_000, FileRole::Original),
                make("ssd_hot", "2026/IMG_DUP.CR3", "hash_managed_dup", 50_000, FileRole::Original),
                make("ssd_hot", "2026/work/IMG_DUP_copy.CR3", "hash_managed_dup", 50_000, FileRole::Original),
                make("ssd_hot", "2026/a.xmp", "hash_xmp_dup", 500, FileRole::Sidecar),
                make("ssd_hot", "2026/b.xmp", "hash_xmp_dup", 500, FileRole::Sidecar),
            ],
            run_hot,
        )
        .unwrap();
        db.upsert_scanned_batch(
            &[
                make("nas_backup", "2026/IMG_OK.CR3", "hash_backup_ok", 10_000, FileRole::Backup),
                make("nas_backup", "2012/A.CR3", "hash_nas_dup", 20_000, FileRole::Backup),
                make("nas_backup", "2012/A_1.CR3", "hash_nas_dup", 20_000, FileRole::Backup),
            ],
            run_nas,
        )
        .unwrap();

        // Default: only actionable managed duplicate (hash_managed_dup) is returned
        let default_rep = db.find_duplicates(&DuplicateFilter::default()).unwrap();
        assert_eq!(default_rep.total_groups, 1);
        assert_eq!(default_rep.actionable_groups, 1);
        assert_eq!(default_rep.protected_only_groups, 0);
        assert_eq!(default_rep.reclaimable_bytes, 50_000);
        assert_eq!(default_rep.groups[0].hash_hex, "hash_managed_dup");
        assert_eq!(default_rep.groups[0].files[0].recommendation, "keep");
        assert_eq!(default_rep.groups[0].files[0].rel_path, "2026/IMG_DUP.CR3");
        assert_eq!(default_rep.groups[0].files[1].recommendation, "duplicate");

        // include_backups = true: ALSO shows backup-internal dup as protected (reclaimable_bytes unchanged!),
        // and STILL never flags intentional Hot + Backup pair (hash_backup_ok).
        let with_bak = db
            .find_duplicates(&DuplicateFilter {
                include_backups: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(with_bak.total_groups, 2);
        assert_eq!(with_bak.actionable_groups, 1);
        assert_eq!(with_bak.protected_only_groups, 1);
        assert_eq!(with_bak.reclaimable_bytes, 50_000);
        assert_eq!(with_bak.total_redundant_bytes, 70_000);
        let nas_grp = with_bak
            .groups
            .iter()
            .find(|g| g.hash_hex == "hash_nas_dup")
            .unwrap();
        assert_eq!(nas_grp.reclaimable_bytes, 0);
        assert!(nas_grp.files.iter().all(|f| f.protected && f.recommendation == "protected"));
        assert!(!with_bak.groups.iter().any(|g| g.hash_hex == "hash_backup_ok"));

        // include_sidecars = true picks up sidecar dup as well
        let with_sidecar = db
            .find_duplicates(&DuplicateFilter {
                include_sidecars: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(with_sidecar.total_groups, 2);
    }

    #[test]
    fn find_filters_by_focal_iso_aperture_path_asset_unbacked_and_resolves_companions() {
        let mut db = Database::open_in_memory().unwrap();
        db.upsert_host("pc", "PC", "windows").unwrap();
        let hot = StorageConfig {
            id: "ssd_hot".into(),
            root: PathBuf::from(r"G:\Photos"),
            media: MediaType::Nvme,
            tier: StorageTier::Hot,
            access_mode: AccessMode::Managed,
            backup_mode: BackupMode::None,
            failure_domain: None,
            exclude_paths: vec![],
        };
        let nas = StorageConfig {
            id: "nas_bak".into(),
            root: PathBuf::from(r"\\eagle\photos"),
            media: MediaType::Nas,
            tier: StorageTier::Backup,
            access_mode: AccessMode::ReadOnly,
            backup_mode: BackupMode::External,
            failure_domain: None,
            exclude_paths: vec![],
        };
        db.upsert_storage(&hot, "pc", StorageStatus::Online).unwrap();
        db.upsert_storage(&nas, "pc", StorageStatus::Online).unwrap();
        let run_h = db.start_scan_run(&hot.id, "incremental").unwrap();
        let run_n = db.start_scan_run(&nas.id, "incremental").unwrap();

        let mk = |sid: &str,
                  rel: &str,
                  ext: &str,
                  kind: MediaKind,
                  fmt: &str,
                  role: FileRole,
                  hash: &str,
                  fl: Option<f64>,
                  fnum: Option<f64>,
                  iso: Option<i64>| UpsertFileInput {
            storage_id: sid.into(),
            rel_path: rel.into(),
            file_name: rel.rsplit('/').next().unwrap().into(),
            file_stem: rel
                .rsplit('/')
                .next()
                .unwrap()
                .split('.')
                .next()
                .unwrap()
                .into(),
            ext: ext.into(),
            byte_size: 1024,
            mtime_ns: 100,
            ctime_ns: None,
            media_kind: kind,
            format: fmt.into(),
            role,
            hash_algo: HashAlgorithm::Blake3,
            hash_hex: hash.into(),
            partial_hash: None,
            metadata: if role == FileRole::Sidecar {
                None
            } else {
                Some(ExtractedMetadata {
                    extractor: "native-cr3".into(),
                    captured_at: Some("2025-11-03T10:00:00".into()),
                    camera_model: Some("Canon EOS R5m2".into()),
                    lens_model: Some("RF200-800mm F6.3-9 IS USM".into()),
                    focal_length_mm: fl,
                    f_number: fnum,
                    iso,
                    ..Default::default()
                })
            },
        };

        db.upsert_scanned_batch(
            &[
                mk(
                    "ssd_hot",
                    "2025/2025_11_03/IMG_1.CR3",
                    ".cr3",
                    MediaKind::Raw,
                    "cr3",
                    FileRole::Original,
                    "h_cr3",
                    Some(800.0),
                    Some(9.0),
                    Some(6400),
                ),
                mk(
                    "ssd_hot",
                    "2025/2025_11_03/IMG_1.JPG",
                    ".jpg",
                    MediaKind::Image,
                    "jpeg",
                    FileRole::Original,
                    "h_jpg",
                    Some(800.0),
                    Some(9.0),
                    Some(6400),
                ),
                mk(
                    "ssd_hot",
                    "2025/2025_11_03/IMG_1.xmp",
                    ".xmp",
                    MediaKind::Other,
                    "xmp",
                    FileRole::Sidecar,
                    "h_xmp",
                    None,
                    None,
                    None,
                ),
                mk(
                    "ssd_hot",
                    "2025/2025_11_03/IMG_2.JPG",
                    ".jpg",
                    MediaKind::Image,
                    "jpeg",
                    FileRole::Original,
                    "h_wide",
                    Some(35.0),
                    Some(1.8),
                    Some(100),
                ),
            ],
            run_h,
        )
        .unwrap();
        // Back up only IMG_1.CR3 to NAS; IMG_1.JPG and IMG_2.JPG remain unbacked
        db.upsert_scanned_batch(
            &[mk(
                "nas_bak",
                "2025/2025_11_03/IMG_1.CR3",
                ".cr3",
                MediaKind::Raw,
                "cr3",
                FileRole::Backup,
                "h_cr3",
                Some(800.0),
                Some(9.0),
                Some(6400),
            )],
            run_n,
        )
        .unwrap();

        // Filter by focal >= 500, iso >= 3200, format = jpeg -> matches only IMG_1.JPG
        let matched = db
            .find(&FindFilter {
                format: Some("jpeg".into()),
                min_focal_mm: Some(500.0),
                min_iso: Some(3200),
                path_contains: Some("2025_11_03".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].rel_path, "2025/2025_11_03/IMG_1.JPG");

        // Resolve companions for IMG_1.JPG -> pulls CR3 (hot + nas), JPG (hot), and XMP sidecar!
        let comps = db.resolve_copy_candidates(&matched, true).unwrap();
        assert!(comps.iter().any(|c| c.rel_path.ends_with("IMG_1.CR3") && c.storage_id == "ssd_hot"));
        assert!(comps.iter().any(|c| c.rel_path.ends_with("IMG_1.JPG")));
        assert!(comps.iter().any(|c| c.rel_path.ends_with("IMG_1.xmp") && c.role == "sidecar"));

        // Unbacked filter excludes IMG_1.CR3 (backed up) and returns unbacked files
        let unbacked = db
            .find(&FindFilter {
                unbacked_only: true,
                ..Default::default()
            })
            .unwrap();
        assert!(!unbacked.iter().any(|r| r.rel_path.ends_with("IMG_1.CR3")));
        assert!(unbacked.iter().any(|r| r.rel_path.ends_with("IMG_2.JPG")));

        // Wide aperture filter (max_f_number = 2.0)
        let wide = db
            .find(&FindFilter {
                max_f_number: Some(2.0),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(wide.len(), 1);
        assert_eq!(wide[0].rel_path, "2025/2025_11_03/IMG_2.JPG");

        // Filtered stats & focal distribution
        let f_filter = StatsFilter {
            year: Some("2025".into()),
            camera: Some("R5m2".into()),
            ..Default::default()
        };
        let f_stats = db.stats_filtered(&f_filter).unwrap();
        assert_eq!(f_stats.total_assets, 2);

        let f_dist = db.focal_distribution(&f_filter).unwrap();
        assert_eq!(f_dist.total_with_focal, 2);
        assert_eq!(f_dist.min_mm, Some(35.0));
        assert_eq!(f_dist.max_mm, Some(800.0));
        assert!(f_dist.buckets.iter().any(|b| b.label == "601-800mm" && b.count == 1));
        assert!(f_dist.buckets.iter().any(|b| b.label == "24-35mm" && b.count == 1));
        assert!(f_dist.top_exact_mm.iter().any(|e| e.focal_mm == 800 && e.count == 1));
    }
}
