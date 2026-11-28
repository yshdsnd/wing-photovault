use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui;

#[derive(Debug, Clone)]
pub struct FindParams {
    pub storage: String,
    pub ext_format: String,
    pub camera: String,
    pub lens: String,
    pub from: String,
    pub to: String,
    pub min_focal: String,
    pub max_focal: String,
    pub min_iso: String,
    pub path_contains: String,
    pub unbacked: bool,
    pub limit: String,
    pub cursor: String,
    pub page_info: bool,
    pub copy_to: String,
    pub with_companions: bool,
    pub copy_dry_run: bool,
    pub json_output: bool,
}

impl Default for FindParams {
    fn default() -> Self {
        Self {
            storage: String::new(),
            ext_format: String::new(),
            camera: String::new(),
            lens: String::new(),
            from: String::new(),
            to: String::new(),
            min_focal: String::new(),
            max_focal: String::new(),
            min_iso: String::new(),
            path_contains: String::new(),
            unbacked: false,
            limit: "100".to_string(),
            cursor: String::new(),
            page_info: false,
            copy_to: String::new(),
            with_companions: true,
            copy_dry_run: true,
            json_output: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScanParams {
    pub storage: String,
    pub full: bool,
    pub cooldown_every: usize,
    pub cooldown_secs: u64,
    pub max_files: String,
}

#[derive(Debug, Clone)]
pub struct IngestParams {
    pub source_dir: String,
    pub dst_storage_id: String,
    pub date_layout: String,
    pub subdir: String,
    pub include_sidecars: bool,
}

impl Default for IngestParams {
    fn default() -> Self {
        Self {
            source_dir: String::new(),
            dst_storage_id: String::new(),
            date_layout: "YYYY/YYYY_MM_DD".to_string(),
            subdir: String::new(),
            include_sidecars: false,
        }
    }
}

/// Detect a usable `wingpv` binary without spawning a shell.
pub fn detect_wingpv_binary() -> PathBuf {
    let exe_name = if cfg!(windows) { "wingpv.exe" } else { "wingpv" };

    if let Ok(cur_exe) = std::env::current_exe() {
        if let Some(dir) = cur_exe.parent() {
            let sibling = dir.join(exe_name);
            if sibling.is_file() {
                return sibling;
            }
            // Also check repo root target/{release,debug} when running from tools/wingpv-mini-ui/target/...
            for ancestor in dir.ancestors().take(6) {
                for profile in ["release", "debug"] {
                    let cand = ancestor.join("target").join(profile).join(exe_name);
                    if cand.is_file() {
                        return cand;
                    }
                }
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        for profile in ["release", "debug"] {
            let cand = cwd.join("target").join(profile).join(exe_name);
            if cand.is_file() {
                return cand;
            }
        }
    }

    PathBuf::from(exe_name)
}

/// Build safe argv vectors for `wingpv` invocations (never concatenated into a shell string).
pub fn build_simple_args(config_path: &Path, json: bool, subcommand: &str) -> Vec<String> {
    let mut args = vec![
        "--config".to_string(),
        config_path.to_string_lossy().into_owned(),
    ];
    if json {
        args.push("--format".to_string());
        args.push("json".to_string());
    }
    args.push(subcommand.to_string());
    args
}

pub fn build_normalize_lenses_args(config_path: &Path, json: bool, dry_run: bool) -> Vec<String> {
    let mut args = build_simple_args(config_path, json, "normalize-lenses");
    if dry_run {
        args.push("--dry-run".to_string());
    }
    args
}

pub fn build_link_derivatives_args(config_path: &Path, json: bool, dry_run: bool) -> Vec<String> {
    let mut args = build_simple_args(config_path, json, "link-derivatives");
    if dry_run {
        args.push("--dry-run".to_string());
    }
    args
}

pub fn build_verify_args(
    config_path: &Path,
    json: bool,
    dry_run: bool,
    limit: usize,
) -> Vec<String> {
    let mut args = build_simple_args(config_path, json, "verify");
    if dry_run {
        args.push("--dry-run".to_string());
    }
    if limit > 0 {
        args.push("--limit".to_string());
        args.push(limit.to_string());
    }
    args
}

pub fn build_ingest_args(config_path: &Path, params: &IngestParams, dry_run: bool) -> Vec<String> {
    let mut args = build_simple_args(config_path, false, "ingest");
    args.push(params.source_dir.trim().to_string());
    args.push("--to".to_string());
    args.push(params.dst_storage_id.trim().to_string());
    if !params.date_layout.trim().is_empty() {
        args.push("--date-layout".to_string());
        args.push(params.date_layout.trim().to_string());
    }
    if !params.subdir.trim().is_empty() {
        args.push("--subdir".to_string());
        args.push(params.subdir.trim().to_string());
    }
    if params.include_sidecars {
        args.push("--include-sidecars".to_string());
    }
    if dry_run {
        args.push("--dry-run".to_string());
    }
    args
}

#[allow(clippy::too_many_arguments)]
pub fn build_stats_args(
    config_path: &Path,
    json: bool,
    percent: bool,
    by_year: bool,
    gear_history: bool,
    cost_per_shot: bool,
    split_extenders: bool,
    focal_dist: bool,
    focal_35mm: bool,
) -> Vec<String> {
    let mut args = build_simple_args(config_path, json, "stats");
    if percent {
        args.push("--percent".to_string());
    }
    if by_year {
        args.push("--by-year".to_string());
    }
    if gear_history {
        args.push("--gear-history".to_string());
    }
    if cost_per_shot {
        args.push("--cost-per-shot".to_string());
    }
    if split_extenders {
        args.push("--split-extenders".to_string());
    }
    if focal_dist {
        args.push("--focal-dist".to_string());
    }
    if focal_35mm {
        args.push("--focal-35mm".to_string());
    }
    args
}

pub fn build_duplicates_args(
    config_path: &Path,
    json: bool,
    include_backups: bool,
    limit: usize,
) -> Vec<String> {
    let mut args = build_simple_args(config_path, json, "duplicates");
    if include_backups {
        args.push("--include-backups".to_string());
    }
    args.push("--limit".to_string());
    args.push(limit.to_string());
    args
}

pub fn build_find_args(config_path: &Path, params: &FindParams) -> Vec<String> {
    let mut args = build_simple_args(config_path, params.json_output, "find");
    if !params.storage.trim().is_empty() {
        args.push("--storage".to_string());
        args.push(params.storage.trim().to_string());
    }
    if !params.ext_format.trim().is_empty() {
        args.push("--ext-format".to_string());
        args.push(params.ext_format.trim().to_string());
    }
    if !params.camera.trim().is_empty() {
        args.push("--camera".to_string());
        args.push(params.camera.trim().to_string());
    }
    if !params.lens.trim().is_empty() {
        args.push("--lens".to_string());
        args.push(params.lens.trim().to_string());
    }
    if !params.from.trim().is_empty() {
        args.push("--from".to_string());
        args.push(params.from.trim().to_string());
    }
    if !params.to.trim().is_empty() {
        args.push("--to".to_string());
        args.push(params.to.trim().to_string());
    }
    if !params.min_focal.trim().is_empty() {
        args.push("--min-focal".to_string());
        args.push(params.min_focal.trim().to_string());
    }
    if !params.max_focal.trim().is_empty() {
        args.push("--max-focal".to_string());
        args.push(params.max_focal.trim().to_string());
    }
    if !params.min_iso.trim().is_empty() {
        args.push("--min-iso".to_string());
        args.push(params.min_iso.trim().to_string());
    }
    if !params.path_contains.trim().is_empty() {
        args.push("--path".to_string());
        args.push(params.path_contains.trim().to_string());
    }
    if params.unbacked {
        args.push("--unbacked".to_string());
    }
    if let Ok(n) = params.limit.trim().parse::<i64>() {
        if n > 0 {
            args.push("--limit".to_string());
            args.push(n.to_string());
        }
    }
    if !params.cursor.trim().is_empty() {
        args.push("--cursor".to_string());
        args.push(params.cursor.trim().to_string());
    }
    if params.page_info {
        args.push("--page-info".to_string());
    }
    if !params.copy_to.trim().is_empty() {
        args.push("--copy-to".to_string());
        args.push(params.copy_to.trim().to_string());
        if params.with_companions {
            args.push("--with-companions".to_string());
        }
        if params.copy_dry_run {
            args.push("--dry-run".to_string());
        }
    }
    args
}

pub fn build_scan_args(config_path: &Path, params: &ScanParams) -> Vec<String> {
    let mut args = build_simple_args(config_path, false, "scan");
    if !params.storage.trim().is_empty() {
        args.push("--storage".to_string());
        args.push(params.storage.trim().to_string());
    }
    if params.full {
        args.push("--full".to_string());
    }
    if params.cooldown_every > 0 && params.cooldown_secs > 0 {
        args.push("--cooldown-every".to_string());
        args.push(params.cooldown_every.to_string());
        args.push("--cooldown-secs".to_string());
        args.push(params.cooldown_secs.to_string());
    }
    if let Ok(n) = params.max_files.trim().parse::<usize>() {
        if n > 0 {
            args.push("--max-files".to_string());
            args.push(n.to_string());
        }
    }
    args
}

pub fn build_export_manifest_args(
    config_path: &Path,
    storage_id: &str,
    output_path: &Path,
) -> Vec<String> {
    let mut args = build_simple_args(config_path, false, "export-manifest");
    args.push("--storage".to_string());
    args.push(storage_id.trim().to_string());
    args.push("--output".to_string());
    args.push(output_path.to_string_lossy().into_owned());
    args
}

pub fn build_import_manifest_args(
    config_path: &Path,
    input_path: &Path,
    storage_override: &str,
) -> Vec<String> {
    let mut args = build_simple_args(config_path, false, "import-manifest");
    if !storage_override.trim().is_empty() {
        args.push("--storage".to_string());
        args.push(storage_override.trim().to_string());
    }
    // Guard positional path against option injection (e.g. filenames starting with '-')
    args.push("--".to_string());
    args.push(input_path.to_string_lossy().into_owned());
    args
}

const MAX_LOG_CHARS: usize = 256 * 1024;

#[derive(Default)]
struct SharedOutput {
    running: bool,
    command_line: String,
    stdout: String,
    stderr: String,
    exit_status: Option<String>,
    child: Option<Child>,
}

/// Non-blocking background runner for `wingpv` CLI commands.
pub struct CommandRunner {
    shared: Arc<Mutex<SharedOutput>>,
}

impl Default for CommandRunner {
    fn default() -> Self {
        Self {
            shared: Arc::new(Mutex::new(SharedOutput::default())),
        }
    }
}

impl CommandRunner {
    pub fn is_running(&self) -> bool {
        self.shared.lock().map(|s| s.running).unwrap_or(false)
    }

    pub fn snapshot(&self) -> (bool, String, String, String, Option<String>) {
        let guard = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        (
            guard.running,
            guard.command_line.clone(),
            guard.stdout.clone(),
            guard.stderr.clone(),
            guard.exit_status.clone(),
        )
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.shared.lock() {
            if !guard.running {
                guard.command_line.clear();
                guard.stdout.clear();
                guard.stderr.clear();
                guard.exit_status = None;
            }
        }
    }

    pub fn cancel(&self) {
        if let Ok(mut guard) = self.shared.lock() {
            if let Some(child) = guard.child.as_mut() {
                let _ = child.kill();
                append_bounded(&mut guard.stderr, "\n[cancelled by user]\n");
            }
        }
    }

    pub fn spawn(&self, bin_path: PathBuf, args: Vec<String>, ctx: egui::Context) {
        {
            let mut guard = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            if guard.running {
                return;
            }
            guard.running = true;
            guard.command_line = format!("{} {}", bin_path.display(), args.join(" "));
            guard.stdout.clear();
            guard.stderr.clear();
            guard.exit_status = None;
            guard.child = None;
        }

        let shared = Arc::clone(&self.shared);
        thread::spawn(move || {
            let mut cmd = Command::new(&bin_path);
            cmd.args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(err) => {
                    if let Ok(mut guard) = shared.lock() {
                        append_bounded(
                            &mut guard.stderr,
                            &format!(
                                "Failed to launch '{}': {err}\nCheck the wingpv binary path.\n",
                                bin_path.display()
                            ),
                        );
                        guard.running = false;
                        guard.exit_status = Some("spawn error".to_string());
                    }
                    ctx.request_repaint();
                    return;
                }
            };

            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            if let Ok(mut guard) = shared.lock() {
                guard.child = Some(child);
            }

            let shared_out = Arc::clone(&shared);
            let ctx_out = ctx.clone();
            let t_out = thread::spawn(move || {
                if let Some(pipe) = stdout {
                    stream_pipe(pipe, &shared_out, false, &ctx_out);
                }
            });

            let shared_err = Arc::clone(&shared);
            let ctx_err = ctx.clone();
            let t_err = thread::spawn(move || {
                if let Some(pipe) = stderr {
                    stream_pipe(pipe, &shared_err, true, &ctx_err);
                }
            });

            let _ = t_out.join();
            let _ = t_err.join();

            let status_text = {
                let mut child_opt = shared.lock().ok().and_then(|mut g| g.child.take());
                match child_opt.as_mut().map(Child::wait) {
                    Some(Ok(st)) => match st.code() {
                        Some(0) => "exit 0 (OK)".to_string(),
                        Some(code) => format!("exit {code}"),
                        None => "terminated".to_string(),
                    },
                    Some(Err(err)) => format!("wait error: {err}"),
                    None => "finished".to_string(),
                }
            };

            if let Ok(mut guard) = shared.lock() {
                guard.running = false;
                guard.exit_status = Some(status_text);
            }
            ctx.request_repaint();
        });
    }
}

fn stream_pipe<R: std::io::Read>(
    pipe: R,
    shared: &Arc<Mutex<SharedOutput>>,
    is_stderr: bool,
    ctx: &egui::Context,
) {
    let mut reader = BufReader::new(pipe);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let chunk = String::from_utf8_lossy(&buf);
                let normalized = chunk.replace('\r', "\n");
                if let Ok(mut guard) = shared.lock() {
                    if is_stderr {
                        append_bounded(&mut guard.stderr, &normalized);
                    } else {
                        append_bounded(&mut guard.stdout, &normalized);
                    }
                }
                ctx.request_repaint();
            }
            Err(_) => break,
        }
    }
}

fn append_bounded(target: &mut String, text: &str) {
    target.push_str(text);
    if target.len() > MAX_LOG_CHARS {
        let mut cut = target.len() - MAX_LOG_CHARS;
        while cut < target.len() && !target.is_char_boundary(cut) {
            cut += 1;
        }
        target.drain(..cut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_safe_cli_args_with_option_injection_guard() {
        let cfg = Path::new(r"C:\test\wing-photovault.toml");
        let import_args = build_import_manifest_args(cfg, Path::new("-tricky.jsonl"), "nas_1");
        assert_eq!(
            import_args,
            vec![
                "--config",
                r"C:\test\wing-photovault.toml",
                "import-manifest",
                "--storage",
                "nas_1",
                "--",
                "-tricky.jsonl",
            ]
        );

        let norm_args = build_normalize_lenses_args(cfg, true, true);
        assert_eq!(
            norm_args,
            vec![
                "--config",
                r"C:\test\wing-photovault.toml",
                "--format",
                "json",
                "normalize-lenses",
                "--dry-run",
            ]
        );

        let stats_args = build_stats_args(cfg, false, true, true, true, true, true, true, true);
        assert_eq!(
            stats_args,
            vec![
                "--config",
                r"C:\test\wing-photovault.toml",
                "stats",
                "--percent",
                "--by-year",
                "--gear-history",
                "--cost-per-shot",
                "--split-extenders",
                "--focal-dist",
                "--focal-35mm",
            ]
        );

        let dup_args = build_duplicates_args(cfg, true, true, 50);
        assert_eq!(
            dup_args,
            vec![
                "--config",
                r"C:\test\wing-photovault.toml",
                "--format",
                "json",
                "duplicates",
                "--include-backups",
                "--limit",
                "50",
            ]
        );

        let deriv_args = build_link_derivatives_args(cfg, false, true);
        assert_eq!(
            deriv_args,
            vec![
                "--config",
                r"C:\test\wing-photovault.toml",
                "link-derivatives",
                "--dry-run",
            ]
        );

        let verify_args = build_verify_args(cfg, false, true, 100);
        assert_eq!(
            verify_args,
            vec![
                "--config",
                r"C:\test\wing-photovault.toml",
                "verify",
                "--dry-run",
                "--limit",
                "100",
            ]
        );

        let ingest = IngestParams {
            source_dir: r"E:\DCIM".into(),
            dst_storage_id: "ssd_hot_g".into(),
            date_layout: "YYYY/YYYY_MM_DD".into(),
            subdir: "haneda".into(),
            include_sidecars: true,
        };
        let ingest_args = build_ingest_args(cfg, &ingest, true);
        assert_eq!(
            ingest_args,
            vec![
                "--config",
                r"C:\test\wing-photovault.toml",
                "ingest",
                r"E:\DCIM",
                "--to",
                "ssd_hot_g",
                "--date-layout",
                "YYYY/YYYY_MM_DD",
                "--subdir",
                "haneda",
                "--include-sidecars",
                "--dry-run",
            ]
        );

        let scan = ScanParams {
            storage: "ssd_hot_g".into(),
            cooldown_every: 500,
            cooldown_secs: 5,
            max_files: "1000".into(),
            ..Default::default()
        };
        let scan_args = build_scan_args(cfg, &scan);
        assert_eq!(
            scan_args,
            vec![
                "--config",
                r"C:\test\wing-photovault.toml",
                "scan",
                "--storage",
                "ssd_hot_g",
                "--cooldown-every",
                "500",
                "--cooldown-secs",
                "5",
                "--max-files",
                "1000",
            ]
        );
    }
}
