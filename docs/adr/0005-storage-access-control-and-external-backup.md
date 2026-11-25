# ADR-0005: ストレージの役割・アクセス権限・接続状態の分離と External Backup

- Status: Accepted
- Date: 2026-11-25

## 背景
写真バックアップには WING 自身が書き込む運用（Managed Backup）だけでなく、ランサムウェア対策として NAS 側が独立して Pull 型バックアップを行い Windows 側へ読み取り専用共有で公開する運用（External Backup）も重要である。また、外付けドライブや NAS の一時未接続（Offline）とファイル消失（Missing）を混同してはならない。

## 決定
ストレージに関して以下を独立した軸としてモデル化し、Core で強制する。
1. **役割（`tier`）**: `inbox` / `hot` / `archive` / `backup` / `quarantine`
2. **アクセス権限（`access_mode`）**: `managed` / `read_only`
3. **バックアップ種別（`backup_mode`）**: `none` / `managed` / `external`
4. **接続・健全状態（`status`）**: `online` / `offline` / `failed` / `retired`（ファイルの `present` / `missing` / `corrupted` とは分離）
5. **障害ドメイン（`failure_domain`）**: 同一物理ディスク・同一筐体内のコピーを独立した冗長性とみなさない

`read_only` または `external` バックアップのストレージに対しては、Core がすべての変更操作を拒否し、重複削除・隔離候補からも除外する。

## 代替案
1. **`tier = backup` のみで一律管理する**: WING が書き込めるバックアップと、OS/NAS 側で読み取り専用強制される外部バックアップを区別できず、誤操作や権限設計の曖昧さを招く。
2. **ファイル側の `presence = offline` で未接続を表す**: ストレージ単位の未接続と個別ファイルの消失・破損が混同され、復旧計画の精度が落ちる。

## 理由
- アプリ内の Read Only 設定だけに頼らず、NAS 側 SMB 読み取り専用共有＋ZFS スナップショット等の OS/インフラ層防御と自然に協調できる。
- 存在確認日時（`last_seen_at`）と整合性検証日時（`last_verified_at`）の分離と組み合わせ、オフライン媒体の健全性を安全に扱える。

## 影響
- `storages` スキーマと設定ファイル（`[[storages]]`）の項目が増えるが、安全性と運用表現力が大きく向上する。
