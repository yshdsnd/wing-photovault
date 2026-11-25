# WING PhotoVault — Development Instructions (`GEMINI.md`)

This file is the canonical source of project-wide development rules, invariants, and architectural constraints for WING PhotoVault.

## 1. Project Overview & Canonical Docs

- **Product Name**: WING PhotoVault (BSD 2-Clause, personal open-source project)
  - GUI: **WING PhotoVault UI** (`WING-UI`)
  - CLI: **WING PhotoVault CLI** (`WING-CLI`, binary: `wingpv`)
  - Core: **WING PhotoVault Core** (`WING-Core`)
- **Crates**: `wing-photovault-core`, `wing-photovault-db`, `wing-photovault-cli`
- **Canonical Documentation**:
  - Specification ("What"): `docs/WING-PhotoVault-spec.md`
  - Database Schema: `docs/db-schema.md`
  - Architecture Decision Records ("Why"): `docs/adr/README.md` (`docs/adr/0001-*.md` ...)
- **Tone & Claims**: This is a personal free OSS project. Never add enterprise SLA, warranty, or commercial support claims to docs or README.

## 2. Core Safety Invariants (Non-Negotiable)

Every change in Core, DB, CLI, and future UI/API must uphold these invariants:

1. **Never lose originals**: Never delete a source file before the destination is written, flushed, and verified (size + hash) and journaled.
2. **Never overwrite differing content**: If a destination exists with different content, refuse to overwrite.
3. **Fail Closed**: When storage status, access permissions, or file state is unknown, reject mutating operations.
4. **Respect `read_only` and `external` backups**: Never modify, move, delete, or nominate for duplicate quarantine any file on a `read_only` or `external` backup storage, or outside configured managed roots.
5. **Offline is NOT missing**: Never mark files on an inaccessible/offline storage as `missing` or deleted. Keep storage status (`online`/`offline`/`failed`/`retired`) strictly separate from file presence (`present`/`missing`/`corrupted`).
6. **Preserve Asset identity**: Physical moves, tier changes, and restores must preserve `Asset ID` (and external `public_id`), ratings, tags, and burst membership.
7. **Dry-run purity**: `--dry-run` must never mutate photo assets or persistent management state.
8. **Never test destructively on real photo assets**: Always use temporary dummy files (`tempfile`) for tests.

## 3. Architecture & Coding Rules

- **No `unsafe` Rust**: Keep `#![forbid(unsafe_code)]` across workspace crates unless explicitly approved via ADR.
- **UI/Core separation**: `WING-UI` must never directly mutate the SQLite database; all mutations go through `WING-Core` / `WING-CLI`.
- **Local SQLite WAL only**: SQLite WAL DBs live on local SSDs. Never design around sharing a SQLite WAL file directly over NAS (SMB/NFS).
- **32-bit portability**: Keep `wing-photovault-core` and scanner code portable across Windows x64, FreeBSD, macOS, and 32-bit Linux (`armv7l`). Always use `u64` (never `usize`) for file sizes and byte offsets, and isolate OS-specific APIs in `platform` modules.
- **Performance discipline**:
  - Control I/O concurrency per physical device media type (NVMe vs HDD vs NAS); avoid over-parallelizing HDDs.
  - Prefer streaming I/O, bounded queues, batch SQLite transactions, and keyset paging over loading entire datasets into memory.
  - Do not adopt durability-compromising SQLite settings (e.g. `synchronous = OFF`) without explicit measurement and approval.
- **Scope discipline**: Keep changes small and focused. Distinguish "keeping room for future extension" from "prematurely implementing future-phase features". Do not expand Phase 1 scope unnecessarily.

## 4. Security by Design Rules

- **SQL Injection**: Use parameterized queries only. Any dynamic SQL identifiers must be validated against an explicit allowlist.
- **Command Injection**: Never spawn external processes (`exiftool`, external photo editors, `robocopy`, etc.) via shell string concatenation. Always pass argument arrays directly, and guard user/file path arguments against option injection (e.g., `--`).
- **Path Safety**: Enforce managed storage root boundaries; do not follow symlinks or Windows reparse points by default; handle TOCTOU safely during file moves.
- **Untrusted Inputs**: Treat image files, EXIF, CR3 boxes, XMP sidecars, and imported catalogs as untrusted inputs with explicit size, count, and loop bounds.
- **Network Listeners**: Disabled by default. Never implement custom cryptography.
- **Least Privilege**: Do not require Administrator / root privileges for normal operation.

## 5. Documentation, ADR & Verification Workflow

- **Undecided items stay Proposed**: Never mark unverified or undecided design choices as `Accepted` in ADRs or specs.
- **Sync docs with code**: When changing schema, CLI contracts, or invariants, update `docs/db-schema.md`, `docs/WING-PhotoVault-spec.md`, and relevant ADRs accordingly.
- **Before committing**:
  - Run `cargo test --workspace` and verify all tests pass (including invariant tests).
  - Run `cargo clippy --workspace --all-targets` and resolve warnings.
  - Keep `stdout` clean for structured output (`--format json|jsonl`); emit logs only to `stderr`.
- **AI Review**: Treat AI review as an aid across security, data-loss, recovery, concurrency, performance, and ADR compliance—never as a substitute for deterministic tests.
