use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use sha2::Digest;

use crate::hashing::hash_file;
use crate::model::HashAlgorithm;

static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyStatus {
    WouldCopy,
    Copied,
    SkippedIdentical,
    ConflictDifferentContent,
    MissingSource,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifiedCopyResult {
    pub src_path: String,
    pub dst_path: String,
    pub byte_size: u64,
    pub status: CopyStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Safely join a catalog-relative path onto `dest_root`, rejecting path traversal (`..`),
/// drive prefixes, empty paths, or control characters (Fail Closed).
pub fn safe_join_rel_path(dest_root: &Path, rel_path: &str, flat: bool) -> Result<PathBuf, String> {
    if rel_path.is_empty() || rel_path.chars().any(|c| c.is_control()) {
        return Err("invalid relative path: empty or contains control characters".into());
    }

    let mut segments: Vec<&str> = Vec::new();
    for seg in rel_path.split(['/', '\\']) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." || seg.contains(':') {
            return Err(format!("unsafe relative path segment in '{rel_path}'"));
        }
        segments.push(seg);
    }

    if segments.is_empty() {
        return Err(format!("relative path has no filename segments: '{rel_path}'"));
    }

    let mut out = dest_root.to_path_buf();
    if flat {
        out.push(segments[segments.len() - 1]);
    } else {
        for seg in segments {
            out.push(seg);
        }
    }
    Ok(out)
}

/// Non-destructive, hash-verified file copy:
/// - Never mutates or deletes `src`.
/// - Never overwrites an existing `dst` with differing content (Fail Closed).
/// - Skips idempotently if `dst` already exists with identical content hash.
/// - In `dry_run` mode, performs zero directory creation and zero file writes.
/// - Copies via a sibling temporary file, calls `sync_all()`, re-verifies the written file's
///   size and content hash on disk, preserves `mtime` best-effort, then atomically renames.
pub fn verified_copy_file(
    src: &Path,
    dst: &Path,
    algo: HashAlgorithm,
    expected_hash_hex: Option<&str>,
    dry_run: bool,
) -> VerifiedCopyResult {
    let src_str = src.to_string_lossy().into_owned();
    let dst_str = dst.to_string_lossy().into_owned();

    let src_meta = match fs::metadata(src) {
        Ok(m) if m.is_file() => m,
        Ok(_) => {
            return VerifiedCopyResult {
                src_path: src_str,
                dst_path: dst_str,
                byte_size: 0,
                status: CopyStatus::MissingSource,
                detail: Some("source path is not a regular file".into()),
            };
        }
        Err(err) => {
            return VerifiedCopyResult {
                src_path: src_str,
                dst_path: dst_str,
                byte_size: 0,
                status: CopyStatus::MissingSource,
                detail: Some(err.to_string()),
            };
        }
    };
    let byte_size = src_meta.len();
    let src_mtime = src_meta.modified().ok();

    // Check existing destination (idempotent skip if identical; Fail Closed if differing)
    if dst.exists() {
        if let (Ok(cs), Ok(cd)) = (fs::canonicalize(src), fs::canonicalize(dst)) {
            if cs == cd {
                return VerifiedCopyResult {
                    src_path: src_str,
                    dst_path: dst_str,
                    byte_size,
                    status: CopyStatus::SkippedIdentical,
                    detail: Some("source and destination refer to the same file".into()),
                };
            }
        }
        let target_expected = match expected_hash_hex {
            Some(h) => Ok(h.to_string()),
            None => hash_file(src, algo, false).map(|r| r.hash_hex),
        };
        let dst_hash = hash_file(dst, algo, false);
        return match (target_expected, dst_hash) {
            (Ok(exp), Ok(actual)) if actual.hash_hex.eq_ignore_ascii_case(&exp) => {
                VerifiedCopyResult {
                    src_path: src_str,
                    dst_path: dst_str,
                    byte_size,
                    status: CopyStatus::SkippedIdentical,
                    detail: None,
                }
            }
            (Ok(_), Ok(_)) => VerifiedCopyResult {
                src_path: src_str,
                dst_path: dst_str,
                byte_size,
                status: CopyStatus::ConflictDifferentContent,
                detail: Some("destination exists with different content (refusing overwrite)".into()),
            },
            (Err(err), _) | (_, Err(err)) => VerifiedCopyResult {
                src_path: src_str,
                dst_path: dst_str,
                byte_size,
                status: CopyStatus::Failed,
                detail: Some(format!("failed to hash existing destination: {err}")),
            },
        };
    }

    if dry_run {
        return VerifiedCopyResult {
            src_path: src_str,
            dst_path: dst_str,
            byte_size,
            status: CopyStatus::WouldCopy,
            detail: None,
        };
    }

    let Some(parent) = dst.parent() else {
        return VerifiedCopyResult {
            src_path: src_str,
            dst_path: dst_str,
            byte_size,
            status: CopyStatus::Failed,
            detail: Some("destination path has no parent directory".into()),
        };
    };

    if let Err(err) = fs::create_dir_all(parent) {
        return VerifiedCopyResult {
            src_path: src_str,
            dst_path: dst_str,
            byte_size,
            status: CopyStatus::Failed,
            detail: Some(format!("failed to create destination directory: {err}")),
        };
    }

    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(".wingpv-copy-{}-{seq}.tmp", std::process::id());
    let tmp_path = parent.join(tmp_name);

    let copy_outcome = (|| -> Result<String, String> {
        let mut reader = File::open(src).map_err(|e| format!("open source failed: {e}"))?;
        let mut writer = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| format!("create temp file failed: {e}"))?;

        let mut buf = vec![0u8; 64 * 1024];
        let mut written: u64 = 0;

        let streamed_hash = match algo {
            HashAlgorithm::Blake3 => {
                let mut hasher = blake3::Hasher::new();
                loop {
                    let n = reader.read(&mut buf).map_err(|e| format!("read failed: {e}"))?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                    writer
                        .write_all(&buf[..n])
                        .map_err(|e| format!("write failed: {e}"))?;
                    written += n as u64;
                }
                hasher.finalize().to_hex().to_string()
            }
            HashAlgorithm::Sha256 => {
                let mut hasher = sha2::Sha256::new();
                loop {
                    let n = reader.read(&mut buf).map_err(|e| format!("read failed: {e}"))?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                    writer
                        .write_all(&buf[..n])
                        .map_err(|e| format!("write failed: {e}"))?;
                    written += n as u64;
                }
                hex::encode(hasher.finalize())
            }
        };

        writer
            .sync_all()
            .map_err(|e| format!("flush/sync temp file failed: {e}"))?;
        if let Some(mt) = src_mtime {
            let _ = writer.set_modified(mt);
        }
        drop(writer);

        if let Some(exp) = expected_hash_hex {
            if !streamed_hash.eq_ignore_ascii_case(exp) {
                return Err(format!(
                    "source content hash mismatch vs catalog (expected {}, got {})",
                    &exp[..exp.len().min(12)],
                    &streamed_hash[..streamed_hash.len().min(12)]
                ));
            }
        }

        // Re-read temp file from disk to verify written integrity before committing
        let verified = hash_file(&tmp_path, algo, false)
            .map_err(|e| format!("post-write verification hash failed: {e}"))?;
        if verified.bytes_read != written || !verified.hash_hex.eq_ignore_ascii_case(&streamed_hash)
        {
            return Err("post-write verification mismatch on destination disk".into());
        }

        Ok(verified.hash_hex)
    })();

    if let Err(err) = copy_outcome {
        let _ = fs::remove_file(&tmp_path);
        return VerifiedCopyResult {
            src_path: src_str,
            dst_path: dst_str,
            byte_size,
            status: CopyStatus::Failed,
            detail: Some(err),
        };
    }

    // Final non-overwrite guard before rename
    if dst.exists() {
        let _ = fs::remove_file(&tmp_path);
        return VerifiedCopyResult {
            src_path: src_str,
            dst_path: dst_str,
            byte_size,
            status: CopyStatus::ConflictDifferentContent,
            detail: Some("destination appeared concurrently before commit (refusing overwrite)".into()),
        };
    }

    if let Err(err) = fs::rename(&tmp_path, dst) {
        let _ = fs::remove_file(&tmp_path);
        return VerifiedCopyResult {
            src_path: src_str,
            dst_path: dst_str,
            byte_size,
            status: CopyStatus::Failed,
            detail: Some(format!("commit rename failed: {err}")),
        };
    }

    VerifiedCopyResult {
        src_path: src_str,
        dst_path: dst_str,
        byte_size,
        status: CopyStatus::Copied,
        detail: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn dry_run_never_creates_dirs_or_files() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("src.cr3");
        fs::write(&src, b"raw-photo-bytes").unwrap();
        let dst = tmp.path().join("nested").join("dir").join("src.cr3");

        let res = verified_copy_file(&src, &dst, HashAlgorithm::Blake3, None, true);
        assert_eq!(res.status, CopyStatus::WouldCopy);
        assert!(!dst.exists());
        assert!(!tmp.path().join("nested").exists());
    }

    #[test]
    fn verified_copy_succeeds_skips_identical_and_refuses_differing_overwrite() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("IMG_0001.CR3");
        fs::write(&src, b"canon-cr3-original").unwrap();
        let expected = hash_file(&src, HashAlgorithm::Blake3, false)
            .unwrap()
            .hash_hex;
        let dst = tmp.path().join("out").join("2026").join("IMG_0001.CR3");

        // 1. Fresh copy succeeds and verifies
        let r1 = verified_copy_file(&src, &dst, HashAlgorithm::Blake3, Some(&expected), false);
        assert_eq!(r1.status, CopyStatus::Copied);
        assert_eq!(fs::read(&dst).unwrap(), b"canon-cr3-original");

        // 2. Re-running with identical content skips safely
        let r2 = verified_copy_file(&src, &dst, HashAlgorithm::Blake3, Some(&expected), false);
        assert_eq!(r2.status, CopyStatus::SkippedIdentical);

        // 3. Differing content at destination is NEVER overwritten
        fs::write(&dst, b"different-existing-file").unwrap();
        let r3 = verified_copy_file(&src, &dst, HashAlgorithm::Blake3, Some(&expected), false);
        assert_eq!(r3.status, CopyStatus::ConflictDifferentContent);
        assert_eq!(fs::read(&dst).unwrap(), b"different-existing-file");
    }

    #[test]
    fn rejects_path_traversal_and_hash_mismatch() {
        let tmp = tempdir().unwrap();
        assert!(safe_join_rel_path(tmp.path(), "../escape.jpg", false).is_err());
        assert!(safe_join_rel_path(tmp.path(), "C:/evil/a.jpg", false).is_err());
        assert_eq!(
            safe_join_rel_path(tmp.path(), "2026/2026_11_22/IMG_1.CR3", false).unwrap(),
            tmp.path().join("2026").join("2026_11_22").join("IMG_1.CR3")
        );
        assert_eq!(
            safe_join_rel_path(tmp.path(), "2026/2026_11_22/IMG_1.CR3", true).unwrap(),
            tmp.path().join("IMG_1.CR3")
        );

        let src = tmp.path().join("corrupt.cr3");
        fs::write(&src, b"tampered-bytes").unwrap();
        let dst = tmp.path().join("out").join("corrupt.cr3");
        let r = verified_copy_file(
            &src,
            &dst,
            HashAlgorithm::Blake3,
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
            false,
        );
        assert_eq!(r.status, CopyStatus::Failed);
        assert!(!dst.exists());
    }
}
