use std::collections::HashMap;
use std::fs;
use std::path::Path;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use wing_photovault_core::config::StorageConfig;
use wing_photovault_core::model::{ExtractedMetadata, FileRole, HashAlgorithm, MediaKind};

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
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageStatsRow {
    pub storage_id: String,
    pub root_path: String,
    pub media_type: String,
    pub tier: String,
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

    pub fn upsert_storage(&self, storage: &StorageConfig, seen_now: bool) -> DbResult<()> {
        let now = Utc::now().to_rfc3339();
        let last_seen = if seen_now { Some(now.as_str()) } else { None };
        let root_str = storage.root.to_string_lossy();
        self.conn.execute(
            "INSERT INTO storages (id, root_path, media_type, tier, last_seen_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(id) DO UPDATE SET
               root_path = excluded.root_path,
               media_type = excluded.media_type,
               tier = excluded.tier,
               last_seen_at = COALESCE(excluded.last_seen_at, storages.last_seen_at),
               updated_at = excluded.updated_at",
            params![
                storage.id,
                root_str,
                storage.media.as_str(),
                storage.tier.as_str(),
                last_seen,
                now
            ],
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

    pub fn touch_unchanged_files(&mut self, file_ids: &[i64], scan_run_id: i64) -> DbResult<()> {
        if file_ids.is_empty() {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE files SET presence = 'present', last_seen_scan_id = ?1, updated_at = ?2 WHERE id = ?3",
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
                aid
            } else {
                let meta = item.metadata.as_ref();
                tx.execute(
                    "INSERT INTO assets (
                        captured_at, captured_subsec, camera_make, camera_model, camera_serial,
                        lens_model_raw, lens_model, focal_length_mm, focal_length_35mm,
                        f_number, shutter_speed_s, shutter_speed_str, iso, exposure_bias,
                        width, height, orientation, gps_lat, gps_lon, duration_ms, fps,
                        created_at, updated_at
                     ) VALUES (
                        ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                        ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?22
                     )",
                    params![
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

            // 4. Upsert physical file record
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
                        role, presence, last_seen_scan_id, created_at, updated_at
                     ) VALUES (
                        ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'present', ?12, ?13, ?13
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
            "SELECT f.id, f.asset_id, f.storage_id, f.rel_path,
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
                storage_id: r.get(2)?,
                rel_path: r.get(3)?,
                format: r.get(4)?,
                byte_size: r.get(5)?,
                captured_at: r.get(6)?,
                camera_model: r.get(7)?,
                lens_model: r.get(8)?,
                focal_length_mm: r.get(9)?,
                f_number: r.get(10)?,
                shutter_speed_str: r.get(11)?,
                iso: r.get(12)?,
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
                present_files: r.get(4)?,
                missing_files: r.get(5)?,
                total_bytes: r.get(6)?,
                last_seen_at: r.get(7)?,
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
    use wing_photovault_core::model::{MediaType, StorageTier};

    #[test]
    fn shares_content_and_asset_across_hot_and_backup_storages() {
        let mut db = Database::open_in_memory().unwrap();
        let hot = StorageConfig {
            id: "ssd_hot_g".into(),
            root: PathBuf::from(r"G:\Photos"),
            media: MediaType::Nvme,
            tier: StorageTier::Hot,
            exclude_paths: vec![],
        };
        let nas = StorageConfig {
            id: "nas_backup_eagle".into(),
            root: PathBuf::from(r"\\eagle\photos"),
            media: MediaType::Nas,
            tier: StorageTier::Backup,
            exclude_paths: vec![],
        };
        db.upsert_storage(&hot, true).unwrap();
        db.upsert_storage(&nas, true).unwrap();

        let run1 = db.start_scan_run(&hot.id, "incremental").unwrap();
        let run2 = db.start_scan_run(&nas.id, "incremental").unwrap();

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

        // Same content stored under legacy NAS folder layout
        let mut item_nas = item_hot.clone();
        item_nas.storage_id = nas.id.clone();
        item_nas.rel_path = "2026/11_24/RAW/IMG_0001.CR3".into();
        item_nas.role = FileRole::Backup;

        db.upsert_scanned_batch(&[item_hot], run1).unwrap();
        db.upsert_scanned_batch(&[item_nas], run2).unwrap();

        let stats = db.stats().unwrap();
        assert_eq!(stats.total_present_files, 2);
        assert_eq!(stats.total_contents, 1);
        assert_eq!(stats.total_assets, 1);
        assert_eq!(stats.backed_up_contents, 1);
    }
}
