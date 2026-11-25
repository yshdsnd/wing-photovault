use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use std::process::Command;

use exif::{In, Tag, Value};

use crate::config::{ExifBackend, ExifConfig};
use crate::model::ExtractedMetadata;

/// Canon CR3 metadata UUID inside `moov` box: 85c0b687-820f-11e0-8111-f4ce462b6a48
const CANON_CR3_UUID: [u8; 16] = [
    0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48,
];

/// Hard upper bound on ISOBMFF box iterations to guard against malformed/malicious CR3 inputs.
const MAX_BOX_ITERATIONS: usize = 1024;

pub fn extract_metadata(path: &Path, ext: &str, cfg: &ExifConfig) -> Option<ExtractedMetadata> {
    let mut meta = match cfg.backend {
        ExifBackend::None => None,
        ExifBackend::Native => extract_native(path, ext),
        ExifBackend::ExifTool => extract_exiftool(path, &cfg.exiftool_path),
        ExifBackend::Auto => {
            let native = extract_native(path, ext);
            if native
                .as_ref()
                .is_some_and(|m| m.captured_at.is_some() || m.camera_model.is_some())
            {
                native
            } else {
                extract_exiftool(path, &cfg.exiftool_path).or(native)
            }
        }
    };

    if let Some(m) = meta.as_mut() {
        normalize_lens(m, &cfg.lens_aliases);
    }
    meta
}

fn normalize_lens(meta: &mut ExtractedMetadata, aliases: &HashMap<String, String>) {
    if let Some(raw) = meta.lens_model_raw.as_ref() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            meta.lens_model_raw = None;
            meta.lens_model = None;
            return;
        }
        if let Some(mapped) = aliases.get(trimmed) {
            meta.lens_model = Some(mapped.clone());
        } else {
            meta.lens_model = Some(trimmed.to_string());
        }
    }
}

/// Fast Rust-native EXIF extraction for JPEG, CR2/TIFF, and Canon CR3 (ISOBMFF CMT1/CMT2).
pub fn extract_native(path: &Path, ext: &str) -> Option<ExtractedMetadata> {
    match ext {
        ".cr3" => extract_cr3_native(path),
        ".jpg" | ".jpeg" | ".cr2" | ".dng" | ".nef" | ".arw" => extract_container_native(path),
        _ => None,
    }
}

fn extract_container_native(path: &Path) -> Option<ExtractedMetadata> {
    let file = File::open(path).ok()?;
    let mut bufreader = BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut bufreader).ok()?;
    let mut meta = ExtractedMetadata {
        extractor: "native".to_string(),
        ..Default::default()
    };
    populate_from_exif(&exif, &mut meta);
    Some(meta)
}

/// Lightweight ISOBMFF box walker for Canon CR3: skips image data (`mdat`) and reads only `moov` -> Canon `uuid` -> `CMT1`/`CMT2`.
fn extract_cr3_native(path: &Path) -> Option<ExtractedMetadata> {
    let mut file = File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();

    let (moov_start, moov_end) = find_box(&mut file, 0, file_len, b"moov")?;
    let mut cursor = moov_start;
    let mut uuid_payload_range = None;
    let mut iter_count = 0usize;

    while cursor + 8 <= moov_end && iter_count < MAX_BOX_ITERATIONS {
        iter_count += 1;
        let (box_type, payload_start, box_end) = read_box_header(&mut file, cursor, moov_end)?;
        if &box_type == b"uuid" && box_end - payload_start >= 16 {
            file.seek(SeekFrom::Start(payload_start)).ok()?;
            let mut uuid = [0u8; 16];
            file.read_exact(&mut uuid).ok()?;
            if uuid == CANON_CR3_UUID {
                uuid_payload_range = Some((payload_start + 16, box_end));
                break;
            }
        }
        if box_end <= cursor {
            break;
        }
        cursor = box_end;
    }

    let (cmt_start, cmt_end) = uuid_payload_range?;
    let mut meta = ExtractedMetadata {
        extractor: "native-cr3".to_string(),
        ..Default::default()
    };
    let mut parsed_any = false;

    let mut pos = cmt_start;
    let mut cmt_iters = 0usize;
    while pos + 8 <= cmt_end && cmt_iters < MAX_BOX_ITERATIONS {
        cmt_iters += 1;
        let (box_type, payload_start, box_end) = read_box_header(&mut file, pos, cmt_end)?;
        if &box_type == b"CMT1" || &box_type == b"CMT2" || &box_type == b"CMT4" {
            let size = (box_end - payload_start) as usize;
            if size > 0 && size <= 4 * 1024 * 1024 {
                file.seek(SeekFrom::Start(payload_start)).ok()?;
                let mut raw = vec![0u8; size];
                if file.read_exact(&mut raw).is_ok() {
                    let mut cur = Cursor::new(raw);
                    if let Ok(exif) = exif::Reader::new().read_from_container(&mut cur) {
                        populate_from_exif(&exif, &mut meta);
                        parsed_any = true;
                    }
                }
            }
        }
        if box_end <= pos {
            break;
        }
        pos = box_end;
    }

    if parsed_any {
        Some(meta)
    } else {
        None
    }
}

fn find_box(file: &mut File, start: u64, end: u64, target: &[u8; 4]) -> Option<(u64, u64)> {
    let mut pos = start;
    let mut iters = 0usize;
    while pos + 8 <= end && iters < MAX_BOX_ITERATIONS {
        iters += 1;
        let (box_type, payload_start, box_end) = read_box_header(file, pos, end)?;
        if &box_type == target {
            return Some((payload_start, box_end));
        }
        if box_end <= pos {
            break;
        }
        pos = box_end;
    }
    None
}

fn read_box_header(file: &mut File, pos: u64, limit: u64) -> Option<([u8; 4], u64, u64)> {
    file.seek(SeekFrom::Start(pos)).ok()?;
    let mut hdr = [0u8; 8];
    file.read_exact(&mut hdr).ok()?;
    let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
    let box_type = [hdr[4], hdr[5], hdr[6], hdr[7]];

    let (payload_start, box_end) = if size32 == 1 {
        let mut ext = [0u8; 8];
        file.read_exact(&mut ext).ok()?;
        let size64 = u64::from_be_bytes(ext);
        (pos + 16, pos.saturating_add(size64).min(limit))
    } else if size32 == 0 {
        (pos + 8, limit)
    } else if size32 >= 8 {
        (pos + 8, pos.saturating_add(size32).min(limit))
    } else {
        return None;
    };

    if payload_start > box_end {
        return None;
    }
    Some((box_type, payload_start, box_end))
}

fn populate_from_exif(exif: &exif::Exif, meta: &mut ExtractedMetadata) {
    if meta.captured_at.is_none() {
        if let Some(dt) = get_ascii(exif, Tag::DateTimeOriginal)
            .or_else(|| get_ascii(exif, Tag::DateTimeDigitized))
            .or_else(|| get_ascii(exif, Tag::DateTime))
        {
            meta.captured_at = Some(normalize_exif_datetime(&dt));
        }
    }
    if meta.captured_subsec.is_none() {
        meta.captured_subsec = get_ascii(exif, Tag::SubSecTimeOriginal)
            .or_else(|| get_ascii(exif, Tag::SubSecTimeDigitized));
    }
    if meta.camera_make.is_none() {
        meta.camera_make = get_ascii(exif, Tag::Make);
    }
    if meta.camera_model.is_none() {
        meta.camera_model = get_ascii(exif, Tag::Model);
    }
    if meta.camera_serial.is_none() {
        meta.camera_serial = get_ascii(exif, Tag::BodySerialNumber);
    }
    if meta.lens_model_raw.is_none() {
        meta.lens_model_raw = get_ascii(exif, Tag::LensModel);
    }
    if meta.focal_length_mm.is_none() {
        meta.focal_length_mm = get_rational(exif, Tag::FocalLength);
    }
    if meta.focal_length_35mm.is_none() {
        meta.focal_length_35mm = get_u32(exif, Tag::FocalLengthIn35mmFilm).map(|v| v as f64);
    }
    if meta.f_number.is_none() {
        meta.f_number = get_rational(exif, Tag::FNumber);
    }
    if meta.shutter_speed_s.is_none() {
        if let Some((num, den)) = get_rational_pair(exif, Tag::ExposureTime) {
            if den != 0 {
                meta.shutter_speed_s = Some(num as f64 / den as f64);
                meta.shutter_speed_str = Some(format_shutter(num, den));
            }
        }
    }
    if meta.iso.is_none() {
        meta.iso = get_u32(exif, Tag::PhotographicSensitivity).map(|v| v as i64);
    }
    if meta.exposure_bias.is_none() {
        meta.exposure_bias = get_srational(exif, Tag::ExposureBiasValue);
    }
    if meta.width.is_none() {
        meta.width = get_u32(exif, Tag::PixelXDimension)
            .or_else(|| get_u32(exif, Tag::ImageWidth))
            .map(|v| v as i64);
    }
    if meta.height.is_none() {
        meta.height = get_u32(exif, Tag::PixelYDimension)
            .or_else(|| get_u32(exif, Tag::ImageLength))
            .map(|v| v as i64);
    }
    if meta.orientation.is_none() {
        meta.orientation = get_u32(exif, Tag::Orientation).map(|v| v as i64);
    }
}

fn get_ascii(exif: &exif::Exif, tag: Tag) -> Option<String> {
    let field = exif.get_field(tag, In::PRIMARY)?;
    match &field.value {
        Value::Ascii(items) => items
            .first()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map(|s| s.trim_matches('\0').trim().to_string())
            .filter(|s| !s.is_empty()),
        _ => Some(field.display_value().to_string().trim().to_string()).filter(|s| !s.is_empty()),
    }
}

fn get_u32(exif: &exif::Exif, tag: Tag) -> Option<u32> {
    let field = exif.get_field(tag, In::PRIMARY)?;
    field.value.get_uint(0)
}

fn get_rational(exif: &exif::Exif, tag: Tag) -> Option<f64> {
    let (n, d) = get_rational_pair(exif, tag)?;
    if d == 0 {
        None
    } else {
        Some(n as f64 / d as f64)
    }
}

fn get_rational_pair(exif: &exif::Exif, tag: Tag) -> Option<(u32, u32)> {
    let field = exif.get_field(tag, In::PRIMARY)?;
    match &field.value {
        Value::Rational(v) => v.first().map(|r| (r.num, r.denom)),
        _ => None,
    }
}

fn get_srational(exif: &exif::Exif, tag: Tag) -> Option<f64> {
    let field = exif.get_field(tag, In::PRIMARY)?;
    match &field.value {
        Value::SRational(v) => v.first().and_then(|r| {
            if r.denom == 0 {
                None
            } else {
                Some(r.num as f64 / r.denom as f64)
            }
        }),
        _ => None,
    }
}

fn format_shutter(num: u32, den: u32) -> String {
    if num == 0 || den == 0 {
        return "0".to_string();
    }
    if num >= den {
        let secs = num as f64 / den as f64;
        if (secs - secs.round()).abs() < 1e-6 {
            format!("{}s", secs.round() as u64)
        } else {
            format!("{secs:.1}s")
        }
    } else {
        let inv = (den as f64 / num as f64).round() as u64;
        format!("1/{inv}")
    }
}

fn normalize_exif_datetime(raw: &str) -> String {
    // Convert "YYYY:MM:DD HH:MM:SS" -> "YYYY-MM-DDTHH:MM:SS"
    let trimmed = raw.trim();
    if trimmed.len() >= 19 && &trimmed[4..5] == ":" && &trimmed[7..8] == ":" {
        format!(
            "{}-{}-{}T{}",
            &trimmed[0..4],
            &trimmed[5..7],
            &trimmed[8..10],
            &trimmed[11..19]
        )
    } else {
        trimmed.to_string()
    }
}

/// Fallback / reference extractor using ExifTool CLI (`exiftool -json -n`).
pub fn extract_exiftool(path: &Path, exiftool_path: &str) -> Option<ExtractedMetadata> {
    let bin = if exiftool_path.trim().is_empty() {
        "exiftool"
    } else {
        exiftool_path.trim()
    };

    let output = Command::new(bin)
        .args(["-json", "-n", "-charset", "filename=UTF8", "--"])
        .arg(path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let arr: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;
    let obj = arr.first()?.as_object()?;

    let get_str = |k: &str| {
        obj.get(k)
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let get_f64 = |k: &str| obj.get(k).and_then(|v| v.as_f64());
    let get_i64 = |k: &str| {
        obj.get(k)
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64)))
    };

    let captured_at = get_str("DateTimeOriginal")
        .or_else(|| get_str("CreateDate"))
        .map(|s| normalize_exif_datetime(&s));

    let shutter_speed_s = get_f64("ExposureTime").or_else(|| get_f64("ShutterSpeed"));
    let shutter_speed_str = shutter_speed_s.map(|s| {
        if s > 0.0 && s < 1.0 {
            format!("1/{}", (1.0 / s).round() as u64)
        } else {
            format!("{s:.2}s")
        }
    });

    Some(ExtractedMetadata {
        extractor: "exiftool".to_string(),
        captured_at,
        captured_subsec: get_str("SubSecTimeOriginal"),
        camera_make: get_str("Make"),
        camera_model: get_str("Model"),
        camera_serial: get_str("SerialNumber").or_else(|| get_str("InternalSerialNumber")),
        lens_model_raw: get_str("LensModel").or_else(|| get_str("LensID")),
        lens_model: None,
        focal_length_mm: get_f64("FocalLength"),
        focal_length_35mm: get_f64("FocalLengthIn35mmFormat"),
        f_number: get_f64("FNumber"),
        shutter_speed_s,
        shutter_speed_str,
        iso: get_i64("ISO"),
        exposure_bias: get_f64("ExposureCompensation"),
        width: get_i64("ImageWidth"),
        height: get_i64("ImageHeight"),
        orientation: get_i64("Orientation"),
        gps_lat: get_f64("GPSLatitude"),
        gps_lon: get_f64("GPSLongitude"),
        duration_ms: get_f64("Duration").map(|s| (s * 1000.0).round() as i64),
        fps: get_f64("VideoFrameRate"),
        raw_json: serde_json::to_string(obj).ok(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_exif_datetime_format() {
        assert_eq!(
            normalize_exif_datetime("2026:11:24 14:05:09"),
            "2026-11-24T14:05:09"
        );
    }

    #[test]
    fn formats_shutter_speeds() {
        assert_eq!(format_shutter(1, 2000), "1/2000");
        assert_eq!(format_shutter(2, 1), "2s");
    }
}
