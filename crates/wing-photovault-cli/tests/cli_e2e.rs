use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn wingpv_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wingpv")
}

#[test]
fn end_to_end_init_scan_incremental_and_queries() {
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

    // Same CR3 payload stored on Hot SSD and legacy-layout NAS
    fs::write(hot_day.join("IMG_0001.CR3"), b"dummy-cr3-payload-0001").unwrap();
    fs::write(nas_day.join("IMG_0001.CR3"), b"dummy-cr3-payload-0001").unwrap();
    // JPEG on Hot SSD only
    fs::write(hot_day.join("IMG_0001.JPG"), b"dummy-jpg-payload-0001").unwrap();
    // Excluded showcase JPEG
    fs::write(showcase_dir.join("export.jpg"), b"showcase-export").unwrap();

    let config_path = tmp.path().join("wing-photovault.toml");
    let cfg = format!(
        r#"
[general]
db_path = '{}'

[exif]
backend = "native"

[[storages]]
id = "ssd_hot"
root = '{}'
media = "nvme"
tier = "hot"
exclude_paths = ['{}']

[[storages]]
id = "nas_backup"
root = '{}'
media = "nas"
tier = "backup"
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

    // 5. find --ext-format cr3
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
    assert_eq!(rows.as_array().unwrap().len(), 2);

    // 6. benchmark
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
}
