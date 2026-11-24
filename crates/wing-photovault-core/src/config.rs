use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};
use crate::model::{HashAlgorithm, MediaType, StorageTier};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
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
    #[serde(default)]
    pub lens_aliases: HashMap<String, String>,
}

impl Default for ExifConfig {
    fn default() -> Self {
        Self {
            backend: ExifBackend::Auto,
            exiftool_path: String::new(),
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
    pub exclude_paths: Vec<PathBuf>,
}

impl Config {
    pub fn load(path: &Path) -> CoreResult<Self> {
        let raw = fs::read_to_string(path).map_err(|source| CoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let cfg: Config = toml::from_str(&raw).map_err(|source| CoreError::ConfigParse {
            path: path.to_path_buf(),
            source,
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> CoreResult<()> {
        let mut ids = HashSet::new();
        for s in &self.storages {
            if s.id.trim().is_empty() {
                return Err(CoreError::ConfigInvalid("storage id must not be empty".into()));
            }
            if !ids.insert(s.id.as_str()) {
                return Err(CoreError::ConfigInvalid(format!(
                    "duplicate storage id: {}",
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
