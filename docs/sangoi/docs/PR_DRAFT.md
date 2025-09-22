# feat: compact estimator, pins, and dry-run; snapshot scaffolding

## What
- Add provider‑agnostic token estimator and `CompactionReport` with human‑readable completion message
  - `codex-rs/core/src/compact/{estimate.rs,mod.rs}`
- Add pins + retention (first slice) and preserve pins across `/compact`
  - Core state: `pinned_message_ids`
  - Protocol ops: `Op::PinLast`, `Op::UnpinAll`
  - TUI: `/pin`, `/unpin`
- Add `/compact-dry-run` (preview compaction delta without changing history)
  - Protocol op: `Op::CompactDryRun`
  - TUI: `/compact-dry-run`
  - Core handler computes tail(1)+pins estimate and reports delta
- Snapshot scaffolding for PR 3
  - `codex-rs/core/src/compact/snapshot.rs` with `SummaryV1` + atomic `persist_snapshot_atomic(...)`
- Tests
  - Core E2E: `pinned_message_survives_compaction` (pins persist through `/compact`)
- Key files touched
  - Core: `core/src/codex.rs`, `core/src/compact/{estimate.rs,mod.rs,snapshot.rs}`
  - Protocol: `protocol/src/protocol.rs` (new `Op`s)
  - TUI: `tui/src/{slash_command.rs,chatwidget.rs}`
  - Tests: `core/tests/suite/compact.rs`

## Why
- Current `/compact` gives no quantitative feedback and can drop critical context.
- Pins enable user‑directed retention; dry‑run builds confidence by showing impact before committing.
- Snapshot scaffolding prepares for structured, durable state (`summary_v1`) in upcoming PR.

## How
- Estimator: simple, deterministic ≈4 chars/token heuristic over `ResponseItem` content; zero deps, provider‑agnostic.
- Compaction flow now:
  - Run existing summarize turn.
  - Rebuild history as pinned‑messages (from pre‑compact) + tail(1), with first‑text dedupe.
  - Emit completion line with before/after estimates and `%` when context window is known.
- Dry‑run:
  - Simulate tail(1)+pins; compute/report delta; no history mutation.
- Snapshot:
  - Implemented `SummaryV1` type + atomic writer (`*.tmp` → fsync → rename → fsync parent). Not wired yet.
- Protocol extended with non‑breaking enum variants; TUI dispatches corresponding ops.

## Tests
- Unit
  - Estimator basic cases; snapshot atomic write smoke test.
- Integration
  - `pinned_message_survives_compaction`: verifies pinned assistant message remains visible after `/compact`.
  - Full suites executed:
    - `codex-core`: 189 passed, 0 failed
    - `codex-tui`: 237 passed, 0 failed
- Snapshots: no intentional TUI snapshot changes required for this slice.

## Perf
- No hot‑path changes outside `/compact` turns and explicit dry‑run calls.
- Estimator is linear in history size and runs only when invoked.
- No measurable UI regressions expected; binaries unchanged aside from small code additions.

## Follow-ups
- PR 2 (rest): keep‑set by config/flags
  - `--keep-roles` (e.g., `tool,diff,decision`) and `--keep-files "<glob,glob>"`, wire into `/compact` keep‑set alongside pins.
- PR 3: snapshot synthesis
  - Strict system prompt to emit `SummaryV1`; persist via `persist_snapshot_atomic`; replace history with `[snapshot_as_system_message] + tail(N)` and keep pins.
- PR 4: hierarchical compaction + headroom guarantee
  - Anchor → cluster → synthesize → replace; ensure `min_headroom_tokens`.
- PR 5: auto‑compact + archive/restore
  - Trigger on threshold; archive full pre‑compact history to `.codex/history/<ts>.jsonl`; optional restore.

---

To open the PR manually after pushing the branch:

```bash
# If needed, set your git identity (already set locally as repo‑scoped):
# git config user.name "<your-name>"
# git config user.email "<your-email>"

# Push branch (if not already pushed)
git push -u origin feat/compact-pins-dryrun

# Then create the PR (choose web or gh CLI):
# Web: open the GitHub compare page for feat/compact-pins-dryrun → master and paste this body.
# gh CLI (if installed and authenticated):
# gh pr create --base master --head feat/compact-pins-dryrun \
#   --title "feat: compact estimator, pins, and dry-run; snapshot scaffolding" \
#   --body-file sangoi/docs/PR_DRAFT.md
```
