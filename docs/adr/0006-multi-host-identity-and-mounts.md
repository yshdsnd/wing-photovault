# ADR-0006: マルチホスト対応を見据えた永続識別子とホスト別マウントパス

- Status: Proposed
- Date: 2026-11-25

## 背景
主環境は Windows 11 PC だが、NAS（FreeBSD `eagle`、Linux `Saturn`）上でのローカルスキャンや、将来的な macOS からの参照・検索も視野に入っている。ドライブ文字（`Y:` 等）やホスト名は変更され得るため、これらを唯一の識別子にすると同一ストレージを別コピーと誤認したり、ホスト間で資産を突合できなくなる。

## 決定（提案）
1. **ローカル内部主キーと外部交換用永続 ID の分離**: `assets` 等にローカル `id`（SQLite `INTEGER`）とは別に外部交換用の永続 `public_id`（UUID 等）を持たせる。`contents` は `(hash_algo, hash_hex)` をグローバル識別子とする。
2. **Host と Storage の分離**: `hosts` テーブル（永続 `host_id`）と `storage_mounts` テーブル（`(storage_id, host_id) -> root_path`）を導入し、同一 NAS を Windows（`\\eagle\photos`）・FreeBSD（`/tank/photos`）・macOS（`/Volumes/photos`）から参照しても同一ストレージとして認識する。
3. **初期連携方式**: カタログのエクスポート / インポート（JSONL 等）を初期候補とし、リアルタイム同期・競合解決・削除通知・サムネイル転送は必要性に応じて別途決定する。

## 代替案
1. **`storages.root_path` 単一列のまま運用**: 単一 Windows PC では単純だが、NAS ローカルスキャン結果の取り込みや macOS 併用時にパス変換の特例処理が増える。
2. **最初から分散同期 DB を構築**: 複雑性が高く、Phase 1 のスコープを逸脱する。

## 理由
- 識別子とマウント構造だけを早い段階で整えておけば、将来のマルチホスト拡張を妨げずに Phase 1 の実装を小さく保てる。

## 影響
- 同期方式・競合解決・認証方式は未決定のため本 ADR は `Proposed` とし、Phase 1 の Migration v2 実装および NAS スキャン検証を通じて詳細を確定する。
