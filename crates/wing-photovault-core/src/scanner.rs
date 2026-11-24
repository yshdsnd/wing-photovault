use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use walkdir::WalkDir;

use crate::config::StorageConfig;
use crate::model::{classify_extension, MediaKind};

#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub abs_path: PathBuf,
    pub rel_path: String,
    pub file_name: String,
    pub file_stem: String,
    pub ext: String,
    pub byte_size: u64,
    pub mtime_ns: i64,
    pub ctime_ns: Option<i64>,
    pub media_kind: MediaKind,
    pub format: String,
}

#[derive(Debug, Default)]
pub struct DiscoveryReport {
    pub files: Vec<DiscoveredFile>,
    pub errors: Vec<String>,
}

/// Non-destructively walk a storage root, skipping excluded paths and non-matching extensions.
pub fn discover_storage_files(
    storage: &StorageConfig,
    allowed_exts: &HashSet<String>,
    follow_symlinks: bool,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();

    let normalized_excludes: Vec<String> = storage
        .exclude_paths
        .iter()
        .map(|p| normalize_for_prefix_match(p))
        .collect();

    let walker = WalkDir::new(&storage.root)
        .follow_links(follow_symlinks)
        .into_iter()
        .filter_entry(|entry| {
            if normalized_excludes.is_empty() {
                return true;
            }
            let norm = normalize_for_prefix_match(entry.path());
            !normalized_excludes
                .iter()
                .any(|ex| norm == *ex || norm.starts_with(&format!("{ex}/")))
        });

    for entry_res in walker {
        let entry = match entry_res {
            Ok(e) => e,
            Err(err) => {
                report.errors.push(err.to_string());
                continue;
            }
        };

        if !entry.file_type().is_file() {
            continue;
        }

        let abs_path = entry.path().to_path_buf();
        let ext = match abs_path.extension().and_then(|e| e.to_str()) {
            Some(e) => format!(".{}", e.to_ascii_lowercase()),
            None => continue,
        };

        if !allowed_exts.contains(&ext) {
            continue;
        }

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(err) => {
                report
                    .errors
                    .push(format!("{}: {err}", abs_path.display()));
                continue;
            }
        };

        let rel_path = match abs_path.strip_prefix(&storage.root) {
            Ok(rel) => to_forward_slash_path(rel),
            Err(err) => {
                report
                    .errors
                    .push(format!("{}: {err}", abs_path.display()));
                continue;
            }
        };

        let file_name = abs_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let file_stem = abs_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();

        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);
        let ctime_ns = meta
            .created()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos().min(i64::MAX as u128) as i64);

        let (media_kind, format) = classify_extension(&ext);

        report.files.push(DiscoveredFile {
            abs_path,
            rel_path,
            file_name,
            file_stem,
            ext,
            byte_size: meta.len(),
            mtime_ns,
            ctime_ns,
            media_kind,
            format: format.to_string(),
        });
    }

    report
}

fn to_forward_slash_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn normalize_for_prefix_match(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let trimmed = s.trim_end_matches('/');
    if cfg!(windows) {
        trimmed.to_ascii_lowercase()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaType, StorageTier};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn discovers_matching_files_and_skips_excluded_paths() {
        let dir = tempdir().unwrap();
        let keep_dir = dir.path().join("2026").join("2026_11_24").join("100EOSR5");
        let skip_dir = dir.path().join("_Showcase");
        fs::create_dir_all(&keep_dir).unwrap();
        fs::create_dir_all(&skip_dir).unwrap();

        fs::write(keep_dir.join("IMG_0001.CR3"), b"raw").unwrap();
        fs::write(keep_dir.join("IMG_0001.JPG"), b"jpg").unwrap();
        fs::write(keep_dir.join("notes.txt"), b"ignore").unwrap();
        fs::write(skip_dir.join("best.jpg"), b"excluded").unwrap();

        let storage = StorageConfig {
            id: "test_ssd".into(),
            root: dir.path().to_path_buf(),
            media: MediaType::Nvme,
            tier: StorageTier::Hot,
            exclude_paths: vec![skip_dir],
        };
        let exts: HashSet<String> = [".jpg", ".cr3"].into_iter().map(String::from).collect();

        let report = discover_storage_files(&storage, &exts, false);
        assert!(report.errors.is_empty());
        assert_eq!(report.files.len(), 2);
        assert!(report
            .files
            .iter()
            .any(|f| f.rel_path == "2026/2026_11_24/100EOSR5/IMG_0001.CR3"));
    }
}
