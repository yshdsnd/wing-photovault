#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use rayon::prelude::*;
use serde::Serialize;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use wing_photovault_core::config::{Config, ExifBackend, StorageConfig};
use wing_photovault_core::hashing::hash_file;
use wing_photovault_core::metadata::{extract_exiftool, extract_metadata, extract_native};
use wing_photovault_core::model::{
    current_host_os, AccessMode, BackupMode, HashAlgorithm, MediaType, ScanManifestRecord,
    StorageStatus, StorageTier,
};
use wing_photovault_core::scanner::{discover_storage_files, DiscoveredFile};
use wing_photovault_db::{Database, FindFilter, ScanRunSummary, UpsertFileInput};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
    Jsonl,
}

#[derive(Debug, Parser)]
#[command(
    name = "wingpv",
    version,
    about = "WING PhotoVault CLI — fast, safe photo asset indexing and management"
)]
struct Cli {
    /// Path to TOML configuration file
    #[arg(long, global = true, default_value = "wing-photovault.toml")]
    config: PathBuf,

    /// Output format
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,

    /// Enable verbose logging on stderr
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Suppress informational logs on stderr
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Initialize SQLite database and register configured storages
    Init {
        /// Create a starter config file if it does not exist
        #[arg(long)]
        write_config: bool,
    },

    /// Non-destructively scan storages and update the index database
    Scan {
        /// Scan only a specific storage ID
        #[arg(long)]
        storage: Option<String>,

        /// Re-hash and re-extract metadata even for unchanged files
        #[arg(long)]
        full: bool,

        /// Stream scanned records as JSONL to stdout (for NAS remote scan workflow)
        #[arg(long)]
        export_jsonl: bool,
    },

    /// Show database and storage status
    Status,

    /// Show asset, content, format, camera, lens, and backup statistics
    Stats,

    /// Search indexed files
    Find {
        #[arg(long)]
        storage: Option<String>,
        #[arg(long = "ext-format")]
        ext_format: Option<String>,
        #[arg(long)]
        camera: Option<String>,
        #[arg(long)]
        lens: Option<String>,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: i64,
    },

    /// Benchmark scanning, BLAKE3 vs SHA-256 hashing, and EXIF extraction on a directory
    Benchmark {
        /// Target directory to benchmark (read-only)
        path: PathBuf,

        /// Maximum number of files to process
        #[arg(long, default_value_t = 500)]
        limit: usize,

        /// Also benchmark ExifTool CLI for comparison
        #[arg(long)]
        with_exiftool: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose, cli.quiet);

    match cli.command {
        Commands::Init { write_config } => cmd_init(&cli.config, write_config, cli.format),
        Commands::Scan {
            storage,
            full,
            export_jsonl,
        } => cmd_scan(&cli.config, storage.as_deref(), full, export_jsonl, cli.format),
        Commands::Status => cmd_status(&cli.config, cli.format),
        Commands::Stats => cmd_stats(&cli.config, cli.format),
        Commands::Find {
            storage,
            ext_format,
            camera,
            lens,
            from,
            to,
            limit,
        } => cmd_find(
            &cli.config,
            FindFilter {
                storage_id: storage,
                format: ext_format,
                camera,
                lens,
                from_date: from,
                to_date: to,
                limit: Some(limit),
            },
            cli.format,
        ),
        Commands::Benchmark {
            path,
            limit,
            with_exiftool,
        } => cmd_benchmark(&cli.config, &path, limit, with_exiftool, cli.format),
    }
}

fn init_logging(verbose: bool, quiet: bool) {
    let level = if quiet {
        "error"
    } else if verbose {
        "debug"
    } else {
        "info"
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn load_config_or_default(path: &Path) -> Result<Config> {
    if path.exists() {
        Config::load(path).with_context(|| format!("failed to load config: {}", path.display()))
    } else {
        Ok(Config {
            host: Default::default(),
            general: Default::default(),
            scan: Default::default(),
            hash: Default::default(),
            performance: Default::default(),
            exif: Default::default(),
            storages: Vec::new(),
        })
    }
}

fn cmd_init(config_path: &Path, write_config: bool, format: OutputFormat) -> Result<()> {
    if write_config && !config_path.exists() {
        let example = include_str!("../../../config.example.toml");
        std::fs::write(config_path, example)
            .with_context(|| format!("failed to write {}", config_path.display()))?;
        info!(path = %config_path.display(), "wrote starter config");
    }

    let cfg = load_config_or_default(config_path)?;
    let db = Database::open(&cfg.general.db_path)
        .with_context(|| format!("failed to open DB: {}", cfg.general.db_path.display()))?;

    db.upsert_host(&cfg.host.id, &cfg.host.name, current_host_os())?;

    for s in &cfg.storages {
        let status = if s.root.exists() {
            StorageStatus::Online
        } else {
            StorageStatus::Offline
        };
        db.upsert_storage(s, &cfg.host.id, status)?;
    }

    #[derive(Serialize)]
    struct InitResult {
        db_path: String,
        storages_registered: usize,
    }

    let res = InitResult {
        db_path: cfg.general.db_path.to_string_lossy().into_owned(),
        storages_registered: cfg.storages.len(),
    };

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            println!("{}", serde_json::to_string_pretty(&res)?);
        }
        OutputFormat::Text => {
            println!("Initialized database: {}", res.db_path);
            println!("Registered storages: {}", res.storages_registered);
        }
    }
    Ok(())
}

fn cmd_scan(
    config_path: &Path,
    storage_filter: Option<&str>,
    full: bool,
    export_jsonl: bool,
    format: OutputFormat,
) -> Result<()> {
    let cfg = Config::load(config_path)
        .with_context(|| format!("failed to load config: {}", config_path.display()))?;
    let mut db = Database::open(&cfg.general.db_path)?;
    db.upsert_host(&cfg.host.id, &cfg.host.name, current_host_os())?;
    let allowed_exts = cfg.normalized_extensions();

    let targets: Vec<&StorageConfig> = cfg
        .storages
        .iter()
        .filter(|s| storage_filter.is_none_or(|id| s.id == id))
        .collect();

    if targets.is_empty() {
        anyhow::bail!("no matching storages found in {}", config_path.display());
    }

    let mut summaries = Vec::new();
    let mode = if full || !cfg.scan.incremental {
        "full"
    } else {
        "incremental"
    };

    for storage in targets {
        if !storage.root.exists() {
            warn!(
                storage_id = %storage.id,
                root = %storage.root.display(),
                "storage root is offline/inaccessible; skipping without marking files missing"
            );
            db.upsert_storage(storage, &cfg.host.id, StorageStatus::Offline)?;
            continue;
        }

        db.upsert_storage(storage, &cfg.host.id, StorageStatus::Online)?;
        let scan_run_id = db.start_scan_run(&storage.id, mode)?;
        let started = Instant::now();

        let existing = db.load_storage_file_index(&storage.id)?;
        let discovery = discover_storage_files(storage, &allowed_exts, cfg.scan.follow_symlinks);
        let mut summary = ScanRunSummary {
            scan_run_id,
            storage_id: storage.id.clone(),
            mode: mode.to_string(),
            status: "completed".to_string(),
            files_scanned: discovery.files.len() as u64,
            error_count: discovery.errors.len() as u64,
            ..Default::default()
        };

        for err in &discovery.errors {
            warn!(storage_id = %storage.id, error = %err, "discovery error");
        }

        let mut unchanged_ids: Vec<i64> = Vec::new();
        let mut to_process: Vec<DiscoveredFile> = Vec::new();

        for f in discovery.files {
            if mode == "incremental" {
                if let Some(prev) = existing.get(&f.rel_path) {
                    if prev.byte_size == f.byte_size
                        && prev.mtime_ns == f.mtime_ns
                        && prev.content_id.is_some()
                    {
                        unchanged_ids.push(prev.file_id);
                        continue;
                    }
                }
            }
            to_process.push(f);
        }

        summary.files_skipped = unchanged_ids.len() as u64;
        db.touch_unchanged_files(&unchanged_ids, scan_run_id)?;

        let workers = cfg.performance.workers_for(storage.media);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .context("failed to build worker pool")?;

        let batch_size = cfg.performance.db_batch_size.max(1);
        let role = storage.tier.default_file_role();

        for chunk in to_process.chunks(batch_size) {
            let results: Vec<Result<(UpsertFileInput, u64)>> = pool.install(|| {
                chunk
                    .par_iter()
                    .map(|f| {
                        let hash = hash_file(
                            &f.abs_path,
                            cfg.hash.algorithm,
                            cfg.hash.partial_hash_enabled,
                        )?;
                        let meta = extract_metadata(&f.abs_path, &f.ext, &cfg.exif);
                        let input = UpsertFileInput {
                            storage_id: storage.id.clone(),
                            rel_path: f.rel_path.clone(),
                            file_name: f.file_name.clone(),
                            file_stem: f.file_stem.clone(),
                            ext: f.ext.clone(),
                            byte_size: f.byte_size,
                            mtime_ns: f.mtime_ns,
                            ctime_ns: f.ctime_ns,
                            media_kind: f.media_kind,
                            format: f.format.clone(),
                            role,
                            hash_algo: hash.algorithm,
                            hash_hex: hash.hash_hex,
                            partial_hash: hash.partial_hash,
                            metadata: meta,
                        };
                        Ok((input, hash.bytes_read))
                    })
                    .collect()
            });

            let mut batch_inputs = Vec::with_capacity(results.len());
            for r in results {
                match r {
                    Ok((input, bytes)) => {
                        summary.bytes_hashed += bytes;
                        if export_jsonl {
                            let rec = ScanManifestRecord {
                                storage_id: input.storage_id.clone(),
                                rel_path: input.rel_path.clone(),
                                file_name: input.file_name.clone(),
                                file_stem: input.file_stem.clone(),
                                ext: input.ext.clone(),
                                byte_size: input.byte_size,
                                mtime_ns: input.mtime_ns,
                                ctime_ns: input.ctime_ns,
                                media_kind: input.media_kind,
                                format: input.format.clone(),
                                hash_algo: input.hash_algo,
                                hash_hex: input.hash_hex.clone(),
                                partial_hash: input.partial_hash.clone(),
                                metadata: input.metadata.clone(),
                            };
                            println!("{}", serde_json::to_string(&rec)?);
                        }
                        batch_inputs.push(input);
                    }
                    Err(err) => {
                        summary.error_count += 1;
                        warn!(storage_id = %storage.id, error = %err, "file processing error");
                    }
                }
            }

            let (added, updated) = db.upsert_scanned_batch(&batch_inputs, scan_run_id)?;
            summary.files_added += added;
            summary.files_updated += updated;
        }

        summary.files_missing = db.mark_unseen_as_missing(&storage.id, scan_run_id)?;
        db.finish_scan_run(&summary, None)?;

        info!(
            storage_id = %storage.id,
            elapsed_ms = started.elapsed().as_millis(),
            scanned = summary.files_scanned,
            added = summary.files_added,
            updated = summary.files_updated,
            skipped = summary.files_skipped,
            missing = summary.files_missing,
            "scan completed"
        );
        summaries.push(summary);
    }

    if !export_jsonl {
        match format {
            OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&summaries)?),
            OutputFormat::Jsonl => {
                for s in &summaries {
                    println!("{}", serde_json::to_string(s)?);
                }
            }
            OutputFormat::Text => {
                for s in &summaries {
                    println!(
                        "[{}] mode={} scanned={} added={} updated={} skipped={} missing={} hashed={} errors={}",
                        s.storage_id,
                        s.mode,
                        s.files_scanned,
                        s.files_added,
                        s.files_updated,
                        s.files_skipped,
                        s.files_missing,
                        human_bytes(s.bytes_hashed),
                        s.error_count,
                    );
                }
            }
        }
    }

    Ok(())
}

fn cmd_status(config_path: &Path, format: OutputFormat) -> Result<()> {
    let cfg = load_config_or_default(config_path)?;
    let db = Database::open(&cfg.general.db_path)?;
    let st = db.status()?;

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            println!("{}", serde_json::to_string_pretty(&st)?);
        }
        OutputFormat::Text => {
            println!("Schema version : v{}", st.schema_version);
            println!("Assets         : {}", st.total_assets);
            println!("Contents       : {}", st.total_contents);
            println!(
                "Present files  : {} ({})",
                st.total_present_files,
                human_bytes(st.total_present_bytes.max(0) as u64)
            );
            println!("Missing files  : {}", st.total_missing_files);
            println!("\nStorages:");
            for s in &st.storages {
                println!(
                    "  - {:<18} [{:<7} / {:<4} / {:<9} / {:<7}] present={:<7} missing={:<5} size={:<10} domain={} root={}",
                    s.storage_id,
                    s.tier,
                    s.media_type,
                    s.access_mode,
                    s.status,
                    s.present_files,
                    s.missing_files,
                    human_bytes(s.total_bytes.max(0) as u64),
                    s.failure_domain.as_deref().unwrap_or("-"),
                    s.root_path
                );
            }
        }
    }
    Ok(())
}

fn cmd_stats(config_path: &Path, format: OutputFormat) -> Result<()> {
    let cfg = load_config_or_default(config_path)?;
    let db = Database::open(&cfg.general.db_path)?;
    let report = db.stats()?;

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        OutputFormat::Text => {
            println!("Logical Assets    : {}", report.total_assets);
            println!("Unique Contents   : {}", report.total_contents);
            println!(
                "Backed-up Contents: {} / {}",
                report.backed_up_contents, report.total_contents
            );
            println!(
                "Physical Files    : {} ({})",
                report.total_present_files,
                human_bytes(report.total_present_bytes.max(0) as u64)
            );

            println!("\nBy Format:");
            for b in &report.by_format {
                println!(
                    "  - {:<10} {:>8} files  ({})",
                    b.label,
                    b.count,
                    human_bytes(b.total_bytes.max(0) as u64)
                );
            }

            println!("\nBy Camera (Assets):");
            for b in &report.by_camera {
                println!("  - {:<32} {:>8}", b.label, b.count);
            }

            println!("\nBy Lens (Assets):");
            for b in &report.by_lens {
                println!("  - {:<32} {:>8}", b.label, b.count);
            }
        }
    }
    Ok(())
}

fn cmd_find(config_path: &Path, filter: FindFilter, format: OutputFormat) -> Result<()> {
    let cfg = load_config_or_default(config_path)?;
    let db = Database::open(&cfg.general.db_path)?;
    let rows = db.find(&filter)?;

    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&rows)?),
        OutputFormat::Jsonl => {
            for r in &rows {
                println!("{}", serde_json::to_string(r)?);
            }
        }
        OutputFormat::Text => {
            for r in &rows {
                println!(
                    "{} | {:<16} | {:<4} | {:<19} | {:<18} | {}",
                    r.storage_id,
                    r.format,
                    r.iso.map(|i| format!("ISO{i}")).unwrap_or_else(|| "-".into()),
                    r.captured_at.as_deref().unwrap_or("-"),
                    r.camera_model.as_deref().unwrap_or("-"),
                    r.rel_path
                );
            }
            eprintln!("Found {} file(s).", rows.len());
        }
    }
    Ok(())
}

fn cmd_benchmark(
    config_path: &Path,
    target_path: &Path,
    limit: usize,
    with_exiftool: bool,
    format: OutputFormat,
) -> Result<()> {
    let cfg = load_config_or_default(config_path)?;
    let allowed_exts = cfg.normalized_extensions();

    let tmp_storage = StorageConfig {
        id: "benchmark".into(),
        root: target_path.to_path_buf(),
        media: MediaType::Nvme,
        tier: StorageTier::Hot,
        access_mode: AccessMode::ReadOnly,
        backup_mode: BackupMode::None,
        failure_domain: None,
        exclude_paths: Vec::new(),
    };

    let t0 = Instant::now();
    let mut discovery = discover_storage_files(&tmp_storage, &allowed_exts, false);
    let discover_secs = t0.elapsed().as_secs_f64();
    if discovery.files.len() > limit {
        discovery.files.truncate(limit);
    }

    let file_count = discovery.files.len();
    let total_bytes: u64 = discovery.files.iter().map(|f| f.byte_size).sum();

    let t_b3 = Instant::now();
    let b3_ok = discovery
        .files
        .par_iter()
        .filter(|f| hash_file(&f.abs_path, HashAlgorithm::Blake3, true).is_ok())
        .count();
    let b3_secs = t_b3.elapsed().as_secs_f64().max(1e-9);

    let t_s256 = Instant::now();
    let s256_ok = discovery
        .files
        .par_iter()
        .filter(|f| hash_file(&f.abs_path, HashAlgorithm::Sha256, true).is_ok())
        .count();
    let s256_secs = t_s256.elapsed().as_secs_f64().max(1e-9);

    let t_exif = Instant::now();
    let native_exif_ok = discovery
        .files
        .par_iter()
        .filter(|f| extract_native(&f.abs_path, &f.ext).is_some())
        .count();
    let native_exif_secs = t_exif.elapsed().as_secs_f64().max(1e-9);

    let (exiftool_ok, exiftool_secs) = if with_exiftool || cfg.exif.backend == ExifBackend::ExifTool
    {
        let t = Instant::now();
        let ok = discovery
            .files
            .iter()
            .filter(|f| extract_exiftool(&f.abs_path, &cfg.exif.exiftool_path).is_some())
            .count();
        (Some(ok), Some(t.elapsed().as_secs_f64().max(1e-9)))
    } else {
        (None, None)
    };

    #[derive(Serialize)]
    struct BenchReport {
        path: String,
        files_tested: usize,
        total_bytes: u64,
        discover_secs: f64,
        blake3_secs: f64,
        blake3_mb_per_sec: f64,
        blake3_ok: usize,
        sha256_secs: f64,
        sha256_mb_per_sec: f64,
        sha256_ok: usize,
        native_exif_secs: f64,
        native_exif_files_per_sec: f64,
        native_exif_ok: usize,
        exiftool_secs: Option<f64>,
        exiftool_ok: Option<usize>,
    }

    let mb = total_bytes as f64 / (1000.0 * 1000.0);
    let report = BenchReport {
        path: target_path.to_string_lossy().into_owned(),
        files_tested: file_count,
        total_bytes,
        discover_secs,
        blake3_secs: b3_secs,
        blake3_mb_per_sec: mb / b3_secs,
        blake3_ok: b3_ok,
        sha256_secs: s256_secs,
        sha256_mb_per_sec: mb / s256_secs,
        sha256_ok: s256_ok,
        native_exif_secs,
        native_exif_files_per_sec: file_count as f64 / native_exif_secs,
        native_exif_ok,
        exiftool_secs,
        exiftool_ok,
    };

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        OutputFormat::Text => {
            println!("Benchmark target : {}", report.path);
            println!(
                "Files tested     : {} ({})",
                report.files_tested,
                human_bytes(report.total_bytes)
            );
            println!("Discovery        : {:.3}s", report.discover_secs);
            println!(
                "BLAKE3           : {:.3}s ({:.1} MB/s, ok={})",
                report.blake3_secs, report.blake3_mb_per_sec, report.blake3_ok
            );
            println!(
                "SHA-256          : {:.3}s ({:.1} MB/s, ok={})",
                report.sha256_secs, report.sha256_mb_per_sec, report.sha256_ok
            );
            println!(
                "Native EXIF      : {:.3}s ({:.1} files/s, parsed={}/{})",
                report.native_exif_secs,
                report.native_exif_files_per_sec,
                report.native_exif_ok,
                report.files_tested
            );
            if let (Some(secs), Some(ok)) = (report.exiftool_secs, report.exiftool_ok) {
                println!(
                    "ExifTool (CLI)   : {:.3}s ({:.1} files/s, parsed={}/{})",
                    secs,
                    report.files_tested as f64 / secs,
                    ok,
                    report.files_tested
                );
            }
        }
    }

    Ok(())
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut val = bytes as f64;
    let mut idx = 0;
    while val >= 1000.0 && idx < UNITS.len() - 1 {
        val /= 1000.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{bytes} B")
    } else {
        format!("{val:.2} {}", UNITS[idx])
    }
}
