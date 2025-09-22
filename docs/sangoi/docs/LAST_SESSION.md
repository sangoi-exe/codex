# Last Session Summary — Compact Roadmap Progress

This document captures exactly what was implemented in this session, how to validate it, and the logical next steps to continue the COMPact roadmap in the next session. All paths below are relative to `work/codex`.

---

## What Changed (today)

- Token estimator and compaction report (PR 1 scope)
  - Added a provider‑agnostic token estimator and a small `CompactionReport`:
    - `codex-rs/core/src/compact/estimate.rs`
    - `codex-rs/core/src/compact/mod.rs` (exports + completion message formatter)
  - The `/compact` flow now prints an estimated before/after delta and, when the model context window is known, the remaining percentage before and after compaction.

- Pins and retention (PR 2, first slice)
  - Protocol: new operations
    - `Op::PinLast` — pin the most recent message that still retains a provider id.
    - `Op::UnpinAll` — clear all pins.
  - Core session state
    - `pinned_message_ids: HashSet<String>` added to session `State`.
  - Compaction behavior
    - After the existing summarize step, history is rebuilt as: pinned messages from pre‑compaction history + the last message (tail(1)), with dedupe by first text content to avoid duplicates.
  - TUI slash commands
    - `/pin` → sends `Op::PinLast`.
    - `/unpin` → sends `Op::UnpinAll`.
    - Files: `codex-rs/tui/src/slash_command.rs`, `codex-rs/tui/src/chatwidget.rs`.

- Dry‑run compaction (controller‑level, preview only)
  - Protocol: `Op::CompactDryRun`.
  - TUI: `/compact-dry-run`.
  - Core handler computes before/after estimates as if we kept tail(1)+pins, and prints a report; it does not modify history.

- Snapshot scaffolding (PR 3 prep)
  - `codex-rs/core/src/compact/snapshot.rs` with:
    - `SummaryV1` struct matching the plan’s JSON wire schema.
    - `persist_snapshot_atomic(codex_home, snapshot)` with atomic write (`*.tmp` → fsync → rename → fsync parent).
  - Exported via `compact::mod.rs`. Currently staged (emits an unused‑exports warning) to be wired in PR 3.

- Tests
  - Core E2E additions: `pinned_message_survives_compaction`:
    - Verifies that a pinned assistant message remains visible to the next request after `/compact`.
    - File: `codex-rs/core/tests/suite/compact.rs`.

---

## Validation Performed

- Formatting
  - `cargo fmt --all`

- Tests (local run)
  - `cargo test -p codex-core` → 189 passed, 0 failed
  - `cargo test -p codex-tui` → 237 passed, 0 failed
  - Notes: a few long‑running integration tests (“running for over 60 seconds”) are expected; they completed successfully.

- Warnings
  - Unused exports in `compact::snapshot` (staged for PR 3).
  - Safe to ignore until we wire snapshot synthesis + persistence.

---

## Usage Notes (new capabilities)

- Pinning critical context
  - Type `/pin` to pin the most recent message that still has a provider id.
  - Type `/unpin` to clear all pins.

- Compaction
  - `/compact` now prints: `Compaction complete: ~X → ~Y tokens; saved ~Z; remaining A% → B%` when context window is known.
  - Pinned messages are preserved across compaction.

- Dry‑run
  - `/compact-dry-run` prints the same delta without altering history.

---

## How To Proceed (next session)

The plan continues through PR 2 (remaining keep‑set features), PR 3 (snapshot synthesis), PR 4 (hierarchical compaction proper), and PR 5 (auto‑compact + archive/restore). Below is the recommended order and concrete tasks/files.

1) PR 2 — Keep‑set by role and file globs
- Goal: Honor `--keep-roles` and `--keep-files` settings alongside pins.
- Tasks
  - Config surface
    - Add config keys under `[compact]` to `codex-rs/core/src/config.rs` and config types in `config_types.rs`.
    - Consider CLI flags for TUI or Exec modes (if required now) — or defer to config‑only for first pass.
  - Keep‑set integration
    - In `/compact` flow, compute a keep‑set = pins + messages matching roles + messages referencing files matching globs.
    - Where to wire: `codex-rs/core/src/codex.rs` (inside `run_compact_task` after model completes and before rebuilding history).
  - Tests
    - Unit: role parsing, glob matching.
    - Integration: feed conversation with tool/diff/decision and assert those survive compaction.

2) PR 3 — Snapshot synthesis (SummaryV1)
- Goal: Replace “one big summary message” with a structured snapshot stored to disk and a short recent tail in history.
- Tasks
  - Prompting
    - Add a strict system prompt (see sangoi/plans/COMPACT.md §14) and a helper `synthesize_snapshot(session, clusters)`.
    - Location: `codex-rs/core/src/compact/snapshot.rs` (new function) and a `prompt_for_snapshot.md` file (similar to existing `prompt_for_compact_command.md`).
  - Persistence
    - Use `persist_snapshot_atomic(codex_home, &snapshot)` to write `.codex/session.json`.
  - History replacement
    - Replace transcript in memory with `[snapshot_as_system_message] + tail(N)`; keep any pins.
  - Tests
    - Validate `SummaryV1` JSON structure, minimal required fields present.
    - Snapshot survives reload (read session.json back and assert content).

3) PR 4 — Hierarchical compaction + headroom guarantee
- Goal: Implement anchor → cluster → synthesize → replace, and guarantee `min_headroom_tokens`.
- Tasks
  - Clustering module (time/topic windows) and a loop that collapses oldest clusters until headroom is met.
  - Plumb `--max-tail` and `--min-headroom` (config fallback).
  - Tests: headroom loop, “recall after compaction” with snapshot present.

4) PR 5 — Auto‑compact + archive/restore
- Goal: Preemptively compact when usage exceeds threshold.
- Tasks
  - Track token usage percent vs. configured threshold.
  - On trigger, run compaction flow; write archive to `.codex/history/<ts>.jsonl`.
  - Optional: `/history restore <ts>`.
  - Tests: trigger path and archive readability.

---

## Minimal Dev Checklist (next session)

- Build & test
  - `cargo fmt --all`
  - `cargo test -p codex-core`
  - `cargo test -p codex-tui`
  - Optionally: `cargo clippy -- -D warnings` (after snapshot wiring to eliminate the staged warnings)

- Manual checks
  - Run TUI and try `/pin`, `/compact-dry-run`, `/compact`, `/unpin` on a short conversation.

- PR hygiene (from AGENTS.md)
  - One concern per PR; keep diffs surgical.
  - If you create a PR, use messages like `feat: compact pins + dry-run` and fill the PR template (What/Why/How/Tests/Perf).

---

## File Map (touched today)

- Core
  - `codex-rs/core/src/compact/estimate.rs`
  - `codex-rs/core/src/compact/mod.rs`
  - `codex-rs/core/src/compact/snapshot.rs`
  - `codex-rs/core/src/codex.rs` (handlers for Compact, CompactDryRun, PinLast, UnpinAll; compaction rebuild logic; reporting)
- Protocol
  - `codex-rs/protocol/src/protocol.rs` (new `Op` variants)
- TUI
  - `codex-rs/tui/src/slash_command.rs` (added `/pin`, `/unpin`, `/compact-dry-run`)
  - `codex-rs/tui/src/chatwidget.rs` (dispatch ops)
- Tests
  - `codex-rs/core/tests/suite/compact.rs` (added `pinned_message_survives_compaction`)

---

## Known Constraints / Non‑Goals (unchanged)

- Snapshot is not yet synthesized from the model (staged for PR 3).
- Keep‑files/keep‑roles flags not yet present (next PR 2 slice).
- No changes were made to sandbox env var logic (`CODEX_SANDBOX_*`).

---

## Quick Start (next session)

1) Pull latest and build
- `cargo fmt --all`
- `cargo test -p codex-core -p codex-tui`

2) Pick the next PR target
- For fastest progress, start with PR 2 keep‑set flags, then PR 3 snapshot synthesis.

3) Implementation pointers
- Keep‑set: add config + helpers; wire into `run_compact_task` before rebuilding history.
- Snapshot: add a new prompt file and a `synthesize_snapshot(..)` function; call it during `/compact` and persist via `persist_snapshot_atomic`.

This should be everything needed to resume quickly next time.

