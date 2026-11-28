use chrono::{DateTime, Local};

/// Folder layout strategy when ingesting from temporary removable media (SD / CFexpress cards).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IngestDateLayout {
    /// `YYYY/YYYY_MM_DD/<filename>` (matches standard photo archive hierarchy)
    #[default]
    YearYmdUnderscore,
    /// `YYYY/MM/YYYY_MM_DD/<filename>`
    YearMonthYmdUnderscore,
    /// `YYYY-MM-DD/<filename>`
    YmdHyphen,
    /// `<filename>` directly under destination root
    Flat,
}

impl IngestDateLayout {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "yyyy/yyyy_mm_dd" | "ymd_underscore" | "default" => Ok(Self::YearYmdUnderscore),
            "yyyy/mm/yyyy_mm_dd" | "ym_ymd_underscore" => Ok(Self::YearMonthYmdUnderscore),
            "yyyy-mm-dd" | "ymd_hyphen" => Ok(Self::YmdHyphen),
            "flat" => Ok(Self::Flat),
            other => Err(format!(
                "unsupported --date-layout '{other}' (expected: YYYY/YYYY_MM_DD, YYYY/MM/YYYY_MM_DD, YYYY-MM-DD, or flat)"
            )),
        }
    }
}

/// Sanitize an optional user-provided `--subdir` tag so it cannot inject path separators or traversal.
pub fn sanitize_subdir_suffix(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.len() > 80
        || trimmed.contains("..")
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains(':')
        || trimmed.chars().any(|c| c.is_control())
    {
        return Err(format!(
            "invalid --subdir value '{trimmed}': must be a single safe folder suffix without path separators"
        ));
    }
    Ok(trimmed.to_string())
}

/// Extract `(YYYY, MM, DD)` from an ISO EXIF `captured_at` string (e.g. `"2026-11-28T14:30:00"`),
/// falling back to local calendar date from Unix `mtime_ns` when EXIF date is absent/invalid.
pub fn resolve_ingest_ymd(captured_at: Option<&str>, mtime_ns: i64) -> (String, String, String) {
    if let Some(ts) = captured_at.map(str::trim) {
        let bytes = ts.as_bytes();
        if bytes.len() >= 10
            && bytes[0..4].iter().all(|b| b.is_ascii_digit())
            && bytes[5..7].iter().all(|b| b.is_ascii_digit())
            && bytes[8..10].iter().all(|b| b.is_ascii_digit())
            && (bytes[4] == b'-' || bytes[4] == b':')
            && (bytes[7] == b'-' || bytes[7] == b':')
        {
            let y = &ts[0..4];
            let m = &ts[5..7];
            let d = &ts[8..10];
            if ("1970"..="2999").contains(&y)
                && ("01"..="12").contains(&m)
                && ("01"..="31").contains(&d)
            {
                return (y.to_string(), m.to_string(), d.to_string());
            }
        }
    }

    let secs = mtime_ns.div_euclid(1_000_000_000);
    let nsec = mtime_ns.rem_euclid(1_000_000_000) as u32;
    if let Some(utc) = DateTime::from_timestamp(secs, nsec) {
        let local = utc.with_timezone(&Local);
        return (
            local.format("%Y").to_string(),
            local.format("%m").to_string(),
            local.format("%d").to_string(),
        );
    }

    ("unknown".to_string(), "00".to_string(), "00".to_string())
}

/// Build the normalized forward-slash relative path inside the destination storage for an ingested file.
pub fn build_ingest_rel_path(
    layout: IngestDateLayout,
    captured_at: Option<&str>,
    mtime_ns: i64,
    subdir_suffix: Option<&str>,
    file_name: &str,
) -> String {
    let (y, m, d) = resolve_ingest_ymd(captured_at, mtime_ns);
    let suffix = subdir_suffix
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("_{s}"))
        .unwrap_or_default();

    match layout {
        IngestDateLayout::YearYmdUnderscore => {
            format!("{y}/{y}_{m}_{d}{suffix}/{file_name}")
        }
        IngestDateLayout::YearMonthYmdUnderscore => {
            format!("{y}/{m}/{y}_{m}_{d}{suffix}/{file_name}")
        }
        IngestDateLayout::YmdHyphen => {
            format!("{y}-{m}-{d}{suffix}/{file_name}")
        }
        IngestDateLayout::Flat => {
            if let Some(sub) = subdir_suffix.map(str::trim).filter(|s| !s.is_empty()) {
                format!("{sub}/{file_name}")
            } else {
                file_name.to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_expected_ingest_relative_paths() {
        let p1 = build_ingest_rel_path(
            IngestDateLayout::YearYmdUnderscore,
            Some("2026-11-28T09:15:00"),
            0,
            None,
            "1M5A1060.CR3",
        );
        assert_eq!(p1, "2026/2026_11_28/1M5A1060.CR3");

        let p2 = build_ingest_rel_path(
            IngestDateLayout::YearYmdUnderscore,
            Some("2026-11-28T09:15:00"),
            0,
            Some("haneda"),
            "1M5A1060.CR3",
        );
        assert_eq!(p2, "2026/2026_11_28_haneda/1M5A1060.CR3");

        let p3 = build_ingest_rel_path(
            IngestDateLayout::YearMonthYmdUnderscore,
            Some("2026-11-28T09:15:00"),
            0,
            None,
            "1M5A1060.JPG",
        );
        assert_eq!(p3, "2026/11/2026_11_28/1M5A1060.JPG");

        let p4 = build_ingest_rel_path(
            IngestDateLayout::YmdHyphen,
            Some("2026-11-28T09:15:00"),
            0,
            None,
            "1M5A1060.JPG",
        );
        assert_eq!(p4, "2026-11-28/1M5A1060.JPG");

        assert!(sanitize_subdir_suffix("../escape").is_err());
        assert!(sanitize_subdir_suffix("a/b").is_err());
        assert_eq!(sanitize_subdir_suffix("haneda").unwrap(), "haneda");
    }
}
