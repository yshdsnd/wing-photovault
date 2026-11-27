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
    pub limit: String,
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
            limit: "100".to_string(),
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

pub fn build_stats_args(config_path: &Path, json: bool, percent: bool) -> Vec<String> {
    let mut args = build_simple_args(config_path, json, "stats");
    if percent {
        args.push("--percent".to_string());
    }
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
    if let Ok(n) = params.limit.trim().parse::<i64>() {
        if n > 0 {
            args.push("--limit".to_string());
            args.push(n.to_string());
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
