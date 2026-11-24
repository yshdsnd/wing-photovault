# WING PhotoVault

大量の写真・動画の取り込み、選別、保存、重複排除、階層管理（SSD / HDD / NAS）、バックアップ、検索、閲覧までのライフサイクルを効率化するための写真資産管理ソフトウェアです。

航空機や野鳥の撮影など、1回の撮影で数千〜数万枚の RAW / JPEG / 動画を扱うユースケースを想定しています（RAW現像・画像編集ソフトの代替は目的としていません）。

> WING is not an acronym. It is a concept.
>
> PhotoVault keeps your memories safe. WING gives them wings.

## 構成

| コンポーネント | 正式名称 | 略称 / 識別名 | 役割 |
|---|---|---|---|
| GUIフロントエンド | WING PhotoVault UI | WING-UI | 検索・閲覧・連写選別・統計表示・管理操作 |
| CLI | WING PhotoVault CLI | WING-CLI (`wingpv`) | 管理操作のCLI・構造化出力（JSON / JSONL）・定期実行 |
| コア / バックエンド | WING PhotoVault Core | WING-Core | スキャン、ハッシュ、EXIF抽出、DB管理、安全な移動・隔離・バックアップ |

## 主な特徴（予定）

- **Safety First**: オリジナルを失わないことを最優先とし、ジャーナル管理・コピー後検証・直接削除しない隔離（Quarantine）を採用
- **論理資産と物理ファイルの分離**: RAW+JPEG や NAS バックアップを論理的な1枚の写真（Asset）として管理し、移動後も評価・タグ・連写グループを維持
- **高速な処理**: Rust によるネイティブ実装、ストレージ種別（NVMe SSD / HDD / NAS）に応じた並列I/O制御、SQLite WAL によるインデックス管理
- **GUI と CLI の分離**: `wingpv` CLI 単体で検索・統計・定期バックアップが完結し、WING-UI は CLI 経由で安全に連携

## 現在のステータス

**設計・仕様策定中（Draft）** です。仕様は開発を進めながら見直し・調整を行います。

詳細は [docs/WING-PhotoVault-spec.md](docs/WING-PhotoVault-spec.md) を参照してください。

## ライセンス

[BSD 2-Clause License](LICENSE)
