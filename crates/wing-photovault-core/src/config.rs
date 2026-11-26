use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};
use crate::model::{AccessMode, BackupMode, HashAlgorithm, MediaType, StorageTier};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub host: HostConfig,
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub scan: ScanConfig,
    #[serde(default)]
    pub hash: HashConfig,
    #[serde(default)]
    pub performance: PerformanceConfig,
    #[serde(default)]
    pub exif: ExifConfig,
    #[serde(default)]
    pub storages: Vec<StorageConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    #[serde(default = "default_host_id")]
    pub id: String,
    #[serde(default = "default_host_name")]
    pub name: String,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            id: default_host_id(),
            name: default_host_name(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,
    #[serde(default = "default_log_dir")]
    pub log_dir: PathBuf,
    #[serde(default = "default_image_exts")]
    pub image_exts: Vec<String>,
    #[serde(default = "default_video_exts")]
    pub video_exts: Vec<String>,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            log_dir: default_log_dir(),
            image_exts: default_image_exts(),
            video_exts: default_video_exts(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanConfig {
    #[serde(default = "default_true")]
    pub incremental: bool,
    #[serde(default)]
    pub follow_symlinks: bool,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            incremental: true,
            follow_symlinks: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HashConfig {
    #[serde(default)]
    pub algorithm: HashAlgorithm,
    #[serde(default = "default_true")]
    pub partial_hash_enabled: bool,
}

impl Default for HashConfig {
    fn default() -> Self {
        Self {
            algorithm: HashAlgorithm::Blake3,
            partial_hash_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceConfig {
    #[serde(default = "default_ssd_workers")]
    pub ssd_io_workers: usize,
    #[serde(default = "default_hdd_workers")]
    pub hdd_io_workers: usize,
    #[serde(default = "default_nas_workers")]
    pub nas_io_workers: usize,
    #[serde(default)]
    pub cpu_workers: usize,
    #[serde(default = "default_db_batch_size")]
    pub db_batch_size: usize,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            ssd_io_workers: default_ssd_workers(),
            hdd_io_workers: default_hdd_workers(),
            nas_io_workers: default_nas_workers(),
            cpu_workers: 0,
            db_batch_size: default_db_batch_size(),
        }
    }
}

impl PerformanceConfig {
    pub fn workers_for(&self, media: MediaType) -> usize {
        let n = match media {
            MediaType::Nvme | MediaType::Ssd => self.ssd_io_workers,
            MediaType::Hdd | MediaType::Removable => self.hdd_io_workers,
            MediaType::Nas => self.nas_io_workers,
        };
        n.max(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ExifBackend {
    #[default]
    Auto,
    Native,
    ExifTool,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExifConfig {
    #[serde(default)]
    pub backend: ExifBackend,
    #[serde(default)]
    pub exiftool_path: String,
    #[serde(default = "default_true")]
    pub persistent_exiftool: bool,
    #[serde(default)]
    pub lens_aliases: HashMap<String, String>,
}

impl Default for ExifConfig {
    fn default() -> Self {
        Self {
            backend: ExifBackend::Auto,
            exiftool_path: String::new(),
            persistent_exiftool: true,
            lens_aliases: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    pub id: String,
    pub root: PathBuf,
    pub media: MediaType,
    pub tier: StorageTier,
    #[serde(default)]
    pub access_mode: AccessMode,
    #[serde(default)]
    pub backup_mode: BackupMode,
    #[serde(default)]
    pub failure_domain: Option<String>,
    #[serde(default)]
    pub exclude_paths: Vec<PathBuf>,
}

impl StorageConfig {
    /// Core invariant (Fail Closed): read_only or external backup storages are never mutable by WING.
    pub fn allows_mutation(&self) -> bool {
        self.access_mode == AccessMode::Managed && self.backup_mode != BackupMode::External
    }

    /// Core invariant: read_only, backup tier, or external backup storages are excluded from duplicate quarantine.
    pub fn allows_duplicate_quarantine(&self) -> bool {
        self.allows_mutation() && self.tier != StorageTier::Backup
    }
}

impl Config {
    pub fn load(path: &Path) -> CoreResult<Self> {
        let raw = fs::read_to_string(path).map_err(|source| CoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let cfg: Config = toml::from_str(&raw).map_err(|source| CoreError::ConfigParse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> CoreResult<()> {
        if self.host.id.trim().is_empty() {
            return Err(CoreError::ConfigInvalid("host.id must not be empty".into()));
        }
        let mut ids = HashSet::new();
        for s in &self.storages {
            if s.id.trim().is_empty() {
                return Err(CoreError::ConfigInvalid(
                    "storage id must not be empty".into(),
                ));
            }
            if !ids.insert(s.id.as_str()) {
                return Err(CoreError::ConfigInvalid(format!(
                    "duplicate storage id: {}",
                    s.id
                )));
            }
            // Fail Closed: external backups must never be configured as writable/managed
            if s.backup_mode == BackupMode::External && s.access_mode != AccessMode::ReadOnly {
                return Err(CoreError::ConfigInvalid(format!(
                    "storage '{}' has backup_mode = 'external' and must set access_mode = 'read_only'",
                    s.id
                )));
            }
        }
        Ok(())
    }

    pub fn normalized_extensions(&self) -> HashSet<String> {
        self.general
            .image_exts
            .iter()
            .chain(self.general.video_exts.iter())
            .map(|e| {
                let lower = e.trim().to_ascii_lowercase();
                if lower.starts_with('.') {
                    lower
                } else {
                    format!(".{lower}")
                }
            })
            .collect()
    }
}

fn default_host_id() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "local-host".to_string())
        .to_ascii_lowercase()
}

fn default_host_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "local-host".to_string())
}

fn default_db_path() -> PathBuf {
    PathBuf::from("./wing-photovault.db")
}

fn default_log_dir() -> PathBuf {
    PathBuf::from("./logs")
}

fn default_image_exts() -> Vec<String> {
    vec![
        ".jpg", ".jpeg", ".cr3", ".cr2", ".crw", ".hif", ".heif", ".heic",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn default_video_exts() -> Vec<String> {
    vec![".mp4", ".mov", ".m4v"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn default_true() -> bool {
    true
}

fn default_ssd_workers() -> usize {
    4
}

fn default_hdd_workers() -> usize {
    1
}

fn default_nas_workers() -> usize {
    2
}

fn default_db_batch_size() -> usize {
    1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_external_backup_with_managed_access_mode() {
        let cfg = Config {
            host: HostConfig::default(),
            general: GeneralConfig::default(),
            scan: ScanConfig::default(),
            hash: HashConfig::default(),
            performance: PerformanceConfig::default(),
            exif: ExifConfig::default(),
            storages: vec![StorageConfig {
                id: "nas_ext".into(),
                root: PathBuf::from(r"\\eagle\photos"),
                media: MediaType::Nas,
                tier: StorageTier::Backup,
                access_mode: AccessMode::Managed,
                backup_mode: BackupMode::External,
                failure_domain: Some("nas-eagle".into()),
                exclude_paths: vec![],
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn enforces_read_only_and_backup_invariants() {
        let ext = StorageConfig {
            id: "nas_ext".into(),
            root: PathBuf::from(r"\\eagle\photos"),
            media: MediaType::Nas,
            tier: StorageTier::Backup,
            access_mode: AccessMode::ReadOnly,
            backup_mode: BackupMode::External,
            failure_domain: Some("nas-eagle".into()),
            exclude_paths: vec![],
        };
        assert!(!ext.allows_mutation());
        assert!(!ext.allows_duplicate_quarantine());

        let managed_backup = StorageConfig {
            id: "nas_mgr".into(),
            root: PathBuf::from(r"\\eagle\photos2"),
            media: MediaType::Nas,
            tier: StorageTier::Backup,
            access_mode: AccessMode::Managed,
            backup_mode: BackupMode::Managed,
            failure_domain: Some("nas-eagle".into()),
            exclude_paths: vec![],
        };
        assert!(managed_backup.allows_mutation());
        assert!(!managed_backup.allows_duplicate_quarantine());
    }
}
