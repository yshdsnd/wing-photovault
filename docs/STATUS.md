# WING PhotoVault — 開発ステータス・引き継ぎメモ

- Last Updated: 2026-11-28
- Current Phase: **Phase 2（操作ジャーナル・整合性検証・リムーバブル取り込み・安全隔離）進行中**
- Branch: `main`（全 38 テスト合格 / `cargo clippy --workspace --all-targets -- -D warnings` 警告 0）

> 別マシン（bhyve ゲスト等）で作業を引き継ぐ際は、`git pull` のあと [GEMINI.md](../GEMINI.md) と本書を読んでから進めてください。

---

## 1. 実装済み機能（Phase 1）

### Core (`crates/wing-photovault-core`)
- 複数ストレージ定義（`[[storages]]`）：`media`（`nvme`/`ssd`/`hdd`/`nas`/`removable`）、`tier`、`access_mode`（`managed`/`read_only`）、`backup_mode`（`none`/`managed`/`external`）、`failure_domain`、`exclude_paths`
- Fail Closed 設定検証（`backup_mode = "external"` は `access_mode = "read_only"` を必須強制）
- ストリーミング BLAKE3 / SHA-256 ハッシュ＋部分ハッシュ（32-bit 安全 `u64`）
- ハイブリッド EXIF 解析（`MetadataSession`）:
  - **高速 Rust ネイティブ**: JPEG / CR2 / Canon CR3（ISOBMFF `moov -> uuid -> CMT1/CMT2` 直接走査、画像本体スキップ）＋ CRW 同伴 `.THM` ネイティブ読み取り
  - **常駐 ExifTool（`-stay_open True`）**: `backend = "auto"` 時、CRW・動画（MP4/MOV）・HEIF、旧型 CR2、および `LensModel` 欠落の Canon EOS CR3/JPEG（旧機種 JPEG 単体・Sigma 17-50mm 等のサードパーティ EF レンズ）のみ自動フォールバック（コンデジ・スマホ JPEG は除外）。`[exif.lens_aliases]` による表記正規化対応
- スキャナで macOS AppleDouble リソースフォーク（`._*`）を自動除外
- マニフェスト入力検証（`ScanManifestRecord::validate`: `..` パストラバーサル・不正ハッシュ拒否）

### DB (`crates/wing-photovault-db`)
- SQLite WAL（`rusqlite` bundled）＋前方マイグレーション（**Schema v4** 適用済み）
  - v1: `storages`, `assets`, `contents`, `content_metadata`, `scan_runs`, `files`
  - v2: `hosts`, `storage_mounts`, `storages` 権限/状態/障害ドメイン、`assets.public_id`（UUIDv7）、`files.last_seen_at` / `last_verified_at`、ストレージ状態（`online`/`offline`/`failed`/`retired`）とファイル存在状態（`present`/`missing`/`corrupted`）の完全分離
  - v3: `idx_files_storage_presence_stem(storage_id, presence, file_stem)` 複合インデックス（大規模ストレージでのサイドカー親 Asset 検索・RAW+JPEG 統合を O(log N) 化）
  - v4: `operation_journal` テーブル（コピー・取り込み・移動・隔離・復元・整合性検証の監査ログ兼クラッシュリカバリ用状態遷移テーブル）
- 差分スキャン（未変更ファイルは `last_seen_at` のみ更新し `last_verified_at` を保持）
- RAW + JPEG 論理 Asset 統合（同一フォルダ＋兄弟 `RAW/`・`JPEG/` フォルダ対応、曖昧時は非統合）
  - O(1) バッチ内ペアリング（途中中断してもコミット済み分は統合済み）＋スキャン末尾の冪等スイープ
- サイドカー認識・親 Asset 自動紐付け（`.xmp`, `.thm`, 二重拡張子 `.CR3.xmp` 対応、`FileRole::Sidecar`、偽 Asset 非生成・曖昧時 Fail Closed）＋動画（MP4/MOV/AVI）メタデータ（`duration_ms`, `fps`）検索出力対応
- マニフェストのストリーミング書き出し（`stream_storage_manifest`）

### CLI (`crates/wing-photovault-cli` / バイナリ名 `wingpv`)
- `init` / `scan`（3 ステージ有界キュー・ストリーミングパイプライン、`PauseGate` 付きクールダウン制御、`--storage`, `--full`, `--export-jsonl`, `--cooldown-every`, `--cooldown-secs`, `--max-files`）
- `export-manifest` / `import-manifest`（FreeBSD NAS ローカルスキャン → Windows DB 取り込み、`[exif.lens_aliases]` 自動適用）
- `normalize-lenses`（再スキャン不要で既存 DB の `lens_model_raw` から `[exif.lens_aliases]` を再適用、`--dry-run` 読み取り専用プレビュー対応）
- `link-derivatives`（Lightroom Classic 等のレタッチ済み／リサイズ書き出し JPEG を撮影日時 `captured_at`＋`subsec`/ファイル名/カメラ・シリアル互換ガードで元の論理 Asset に関連付け、`role = 'exported'` 付与・元 Asset ID 維持・曖昧時 Fail Closed・`--dry-run` 読み取り専用プレビュー対応）
- `verify` / `journal`（Phase 2-1: 実体ファイルのハッシュ再計算によるサイレントビットロット・欠損・外部変更検知、他ストレージ上の健全コピー有無の即時表示、`--older-than-days` / `--limit` / `--cooldown-*` / `--dry-run` 対応、異常の `operation_journal` 記録と `journal [--incomplete]` 閲覧）
- `status` / `stats`（`--percent`, `--by-year`, `--gear-history` 使用機材年表、`--cost-per-shot` 1ショット単価、`--split-extenders` テレコン分解集計、`--focal-dist` / `--focal-35mm` 焦点距離＆35mm換算分布ヒストグラム＋`[stats.camera_crop_factors]`、`--year`/`--camera`/`--lens` 絞り込み、`--export-timeline-svg` スタンドアロン SVG 年表出力、未バックアップ件数＆切り捨て%表示対応）/ `duplicates`（読み取り専用重複検出、`read_only`/`external` バックアップ保護、`--include-backups`/`--include-sidecars`/`--min-bytes`/`--limit` 対応）/ `find`（キーセットページング `--cursor`, `--page-info`、焦点距離/ISO/F値/パス/`--asset-id`/`--unbacked` フィルタ、ハッシュ検証付き安全コピー `--copy-to` + `--with-companions` + `--flat` + `--dry-run` 対応）/ `benchmark`（ディレクトリ実測＋`--db-scale <N>` 大規模ダミー DB ベンチマーク、`--format text|json|jsonl`、`stderr` リアルタイム進捗バー＋ETA）

### テスト用ミニ UI (`tools/wingpv-mini-ui` / バイナリ名 `wingpv-mini-ui`)
- Rust (`egui` / `eframe` + `rfd`) による単体実行 `.exe` ツール（ヘッドレス FreeBSD ビルドに影響しないようワークスペース `exclude` で分離しつつ、`wing-photovault-core` の型・Fail Closed 検証を共有）
- フォルダ選択付き `.toml` 設定エディタ（`external` → `read_only` 自動強制、`[exif.lens_aliases]` 編集、TOML プレビュー＆ラウンドトリップ検証）＋ `wingpv` CLI テスト実行（`init`/`status`/`stats`/`find`/`normalize-lenses`/`scan`/`export-manifest`/`import-manifest`、非同期リアルタイム出力・中断対応）

---

## 2. 実データでの確認済みベンチマーク・解析結果

- **NVMe SSD（Canon EOS R5m2: CR3 + JPEG）**:
  - 200 ファイル / 8.11 GB: コールド BLAKE3 **3,540 MB/s**、ウォーム BLAKE3 **36.2 GB/s** / SHA-256 **33.2 GB/s**
  - Native CR3 EXIF: **92,000〜97,000 files/s**（レンズ名・焦点距離・F値・SS・ISO すべてネイティブ抽出）
  - 322 ファイル / 13.73 GB 初回フルスキャン（DB 登録込み）: **1.08〜2.83 秒**、2回目差分スキャン: **2 ms**、RAW+JPEG 統合: 322 files → 161 Logical Assets
- **HDD（Canon EOS 7D Mark II / 6D Mark II / 20D CR2 + EOS 10D CRW）**:
  - `hdd_io_workers = 1` で **115〜173 MB/s**（シーク抑制確認済み）、途中中断からのバッチ再開＆バッチ内ペアリング動作確認済み
  - 常駐 ExifTool フォールバックにより EOS 10D（CRW 284枚）＋ EOS 20D（CR2 449枚）のカメラ名・焦点距離レンジまで全件抽出確認済み

---

## 3. 進行中・次のタスク候補

1. **FreeBSD NAS ローカルスキャン実証（進行中）**（手順: [docs/nas-local-scan-freebsd.md](nas-local-scan-freebsd.md)）
   - FreeBSD 側での 39.5 万ファイルスキャン完走 → `export-manifest` → Windows 側 `import-manifest` で `Backed-up Contents`・24 年分使用機材年表（`stats --percent --by-year --gear-history`）確認
2. **Phase 2 準備**（Phase 1 実装系タスクは完了済み）
   - `duplicates` コマンド（サイズ→部分ハッシュ→全体ハッシュ、Backup / ReadOnly 除外）
   - 操作ジャーナル（`operation_journal`）・隔離（`quarantine`）・復元（`restore`）・`--dry-run`

---

## 4. 2 台運用時の注意

- 作業開始前に `git pull`、区切りごとに `cargo clippy --workspace --all-targets -- -D warnings`・`cargo test --workspace` を通してから `git commit && git push` する。
- ローカル専用設定（`wing-photovault.toml`, `test-*.toml`）と DB（`*.db*`）は `.gitignore` 対象のため、各マシンで個別に配置して運用する。
