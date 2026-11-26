# WING PhotoVault — 開発ステータス・引き継ぎメモ

- Last Updated: 2026-11-26
- Current Phase: **Phase 1（読み取り専用インデックス・検索・統計・ベンチマーク・NAS マニフェスト連携）**
- Branch: `main`（全 14 テスト合格 / `cargo clippy --workspace --all-targets -- -D warnings` 警告 0）

> 別マシン（bhyve ゲスト等）で作業を引き継ぐ際は、`git pull` のあと [GEMINI.md](../GEMINI.md) と本書を読んでから進めてください。

---

## 1. 実装済み機能（Phase 1）

### Core (`crates/wing-photovault-core`)
- 複数ストレージ定義（`[[storages]]`）：`media`（`nvme`/`ssd`/`hdd`/`nas`/`removable`）、`tier`、`access_mode`（`managed`/`read_only`）、`backup_mode`（`none`/`managed`/`external`）、`failure_domain`、`exclude_paths`
- Fail Closed 設定検証（`backup_mode = "external"` は `access_mode = "read_only"` を必須強制）
- ストリーミング BLAKE3 / SHA-256 ハッシュ＋部分ハッシュ（32-bit 安全 `u64`）
- ハイブリッド EXIF 解析（`MetadataSession`）:
  - **高速 Rust ネイティブ**: JPEG / CR2 / Canon CR3（ISOBMFF `moov -> uuid -> CMT1/CMT2` 直接走査、画像本体スキップ）＋ CRW 同伴 `.THM` ネイティブ読み取り
  - **常駐 ExifTool（`-stay_open True`）**: `backend = "auto"` 時、CRW・動画（MP4/MOV）・HEIF および旧型 CR2（EOS 20D 等の `LensModel` 欠落ファイル）のみ自動フォールバック。`[exif.lens_aliases]` による表記正規化対応
- マニフェスト入力検証（`ScanManifestRecord::validate`: `..` パストラバーサル・不正ハッシュ拒否）

### DB (`crates/wing-photovault-db`)
- SQLite WAL（`rusqlite` bundled）＋前方マイグレーション（**Schema v2** 適用済み）
  - v1: `storages`, `assets`, `contents`, `content_metadata`, `scan_runs`, `files`
  - v2: `hosts`, `storage_mounts`, `storages` 権限/状態/障害ドメイン、`assets.public_id`（UUIDv7）、`files.last_seen_at` / `last_verified_at`、ストレージ状態（`online`/`offline`/`failed`/`retired`）とファイル存在状態（`present`/`missing`/`corrupted`）の完全分離
- 差分スキャン（未変更ファイルは `last_seen_at` のみ更新し `last_verified_at` を保持）
- RAW + JPEG 論理 Asset 統合（同一フォルダ＋兄弟 `RAW/`・`JPEG/` フォルダ対応、曖昧時は非統合）
  - O(1) バッチ内ペアリング（途中中断してもコミット済み分は統合済み）＋スキャン末尾の冪等スイープ
- マニフェストのストリーミング書き出し（`stream_storage_manifest`）

### CLI (`crates/wing-photovault-cli` / バイナリ名 `wingpv`)
- `init` / `scan`（`--storage`, `--full`, `--export-jsonl`, `--cooldown-every`, `--cooldown-secs`, `--max-files`）
- `export-manifest` / `import-manifest`（FreeBSD NAS ローカルスキャン → Windows DB 取り込み）
- `status` / `stats` / `find` / `benchmark`（`--format text|json|jsonl`、`stderr` リアルタイム進捗バー＋ETA）

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

1. **FreeBSD NAS ローカルスキャン実証**（手順: [docs/nas-local-scan-freebsd.md](nas-local-scan-freebsd.md)）
   - FreeBSD 側でのビルド・インターバル付きスキャン完走 → `export-manifest` → Windows 側 `import-manifest` で `Backed-up Contents` 確認
2. **Phase 1 残タスク（大規模・品質仕上げ）**
   - `find` のキーセットページング対応（数十万〜数百万件向け）
   - `wingpv benchmark --db-scale <N>`（1万〜100万件のダミー DB 検索・集計ベンチマーク）
   - 走査〜ハッシュ/EXIF〜DB 書込の有界キューによるストリーミングパイプライン化（全件 `Vec` 保持の解消）
   - 動画（MP4/MOV）のメタデータ取得確認、`.xmp` / `.thm` サイドカー認識の土台
3. **Phase 2 準備**
   - `duplicates` コマンド（サイズ→部分ハッシュ→全体ハッシュ、Backup / ReadOnly 除外）
   - 操作ジャーナル（`operation_journal`）・隔離（`quarantine`）・復元（`restore`）・`--dry-run`

---

## 4. 2 台運用時の注意

- 作業開始前に `git pull`、区切りごとに `cargo clippy --workspace --all-targets -- -D warnings`・`cargo test --workspace` を通してから `git commit && git push` する。
- ローカル専用設定（`wing-photovault.toml`, `test-*.toml`）と DB（`*.db*`）は `.gitignore` 対象のため、各マシンで個別に配置して運用する。
