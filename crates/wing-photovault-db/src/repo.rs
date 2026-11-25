use std::collections::HashMap;
use std::fs;
use std::path::Path;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use uuid::Uuid;
use wing_photovault_core::config::StorageConfig;
use wing_photovault_core::model::{
    ExtractedMetadata, FileRole, HashAlgorithm, MediaKind, StorageStatus,
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

#[derive(Debug, Clone, Default)]
pub struct FindFilter {
    pub storage_id: Option<String>,
    pub format: Option<String>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub limit: Option<i64>,
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

            // 3. Resolve asset_id: reuse existing asset linked to this content (e.g. backup copy on NAS)
            let existing_asset_id: Option<i64> = tx
                .query_row(
                    "SELECT asset_id FROM files WHERE content_id = ?1 AND asset_id IS NOT NULL LIMIT 1",
                    params![content_id],
                    |r| r.get(0),
                )
                .optional()?;

            let asset_id = if let Some(aid) = existing_asset_id {
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
        let st = self.status()?;

        let backed_up_contents: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT f.content_id)
             FROM files f
             JOIN storages s ON s.id = f.storage_id
             WHERE f.presence = 'present' AND f.content_id IS NOT NULL AND s.tier = 'backup'",
            [],
            |r| r.get(0),
        )?;

        let by_format = self.query_buckets(
            "SELECT COALESCE(c.format, 'unknown'), COUNT(*), COALESCE(SUM(f.byte_size), 0)
             FROM files f
             LEFT JOIN contents c ON c.id = f.content_id
             WHERE f.presence = 'present'
             GROUP BY COALESCE(c.format, 'unknown')
             ORDER BY COUNT(*) DESC",
        )?;

        let by_camera = self.query_buckets(
            "SELECT COALESCE(a.camera_model, '(unknown)'), COUNT(DISTINCT a.id), 0
             FROM assets a
             GROUP BY COALESCE(a.camera_model, '(unknown)')
             ORDER BY COUNT(DISTINCT a.id) DESC
             LIMIT 30",
        )?;

        let by_lens = self.query_buckets(
            "SELECT COALESCE(a.lens_model, '(unknown)'), COUNT(DISTINCT a.id), 0
             FROM assets a
             GROUP BY COALESCE(a.lens_model, '(unknown)')
             ORDER BY COUNT(DISTINCT a.id) DESC
             LIMIT 30",
        )?;

        Ok(StatsReport {
            total_assets: st.total_assets,
            total_contents: st.total_contents,
            total_present_files: st.total_present_files,
            total_present_bytes: st.total_present_bytes,
            backed_up_contents,
            by_format,
            by_camera,
            by_lens,
            by_storage: st.storages,
        })
    }

    pub fn find(&self, filter: &FindFilter) -> DbResult<Vec<FindRow>> {
        let mut sql = String::from(
            "SELECT f.id, f.asset_id, a.public_id, f.storage_id, f.rel_path,
                    COALESCE(c.format, f.ext), f.byte_size,
                    a.captured_at, a.camera_model, a.lens_model,
                    a.focal_length_mm, a.f_number, a.shutter_speed_str, a.iso
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

        sql.push_str(" ORDER BY COALESCE(a.captured_at, '') DESC, f.id ASC");

        if let Some(limit) = filter.limit {
            sql.push_str(" LIMIT ?");
            bind_values.push(limit.into());
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
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
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
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| {
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
        assert_eq!(db.stats().unwrap().total_assets, 8);

        let rep = db.pair_raw_jpeg_assets(&hot.id).unwrap();
        assert_eq!(rep.pairs_merged, 2);
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
}
