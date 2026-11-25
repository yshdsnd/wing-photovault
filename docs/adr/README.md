# Architecture Decision Records (ADR)

WING PhotoVault の主要な設計判断と「なぜその方針を選んだか」を記録する。

## 運用ルール
- 各 ADR には **状態（Status）・背景・決定・代替案・理由・影響** を記載する。
- 未決定事項や検証待ちの方針は `Proposed` とし、合意・検証前に `Accepted` にしない。
- 仕様書（[../WING-PhotoVault-spec.md](../WING-PhotoVault-spec.md)）は「何を実現するか」、ADR は「なぜ選んだか」、[../../GEMINI.md](../../GEMINI.md) は「開発時に守ること」を担う。

## 一覧

| ID | タイトル | Status | Date |
|---|---|---|---|
| [0001](0001-rust-core-and-cli.md) | Rust による WING-Core / WING-CLI の実装 | Accepted | 2026-11-25 |
| [0002](0002-ui-core-separation.md) | WING-UI と WING-Core / WING-CLI の分離 | Accepted | 2026-11-25 |
| [0003](0003-asset-content-file-storage-model.md) | 論理資産・内容・物理ファイル・ストレージの分離 | Accepted | 2026-11-25 |
| [0004](0004-local-sqlite-catalog.md) | ローカル SSD 上の SQLite WAL カタログ採用と NAS 直接共有の禁止 | Accepted | 2026-11-25 |
| [0005](0005-storage-access-control-and-external-backup.md) | ストレージの役割・アクセス権限・接続状態の分離と External Backup | Accepted | 2026-11-25 |
| [0006](0006-multi-host-identity-and-mounts.md) | マルチホスト対応を見据えた永続識別子とホスト別マウントパス | Proposed | 2026-11-25 |
| [0007](0007-security-by-design.md) | Security by Design と Core 不変条件（Fail Closed） | Accepted | 2026-11-25 |
| [0008](0008-performance-and-scalability.md) | 3つの性能軸・パイプライン・有界キュー・計測駆動 | Accepted | 2026-11-25 |
| [0009](0009-no-virtual-filesystem.md) | 仮想ファイルシステム（WinFsp / Dokan 等）を当面採用しない | Accepted | 2026-11-25 |
