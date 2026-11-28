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
            apply_lens_aliases(m, &self.cfg.lens_aliases);
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
    // Legacy Canon CR2/CRW (e.g. EOS 10D/20D) and Canon EOS CR3/JPEG with 3rd-party or
    // legacy EF lenses (e.g. EOS 90D + Sigma 17-50mm, EOS 40D JPEG-only) often leave
    // standard Exif 2.3 LensModel (0xA434) empty while storing LensID in Canon MakerNote.
    // Restricting CR3/JPEG fallback to interchangeable-lens Canon EOS avoids needless
    // ExifTool overhead on fixed-lens compact cameras and smartphones.
    if m.lens_model_raw.is_none()
        && (matches!(ext, ".cr2" | ".crw")
            || (matches!(ext, ".cr3" | ".jpg" | ".jpeg")
                && m.camera_model
                    .as_deref()
                    .is_some_and(|c| c.starts_with("Canon EOS"))))
    {
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
        apply_lens_aliases(m, &cfg.lens_aliases);
    }
    meta
}

/// Resolve a raw lens string through `[exif.lens_aliases]`.
/// Returns `None` if `raw` is empty/whitespace-only; otherwise returns the trimmed alias target
/// (if configured and non-empty) or the trimmed raw string itself.
pub fn resolve_lens_alias(raw: &str, aliases: &HashMap<String, String>) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(mapped) = aliases.get(trimmed) {
        let mapped_trimmed = mapped.trim();
        if !mapped_trimmed.is_empty() {
            return Some(mapped_trimmed.to_string());
        }
    }
    Some(trimmed.to_string())
}

/// Apply `[exif.lens_aliases]` to extracted metadata without mutating non-empty `lens_model_raw`.
pub fn apply_lens_aliases(meta: &mut ExtractedMetadata, aliases: &HashMap<String, String>) {
    if let Some(raw) = meta.lens_model_raw.as_ref() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            meta.lens_model_raw = None;
            meta.lens_model = None;
            return;
        }
        meta.lens_model = resolve_lens_alias(trimmed, aliases);
    } else if let Some(existing) = meta.lens_model.as_ref() {
        meta.lens_model = resolve_lens_alias(existing, aliases);
    }
}

/// Split a combined lens + teleconverter string into `(parent_lens, Option<extender>)`.
///
/// Examples:
/// - `"RF100-500mm F4.5-7.1 L IS USM + EXTENDER RF1.4x"` -> `("RF100-500mm F4.5-7.1 L IS USM", Some("EXTENDER RF1.4x"))`
/// - `"EF100-400mm f/4.5-5.6L IS II USM +1.4x III"` -> `("EF100-400mm f/4.5-5.6L IS II USM", Some("EXTENDER EF1.4x III"))`
/// - `"60-600mm F4.5-6.3 DG OS HSM | Sports 018 +2x"` -> `("60-600mm F4.5-6.3 DG OS HSM | Sports 018", Some("Teleconverter 2x"))`
/// - `"RF24-70mm F2.8 L IS USM"` -> `("RF24-70mm F2.8 L IS USM", None)`
pub fn split_lens_extender(lens: &str) -> (String, Option<String>) {
    let trimmed = lens.trim();
    if trimmed.is_empty() {
        return (String::new(), None);
    }

    let split = trimmed
        .rsplit_once(" + ")
        .or_else(|| trimmed.rsplit_once(" +"));

    if let Some((left, right)) = split {
        let parent = left.trim();
        let ext_raw = right.trim();
        if !parent.is_empty() && !ext_raw.is_empty() && is_extender_token(ext_raw) {
            return (
                parent.to_string(),
                Some(normalize_extender_name(parent, ext_raw)),
            );
        }
    }

    (trimmed.to_string(), None)
}

fn is_extender_token(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    if lower.starts_with("extender") || lower.starts_with("teleconverter") {
        return true;
    }
    if let Some(rest) = lower.strip_prefix("tc") {
        if rest.is_empty()
            || rest.starts_with(' ')
            || rest.starts_with('-')
            || rest.starts_with(|c: char| c.is_ascii_digit())
        {
            return true;
        }
    }
    let mult_candidate = lower
        .strip_prefix("rf")
        .or_else(|| lower.strip_prefix("ef"))
        .unwrap_or(&lower)
        .trim_start();
    [
        "1.4x", "1.4×", "1.7x", "1.7×", "2x", "2.0x", "2×", "2.0×",
    ]
    .iter()
    .any(|prefix| mult_candidate.starts_with(prefix))
}

fn normalize_extender_name(parent: &str, ext_raw: &str) -> String {
    let lower = ext_raw.to_ascii_lowercase();
    if lower.starts_with("extender")
        || lower.starts_with("teleconverter")
        || lower.starts_with("tc")
    {
        return ext_raw.to_string();
    }
    if lower.starts_with("rf") || lower.starts_with("ef") {
        return format!("EXTENDER {ext_raw}");
    }

    let parent_upper = parent.to_ascii_uppercase();
    if parent_upper.starts_with("RF") || parent_upper.starts_with("CANON RF") {
        format!("EXTENDER RF{ext_raw}")
    } else if parent_upper.starts_with("EF") || parent_upper.starts_with("CANON EF") {
        format!("EXTENDER EF{ext_raw}")
    } else {
        format!("Teleconverter {ext_raw}")
    }
}

/// Source used to determine a 35mm-equivalent focal length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focal35Source {
    /// Explicitly configured in `[stats.camera_crop_factors]`.
    ConfigCrop,
    /// Read directly from EXIF `FocalLengthIn35mmFilm` (`0xA405`).
    Exif35mm,
    /// Derived from built-in camera sensor crop-factor lookup.
    BuiltinCrop,
}

impl Focal35Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Focal35Source::ConfigCrop => "config",
            Focal35Source::Exif35mm => "exif",
            Focal35Source::BuiltinCrop => "builtin",
        }
    }
}

/// Result of resolving an asset's 35mm-equivalent focal length.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Focal35Resolution {
    pub focal_35mm: f64,
    pub crop_factor: f64,
    pub source: Focal35Source,
}

/// Returns `true` when `crop_factor` represents a 35mm full-frame sensor (~1.0x).
pub fn is_full_frame_crop(crop_factor: f64) -> bool {
    (crop_factor - 1.0).abs() < 0.02
}

/// Resolve an asset's 35mm-equivalent focal length.
///
/// Priority (Fail Closed — unknown cameras without EXIF 35mm return `None` rather than guessing `1.0x`):
/// 1. Explicit `[stats.camera_crop_factors]` override (exact match, then case-insensitive match).
/// 2. EXIF `focal_length_35mm` when present and positive.
/// 3. Built-in sensor crop factor table (`infer_camera_crop_factor`).
pub fn resolve_focal_length_35mm(
    focal_mm: f64,
    exif_focal_35mm: Option<f64>,
    camera_model: Option<&str>,
    custom_crop_factors: &HashMap<String, f64>,
) -> Option<Focal35Resolution> {
    if !focal_mm.is_finite() || focal_mm <= 0.0 {
        return None;
    }

    if let Some(cam) = camera_model.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(&factor) = custom_crop_factors.get(cam).or_else(|| {
            let lower = cam.to_ascii_lowercase();
            custom_crop_factors
                .iter()
                .find(|(k, _)| k.trim().to_ascii_lowercase() == lower)
                .map(|(_, v)| v)
        }) {
            if factor.is_finite() && factor > 0.0 {
                return Some(Focal35Resolution {
                    focal_35mm: focal_mm * factor,
                    crop_factor: factor,
                    source: Focal35Source::ConfigCrop,
                });
            }
        }
    }

    if let Some(f35) = exif_focal_35mm {
        if f35.is_finite() && f35 > 0.0 {
            return Some(Focal35Resolution {
                focal_35mm: f35,
                crop_factor: f35 / focal_mm,
                source: Focal35Source::Exif35mm,
            });
        }
    }

    if let Some(cam) = camera_model {
        if let Some(factor) = infer_camera_crop_factor(cam) {
            return Some(Focal35Resolution {
                focal_35mm: focal_mm * factor,
                crop_factor: factor,
                source: Focal35Source::BuiltinCrop,
            });
        }
    }

    None
}

fn has_prefix_boundary(s: &str, prefix: &str) -> bool {
    if let Some(rest) = s.strip_prefix(prefix) {
        rest.is_empty()
            || rest.starts_with(' ')
            || rest.starts_with('-')
            || rest.starts_with('_')
            || rest.starts_with('/')
    } else {
        false
    }
}

/// Infer standard 35mm crop factor from a camera model string when EXIF `FocalLengthIn35mmFilm`
/// is missing (notably Canon EOS cameras, which never populate EXIF tag `0xA405`).
pub fn infer_camera_crop_factor(camera_model: &str) -> Option<f64> {
    let norm: String = camera_model
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase();
    if norm.is_empty() {
        return None;
    }

    // 1. Canon EOS bodies
    if let Some(eos_raw) = norm
        .strip_prefix("CANON EOS ")
        .or_else(|| norm.strip_prefix("CANON EOS-"))
        .or_else(|| norm.strip_prefix("EOS "))
        .or_else(|| norm.strip_prefix("EOS-"))
    {
        let eos = eos_raw.trim();

        // Canon EOS APS-C (1.6x) — check multi-digit R7/R10/R50/R100 before R/R1/R5/R6
        const CANON_APSC_PREFIXES: &[&str] = &[
            "R7", "R10", "R50", "R100", "7D", "10D", "20D", "20DA", "30D", "40D", "50D", "60D",
            "60DA", "70D", "77D", "80D", "90D", "D30", "D60", "100D", "200D", "250D", "300D",
            "350D", "400D", "450D", "500D", "550D", "600D", "650D", "700D", "750D", "760D", "800D",
            "850D", "1000D", "1100D", "1200D", "1300D", "1500D", "2000D", "3000D", "4000D",
            "KISS", "REBEL", "DIGITAL REBEL", "M", "M2", "M3", "M5", "M6", "M10", "M50", "M100",
            "M200",
        ];
        if CANON_APSC_PREFIXES
            .iter()
            .any(|p| has_prefix_boundary(eos, p))
        {
            return Some(1.6);
        }

        // Canon EOS Full Frame (1.0x)
        const CANON_FF_PREFIXES: &[&str] = &[
            "R", "RP", "R1", "R3", "R5", "R5M2", "R5C", "R6", "R6M2", "R8", "5D", "5DS", "6D",
            "1DS", "1D X", "1DX", "1D C", "1DC",
        ];
        if CANON_FF_PREFIXES
            .iter()
            .any(|p| has_prefix_boundary(eos, p))
        {
            return Some(1.0);
        }

        // Canon EOS-1D (APS-H 1.3x: 1D, 1D Mark II/IIN/III/IV)
        if has_prefix_boundary(eos, "1D") {
            return Some(1.3);
        }
    }

    // 2. Nikon bodies
    if let Some(nk) = norm.strip_prefix("NIKON ").map(str::trim) {
        const NIKON_APSC: &[&str] = &[
            "Z 30", "Z30", "Z 50", "Z50", "Z FC", "ZFC", "D1", "D1H", "D1X", "D2H", "D2HS", "D2X",
            "D2XS", "D40", "D40X", "D50", "D60", "D70", "D70S", "D80", "D90", "D100", "D200",
            "D300", "D300S", "D500", "D3000", "D3100", "D3200", "D3300", "D3400", "D3500", "D5000",
            "D5100", "D5200", "D5300", "D5500", "D5600", "D7000", "D7100", "D7200", "D7500",
        ];
        if NIKON_APSC.iter().any(|p| has_prefix_boundary(nk, p)) {
            return Some(1.5);
        }
        const NIKON_FF: &[&str] = &[
            "Z 5", "Z5", "Z 6", "Z6", "Z 6_2", "Z6_2", "Z 6_3", "Z6_3", "Z 7", "Z7", "Z 7_2",
            "Z7_2", "Z 8", "Z8", "Z 9", "Z9", "Z F", "ZF", "D3", "D3S", "D3X", "D4", "D4S", "D5",
            "D6", "D600", "D610", "D700", "D750", "D780", "D800", "D800E", "D810", "D810A", "D850",
            "DF",
        ];
        if NIKON_FF.iter().any(|p| has_prefix_boundary(nk, p)) {
            return Some(1.0);
        }
    }

    // 3. Sony Alpha / NEX
    if norm.starts_with("ILCE-6")
        || norm.starts_with("ILCE-5")
        || norm.starts_with("ILCE-3")
        || norm.starts_with("NEX-")
        || norm.starts_with("ZV-E10")
        || norm.starts_with("ILME-FX30")
    {
        return Some(1.5);
    }
    if norm.starts_with("ILCE-7")
        || norm.starts_with("ILCE-9")
        || norm.starts_with("ILCE-1")
        || has_prefix_boundary(&norm, "ZV-E1")
        || norm.starts_with("DSC-RX1")
        || has_prefix_boundary(&norm, "DSLR-A850")
        || has_prefix_boundary(&norm, "DSLR-A900")
        || norm.starts_with("SLT-A99")
    {
        return Some(1.0);
    }

    // 4. Fujifilm / Medium Format / Micro Four Thirds / Leica / Panasonic FF
    if norm.starts_with("GFX") || norm.starts_with("X1D") || norm.starts_with("X2D") {
        return Some(0.79);
    }
    if norm == "MAMIYA ZD" {
        return Some(0.72);
    }
    if norm.starts_with("X-T")
        || norm.starts_with("X-H")
        || norm.starts_with("X-PRO")
        || norm.starts_with("X-E")
        || norm.starts_with("X-S")
        || norm.starts_with("X-M")
        || norm.starts_with("X100")
        || norm.starts_with("GR III")
        || norm == "GR II"
        || norm == "GR"
    {
        return Some(1.5);
    }
    if norm.starts_with("OM-1")
        || norm.starts_with("OM-5")
        || norm.starts_with("E-M1")
        || norm.starts_with("E-M5")
        || norm.starts_with("E-M10")
        || norm.starts_with("PEN-F")
        || norm.starts_with("E-PL")
        || norm.starts_with("DC-G9")
        || norm.starts_with("DC-GH")
        || norm.starts_with("DMC-G")
        || norm.starts_with("DMC-GH")
        || norm.starts_with("DMC-GX")
        || norm.starts_with("DMC-GM")
        || norm.starts_with("DMC-GF")
    {
        return Some(2.0);
    }
    if norm.starts_with("DC-S1")
        || norm.starts_with("DC-S5")
        || has_prefix_boundary(&norm, "DC-S9")
        || norm.starts_with("LEICA M")
        || norm.starts_with("LEICA SL")
        || norm.starts_with("LEICA Q")
        || norm.starts_with("SIGMA FP")
    {
        return Some(1.0);
    }

    // 5. Notable fixed-lens compacts (fallback only when EXIF FocalLengthIn35mmFilm is absent)
    match norm.as_str() {
        "DMC-FZ1" => Some(7.61),
        "DIMAGE X" => Some(6.49),
        "CANON IXY DIGITAL 910 IS" | "CANON POWERSHOT SD870 IS" | "CANON DIGITAL IXUS 860 IS" => {
            Some(6.08)
        }
        "CANON DIGITAL IXUS 40" | "CANON IXY DIGITAL 50" | "CANON POWERSHOT SD300" => Some(6.03),
        "CANON IXY DIGITAL 500" | "CANON POWERSHOT S500" | "CANON DIGITAL IXUS 500" => Some(4.86),
        "CANON POWERSHOT G3" => Some(4.87),
        "FINEPIX F30" => Some(4.50),
        "FINEPIX4500" => Some(4.14),
        "DSC-F828" => Some(3.93),
        "DSC-P150" => Some(4.81),
        _ => None,
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

        // EOS 90D + 3rd-party lens (empty LensModel in CR3/JPG) and legacy EOS 40D JPEG-only
        let eos_90d_sigma = ExtractedMetadata {
            captured_at: Some("2019-10-13T10:00:00".into()),
            camera_model: Some("Canon EOS 90D".into()),
            lens_model_raw: None,
            ..Default::default()
        };
        assert!(needs_exiftool_fallback(".cr3", Some(&eos_90d_sigma)));
        assert!(needs_exiftool_fallback(".jpg", Some(&eos_90d_sigma)));

        // Fixed-lens compact camera / smartphone JPEG must NOT fall back to ExifTool
        let compact_jpg = ExtractedMetadata {
            captured_at: Some("2015-05-01T12:00:00".into()),
            camera_model: Some("Canon PowerShot G7 X".into()),
            lens_model_raw: None,
            ..Default::default()
        };
        assert!(!needs_exiftool_fallback(".jpg", Some(&compact_jpg)));
    }

    #[test]
    fn exiftool_json_falls_back_to_lens_id_when_lens_model_is_empty() {
        let json = br#"[{
            "Model": "Canon EOS 90D",
            "DateTimeOriginal": "2019:10:13 11:20:00",
            "LensModel": "",
            "LensID": "Sigma 17-50mm f/2.8 EX DC OS HSM",
            "FocalLength": "33.0 mm",
            "FNumber": 5.6,
            "ExposureTime": "1/80",
            "ISO": 200
        }]"#;
        let meta = parse_exiftool_json(json, "exiftool-stay-open").unwrap();
        assert_eq!(
            meta.lens_model_raw.as_deref(),
            Some("Sigma 17-50mm f/2.8 EX DC OS HSM")
        );
    }

    #[test]
    fn resolves_and_applies_lens_aliases_preserving_raw() {
        let mut aliases = HashMap::new();
        aliases.insert(
            "Unknown 100-400mm".to_string(),
            "EF100-400mm f/4.5-5.6L IS USM".to_string(),
        );
        aliases.insert(
            "15-30mm".to_string(),
            "  SIGMA 15-30mm F3.5-4.5 EX DG ASPHERICAL  ".to_string(),
        );

        assert_eq!(
            resolve_lens_alias("  Unknown 100-400mm ", &aliases).as_deref(),
            Some("EF100-400mm f/4.5-5.6L IS USM")
        );
        assert_eq!(
            resolve_lens_alias("15-30mm", &aliases).as_deref(),
            Some("SIGMA 15-30mm F3.5-4.5 EX DG ASPHERICAL")
        );
        assert_eq!(
            resolve_lens_alias("RF200-800mm F6.3-9 IS USM", &aliases).as_deref(),
            Some("RF200-800mm F6.3-9 IS USM")
        );
        assert_eq!(resolve_lens_alias("   ", &aliases), None);

        let mut meta = ExtractedMetadata {
            lens_model_raw: Some("Unknown 100-400mm".into()),
            lens_model: Some("Unknown 100-400mm".into()),
            ..Default::default()
        };
        apply_lens_aliases(&mut meta, &aliases);
        assert_eq!(meta.lens_model_raw.as_deref(), Some("Unknown 100-400mm"));
        assert_eq!(
            meta.lens_model.as_deref(),
            Some("EF100-400mm f/4.5-5.6L IS USM")
        );

        // Removing the alias reverts lens_model back to lens_model_raw
        apply_lens_aliases(&mut meta, &HashMap::new());
        assert_eq!(meta.lens_model_raw.as_deref(), Some("Unknown 100-400mm"));
        assert_eq!(meta.lens_model.as_deref(), Some("Unknown 100-400mm"));
    }

    #[test]
    fn splits_lens_and_extender_variants_cleanly() {
        assert_eq!(
            split_lens_extender("RF100-500mm F4.5-7.1 L IS USM + EXTENDER RF1.4x"),
            (
                "RF100-500mm F4.5-7.1 L IS USM".to_string(),
                Some("EXTENDER RF1.4x".to_string())
            )
        );
        assert_eq!(
            split_lens_extender("RF200-800mm F6.3-9 IS USM + EXTENDER RF2x"),
            (
                "RF200-800mm F6.3-9 IS USM".to_string(),
                Some("EXTENDER RF2x".to_string())
            )
        );
        assert_eq!(
            split_lens_extender("EF100-400mm f/4.5-5.6L IS II USM +1.4x III"),
            (
                "EF100-400mm f/4.5-5.6L IS II USM".to_string(),
                Some("EXTENDER EF1.4x III".to_string())
            )
        );
        assert_eq!(
            split_lens_extender("EF100-400mm f/4.5-5.6L IS II USM +2x III"),
            (
                "EF100-400mm f/4.5-5.6L IS II USM".to_string(),
                Some("EXTENDER EF2x III".to_string())
            )
        );
        assert_eq!(
            split_lens_extender("EF100-400mm f/4.5-5.6L IS USM +1.4x"),
            (
                "EF100-400mm f/4.5-5.6L IS USM".to_string(),
                Some("EXTENDER EF1.4x".to_string())
            )
        );
        assert_eq!(
            split_lens_extender("60-600mm F4.5-6.3 DG OS HSM | Sports 018 +2x"),
            (
                "60-600mm F4.5-6.3 DG OS HSM | Sports 018".to_string(),
                Some("Teleconverter 2x".to_string())
            )
        );
        assert_eq!(
            split_lens_extender("RF24-70mm F2.8 L IS USM"),
            ("RF24-70mm F2.8 L IS USM".to_string(), None)
        );
        assert_eq!(
            split_lens_extender("Some Lens + Hood"),
            ("Some Lens + Hood".to_string(), None)
        );
    }

    #[test]
    fn infers_camera_crop_factors_and_resolves_35mm_equivalent() {
        // Canon Full Frame (1.0x)
        assert_eq!(infer_camera_crop_factor("Canon EOS R5m2"), Some(1.0));
        assert_eq!(infer_camera_crop_factor("Canon EOS R6m2"), Some(1.0));
        assert_eq!(infer_camera_crop_factor("Canon EOS 6D Mark II"), Some(1.0));
        assert_eq!(infer_camera_crop_factor("Canon EOS R"), Some(1.0));
        assert_eq!(infer_camera_crop_factor("Canon EOS-1Ds Mark III"), Some(1.0));

        // Canon APS-C (1.6x) & APS-H (1.3x)
        assert_eq!(infer_camera_crop_factor("Canon EOS R7"), Some(1.6));
        assert_eq!(infer_camera_crop_factor("Canon EOS R10"), Some(1.6));
        assert_eq!(infer_camera_crop_factor("Canon EOS 7D Mark II"), Some(1.6));
        assert_eq!(infer_camera_crop_factor("Canon EOS 90D"), Some(1.6));
        assert_eq!(infer_camera_crop_factor("Canon EOS 10D"), Some(1.6));
        assert_eq!(infer_camera_crop_factor("Canon EOS-1D Mark II"), Some(1.3));

        // Nikon / Sony / MFT / Medium Format / Compacts
        assert_eq!(infer_camera_crop_factor("NIKON D3"), Some(1.0));
        assert_eq!(infer_camera_crop_factor("NIKON D80"), Some(1.5));
        assert_eq!(infer_camera_crop_factor("NIKON Z 50"), Some(1.5));
        assert_eq!(infer_camera_crop_factor("NIKON Z 8"), Some(1.0));
        assert_eq!(infer_camera_crop_factor("ILCE-7"), Some(1.0));
        assert_eq!(infer_camera_crop_factor("ILCE-6700"), Some(1.5));
        assert_eq!(infer_camera_crop_factor("OM-1"), Some(2.0));
        assert_eq!(infer_camera_crop_factor("MAMIYA ZD"), Some(0.72));
        assert_eq!(infer_camera_crop_factor("DiMAGE X"), Some(6.49));
        assert_eq!(infer_camera_crop_factor("Unknown Mystery Cam"), None);

        // Priority: ConfigCrop > Exif35mm > BuiltinCrop
        let empty = HashMap::new();
        let r7 = resolve_focal_length_35mm(500.0, None, Some("Canon EOS R7"), &empty).unwrap();
        assert_eq!(r7.source, Focal35Source::BuiltinCrop);
        assert!((r7.focal_35mm - 800.0).abs() < 1e-6);
        assert!(!is_full_frame_crop(r7.crop_factor));

        let fz1 = resolve_focal_length_35mm(55.2, Some(420.0), Some("DMC-FZ1"), &empty).unwrap();
        assert_eq!(fz1.source, Focal35Source::Exif35mm);
        assert!((fz1.focal_35mm - 420.0).abs() < 1e-6);

        let mut custom = HashMap::new();
        custom.insert("Unknown Mystery Cam".to_string(), 2.7);
        let cust = resolve_focal_length_35mm(10.0, Some(30.0), Some("Unknown Mystery Cam"), &custom)
            .unwrap();
        assert_eq!(cust.source, Focal35Source::ConfigCrop);
        assert!((cust.focal_35mm - 27.0).abs() < 1e-6);

        // Fail Closed: unknown camera without EXIF 35mm returns None
        assert!(resolve_focal_length_35mm(10.0, None, Some("Unknown Mystery Cam"), &empty).is_none());
    }
}
