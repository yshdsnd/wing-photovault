# ADR-0001: Rust による WING-Core / WING-CLI の実装

- Status: Accepted
- Date: 2026-11-25

## 背景
WING PhotoVault は、数TB〜数十TB・数十万〜数百万枚の写真/動画を対象に、高速なスキャン・ハッシュ計算・EXIF 抽出・安全なファイル操作を行う必要がある。また Windows 11 x64 を主対象としつつ、NAS ホスト（FreeBSD 15.x や Linux）上でのローカルスキャン実行も想定している。

## 決定
コアロジック（`wing-photovault-core`）、DB 層（`wing-photovault-db`）、CLI（`wing-photovault-cli` / `wingpv`）を **Rust Stable** で実装する。原則として `#![forbid(unsafe_code)]` とし、同期 I/O とワーカースレッドを基本とする。

## 代替案
1. **Python のみで実装**: 開発速度は速いが、大量ファイルの並列ハッシュ・EXIF 解析・低メモリ制御で性能限界が出やすく、NAS（FreeBSD/Linux）への単一バイナリ配布も重い。
2. **C++ / C#**: C++ はメモリ安全性・依存管理コストが高い。C#/.NET は Windows 親和性が高いが、FreeBSD / 旧 Linux NAS での軽量実行に難がある。
3. **Go**: 配布性と並列性は優秀だが、型による厳格な不変条件表現や SIMD 最適化済みハッシュ（BLAKE3）等のエコシステムでは Rust が優位。

## 理由
- ネイティブ性能・低メモリ消費・データ競合をコンパイル時に防ぐ型安全性。
- `rusqlite`（bundled）や `blake3` により、外部ランタイム不要の単一バイナリとして Windows / FreeBSD / Linux へ展開しやすい。

## 影響
- ビルド時間や初期学習コストは Python より増えるが、コアの安全性・性能・移植性の基盤が強固になる。
- Rust だけでデータ消失は防げないため、ジャーナル・検証・テストを別途徹底する。
