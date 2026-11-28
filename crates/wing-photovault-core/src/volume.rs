use std::fs;
use std::path::Path;

/// Name of the optional marker file placed at a storage root to identify removable volumes
/// portably across Windows, FreeBSD, macOS, and Linux without requiring OS-specific FFI.
pub const VOLUME_ID_FILENAME: &str = ".wing-volume-id";

/// Result of verifying a storage root's volume identity against its configured/expected `volume_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeCheck {
    /// No expected `volume_id` is configured; `detected` holds any marker found on disk.
    Unconstrained { detected: Option<String> },
    /// Detected marker matches the expected `volume_id`.
    Matched(String),
    /// Expected `volume_id` is configured, but the marker file is missing or unreadable (Fail Closed).
    MissingMarker { expected: String },
    /// Detected marker does NOT match the expected `volume_id` (wrong removable drive mounted!).
    Mismatch { expected: String, found: String },
}

impl VolumeCheck {
    pub fn is_ok(&self) -> bool {
        matches!(
            self,
            VolumeCheck::Unconstrained { .. } | VolumeCheck::Matched(_)
        )
    }

    pub fn detected(&self) -> Option<&str> {
        match self {
            VolumeCheck::Unconstrained { detected } => detected.as_deref(),
            VolumeCheck::Matched(id) => Some(id.as_str()),
            VolumeCheck::MissingMarker { .. } => None,
            VolumeCheck::Mismatch { found, .. } => Some(found.as_str()),
        }
    }
}

/// Read and sanitize `<root>/.wing-volume-id` if present (bounded to 256 bytes, single line).
pub fn read_volume_id_marker(root: &Path) -> Option<String> {
    let marker = root.join(VOLUME_ID_FILENAME);
    let raw = fs::read_to_string(marker).ok()?;
    let line = raw.lines().next()?.trim();
    if line.is_empty() || line.len() > 128 || line.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(line.to_string())
}

/// Verify that `root` matches `expected_volume_id` when one is configured (Fail Closed).
pub fn verify_volume_identity(root: &Path, expected_volume_id: Option<&str>) -> VolumeCheck {
    let detected = read_volume_id_marker(root);
    let Some(expected) = expected_volume_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return VolumeCheck::Unconstrained { detected };
    };
    match detected {
        Some(found) if found == expected => VolumeCheck::Matched(found),
        Some(found) => VolumeCheck::Mismatch {
            expected: expected.to_string(),
            found,
        },
        None => VolumeCheck::MissingMarker {
            expected: expected.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_volume_id_marker_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // No expected volume_id -> unconstrained OK
        assert_eq!(
            verify_volume_identity(root, None),
            VolumeCheck::Unconstrained { detected: None }
        );

        // Expected configured, marker missing -> MissingMarker (Fail Closed)
        assert_eq!(
            verify_volume_identity(root, Some("hdd-archive-01")),
            VolumeCheck::MissingMarker {
                expected: "hdd-archive-01".into()
            }
        );

        // Write matching marker -> Matched
        fs::write(root.join(VOLUME_ID_FILENAME), "hdd-archive-01\n").unwrap();
        assert_eq!(
            verify_volume_identity(root, Some("hdd-archive-01")),
            VolumeCheck::Matched("hdd-archive-01".into())
        );

        // Different drive mounted -> Mismatch (Fail Closed)
        assert_eq!(
            verify_volume_identity(root, Some("hdd-archive-02")),
            VolumeCheck::Mismatch {
                expected: "hdd-archive-02".into(),
                found: "hdd-archive-01".into()
            }
        );
    }
}
