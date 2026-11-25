use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    Nvme,
    Ssd,
    Hdd,
    Nas,
    Removable,
}

impl MediaType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nvme => "nvme",
            Self::Ssd => "ssd",
            Self::Hdd => "hdd",
            Self::Nas => "nas",
            Self::Removable => "removable",
        }
    }
}

impl fmt::Display for MediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StorageTier {
    Inbox,
    Hot,
    Archive,
    Backup,
    Quarantine,
}

impl StorageTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Hot => "hot",
            Self::Archive => "archive",
            Self::Backup => "backup",
            Self::Quarantine => "quarantine",
        }
    }

    pub fn default_file_role(self) -> FileRole {
        match self {
            Self::Backup => FileRole::Backup,
            Self::Quarantine => FileRole::Quarantined,
            Self::Inbox | Self::Hot | Self::Archive => FileRole::Original,
        }
    }
}

impl fmt::Display for StorageTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    #[default]
    Managed,
    ReadOnly,
}

impl AccessMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::ReadOnly => "read_only",
        }
    }
}

impl fmt::Display for AccessMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BackupMode {
    #[default]
    None,
    Managed,
    External,
}

impl BackupMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Managed => "managed",
            Self::External => "external",
        }
    }
}

impl fmt::Display for BackupMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StorageStatus {
    #[default]
    Online,
    Offline,
    Failed,
    Retired,
}

impl StorageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
            Self::Failed => "failed",
            Self::Retired => "retired",
        }
    }
}

impl fmt::Display for StorageStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub fn current_host_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "freebsd") {
        "freebsd"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileRole {
    Original,
    Backup,
    Quarantined,
    Exported,
    Sidecar,
}

impl FileRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Backup => "backup",
            Self::Quarantined => "quarantined",
            Self::Exported => "exported",
            Self::Sidecar => "sidecar",
        }
    }
}

/// File-level presence, strictly separated from StorageStatus (online/offline/failed/retired).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Presence {
    Present,
    Missing,
    Corrupted,
}

impl Presence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Missing => "missing",
            Self::Corrupted => "corrupted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgorithm {
    #[default]
    Blake3,
    Sha256,
}

impl HashAlgorithm {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blake3 => "blake3",
            Self::Sha256 => "sha256",
        }
    }
}

impl fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Raw,
    Video,
    Other,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Raw => "raw",
            Self::Video => "video",
            Self::Other => "other",
        }
    }
}

/// Classify lowercase extension (with leading dot, e.g. ".cr3") into (MediaKind, format name).
pub fn classify_extension(ext: &str) -> (MediaKind, &'static str) {
    match ext {
        ".jpg" | ".jpeg" => (MediaKind::Image, "jpeg"),
        ".hif" | ".heif" | ".heic" => (MediaKind::Image, "heif"),
        ".cr3" => (MediaKind::Raw, "cr3"),
        ".cr2" => (MediaKind::Raw, "cr2"),
        ".crw" => (MediaKind::Raw, "crw"),
        ".dng" => (MediaKind::Raw, "dng"),
        ".arw" => (MediaKind::Raw, "arw"),
        ".nef" => (MediaKind::Raw, "nef"),
        ".mp4" | ".m4v" => (MediaKind::Video, "mp4"),
        ".mov" => (MediaKind::Video, "mov"),
        _ => (MediaKind::Other, "unknown"),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractedMetadata {
    pub extractor: String,
    pub captured_at: Option<String>,
    pub captured_subsec: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub camera_serial: Option<String>,
    pub lens_model_raw: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f64>,
    pub focal_length_35mm: Option<f64>,
    pub f_number: Option<f64>,
    pub shutter_speed_s: Option<f64>,
    pub shutter_speed_str: Option<String>,
    pub iso: Option<i64>,
    pub exposure_bias: Option<f64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub duration_ms: Option<i64>,
    pub fps: Option<f64>,
    pub raw_json: Option<String>,
}

/// Portable JSONL scan record shared between local scan and NAS remote scan/manifest import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanManifestRecord {
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
    pub hash_algo: HashAlgorithm,
    pub hash_hex: String,
    pub partial_hash: Option<String>,
    pub metadata: Option<ExtractedMetadata>,
}
