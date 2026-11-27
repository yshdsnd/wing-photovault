# WING PhotoVault Mini UI (`wingpv-mini-ui`)

Windows 等で `.toml` 設定ファイルを GUI で簡単に生成・編集し、`wingpv` CLI をテスト実行するための軽量プロトタイプ UI（`egui` / `eframe`）です。

- 本番ワークスペース（`core` / `db` / `cli`）からは `Cargo.toml` の `exclude` で分離しているため、FreeBSD NAS 等のヘッドレス環境でのビルドには影響しません。
- SQLite DB を直接更新せず、すべての操作を `wingpv` CLI の引数配列実行経由で行います（UI/Core 分離）。
- `#![forbid(unsafe_code)]` 準拠。

## ビルド・起動方法（Windows）

```powershell
# 1. CLI (wingpv.exe) をビルド
cargo build --release

# 2. Mini UI (wingpv-mini-ui.exe) をビルド
cargo build --release --manifest-path tools/wingpv-mini-ui/Cargo.toml

# 起動（ダブルクリック可）
.\tools\wingpv-mini-ui\target\release\wingpv-mini-ui.exe
```

## 主な機能
1. **設定エディタ (`.toml`)**: フォルダ選択ダイアログによる `[[storages]]` 追加、`external` バックアップ時の `read_only` 自動強制（Fail Closed）、`[exif.lens_aliases]` 編集、リアルタイム TOML プレビュー＆検証付き保存
2. **状態・統計・検索**: `init` / `status` / `stats` / `normalize-lenses --dry-run`（および確認付き実適用）/ `find` のテスト実行
3. **スキャン・マニフェスト**: `scan`（クールダウン・`--max-files` 対応）/ `export-manifest` / `import-manifest` のバックグラウンド実行と `stdout` / `stderr` リアルタイム表示・中断
