use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{CoreError, CoreResult};
use crate::model::HashAlgorithm;

const STREAM_BUFFER_SIZE: usize = 256 * 1024;
const PARTIAL_WINDOW_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct FileHashResult {
    pub algorithm: HashAlgorithm,
    pub hash_hex: String,
    pub partial_hash: Option<String>,
    pub bytes_read: u64,
}

/// Stream-hash a file without loading it fully into memory (safe for 32-bit targets and large videos).
pub fn hash_file(
    path: &Path,
    algorithm: HashAlgorithm,
    compute_partial: bool,
) -> CoreResult<FileHashResult> {
    let mut file = File::open(path).map_err(|source| CoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let partial_hash = if compute_partial {
        Some(compute_partial_hash(&mut file, path)?)
    } else {
        None
    };

    file.seek(SeekFrom::Start(0)).map_err(|source| CoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let mut buf = vec![0u8; STREAM_BUFFER_SIZE];
    let mut bytes_read: u64 = 0;

    let hash_hex = match algorithm {
        HashAlgorithm::Blake3 => {
            let mut hasher = blake3::Hasher::new();
            loop {
                let n = file.read(&mut buf).map_err(|source| CoreError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                bytes_read += n as u64;
            }
            hasher.finalize().to_hex().to_string()
        }
        HashAlgorithm::Sha256 => {
            let mut hasher = Sha256::new();
            loop {
                let n = file.read(&mut buf).map_err(|source| CoreError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                bytes_read += n as u64;
            }
            hex::encode(hasher.finalize())
        }
    };

    Ok(FileHashResult {
        algorithm,
        hash_hex,
        partial_hash,
        bytes_read,
    })
}

/// Fast partial hash: BLAKE3 over (file_size_le || first 64 KiB || last 64 KiB).
fn compute_partial_hash(file: &mut File, path: &Path) -> CoreResult<String> {
    let len = file
        .metadata()
        .map_err(|source| CoreError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .len();

    let mut hasher = blake3::Hasher::new();
    hasher.update(&len.to_le_bytes());

    let mut buf = vec![0u8; PARTIAL_WINDOW_SIZE];
    file.seek(SeekFrom::Start(0)).map_err(|source| CoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let head_n = read_up_to(file, &mut buf, path)?;
    hasher.update(&buf[..head_n]);

    if len > PARTIAL_WINDOW_SIZE as u64 {
        let tail_start = len.saturating_sub(PARTIAL_WINDOW_SIZE as u64);
        file.seek(SeekFrom::Start(tail_start))
            .map_err(|source| CoreError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let tail_n = read_up_to(file, &mut buf, path)?;
        hasher.update(&buf[..tail_n]);
    }

    Ok(hasher.finalize().to_hex().to_string())
}

fn read_up_to(file: &mut File, buf: &mut [u8], path: &Path) -> CoreResult<usize> {
    let mut total = 0;
    while total < buf.len() {
        let n = file.read(&mut buf[total..]).map_err(|source| CoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn hashes_blake3_and_sha256_consistently() {
        let mut tmp = NamedTempFile::new().unwrap();
        let data = b"WING PhotoVault test payload 1234567890";
        tmp.write_all(data).unwrap();

        let b3 = hash_file(tmp.path(), HashAlgorithm::Blake3, true).unwrap();
        assert_eq!(b3.bytes_read, data.len() as u64);
        assert_eq!(b3.hash_hex, blake3::hash(data).to_hex().to_string());
        assert!(b3.partial_hash.is_some());

        let s256 = hash_file(tmp.path(), HashAlgorithm::Sha256, false).unwrap();
        assert_eq!(s256.bytes_read, data.len() as u64);
        assert_eq!(s256.hash_hex.len(), 64);
        assert!(s256.partial_hash.is_none());
    }
}
