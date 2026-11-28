use std::collections::HashMap;
use std::path::PathBuf;

use wing_photovault_core::config::{
    Config, ExifBackend, ExifConfig, GeneralConfig, HashConfig, HostConfig, PerformanceConfig,
    ScanConfig, StatsConfig, StorageConfig,
};
use wing_photovault_core::model::{AccessMode, BackupMode, HashAlgorithm, MediaType, StorageTier};

/// Editable storage row in the mini UI.
#[derive(Debug, Clone)]
pub struct StorageDraft {
    pub id: String,
    pub root: String,
    pub media: MediaType,
    pub tier: StorageTier,
    pub access_mode: AccessMode,
    pub backup_mode: BackupMode,
    pub failure_domain: String,
    pub volume_id: String,
    /// One path per line in the UI.
    pub exclude_paths_text: String,
}

impl StorageDraft {
    pub fn new_managed(index: usize) -> Self {
        Self {
            id: format!("storage_{index}"),
            root: String::new(),
            media: MediaType::Ssd,
            tier: StorageTier::Hot,
            access_mode: AccessMode::Managed,
            backup_mode: BackupMode::None,
            failure_domain: String::new(),
            volume_id: String::new(),
            exclude_paths_text: String::new(),
        }
    }

    pub fn new_external_nas(index: usize) -> Self {
        Self {
            id: format!("nas_backup_{index}"),
            root: r"\\eagle\photos".to_string(),
            media: MediaType::Nas,
            tier: StorageTier::Backup,
            access_mode: AccessMode::ReadOnly,
            backup_mode: BackupMode::External,
            failure_domain: "nas-eagle".to_string(),
            volume_id: String::new(),
            exclude_paths_text: String::new(),
        }
    }

    /// Enforce Fail Closed invariant in UI state: external backups are always read_only.
    pub fn enforce_invariants(&mut self) {
        if self.backup_mode == BackupMode::External {
            self.access_mode = AccessMode::ReadOnly;
        }
    }

    pub fn from_config(s: &StorageConfig) -> Self {
        let exclude_paths_text = s
            .exclude_paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        let mut draft = Self {
            id: s.id.clone(),
            root: s.root.to_string_lossy().into_owned(),
            media: s.media,
            tier: s.tier,
            access_mode: s.access_mode,
            backup_mode: s.backup_mode,
            failure_domain: s.failure_domain.clone().unwrap_or_default(),
            volume_id: s.volume_id.clone().unwrap_or_default(),
            exclude_paths_text,
        };
        draft.enforce_invariants();
        draft
    }

    pub fn to_config(&self) -> StorageConfig {
        let exclude_paths = self
            .exclude_paths_text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect();
        let failure_domain = {
            let trimmed = self.failure_domain.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };
        let volume_id = {
            let trimmed = self.volume_id.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };
        let access_mode = if self.backup_mode == BackupMode::External {
            AccessMode::ReadOnly
        } else {
            self.access_mode
        };

        StorageConfig {
            id: self.id.trim().to_string(),
            root: PathBuf::from(self.root.trim()),
            media: self.media,
            tier: self.tier,
            access_mode,
            backup_mode: self.backup_mode,
            failure_domain,
            volume_id,
            exclude_paths,
        }
    }
}

/// Editable lens alias row (`lens_model_raw` -> `lens_model`).
#[derive(Debug, Clone)]
pub struct LensAliasRow {
    pub raw: String,
    pub normalized: String,
}

/// Editable TOML configuration draft backed by `wing_photovault_core::config::Config`.
#[derive(Debug, Clone)]
pub struct ConfigDraft {
    pub host_id: String,
    pub host_name: String,
    pub db_path: String,
    pub log_dir: String,
    pub image_exts_csv: String,
    pub video_exts_csv: String,
    pub sidecar_exts_csv: String,
    pub incremental: bool,
    pub follow_symlinks: bool,
    pub hash_algorithm: HashAlgorithm,
    pub partial_hash_enabled: bool,
    pub ssd_io_workers: usize,
    pub hdd_io_workers: usize,
    pub nas_io_workers: usize,
    pub cpu_workers: usize,
    pub db_batch_size: usize,
    pub exif_backend: ExifBackend,
    pub exiftool_path: String,
    pub persistent_exiftool: bool,
    pub lens_aliases: Vec<LensAliasRow>,
    pub stats: StatsConfig,
    pub storages: Vec<StorageDraft>,
}

impl Default for ConfigDraft {
    fn default() -> Self {
        let cfg = Config {
            host: HostConfig::default(),
            general: GeneralConfig::default(),
            scan: ScanConfig::default(),
            hash: HashConfig::default(),
            performance: PerformanceConfig::default(),
            exif: ExifConfig::default(),
            stats: StatsConfig::default(),
            storages: vec![],
        };
        Self::from_config(&cfg)
    }
}

impl ConfigDraft {
    pub fn from_config(cfg: &Config) -> Self {
        let mut alias_pairs: Vec<(String, String)> = cfg
            .exif
            .lens_aliases
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        alias_pairs.sort_by(|a, b| a.0.cmp(&b.0));

        Self {
            host_id: cfg.host.id.clone(),
            host_name: cfg.host.name.clone(),
            db_path: cfg.general.db_path.to_string_lossy().into_owned(),
            log_dir: cfg.general.log_dir.to_string_lossy().into_owned(),
            image_exts_csv: cfg.general.image_exts.join(", "),
            video_exts_csv: cfg.general.video_exts.join(", "),
            sidecar_exts_csv: cfg.general.sidecar_exts.join(", "),
            incremental: cfg.scan.incremental,
            follow_symlinks: cfg.scan.follow_symlinks,
            hash_algorithm: cfg.hash.algorithm,
            partial_hash_enabled: cfg.hash.partial_hash_enabled,
            ssd_io_workers: cfg.performance.ssd_io_workers,
            hdd_io_workers: cfg.performance.hdd_io_workers,
            nas_io_workers: cfg.performance.nas_io_workers,
            cpu_workers: cfg.performance.cpu_workers,
            db_batch_size: cfg.performance.db_batch_size,
            exif_backend: cfg.exif.backend,
            exiftool_path: cfg.exif.exiftool_path.clone(),
            persistent_exiftool: cfg.exif.persistent_exiftool,
            lens_aliases: alias_pairs
                .into_iter()
                .map(|(raw, normalized)| LensAliasRow { raw, normalized })
                .collect(),
            stats: cfg.stats.clone(),
            storages: cfg.storages.iter().map(StorageDraft::from_config).collect(),
        }
    }

    pub fn from_toml_str(raw: &str) -> Result<Self, String> {
        let cfg: Config = toml::from_str(raw).map_err(|e| format!("TOML parse error: {e}"))?;
        cfg.validate()
            .map_err(|e| format!("Config validation error: {e}"))?;
        Ok(Self::from_config(&cfg))
    }

    pub fn to_config(&self) -> Config {
        let parse_csv = |s: &str| -> Vec<String> {
            s.split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(String::from)
                .collect()
        };

        let mut lens_aliases = HashMap::new();
        for row in &self.lens_aliases {
            let k = row.raw.trim();
            let v = row.normalized.trim();
            if !k.is_empty() && !v.is_empty() {
                lens_aliases.insert(k.to_string(), v.to_string());
            }
        }

        Config {
            host: HostConfig {
                id: self.host_id.trim().to_string(),
                name: self.host_name.trim().to_string(),
            },
            general: GeneralConfig {
                db_path: PathBuf::from(self.db_path.trim()),
                log_dir: PathBuf::from(self.log_dir.trim()),
                image_exts: parse_csv(&self.image_exts_csv),
                video_exts: parse_csv(&self.video_exts_csv),
                sidecar_exts: parse_csv(&self.sidecar_exts_csv),
            },
            scan: ScanConfig {
                incremental: self.incremental,
                follow_symlinks: self.follow_symlinks,
            },
            hash: HashConfig {
                algorithm: self.hash_algorithm,
                partial_hash_enabled: self.partial_hash_enabled,
            },
            performance: PerformanceConfig {
                ssd_io_workers: self.ssd_io_workers.max(1),
                hdd_io_workers: self.hdd_io_workers.max(1),
                nas_io_workers: self.nas_io_workers.max(1),
                cpu_workers: self.cpu_workers,
                db_batch_size: self.db_batch_size.max(1),
            },
            exif: ExifConfig {
                backend: self.exif_backend,
                exiftool_path: self.exiftool_path.trim().to_string(),
                persistent_exiftool: self.persistent_exiftool,
                lens_aliases,
            },
            stats: self.stats.clone(),
            storages: self.storages.iter().map(StorageDraft::to_config).collect(),
        }
    }

    /// Validate against Core Fail Closed rules plus UI-level completeness checks.
    pub fn validate(&self) -> Result<(), String> {
        let cfg = self.to_config();
        cfg.validate().map_err(|e| e.to_string())?;
        if cfg.general.db_path.as_os_str().is_empty() {
            return Err("general.db_path must not be empty".to_string());
        }
        for s in &cfg.storages {
            if s.root.as_os_str().is_empty() {
                return Err(format!("storage '{}' root path must not be empty", s.id));
            }
        }
        // Check duplicate non-empty lens alias keys
        let mut seen = std::collections::HashSet::new();
        for row in &self.lens_aliases {
            let k = row.raw.trim();
            let v = row.normalized.trim();
            if k.is_empty() && v.is_empty() {
                continue;
            }
            if k.is_empty() || v.is_empty() {
                return Err("lens_aliases rows must have both raw and normalized names".to_string());
            }
            if !seen.insert(k) {
                return Err(format!("duplicate lens alias key: '{k}'"));
            }
        }
        Ok(())
    }

    /// Serialize to valid TOML and verify round-trip parsing against `wing_photovault_core::Config`.
    pub fn to_toml_string(&self) -> Result<String, String> {
        self.validate()?;
        let cfg = self.to_config();
        let rendered = toml::to_string_pretty(&cfg)
            .map_err(|e| format!("Failed to serialize TOML: {e}"))?;
        // Round-trip safety check
        let reparsed: Config =
            toml::from_str(&rendered).map_err(|e| format!("Round-trip parse failed: {e}"))?;
        reparsed
            .validate()
            .map_err(|e| format!("Round-trip validation failed: {e}"))?;
        Ok(rendered)
    }

    /// Append a sample set of legacy lens aliases without overwriting existing keys.
    pub fn add_sample_lens_aliases(&mut self) {
        let samples = [
            ("Unknown 100-400mm", "EF100-400mm f/4.5-5.6L IS USM"),
            ("Unknown 10-22mm", "EF-S10-22mm f/3.5-4.5 USM"),
            ("Unknown 17-85mm", "EF-S17-85mm f/4-5.6 IS USM"),
            ("Unknown 28-75mm", "TAMRON SP AF 28-75mm F/2.8 XR Di"),
            ("28-75mm", "TAMRON SP AF 28-75mm F/2.8 XR Di"),
            ("Unknown 90mm", "TAMRON SP AF90mm F/2.8 Di MACRO 1:1"),
            ("15-30mm", "SIGMA 15-30mm F3.5-4.5 EX DG ASPHERICAL"),
            ("Unknown 50mm", "EF50mm f/1.8 II"),
        ];
        for (k, v) in samples {
            if !self.lens_aliases.iter().any(|r| r.raw.trim() == k) {
                self.lens_aliases.push(LensAliasRow {
                    raw: k.to_string(),
                    normalized: v.to_string(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_windows_paths_and_enforces_external_backup_read_only() {
        let mut draft = ConfigDraft {
            host_id: "main-pc".into(),
            host_name: "Main Windows PC".into(),
            db_path: r"C:\Users\yoshi\wing-photovault.db".into(),
            ..Default::default()
        };

        let mut nas = StorageDraft::new_external_nas(1);
        nas.access_mode = AccessMode::Managed; // Should be forced back to ReadOnly
        nas.enforce_invariants();
        assert_eq!(nas.access_mode, AccessMode::ReadOnly);

        let mut ssd = StorageDraft::new_managed(1);
        ssd.id = "ssd_hot_g".into();
        ssd.root = r"G:\Photos".into();
        ssd.media = MediaType::Nvme;
        ssd.exclude_paths_text = "G:\\Photos\\_Showcase\n".into();

        draft.storages = vec![ssd, nas];
        draft.add_sample_lens_aliases();

        let toml_text = draft.to_toml_string().unwrap();
        let reloaded = ConfigDraft::from_toml_str(&toml_text).unwrap();

        assert_eq!(reloaded.storages.len(), 2);
        assert_eq!(reloaded.storages[0].root, r"G:\Photos");
        assert_eq!(reloaded.storages[1].root, r"\\eagle\photos");
        assert_eq!(reloaded.storages[1].access_mode, AccessMode::ReadOnly);
        assert_eq!(reloaded.lens_aliases.len(), 8);
    }

    #[test]
    fn rejects_duplicate_storage_ids_and_empty_roots() {
        let mut draft = ConfigDraft {
            storages: vec![StorageDraft::new_managed(1)],
            ..Default::default()
        };
        assert!(draft.validate().is_err()); // empty root

        draft.storages[0].root = r"F:\Photos".into();
        let mut dup = draft.storages[0].clone();
        dup.root = r"G:\Photos".into();
        draft.storages.push(dup);
        assert!(draft.validate().is_err()); // duplicate storage id
    }
}
