#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config_model;
mod runner;

use std::fs;
use std::path::{Path, PathBuf};

use eframe::egui;
use wing_photovault_core::config::ExifBackend;
use wing_photovault_core::model::{AccessMode, BackupMode, HashAlgorithm, MediaType, StorageTier};

use crate::config_model::{ConfigDraft, LensAliasRow, StorageDraft};
use crate::runner::{
    build_export_manifest_args, build_find_args, build_import_manifest_args,
    build_normalize_lenses_args, build_scan_args, build_simple_args, build_stats_args,
    detect_wingpv_binary, CommandRunner, FindParams, ScanParams,
};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("WING PhotoVault — Mini UI (Config & CLI Test Tool)")
            .with_inner_size([1120.0, 780.0])
            .with_min_inner_size([860.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "WING PhotoVault Mini UI",
        options,
        Box::new(|cc| {
            configure_japanese_fonts(&cc.egui_ctx);
            Ok(Box::new(MiniUiApp::new()))
        }),
    )
}

fn configure_japanese_fonts(ctx: &egui::Context) {
    let candidates: &[&str] = if cfg!(windows) {
        &[
            r"C:\Windows\Fonts\YuGothR.ttc",
            r"C:\Windows\Fonts\meiryo.ttc",
            r"C:\Windows\Fonts\msgothic.ttc",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        ]
    } else {
        &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/local/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        ]
    };

    for path in candidates {
        if let Ok(bytes) = fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("jp_fallback".to_owned(), egui::FontData::from_owned(bytes));
            if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                family.push("jp_fallback".to_owned());
            }
            if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                family.push("jp_fallback".to_owned());
            }
            ctx.set_fonts(fonts);
            break;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    ConfigEditor,
    InspectQuery,
    ScanManifest,
}

struct MiniUiApp {
    tab: ActiveTab,
    config_path: PathBuf,
    wingpv_bin: PathBuf,
    draft: ConfigDraft,
    dirty: bool,
    status_banner: Option<(bool, String)>,
    show_toml_preview: bool,

    // Inspect / Query tab state
    query_json: bool,
    stats_percent: bool,
    stats_by_year: bool,
    stats_gear_history: bool,
    confirm_normalize_apply: bool,
    find_params: FindParams,

    // Scan / Manifest tab state
    scan_params: ScanParams,
    export_storage_id: String,
    export_output_path: String,
    import_manifest_path: String,
    import_storage_override: String,

    runner: CommandRunner,
}

impl MiniUiApp {
    fn new() -> Self {
        let default_cfg_path = PathBuf::from("wing-photovault.toml");
        let (draft, banner) = if default_cfg_path.is_file() {
            match fs::read_to_string(&default_cfg_path)
                .map_err(|e| e.to_string())
                .and_then(|s| ConfigDraft::from_toml_str(&s))
            {
                Ok(d) => (
                    d,
                    Some((true, format!("読み込み完了: {}", default_cfg_path.display()))),
                ),
                Err(err) => (
                    ConfigDraft::default(),
                    Some((false, format!("既存設定の読み込み失敗: {err}"))),
                ),
            }
        } else {
            (
                ConfigDraft::default(),
                Some((
                    true,
                    "新規ドラフト状態です。ストレージを追加して .toml を保存してください。"
                        .to_string(),
                )),
            )
        };

        Self {
            tab: ActiveTab::ConfigEditor,
            config_path: default_cfg_path,
            wingpv_bin: detect_wingpv_binary(),
            draft,
            dirty: false,
            status_banner: banner,
            show_toml_preview: false,
            query_json: false,
            stats_percent: true,
            stats_by_year: false,
            stats_gear_history: false,
            confirm_normalize_apply: false,
            find_params: FindParams::default(),
            scan_params: ScanParams::default(),
            export_storage_id: String::new(),
            export_output_path: "nas-manifest.jsonl".to_string(),
            import_manifest_path: String::new(),
            import_storage_override: String::new(),
            runner: CommandRunner::default(),
        }
    }

    fn load_from_path(&mut self, path: PathBuf) {
        match fs::read_to_string(&path)
            .map_err(|e| format!("ファイル読み込みエラー: {e}"))
            .and_then(|s| ConfigDraft::from_toml_str(&s))
        {
            Ok(d) => {
                self.config_path = path.clone();
                self.draft = d;
                self.dirty = false;
                self.status_banner = Some((true, format!("読み込みました: {}", path.display())));
            }
            Err(err) => {
                self.status_banner = Some((false, err));
            }
        }
    }

    fn save_to_path(&mut self, path: PathBuf) -> bool {
        match self.draft.to_toml_string() {
            Ok(toml_text) => {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        if let Err(err) = fs::create_dir_all(parent) {
                            self.status_banner = Some((
                                false,
                                format!("ディレクトリ作成失敗: {err}"),
                            ));
                            return false;
                        }
                    }
                }
                match fs::write(&path, toml_text) {
                    Ok(()) => {
                        self.config_path = path.clone();
                        self.dirty = false;
                        self.status_banner =
                            Some((true, format!("保存しました: {}", path.display())));
                        true
                    }
                    Err(err) => {
                        self.status_banner = Some((false, format!("保存エラー: {err}")));
                        false
                    }
                }
            }
            Err(err) => {
                self.status_banner = Some((false, format!("検証エラー（保存中止）: {err}")));
                false
            }
        }
    }

    fn ensure_config_ready(&mut self) -> bool {
        if self.dirty || !self.config_path.is_file() {
            self.status_banner = Some((
                false,
                "設定ファイルが未保存または存在しません。先に上部バーから「保存」してください。"
                    .to_string(),
            ));
            return false;
        }
        true
    }

    fn run_cli(&mut self, ctx: &egui::Context, args: Vec<String>) {
        if !self.ensure_config_ready() {
            return;
        }
        self.runner.spawn(self.wingpv_bin.clone(), args, ctx.clone());
    }

    fn render_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("WING PhotoVault Mini UI");
                ui.separator();
                ui.selectable_value(
                    &mut self.tab,
                    ActiveTab::ConfigEditor,
                    "1. 設定エディタ (.toml)",
                );
                ui.selectable_value(
                    &mut self.tab,
                    ActiveTab::InspectQuery,
                    "2. 状態・統計・検索",
                );
                ui.selectable_value(
                    &mut self.tab,
                    ActiveTab::ScanManifest,
                    "3. スキャン・マニフェスト",
                );
            });

            ui.separator();

            // Config file path & actions
            ui.horizontal(|ui| {
                ui.label("設定ファイル:");
                let mut cfg_str = self.config_path.to_string_lossy().into_owned();
                if ui
                    .add(egui::TextEdit::singleline(&mut cfg_str).desired_width(340.0))
                    .changed()
                {
                    self.config_path = PathBuf::from(cfg_str);
                }
                if ui.button("開く...").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("TOML Config", &["toml"])
                        .pick_file()
                    {
                        self.load_from_path(p);
                    }
                }
                let save_label = if self.dirty { "保存 *" } else { "保存" };
                if ui.button(save_label).clicked() {
                    let target = self.config_path.clone();
                    self.save_to_path(target);
                }
                if ui.button("別名保存...").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("TOML Config", &["toml"])
                        .set_file_name("wing-photovault.toml")
                        .save_file()
                    {
                        self.save_to_path(p);
                    }
                }
                if ui.button("新規リセット").clicked() {
                    self.draft = ConfigDraft::default();
                    self.dirty = true;
                    self.status_banner =
                        Some((true, "ドラフトを初期化しました（未保存）".to_string()));
                }
            });

            // wingpv binary path
            ui.horizontal(|ui| {
                ui.label("wingpv バイナリ:");
                let mut bin_str = self.wingpv_bin.to_string_lossy().into_owned();
                if ui
                    .add(egui::TextEdit::singleline(&mut bin_str).desired_width(340.0))
                    .changed()
                {
                    self.wingpv_bin = PathBuf::from(bin_str);
                }
                if ui.button("選択...").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_file() {
                        self.wingpv_bin = p;
                    }
                }
                if ui.button("自動検出").clicked() {
                    self.wingpv_bin = detect_wingpv_binary();
                }

                ui.separator();
                match self.draft.validate() {
                    Ok(()) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(40, 160, 80),
                            "設定検証: OK (Fail Closed 適合)",
                        );
                    }
                    Err(err) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(210, 60, 60),
                            format!("検証エラー: {err}"),
                        );
                    }
                }
            });

            if let Some((ok, msg)) = &self.status_banner {
                let color = if *ok {
                    egui::Color32::from_rgb(70, 140, 210)
                } else {
                    egui::Color32::from_rgb(210, 60, 60)
                };
                ui.colored_label(color, msg);
            }
            ui.add_space(2.0);
        });
    }

    fn render_config_editor_tab(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;

        egui::ScrollArea::vertical().show(ui, |ui| {
            // 1. Host & General
            ui.collapsing("基本設定 ([host] / [general] / [hash] / [performance] / [exif])", |ui| {
                egui::Grid::new("general_grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("host.id（ホスト固定ID）:");
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut self.draft.host_id).desired_width(280.0))
                            .changed();
                        ui.end_row();

                        ui.label("host.name（表示名）:");
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut self.draft.host_name).desired_width(280.0))
                            .changed();
                        ui.end_row();

                        ui.label("general.db_path（ローカル SSD 上の SQLite DB）:");
                        ui.horizontal(|ui| {
                            changed |= ui
                                .add(egui::TextEdit::singleline(&mut self.draft.db_path).desired_width(360.0))
                                .changed();
                            if ui.button("保存先...").clicked() {
                                if let Some(p) = rfd::FileDialog::new()
                                    .add_filter("SQLite DB", &["db"])
                                    .set_file_name("wing-photovault.db")
                                    .save_file()
                                {
                                    self.draft.db_path = p.to_string_lossy().into_owned();
                                    changed = true;
                                }
                            }
                        });
                        ui.end_row();

                        ui.label("general.log_dir:");
                        ui.horizontal(|ui| {
                            changed |= ui
                                .add(egui::TextEdit::singleline(&mut self.draft.log_dir).desired_width(360.0))
                                .changed();
                            if ui.button("フォルダ...").clicked() {
                                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                                    self.draft.log_dir = p.to_string_lossy().into_owned();
                                    changed = true;
                                }
                            }
                        });
                        ui.end_row();

                        ui.label("image_exts（カンマ区切り）:");
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut self.draft.image_exts_csv).desired_width(420.0))
                            .changed();
                        ui.end_row();

                        ui.label("video_exts（カンマ区切り）:");
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut self.draft.video_exts_csv).desired_width(420.0))
                            .changed();
                        ui.end_row();

                        ui.label("スキャン設定:");
                        ui.horizontal(|ui| {
                            changed |= ui
                                .checkbox(&mut self.draft.incremental, "差分スキャン (incremental)")
                                .changed();
                            changed |= ui
                                .checkbox(
                                    &mut self.draft.follow_symlinks,
                                    "シンボリックリンク追従 (非推奨)",
                                )
                                .changed();
                        });
                        ui.end_row();

                        ui.label("ハッシュ方式:");
                        ui.horizontal(|ui| {
                            changed |= ui
                                .selectable_value(
                                    &mut self.draft.hash_algorithm,
                                    HashAlgorithm::Blake3,
                                    "blake3 (高速・推奨)",
                                )
                                .changed();
                            changed |= ui
                                .selectable_value(
                                    &mut self.draft.hash_algorithm,
                                    HashAlgorithm::Sha256,
                                    "sha256",
                                )
                                .changed();
                            changed |= ui
                                .checkbox(
                                    &mut self.draft.partial_hash_enabled,
                                    "部分ハッシュ有効",
                                )
                                .changed();
                        });
                        ui.end_row();

                        ui.label("並列 I/O ワーカー数:");
                        ui.horizontal(|ui| {
                            ui.label("SSD:");
                            changed |= ui
                                .add(egui::DragValue::new(&mut self.draft.ssd_io_workers).range(1..=32))
                                .changed();
                            ui.label("HDD (1推奨):");
                            changed |= ui
                                .add(egui::DragValue::new(&mut self.draft.hdd_io_workers).range(1..=8))
                                .changed();
                            ui.label("NAS:");
                            changed |= ui
                                .add(egui::DragValue::new(&mut self.draft.nas_io_workers).range(1..=16))
                                .changed();
                            ui.label("DB Batch:");
                            changed |= ui
                                .add(egui::DragValue::new(&mut self.draft.db_batch_size).range(10..=10000))
                                .changed();
                        });
                        ui.end_row();

                        ui.label("EXIF バックエンド:");
                        ui.horizontal(|ui| {
                            for (val, lbl) in [
                                (ExifBackend::Auto, "auto (推奨)"),
                                (ExifBackend::Native, "native"),
                                (ExifBackend::ExifTool, "exiftool"),
                                (ExifBackend::None, "none"),
                            ] {
                                changed |= ui
                                    .selectable_value(&mut self.draft.exif_backend, val, lbl)
                                    .changed();
                            }
                            changed |= ui
                                .checkbox(
                                    &mut self.draft.persistent_exiftool,
                                    "常駐 ExifTool (-stay_open)",
                                )
                                .changed();
                        });
                        ui.end_row();

                        ui.label("exiftool_path（空なら PATH 探索）:");
                        ui.horizontal(|ui| {
                            changed |= ui
                                .add(egui::TextEdit::singleline(&mut self.draft.exiftool_path).desired_width(360.0))
                                .changed();
                            if ui.button("選択...").clicked() {
                                if let Some(p) = rfd::FileDialog::new().pick_file() {
                                    self.draft.exiftool_path = p.to_string_lossy().into_owned();
                                    changed = true;
                                }
                            }
                        });
                        ui.end_row();
                    });
            });

            ui.add_space(8.0);

            // 2. Storages
            ui.horizontal(|ui| {
                ui.heading(format!("ストレージ定義 ([[storages]]: {} 件)", self.draft.storages.len()));
                if ui.button("＋ ローカル SSD/HDD 追加").clicked() {
                    let idx = self.draft.storages.len() + 1;
                    self.draft.storages.push(StorageDraft::new_managed(idx));
                    changed = true;
                }
                if ui.button("＋ 外部 NAS バックアップ追加 (ReadOnly)").clicked() {
                    let idx = self.draft.storages.len() + 1;
                    self.draft.storages.push(StorageDraft::new_external_nas(idx));
                    changed = true;
                }
            });

            let mut remove_storage_idx: Option<usize> = None;
            for (idx, st) in self.draft.storages.iter_mut().enumerate() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong(format!("#{}", idx + 1));
                        ui.label("ID:");
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut st.id).desired_width(170.0))
                            .changed();

                        ui.label("ルートパス:");
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut st.root).desired_width(260.0))
                            .changed();
                        if ui.button("フォルダ...").clicked() {
                            if let Some(p) = rfd::FileDialog::new().pick_folder() {
                                st.root = p.to_string_lossy().into_owned();
                                changed = true;
                            }
                        }

                        if ui.button("削除").clicked() {
                            remove_storage_idx = Some(idx);
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("media:");
                        egui::ComboBox::from_id_salt(format!("media_{idx}"))
                            .selected_text(st.media.as_str())
                            .width(95.0)
                            .show_ui(ui, |ui| {
                                for m in [
                                    MediaType::Nvme,
                                    MediaType::Ssd,
                                    MediaType::Hdd,
                                    MediaType::Nas,
                                    MediaType::Removable,
                                ] {
                                    changed |= ui
                                        .selectable_value(&mut st.media, m, m.as_str())
                                        .changed();
                                }
                            });

                        ui.label("tier:");
                        egui::ComboBox::from_id_salt(format!("tier_{idx}"))
                            .selected_text(st.tier.as_str())
                            .width(85.0)
                            .show_ui(ui, |ui| {
                                for t in [StorageTier::Hot, StorageTier::Archive, StorageTier::Backup]
                                {
                                    changed |= ui
                                        .selectable_value(&mut st.tier, t, t.as_str())
                                        .changed();
                                }
                            });

                        ui.label("backup_mode:");
                        egui::ComboBox::from_id_salt(format!("backup_{idx}"))
                            .selected_text(st.backup_mode.as_str())
                            .width(90.0)
                            .show_ui(ui, |ui| {
                                for b in [
                                    BackupMode::None,
                                    BackupMode::Managed,
                                    BackupMode::External,
                                ] {
                                    if ui
                                        .selectable_value(&mut st.backup_mode, b, b.as_str())
                                        .changed()
                                    {
                                        st.enforce_invariants();
                                        changed = true;
                                    }
                                }
                            });

                        ui.label("access_mode:");
                        let locked_ro = st.backup_mode == BackupMode::External;
                        ui.add_enabled_ui(!locked_ro, |ui| {
                            egui::ComboBox::from_id_salt(format!("access_{idx}"))
                                .selected_text(st.access_mode.as_str())
                                .width(95.0)
                                .show_ui(ui, |ui| {
                                    for a in [AccessMode::Managed, AccessMode::ReadOnly] {
                                        changed |= ui
                                            .selectable_value(&mut st.access_mode, a, a.as_str())
                                            .changed();
                                    }
                                });
                        });
                        if locked_ro {
                            ui.small("(external は read_only 固定)");
                        }

                        ui.label("failure_domain:");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut st.failure_domain)
                                    .desired_width(120.0)
                                    .hint_text("例: pc-nvme-g"),
                            )
                            .changed();
                    });

                    ui.horizontal(|ui| {
                        ui.label("exclude_paths (1行1パス):");
                        changed |= ui
                            .add(
                                egui::TextEdit::multiline(&mut st.exclude_paths_text)
                                    .desired_rows(1)
                                    .desired_width(480.0),
                            )
                            .changed();
                    });
                });
            }
            if let Some(idx) = remove_storage_idx {
                self.draft.storages.remove(idx);
                changed = true;
            }

            ui.add_space(8.0);

            // 3. Lens aliases
            ui.horizontal(|ui| {
                ui.heading(format!(
                    "レンズエイリアス ([exif.lens_aliases]: {} 件)",
                    self.draft.lens_aliases.len()
                ));
                if ui.button("＋ 行を追加").clicked() {
                    self.draft.lens_aliases.push(LensAliasRow {
                        raw: String::new(),
                        normalized: String::new(),
                    });
                    changed = true;
                }
                if ui.button("旧機種レンズ例を挿入").clicked() {
                    self.draft.add_sample_lens_aliases();
                    changed = true;
                }
            });

            let mut remove_alias_idx: Option<usize> = None;
            egui::Grid::new("lens_aliases_grid")
                .num_columns(3)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    if !self.draft.lens_aliases.is_empty() {
                        ui.strong("元のレンズ名 (lens_model_raw)");
                        ui.strong("正規化後 (lens_model)");
                        ui.label("");
                        ui.end_row();
                    }
                    for (idx, row) in self.draft.lens_aliases.iter_mut().enumerate() {
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut row.raw)
                                    .desired_width(240.0)
                                    .hint_text("例: Unknown 100-400mm"),
                            )
                            .changed();
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut row.normalized)
                                    .desired_width(360.0)
                                    .hint_text("例: EF100-400mm f/4.5-5.6L IS USM"),
                            )
                            .changed();
                        if ui.button("削除").clicked() {
                            remove_alias_idx = Some(idx);
                        }
                        ui.end_row();
                    }
                });
            if let Some(idx) = remove_alias_idx {
                self.draft.lens_aliases.remove(idx);
                changed = true;
            }

            ui.add_space(8.0);

            // 4. Live TOML preview
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.show_toml_preview, "生成される TOML プレビューを表示");
                if ui.button("TOML をクリップボードにコピー").clicked() {
                    match self.draft.to_toml_string() {
                        Ok(txt) => {
                            ui.ctx().copy_text(txt);
                            self.status_banner =
                                Some((true, "生成 TOML をクリップボードにコピーしました".into()));
                        }
                        Err(err) => {
                            self.status_banner = Some((false, err));
                        }
                    }
                }
            });
            if self.show_toml_preview {
                let mut preview = match self.draft.to_toml_string() {
                    Ok(txt) => txt,
                    Err(err) => format!("# 検証エラーのため生成できません:\n# {err}"),
                };
                ui.add(
                    egui::TextEdit::multiline(&mut preview)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(16)
                        .desired_width(f32::INFINITY)
                        .interactive(false),
                );
            }
        });

        if changed {
            self.dirty = true;
        }
    }

    fn render_inspect_query_tab(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let running = self.runner.is_running();

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.query_json, "JSON (--format json)");
            ui.checkbox(&mut self.stats_percent, "--percent");
            ui.checkbox(&mut self.stats_by_year, "--by-year");
            ui.checkbox(&mut self.stats_gear_history, "--gear-history");
            ui.separator();

            ui.add_enabled_ui(!running, |ui| {
                if ui.button("init（DB 初期化）").clicked() {
                    let args = build_simple_args(&self.config_path, self.query_json, "init");
                    self.run_cli(ctx, args);
                }
                if ui.button("status").clicked() {
                    let args = build_simple_args(&self.config_path, self.query_json, "status");
                    self.run_cli(ctx, args);
                }
                if ui.button("stats").clicked() {
                    let args = build_stats_args(
                        &self.config_path,
                        self.query_json,
                        self.stats_percent,
                        self.stats_by_year,
                        self.stats_gear_history,
                    );
                    self.run_cli(ctx, args);
                }
                if ui.button("normalize-lenses --dry-run").clicked() {
                    let args =
                        build_normalize_lenses_args(&self.config_path, self.query_json, true);
                    self.run_cli(ctx, args);
                }
            });
        });

        ui.horizontal(|ui| {
            ui.checkbox(
                &mut self.confirm_normalize_apply,
                "DB 更新を許可（normalize-lenses 実適用）",
            );
            ui.add_enabled_ui(!running && self.confirm_normalize_apply, |ui| {
                if ui.button("normalize-lenses（実適用）").clicked() {
                    let args =
                        build_normalize_lenses_args(&self.config_path, self.query_json, false);
                    self.run_cli(ctx, args);
                }
            });
        });

        ui.separator();
        ui.heading("find（検索テスト）");

        egui::Grid::new("find_grid")
            .num_columns(4)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("storage:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.find_params.storage).desired_width(180.0),
                );
                ui.label("ext-format (cr3/jpeg等):");
                ui.add(
                    egui::TextEdit::singleline(&mut self.find_params.ext_format)
                        .desired_width(140.0),
                );
                ui.end_row();

                ui.label("camera (部分一致):");
                ui.add(
                    egui::TextEdit::singleline(&mut self.find_params.camera).desired_width(180.0),
                );
                ui.label("lens (部分一致):");
                ui.add(egui::TextEdit::singleline(&mut self.find_params.lens).desired_width(180.0));
                ui.end_row();

                ui.label("from (YYYY-MM-DD):");
                ui.add(egui::TextEdit::singleline(&mut self.find_params.from).desired_width(180.0));
                ui.label("to (YYYY-MM-DD):");
                ui.add(egui::TextEdit::singleline(&mut self.find_params.to).desired_width(180.0));
                ui.end_row();

                ui.label("limit:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.find_params.limit).desired_width(100.0),
                );
                ui.label("cursor (任意):");
                ui.add(
                    egui::TextEdit::singleline(&mut self.find_params.cursor)
                        .desired_width(220.0)
                        .hint_text("v1:<file_id>:<captured_at>"),
                );
                ui.end_row();

                ui.label("");
                ui.checkbox(&mut self.find_params.page_info, "JSON ページ情報 (--page-info)");
                ui.label("");
                ui.add_enabled_ui(!running, |ui| {
                    if ui.button("find 実行").clicked() {
                        self.find_params.json_output = self.query_json;
                        let args = build_find_args(&self.config_path, &self.find_params);
                        self.run_cli(ctx, args);
                    }
                });
                ui.end_row();
            });

        ui.separator();
        self.render_runner_output(ui);
    }

    fn render_scan_manifest_tab(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let running = self.runner.is_running();

        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.heading("scan（読み取り専用インデックス作成・差分スキャン）");
            ui.horizontal(|ui| {
                ui.label("対象 storage ID（空なら全ストレージ）:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.scan_params.storage).desired_width(180.0),
                );
                ui.checkbox(&mut self.scan_params.full, "--full（全件再ハッシュ）");
            });
            ui.horizontal(|ui| {
                ui.label("クールダウン: 毎");
                ui.add(
                    egui::DragValue::new(&mut self.scan_params.cooldown_every).range(0..=100_000),
                );
                ui.label("ファイルごとに");
                ui.add(egui::DragValue::new(&mut self.scan_params.cooldown_secs).range(0..=300));
                ui.label("秒待機   |   --max-files:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.scan_params.max_files)
                        .desired_width(90.0)
                        .hint_text("無制限"),
                );

                ui.add_enabled_ui(!running, |ui| {
                    if ui.button("scan 開始").clicked() {
                        let args = build_scan_args(&self.config_path, &self.scan_params);
                        self.run_cli(ctx, args);
                    }
                });
            });
        });

        ui.add_space(6.0);

        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.heading("マニフェスト連携 (export-manifest / import-manifest)");
            ui.horizontal(|ui| {
                ui.label("import 入力 JSONL:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.import_manifest_path)
                        .desired_width(300.0),
                );
                if ui.button("選択...").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("JSONL Manifest", &["jsonl", "json"])
                        .pick_file()
                    {
                        self.import_manifest_path = p.to_string_lossy().into_owned();
                    }
                }
                ui.label("--storage (任意):");
                ui.add(
                    egui::TextEdit::singleline(&mut self.import_storage_override)
                        .desired_width(140.0),
                );
                ui.add_enabled_ui(!running && !self.import_manifest_path.trim().is_empty(), |ui| {
                    if ui.button("import-manifest 実行").clicked() {
                        let args = build_import_manifest_args(
                            &self.config_path,
                            Path::new(self.import_manifest_path.trim()),
                            &self.import_storage_override,
                        );
                        self.run_cli(ctx, args);
                    }
                });
            });

            ui.horizontal(|ui| {
                ui.label("export storage ID:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.export_storage_id).desired_width(160.0),
                );
                ui.label("出力ファイル:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.export_output_path).desired_width(240.0),
                );
                if ui.button("保存先...").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("JSONL Manifest", &["jsonl"])
                        .set_file_name("manifest.jsonl")
                        .save_file()
                    {
                        self.export_output_path = p.to_string_lossy().into_owned();
                    }
                }
                ui.add_enabled_ui(
                    !running
                        && !self.export_storage_id.trim().is_empty()
                        && !self.export_output_path.trim().is_empty(),
                    |ui| {
                        if ui.button("export-manifest 実行").clicked() {
                            let args = build_export_manifest_args(
                                &self.config_path,
                                &self.export_storage_id,
                                Path::new(self.export_output_path.trim()),
                            );
                            self.run_cli(ctx, args);
                        }
                    },
                );
            });
        });

        ui.separator();
        self.render_runner_output(ui);
    }

    fn render_runner_output(&self, ui: &mut egui::Ui) {
        let (running, cmd_line, mut stdout, mut stderr, exit_status) = self.runner.snapshot();

        ui.horizontal(|ui| {
            if running {
                ui.spinner();
                ui.strong("実行中...");
                if ui.button("中断 (Cancel)").clicked() {
                    self.runner.cancel();
                }
            } else if let Some(st) = &exit_status {
                ui.strong(format!("結果: {st}"));
                if ui.button("出力クリア").clicked() {
                    self.runner.clear();
                }
            } else {
                ui.label("待機中（上のボタンから wingpv コマンドを実行できます）");
            }
        });

        if !cmd_line.is_empty() {
            ui.small(format!("$ {cmd_line}"));
        }

        let avail_h = ui.available_height().max(180.0);
        let half_w = (ui.available_width() - 16.0).max(200.0) * 0.62;
        let err_w = (ui.available_width() - 16.0 - half_w).max(160.0);

        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.strong("stdout（構造化出力・結果）");
                egui::ScrollArea::vertical()
                    .id_salt("stdout_scroll")
                    .max_height(avail_h - 24.0)
                    .stick_to_bottom(running)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut stdout)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(half_w)
                                .desired_rows(18)
                                .interactive(false),
                        );
                    });
            });

            ui.vertical(|ui| {
                ui.strong("stderr（進捗・ログ）");
                egui::ScrollArea::vertical()
                    .id_salt("stderr_scroll")
                    .max_height(avail_h - 24.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut stderr)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(err_w)
                                .desired_rows(18)
                                .interactive(false),
                        );
                    });
            });
        });
    }
}

impl eframe::App for MiniUiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.render_top_bar(ctx);
        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            ActiveTab::ConfigEditor => self.render_config_editor_tab(ui),
            ActiveTab::InspectQuery => self.render_inspect_query_tab(ctx, ui),
            ActiveTab::ScanManifest => self.render_scan_manifest_tab(ctx, ui),
        });
    }
}
