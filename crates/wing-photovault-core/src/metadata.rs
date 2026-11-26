use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use exif::{In, Tag, Value};

use crate::config::{ExifBackend, ExifConfig};
use crate::model::ExtractedMetadata;

/// Canon CR3 metadata UUID inside `moov` box: 85c0b687-820f-11e0-8111-f4ce462b6a48
const CANON_CR3_UUID: [u8; 16] = [
    0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48,
];

/// Hard upper bound on ISOBMFF box iterations to guard against malformed/malicious CR3 inputs.
const MAX_BOX_ITERATIONS: usize = 1024;

/// Hard upper bound on a single ExifTool JSON response (untrusted input safety bound).
const MAX_EXIFTOOL_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// Thread-safe extraction session that keeps a lazy persistent ExifTool (`-stay_open True`)
/// worker alive for legacy formats (old CR2 missing LensModel, CRW, videos, HEIF) while
/// serving modern JPEG/CR2/CR3 purely via lock-free Rust native parsing.
pub struct MetadataSession {
    cfg: ExifConfig,
    persistent: Mutex<Option<PersistentExifTool>>,
}

impl MetadataSession {
    pub fn new(cfg: ExifConfig) -> Self {
        Self {
            cfg,
            persistent: Mutex::new(None),
        }
    }

    pub fn extract(&self, path: &Path, ext: &str) -> Option<ExtractedMetadata> {
        let mut meta = match self.cfg.backend {
            ExifBackend::None => None,
            ExifBackend::Native => extract_native(path, ext),
            ExifBackend::ExifTool => self.extract_via_exiftool(path),
            ExifBackend::Auto => {
                let native = extract_native(path, ext);
                if needs_exiftool_fallback(ext, native.as_ref()) {
                    match self.extract_via_exiftool(path) {
                        Some(et) => Some(merge_metadata(et, native)),
                        None => native,
                    }
                } else {
                    native
                }
            }
        };

        if let Some(m) = meta.as_mut() {
            normalize_lens(m, &self.cfg.lens_aliases);
        }
        meta
    }

    pub fn extract_via_exiftool(&self, path: &Path) -> Option<ExtractedMetadata> {
        if !self.cfg.persistent_exiftool {
            return extract_exiftool(path, &self.cfg.exiftool_path);
        }
        let mut guard = self.persistent.lock().ok()?;
        if guard.is_none() {
            *guard = PersistentExifTool::spawn(&self.cfg.exiftool_path).ok();
        }
        if let Some(proc) = guard.as_mut() {
            if let Some(meta) = proc.extract(path) {
                return Some(meta);
            }
            // If the persistent process died or errored, reset and fall back once
            *guard = None;
        }
        extract_exiftool(path, &self.cfg.exiftool_path)
    }
}

fn needs_exiftool_fallback(ext: &str, native: Option<&ExtractedMetadata>) -> bool {
    let Some(m) = native else {
        return true;
    };
    if m.captured_at.is_none() || m.camera_model.is_none() {
        return true;
    }
    // Legacy Canon CR2 (e.g. EOS 20D) and CRW store lens info only in Canon MakerNote,
    // not in standard Exif 2.3 LensModel (0xA434).
    if matches!(ext, ".cr2" | ".crw") && m.lens_model_raw.is_none() {
        return true;
    }
    false
}

/// Prefer primary (ExifTool) fields while keeping any non-null fallback (native) fields.
fn merge_metadata(
    mut primary: ExtractedMetadata,
    fallback: Option<ExtractedMetadata>,
) -> ExtractedMetadata {
    let Some(fb) = fallback else {
        return primary;
    };
    if primary.captured_at.is_none() {
        primary.captured_at = fb.captured_at;
    }
    if primary.captured_subsec.is_none() {
        primary.captured_subsec = fb.captured_subsec;
    }
    if primary.camera_make.is_none() {
        primary.camera_make = fb.camera_make;
    }
    if primary.camera_model.is_none() {
        primary.camera_model = fb.camera_model;
    }
    if primary.camera_serial.is_none() {
        primary.camera_serial = fb.camera_serial;
    }
    if primary.lens_model_raw.is_none() {
        primary.lens_model_raw = fb.lens_model_raw;
    }
    if primary.focal_length_mm.is_none() {
        primary.focal_length_mm = fb.focal_length_mm;
    }
    if primary.f_number.is_none() {
        primary.f_number = fb.f_number;
    }
    if primary.shutter_speed_s.is_none() {
        primary.shutter_speed_s = fb.shutter_speed_s;
        primary.shutter_speed_str = fb.shutter_speed_str;
    }
    if primary.iso.is_none() {
        primary.iso = fb.iso;
    }
    primary
}

pub fn extract_metadata(path: &Path, ext: &str, cfg: &ExifConfig) -> Option<ExtractedMetadata> {
    let mut meta = match cfg.backend {
        ExifBackend::None => None,
        ExifBackend::Native => extract_native(path, ext),
        ExifBackend::ExifTool => extract_exiftool(path, &cfg.exiftool_path),
        ExifBackend::Auto => {
            let native = extract_native(path, ext);
            if needs_exiftool_fallback(ext, native.as_ref()) {
                match extract_exiftool(path, &cfg.exiftool_path) {
                    Some(et) => Some(merge_metadata(et, native)),
                    None => native,
                }
            } else {
                native
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

/// Fast Rust-native EXIF extraction for JPEG, CR2/TIFF, Canon CR3 (ISOBMFF CMT1/CMT2),
/// and Canon CRW via companion `.THM` (standard JPEG EXIF sidecar).
pub fn extract_native(path: &Path, ext: &str) -> Option<ExtractedMetadata> {
    match ext {
        ".cr3" => extract_cr3_native(path),
        ".crw" => extract_crw_companion_thm(path),
        ".jpg" | ".jpeg" | ".cr2" | ".dng" | ".nef" | ".arw" => extract_container_native(path),
        _ => None,
    }
}

/// Old Canon CRW cameras write a companion `.THM` file that is a standard EXIF JPEG.
fn extract_crw_companion_thm(crw_path: &Path) -> Option<ExtractedMetadata> {
    for ext_candidate in ["THM", "thm"] {
        let thm_path = crw_path.with_extension(ext_candidate);
        if thm_path.is_file() {
            if let Some(mut meta) = extract_container_native(&thm_path) {
                meta.extractor = "native-thm".to_string();
                return Some(meta);
            }
        }
    }
    None
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
                        if &box_type == b"CMT2" {
                            populate_from_cr3_cmt2(&exif, &mut meta);
                        } else {
                            populate_from_exif(&exif, &mut meta);
                        }
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

/// Canon CR3 stores ExifIFD tags directly in IFD0 of the `CMT2` TIFF box (without an ExifIFD pointer),
/// so `kamadak-exif` tags them with `Context::Tiff`. Match by raw EXIF tag number here.
fn populate_from_cr3_cmt2(exif: &exif::Exif, meta: &mut ExtractedMetadata) {
    for field in exif.fields() {
        if field.ifd_num != In::PRIMARY {
            continue;
        }
        match field.tag.number() {
            0x9003 => {
                // DateTimeOriginal takes precedence over CMT1 DateTime
                if let Some(dt) = value_as_ascii(&field.value) {
                    meta.captured_at = Some(normalize_exif_datetime(&dt));
                }
            }
            0x9004 if meta.captured_at.is_none() => {
                if let Some(dt) = value_as_ascii(&field.value) {
                    meta.captured_at = Some(normalize_exif_datetime(&dt));
                }
            }
            0x9291 => {
                if let Some(s) = value_as_ascii(&field.value) {
                    meta.captured_subsec = Some(s);
                }
            }
            0x9292 if meta.captured_subsec.is_none() => {
                if let Some(s) = value_as_ascii(&field.value) {
                    meta.captured_subsec = Some(s);
                }
            }
            0x829A => {
                if let Value::Rational(v) = &field.value {
                    if let Some(r) = v.first() {
                        if r.denom != 0 {
                            meta.shutter_speed_s = Some(r.num as f64 / r.denom as f64);
                            meta.shutter_speed_str = Some(format_shutter(r.num, r.denom));
                        }
                    }
                }
            }
            0x829D => {
                if let Value::Rational(v) = &field.value {
                    if let Some(r) = v.first() {
                        if r.denom != 0 {
                            meta.f_number = Some(r.num as f64 / r.denom as f64);
                        }
                    }
                }
            }
            0x8827 => {
                if let Some(iso) = field.value.get_uint(0) {
                    meta.iso = Some(iso as i64);
                }
            }
            0x9204 => {
                if let Value::SRational(v) = &field.value {
                    if let Some(r) = v.first() {
                        if r.denom != 0 {
                            meta.exposure_bias = Some(r.num as f64 / r.denom as f64);
                        }
                    }
                }
            }
            0x920A => {
                if let Value::Rational(v) = &field.value {
                    if let Some(r) = v.first() {
                        if r.denom != 0 {
                            meta.focal_length_mm = Some(r.num as f64 / r.denom as f64);
                        }
                    }
                }
            }
            0xA002 => {
                if let Some(w) = field.value.get_uint(0) {
                    meta.width = Some(w as i64);
                }
            }
            0xA003 => {
                if let Some(h) = field.value.get_uint(0) {
                    meta.height = Some(h as i64);
                }
            }
            0xA405 => {
                if let Some(fl35) = field.value.get_uint(0) {
                    meta.focal_length_35mm = Some(fl35 as f64);
                }
            }
            0xA431 => {
                if let Some(serial) = value_as_ascii(&field.value) {
                    meta.camera_serial = Some(serial);
                }
            }
            0xA434 => {
                if let Some(lens) = value_as_ascii(&field.value) {
                    meta.lens_model_raw = Some(lens);
                }
            }
            _ => {}
        }
    }
}

fn value_as_ascii(value: &Value) -> Option<String> {
    match value {
        Value::Ascii(items) => items
            .first()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map(|s| s.trim_matches('\0').trim().to_string())
            .filter(|s| !s.is_empty()),
        _ => None,
    }
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

/// Persistent ExifTool process using `-stay_open True -@ -` to avoid per-file process spawn overhead.
pub struct PersistentExifTool {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    seq: u64,
}

impl PersistentExifTool {
    pub fn spawn(exiftool_path: &str) -> std::io::Result<Self> {
        let bin = if exiftool_path.trim().is_empty() {
            "exiftool"
        } else {
            exiftool_path.trim()
        };

        let mut child = Command::new(bin)
            .args([
                "-stay_open",
                "True",
                "-@",
                "-",
                "-common_args",
                "-json",
                "-charset",
                "filename=UTF8",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("failed to capture exiftool stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("failed to capture exiftool stdout"))?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            seq: 0,
        })
    }

    pub fn extract(&mut self, path: &Path) -> Option<ExtractedMetadata> {
        let raw_str = path.to_str()?;
        // Reject paths containing newlines to prevent protocol injection into `-@ -` argfile stream.
        if raw_str.contains('\n') || raw_str.contains('\r') {
            return None;
        }

        // Guard against option injection without using `--` (which would swallow `-execute<seq>` in `-@ -` mode).
        let safe_path = if raw_str.starts_with('-') {
            format!("./{raw_str}")
        } else {
            raw_str.to_string()
        };

        self.seq = self.seq.wrapping_add(1);
        let marker = format!("{{ready{}}}", self.seq);

        writeln!(self.stdin, "{safe_path}").ok()?;
        writeln!(self.stdin, "-execute{}", self.seq).ok()?;
        self.stdin.flush().ok()?;

        let mut buf = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line).ok()?;
            if n == 0 {
                return None; // EOF: child exited unexpectedly
            }
            if line.trim_end_matches(&['\r', '\n'][..]) == marker {
                break;
            }
            if buf.len().saturating_add(line.len()) > MAX_EXIFTOOL_RESPONSE_BYTES {
                return None;
            }
            buf.extend_from_slice(line.as_bytes());
        }

        parse_exiftool_json(&buf, "exiftool-stay-open")
    }
}

impl Drop for PersistentExifTool {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "-stay_open\nFalse");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }
}

/// Fallback / reference extractor using one-shot ExifTool CLI (`exiftool -json`).
pub fn extract_exiftool(path: &Path, exiftool_path: &str) -> Option<ExtractedMetadata> {
    let bin = if exiftool_path.trim().is_empty() {
        "exiftool"
    } else {
        exiftool_path.trim()
    };

    let output = Command::new(bin)
        .args(["-json", "-charset", "filename=UTF8", "--"])
        .arg(path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    parse_exiftool_json(&output.stdout, "exiftool")
}

fn parse_exiftool_json(bytes: &[u8], extractor_name: &str) -> Option<ExtractedMetadata> {
    let arr: Vec<serde_json::Value> = serde_json::from_slice(bytes).ok()?;
    let obj = arr.first()?.as_object()?;

    let get_str = |k: &str| {
        obj.get(k)
            .and_then(|v| match v {
                serde_json::Value::String(s) => Some(s.trim().to_string()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .filter(|s| !s.is_empty())
    };
    let get_f64 = |k: &str| obj.get(k).and_then(value_to_f64);
    let get_i64 = |k: &str| {
        obj.get(k).and_then(|v| {
            v.as_i64()
                .or_else(|| value_to_f64(v).map(|f| f.round() as i64))
        })
    };

    let captured_at = get_str("DateTimeOriginal")
        .or_else(|| get_str("CreateDate"))
        .or_else(|| get_str("MediaCreateDate"))
        .map(|s| normalize_exif_datetime(&s));

    let (shutter_speed_s, shutter_speed_str) =
        parse_exiftool_shutter(obj.get("ExposureTime").or_else(|| obj.get("ShutterSpeed")));

    // Prefer LensModel, then LensID (decoded MakerNote name on EOS 20D/CRW), then Lens
    let lens_model_raw = get_str("LensModel")
        .or_else(|| get_str("LensID"))
        .or_else(|| get_str("Lens"))
        .filter(|s| !s.eq_ignore_ascii_case("Unknown") && !s.starts_with("Unknown ("));

    let duration_ms = obj.get("Duration").and_then(parse_duration_ms);

    Some(ExtractedMetadata {
        extractor: extractor_name.to_string(),
        captured_at,
        captured_subsec: get_str("SubSecTimeOriginal"),
        camera_make: get_str("Make"),
        camera_model: get_str("Model"),
        camera_serial: get_str("SerialNumber").or_else(|| get_str("InternalSerialNumber")),
        lens_model_raw,
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
        duration_ms,
        fps: get_f64("VideoFrameRate"),
        raw_json: serde_json::to_string(obj).ok(),
    })
}

fn value_to_f64(v: &serde_json::Value) -> Option<f64> {
    if let Some(f) = v.as_f64() {
        return Some(f);
    }
    let s = v.as_str()?.trim();
    // Parse leading numeric token (e.g. "400.0 mm", "+0.33", "29.97")
    let token = s.split_whitespace().next()?;
    if let Some((num, den)) = token.split_once('/') {
        let n: f64 = num.parse().ok()?;
        let d: f64 = den.parse().ok()?;
        if d != 0.0 {
            return Some(n / d);
        }
    }
    token.parse::<f64>().ok()
}

fn parse_exiftool_shutter(val: Option<&serde_json::Value>) -> (Option<f64>, Option<String>) {
    let Some(v) = val else {
        return (None, None);
    };
    if let Some(s) = v.as_str() {
        let trimmed = s.trim();
        let secs = value_to_f64(v);
        return (secs, Some(trimmed.to_string()));
    }
    if let Some(secs) = v.as_f64() {
        let label = if secs > 0.0 && secs < 1.0 {
            format!("1/{}", (1.0 / secs).round() as u64)
        } else {
            format!("{secs:.2}s")
        };
        return (Some(secs), Some(label));
    }
    (None, None)
}

fn parse_duration_ms(v: &serde_json::Value) -> Option<i64> {
    if let Some(secs) = v.as_f64() {
        return Some((secs * 1000.0).round() as i64);
    }
    let s = v.as_str()?.trim();
    // Format "12.34 s"
    if let Some(first) = s.split_whitespace().next() {
        if let Ok(secs) = first.parse::<f64>() {
            return Some((secs * 1000.0).round() as i64);
        }
    }
    // Format "H:MM:SS" or "MM:SS"
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() == 3 {
        let h: f64 = parts[0].parse().ok()?;
        let m: f64 = parts[1].parse().ok()?;
        let sec: f64 = parts[2].parse().ok()?;
        return Some(((h * 3600.0 + m * 60.0 + sec) * 1000.0).round() as i64);
    }
    None
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

    #[test]
    fn parses_cr3_cmt2_exif_tags_stored_in_ifd0() {
        // Minimal little-endian TIFF (CMT2 layout) with ISO (0x8827) and LensModel (0xA434) in IFD0
        let lens = b"RF200-800mm F6.3-9 IS USM\0";
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II"); // Little endian
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset = 8

        let num_entries: u16 = 2;
        tiff.extend_from_slice(&num_entries.to_le_bytes());
        // Value data offset = 8 + 2 + 2*12 + 4 = 38
        let lens_offset: u32 = 38;

        // Entry 1: ISO (0x8827), SHORT (3), count 1, value 2500
        tiff.extend_from_slice(&0x8827u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&2500u16.to_le_bytes());
        tiff.extend_from_slice(&0u16.to_le_bytes());

        // Entry 2: LensModel (0xA434), ASCII (2), count lens.len(), offset lens_offset
        tiff.extend_from_slice(&0xA434u16.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&(lens.len() as u32).to_le_bytes());
        tiff.extend_from_slice(&lens_offset.to_le_bytes());

        // Next IFD offset = 0
        tiff.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(tiff.len(), lens_offset as usize);
        tiff.extend_from_slice(lens);

        let mut cur = Cursor::new(tiff);
        let exif = exif::Reader::new().read_from_container(&mut cur).unwrap();
        let mut meta = ExtractedMetadata {
            extractor: "native-cr3".into(),
            ..Default::default()
        };
        populate_from_cr3_cmt2(&exif, &mut meta);

        assert_eq!(meta.iso, Some(2500));
        assert_eq!(
            meta.lens_model_raw.as_deref(),
            Some("RF200-800mm F6.3-9 IS USM")
        );
    }

    #[test]
    fn parses_exiftool_json_for_legacy_cr2_and_video() {
        let json = br#"[{
            "Make": "Canon",
            "Model": "Canon EOS 20D",
            "DateTimeOriginal": "2005:08:10 10:15:30",
            "LensID": "Canon EF 100-400mm f/4.5-5.6L IS USM",
            "FocalLength": "400.0 mm",
            "FNumber": 5.6,
            "ExposureTime": "1/1000",
            "ISO": 400,
            "Duration": "0:00:12"
        }]"#;
        let meta = parse_exiftool_json(json, "exiftool-stay-open").unwrap();
        assert_eq!(meta.camera_model.as_deref(), Some("Canon EOS 20D"));
        assert_eq!(
            meta.lens_model_raw.as_deref(),
            Some("Canon EF 100-400mm f/4.5-5.6L IS USM")
        );
        assert_eq!(meta.focal_length_mm, Some(400.0));
        assert_eq!(meta.f_number, Some(5.6));
        assert_eq!(meta.shutter_speed_str.as_deref(), Some("1/1000"));
        assert_eq!(meta.iso, Some(400));
        assert_eq!(meta.duration_ms, Some(12_000));
    }

    #[test]
    fn auto_fallback_triggers_for_legacy_cr2_missing_lens_but_not_complete_cr3() {
        let complete_cr3 = ExtractedMetadata {
            captured_at: Some("2026-11-22T11:58:45".into()),
            camera_model: Some("Canon EOS R5m2".into()),
            lens_model_raw: Some("RF200-800mm F6.3-9 IS USM".into()),
            ..Default::default()
        };
        assert!(!needs_exiftool_fallback(".cr3", Some(&complete_cr3)));

        let legacy_20d_cr2 = ExtractedMetadata {
            captured_at: Some("2005-08-10T10:15:30".into()),
            camera_model: Some("Canon EOS 20D".into()),
            lens_model_raw: None,
            ..Default::default()
        };
        assert!(needs_exiftool_fallback(".cr2", Some(&legacy_20d_cr2)));
        assert!(needs_exiftool_fallback(".crw", None));
    }
}
