use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn wingpv_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wingpv")
}

#[test]
fn end_to_end_init_scan_incremental_offline_and_queries() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("test.db");
    let hot_root = tmp.path().join("hot_ssd");
    let nas_root = tmp.path().join("nas_backup");
    let showcase_dir = hot_root.join("_Showcase");

    let hot_day = hot_root.join("2026").join("2026_11_24").join("100EOSR5");
    let nas_day = nas_root.join("2026").join("11_24").join("RAW");
    fs::create_dir_all(&hot_day).unwrap();
    fs::create_dir_all(&nas_day).unwrap();
    fs::create_dir_all(&showcase_dir).unwrap();

    // Same CR3 payload stored on Hot SSD and legacy-layout external read_only NAS
    fs::write(hot_day.join("IMG_0001.CR3"), b"dummy-cr3-payload-0001").unwrap();
    fs::write(nas_day.join("IMG_0001.CR3"), b"dummy-cr3-payload-0001").unwrap();
    // JPEG on Hot SSD only
    fs::write(hot_day.join("IMG_0001.JPG"), b"dummy-jpg-payload-0001").unwrap();
    // Excluded showcase JPEG
    fs::write(showcase_dir.join("export.jpg"), b"showcase-export").unwrap();

    let config_path = tmp.path().join("wing-photovault.toml");
    let cfg = format!(
        r#"
[host]
id = "test-pc"
name = "Test Windows PC"

[general]
db_path = '{}'

[exif]
backend = "native"

[[storages]]
id = "ssd_hot"
root = '{}'
media = "nvme"
tier = "hot"
access_mode = "managed"
backup_mode = "none"
failure_domain = "pc-nvme-1"
exclude_paths = ['{}']

[[storages]]
id = "nas_backup"
root = '{}'
media = "nas"
tier = "backup"
access_mode = "read_only"
backup_mode = "external"
failure_domain = "nas-eagle"
"#,
        db_path.display(),
        hot_root.display(),
        showcase_dir.display(),
        nas_root.display()
    );
    fs::write(&config_path, cfg).unwrap();

    // 1. init
    let out = Command::new(wingpv_bin())
        .args(["--config", config_path.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    assert!(out.status.success(), "init failed: {:?}", out);

    // 2. initial scan
    let out = Command::new(wingpv_bin())
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "--format",
            "json",
            "scan",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "scan failed: {:?}", out);
    let summaries: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(summaries.as_array().unwrap().len(), 2);

    // 3. incremental scan should skip all unchanged files
    let out = Command::new(wingpv_bin())
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "--format",
            "json",
            "scan",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let inc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(inc[0]["files_added"], 0);
    assert_eq!(inc[0]["files_skipped"], 2);
    assert_eq!(inc[1]["files_added"], 0);
    assert_eq!(inc[1]["files_skipped"], 1);

    // 4. stats should show 3 physical files, 2 unique contents, 1 backed-up content
    let out = Command::new(wingpv_bin())
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "--format",
            "json",
            "stats",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stats: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(stats["total_present_files"], 3);
    assert_eq!(stats["total_contents"], 2);
    assert_eq!(stats["backed_up_contents"], 1);

    // 5. find --ext-format cr3 should include persistent asset_public_id
    let out = Command::new(wingpv_bin())
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "--format",
            "json",
            "find",
            "--ext-format",
            "cr3",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert!(arr[0]["asset_public_id"].as_str().is_some());
    assert_eq!(arr[0]["asset_public_id"], arr[1]["asset_public_id"]);

    // 5b. find keyset pagination with --limit 1 --page-info --cursor
    let out = Command::new(wingpv_bin())
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "--format",
            "json",
            "find",
            "--ext-format",
            "cr3",
            "--limit",
            "1",
            "--page-info",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "find page 1 failed: {:?}", out);
    let page1: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(page1["items"].as_array().unwrap().len(), 1);
    assert_eq!(page1["has_more"], true);
    let cursor = page1["next_cursor"].as_str().unwrap();

    let out = Command::new(wingpv_bin())
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "--format",
            "json",
            "find",
            "--ext-format",
            "cr3",
            "--limit",
            "1",
            "--page-info",
            "--cursor",
            cursor,
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "find page 2 failed: {:?}", out);
    let page2: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(page2["items"].as_array().unwrap().len(), 1);
    assert_eq!(page2["has_more"], false);
    assert!(page2["next_cursor"].is_null());
    assert_ne!(page1["items"][0]["file_id"], page2["items"][0]["file_id"]);

    // 6. Invariant: offline storage must NOT mark its files as missing
    let nas_hidden = tmp.path().join("nas_backup_offline");
    fs::rename(&nas_root, &nas_hidden).unwrap();

    let out = Command::new(wingpv_bin())
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "--format",
            "json",
            "scan",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = Command::new(wingpv_bin())
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "--format",
            "json",
            "status",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["schema_version"], 2);
    assert_eq!(status["total_missing_files"], 0);
    let nas_row = status["storages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["storage_id"] == "nas_backup")
        .unwrap();
    assert_eq!(nas_row["status"], "offline");
    assert_eq!(nas_row["access_mode"], "read_only");
    assert_eq!(nas_row["backup_mode"], "external");
    assert_eq!(nas_row["present_files"], 1);
    assert_eq!(nas_row["missing_files"], 0);

    // 7. benchmark
    let out = Command::new(wingpv_bin())
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "--format",
            "json",
            "benchmark",
            hot_root.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let bench: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(bench["blake3_ok"], 3);

    // 8. benchmark --db-scale (temporary synthetic DB; never touches real assets)
    let out = Command::new(wingpv_bin())
        .args(["--format", "json", "benchmark", "--db-scale", "1000"])
        .output()
        .unwrap();
    assert!(out.status.success(), "db-scale benchmark failed: {:?}", out);
    let db_bench: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(db_bench["files_created"], 1000);
    assert_eq!(db_bench["assets_created"], 500);
    assert_eq!(db_bench["present_files_verified"], 1000);
    assert_eq!(db_bench["find_first_page_rows"], 100);
    assert_eq!(db_bench["find_next_page_rows"], 100);
    assert!(db_bench["db_size_bytes"].as_u64().unwrap() > 0);
    assert!(db_bench["normalize_lenses_applied_assets"].as_u64().unwrap() > 0);

    // Fail Closed: --db-scale 0 must be rejected
    let out = Command::new(wingpv_bin())
        .args(["benchmark", "--db-scale", "0"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn nas_export_and_windows_import_manifest_tracks_backups_and_rejects_traversal() {
    let tmp = tempdir().unwrap();

    // Simulate NAS local scan DB
    let nas_db = tmp.path().join("eagle.db");
    let nas_root = tmp.path().join("eagle_tank_photos");
    let nas_day = nas_root.join("2015").join("08_10").join("RAW");
    fs::create_dir_all(&nas_day).unwrap();
    fs::write(nas_day.join("IMG_0001.CR2"), b"same-photo-bytes-on-nas-and-pc").unwrap();

    let nas_cfg_path = tmp.path().join("eagle.toml");
    fs::write(
        &nas_cfg_path,
        format!(
            r#"
[host]
id = "eagle"
name = "Eagle FreeBSD NAS"

[general]
db_path = '{}'

[exif]
backend = "none"

[[storages]]
id = "nas_backup_eagle"
root = '{}'
media = "hdd"
tier = "backup"
access_mode = "read_only"
backup_mode = "external"
failure_domain = "nas-eagle"
"#,
            nas_db.display(),
            nas_root.display()
        ),
    )
    .unwrap();

    // NAS local scan (with --cooldown-every and --max-files exercised)
    let out = Command::new(wingpv_bin())
        .args([
            "--config",
            nas_cfg_path.to_str().unwrap(),
            "scan",
            "--cooldown-every",
            "1",
            "--cooldown-secs",
            "0",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "nas scan failed: {:?}", out);

    // Export manifest from NAS DB
    let manifest_path = tmp.path().join("eagle-manifest.jsonl");
    let out = Command::new(wingpv_bin())
        .args([
            "--config",
            nas_cfg_path.to_str().unwrap(),
            "--format",
            "json",
            "export-manifest",
            "--storage",
            "nas_backup_eagle",
            "--output",
            manifest_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "export-manifest failed: {:?}", out);

    // Append a malicious path-traversal record to verify untrusted-input rejection
    let mut manifest_text = fs::read_to_string(&manifest_path).unwrap();
    manifest_text.push_str(
        r#"{"storage_id":"nas_backup_eagle","rel_path":"../evil.cr2","file_name":"evil.cr2","file_stem":"evil","ext":".cr2","byte_size":10,"mtime_ns":1,"ctime_ns":null,"media_kind":"raw","format":"cr2","hash_algo":"blake3","hash_hex":"0123456789abcdef0123456789abcdef","partial_hash":null,"metadata":null}"#,
    );
    manifest_text.push('\n');
    fs::write(&manifest_path, manifest_text).unwrap();

    // Simulate Windows PC DB with local Hot SSD copy (and offline/unmounted NAS root)
    let win_db = tmp.path().join("win.db");
    let win_hot = tmp.path().join("win_hot_ssd");
    let win_day = win_hot.join("2015").join("2015_08_10");
    fs::create_dir_all(&win_day).unwrap();
    fs::write(win_day.join("IMG_0001.CR2"), b"same-photo-bytes-on-nas-and-pc").unwrap();
    let offline_nas_unc = tmp.path().join("unmounted_eagle_share");

    let win_cfg_path = tmp.path().join("win.toml");
    fs::write(
        &win_cfg_path,
        format!(
            r#"
[host]
id = "main-pc"
name = "Main Windows PC"

[general]
db_path = '{}'

[exif]
backend = "none"

[[storages]]
id = "ssd_hot"
root = '{}'
media = "nvme"
tier = "hot"
access_mode = "managed"
backup_mode = "none"
failure_domain = "pc-nvme-1"

[[storages]]
id = "nas_backup_eagle"
root = '{}'
media = "nas"
tier = "backup"
access_mode = "read_only"
backup_mode = "external"
failure_domain = "nas-eagle"
"#,
            win_db.display(),
            win_hot.display(),
            offline_nas_unc.display()
        ),
    )
    .unwrap();

    // Scan Windows local SSD
    let out = Command::new(wingpv_bin())
        .args(["--config", win_cfg_path.to_str().unwrap(), "scan"])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Import NAS manifest into Windows DB (zero SMB hashing!)
    let out = Command::new(wingpv_bin())
        .args([
            "--config",
            win_cfg_path.to_str().unwrap(),
            "--format",
            "json",
            "import-manifest",
            manifest_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "import-manifest failed: {:?}", out);
    let imp: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(imp["records_read"], 1);
    assert_eq!(imp["files_added"], 1);
    assert_eq!(imp["records_rejected"], 1);

    // Windows stats must now show 1 logical asset, 1 content, 1 backed-up content, 2 physical files!
    let out = Command::new(wingpv_bin())
        .args([
            "--config",
            win_cfg_path.to_str().unwrap(),
            "--format",
            "json",
            "stats",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stats: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(stats["total_assets"], 1);
    assert_eq!(stats["total_contents"], 1);
    assert_eq!(stats["backed_up_contents"], 1);
    assert_eq!(stats["total_present_files"], 2);
}
