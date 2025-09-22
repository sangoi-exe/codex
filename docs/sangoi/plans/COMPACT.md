# ROADMAP.md — Compact without amnesia

Design and implementation plan to replace the current `/compact` behavior with **hierarchical compaction + structured snapshot**, preserving working memory and preventing “context lobotomy.”

> Artifacts: English-only. User may speak Portuguese; translate intent, keep code/docs in English.

---

## 1) Scope

Deliver a robust `/compact` that:
- Preserves key project state via a **structured snapshot** instead of a monolithic paragraph.
- Supports **pins** (messages and files that must never be collapsed).
- Provides **predictable UX**: `--dry-run`, clear completion message, and progress.
- Works even when the provider omits `token_usage` via a **local token estimator**.
- Adds **auto-compact** before context exhaustion to avoid late, lossy compaction.
- Stores durable state out of chat (`.codex/session.json`) and archives history.

**Non-goals**
- Model-specific optimization beyond generic token budgeting.
- Broad refactors of unrelated TUI features.
- Changing sandbox/network policies or environment var logic.

---

## 2) Problem statement (today’s `/compact`)

- Monolithic summary erases **decisions, constraints, TODOs, file/symbol map**.
- No **pinning** or retention policy; critical diffs disappear.
- Manual, late usage; triggers after context is already overloaded.
- Depends on `token_usage`; breaks when providers omit it.
- UX is opaque: no preview, no clear “done,” no headroom guarantee.

---

## 3) Design overview

**Core idea**: replace “one big summary” with a **snapshot JSON** plus a short recent tail of conversation.

- **Structured snapshot (`summary_v1`)** persisted to `.codex/session.json`.
- **Hierarchical compaction** with phases (anchor → cluster → synthesize → replace).
- **Retention policies** via pins, roles, and file globs.
- **Estimator** used when provider lacks `token_usage`.
- **Auto-compact** threshold (default 85%) with a minimum headroom (default 2048 tokens).
- **Archival** of full pre-compaction history to `.codex/history/<ts>.jsonl` and restore points.

---

## 4) Data model: `summary_v1` (wire schema)

```json
{
  "task": "one-sentence current objective",
  "decisions": ["decision A", "decision B"],
  "constraints": ["limit X", "policy Y"],
  "open_questions": ["question 1", "question 2"],
  "todo": ["step 1", "step 2"],
  "files_in_scope": [{"path": "src/foo.rs", "why": "reason"}],
  "symbols": [{"name": "parse_user", "file": "src/auth.rs", "role": "fn"}],
  "env": {"os": "win11", "toolchain": "rust stable", "node": "22.x"},
  "assumptions": ["assumption A"],
  "known_failures": ["flaky test T123"],
  "last_compact_at": "ISO-8601 timestamp"
}
````

**Rules**

* JSON only; no comments.
* Keep names and API flags verbatim.
* Prefer concrete file paths and symbol names.

---

## 5) Algorithm (hierarchical compaction)

1. **Anchor**

   * Build a **keep set** = pins + messages with roles `tool`, `diff`, `decision` + file-glob matches.
2. **Cluster**

   * Group remaining messages by topic/session segment to summarize coherently.
3. **Synthesize snapshot**

   * Call model with a strict **system prompt** to emit `summary_v1` only.
4. **Replace**

   * New history = `[snapshot_as_system_msg] + tail(N)` of most recent relevant messages + pinned messages (already included).
5. **Headroom**

   * Ensure `min_headroom_tokens` after compaction; if not, collapse oldest clusters further until met.

---

## 6) UX & CLI/TUI

**Commands**

* `/compact --dry-run [--keep-files "<glob,glob>"] [--keep-roles "tool,diff,decision"] [--max-tail 12] [--min-headroom 2048]`
* `/compact` (execute with the current or configured options)
* `/pin <id|id..id>` and `/unpin <id|id..id>`
* `/history restore <timestamp>` (optional follow-up PR)

**Dry-run output**

* Show token delta, kept/archived counts, and a preview of items affected.

**Completion message**

* `Compaction complete: 42k → 11k tokens; kept 12; archived 98; headroom 2,048`

**Progress**

* Deterministic steps with a progress bar (e.g., `indicatif`), gated behind `--verbose`.

---

## 7) Persistence

* Snapshot: `.codex/session.json` (overwrites atomically).
* Archive: `.codex/history/<timestamp>.jsonl` (full pre-compaction log).
* Atomic writes: `*.tmp` → `fsync` → `fsync` parent → `rename`.

---

## 8) Token estimation fallback

When `token_usage` is absent:

* **Preferred**: local tokenizer (feature-gated, e.g., `tiktoken-rs`) with a graceful fallback.
* **Fallback**: heuristic estimate (e.g., chars/4) for headroom decisions.
* Estimator scoped behind `--features token-estimator` for portability.

---

## 9) Config (`~/.codex/config.toml`)

```toml
[compact]
mode = "hierarchical"
min_headroom_tokens = 2048
max_tail_messages = 12
keep_roles = ["tool","diff","decision"]
keep_files_glob = ["AGENTS.md","src/**","tests/**"]
auto_compact_threshold_pct = 0.85
```

Runtime flags override config for the current session.

---

## 10) Safety & constraints

* Do not touch sandbox env logic:

  * `CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR`
  * `CODEX_SANDBOX_ENV_VAR`
* No destructive resets.
* English-only artifacts.
* Atomic file operations; preserve EOL/encoding.

---

## 11) Test plan

**Unit**

* `estimate_tokens` with and without provider usage.
* Pin policies: `--keep-roles`, file globs, `/pin` ranges.
* Snapshot synthesis: valid JSON, required fields present.
* Tail selection and headroom guarantee.

**Integration**

* “Recall after compaction”:

  * Given decisions/files before compaction, agent can still answer queries after compaction.
* Dry-run preview correctness.
* Auto-compact at threshold; no thrash.

**TUI snapshot tests (insta)**

* New messages (dry-run report, completion message) golden-tested.
* Deterministic formatting.

**Failure modes**

* Provider returns malformed output → error-first messages, no state corruption.
* Disk errors → atomicity ensures no partial writes.

---

## 12) Metrics & Bench

Measure before/after:

* Time to compact (p50, p95).
* Tokens saved; resulting headroom.
* Memory deltas (if applicable).
* Incidental: binary size and hot symbols (`cargo bloat/llvm-lines`).

**Commands**

```
hyperfine --warmup 3 "target\\release\\codex.exe /compact --dry-run"
```

```
cargo bloat -p codex-rs --release -n 20
```

Record numbers in PR body.

---

## 13) Implementation plan (small, mergeable PRs)

**PR 1 — Scaffolding & Estimator**

* `codex-rs/compact/estimate.rs`: local estimator + provider fallback.
* `CompactionReport` struct + `--dry-run` plumbing (prints before/after estimates).
* TUI: print completion message (no snapshot yet).

**Acceptance**

* Dry-run prints planned delta; no panics when `token_usage` is missing.

---

**PR 2 — Pins & Retention Policies**

* `/pin` and `/unpin` commands + serialization of pinned IDs.
* `--keep-roles`, `--keep-files` parsed and applied in keep set.

**Acceptance**

* Pinned messages and globs are never compacted away.

---

**PR 3 — Snapshot synthesis (summary\_v1)**

* `snapshot.rs` with `SummaryV1` + `synthesize_snapshot(session, clusters)`.
* Strict system prompt; JSON-only enforcement.
* `session.json` persisted via atomic write.

**Acceptance**

* Snapshot is valid JSON; includes decisions/files/symbols; survives reload.

---

**PR 4 — Hierarchical compaction & tail**

* Clustering logic; replacement history = `snapshot_as_system_msg + tail(N)`.
* Headroom guarantee loop until `min_headroom_tokens`.

**Acceptance**

* After compaction, headroom ≥ configured minimum; recall tests pass.

---

**PR 5 — Auto-compact + archive/restore**

* Auto-compact at threshold; `.codex/history/<ts>.jsonl`.
* Optional `/history restore <ts>` (or follow-up PR 6).

**Acceptance**

* Auto-compact triggers preemptively; archive exists and is readable.

---

**PR 6 — Polish & Docs**

* README/CODEX.md sections, flags help, examples.
* Final insta snapshots; review bible links.

---

## 14) System prompt (for snapshot synthesis)

```
SYSTEM: You distill the persistent state of a software project.

Output: valid JSON matching the SummaryV1 schema. No comments, no prose.

Include: current task (1 sentence), decisions, constraints, open_questions,
todo, files_in_scope [{path, why}], symbols [{name, file, role}], env, assumptions,
known_failures, last_compact_at (ISO-8601).

Rules: keep exact API names/flags/paths; remove redundancy; only confirmed facts.
```

---

## 15) Module layout (proposed)

```
codex-rs/
  compact/
    mod.rs
    estimate.rs          // token estimator fallback
    pins.rs              // /pin /unpin, keep set
    cluster.rs           // topic/time clustering
    snapshot.rs          // SummaryV1 + synthesize_snapshot(...)
    archive.rs           // history archive + restore
    replace.rs           // replace history with snapshot + tail
```

---

## 16) CLI flags (help text draft)

* `--dry-run` Show what would change and the token delta; no state change.
* `--keep-files <globs>` Comma-separated globs to exclude from compaction.
* `--keep-roles <roles>` Roles to keep (default: `tool,diff,decision`).
* `--max-tail <n>` Keep last N recent messages (default: 12).
* `--min-headroom <tokens>` Ensure at least this many tokens free (default: 2048).
* `--auto-compact-threshold <0..1>` Trigger auto-compact when usage exceeds threshold (default: 0.85).

---

## 17) Risks & mitigations

* **Model emits invalid JSON** → strict parse + retry once with stronger instruction; otherwise abort with clear error.
* **Estimator inaccuracies** → conservative headroom; allow user override; log estimation source.
* **Pin abuse grows history** → document that pins are user responsibility; expose counts in dry-run.
* **Concurrency/race in file ops** → atomic writes and fsync parent; no partial commits.

---

## 18) Validation checklist (“done when”)

* [ ] Dry-run shows accurate preview and deltas.
* [ ] After compaction, headroom ≥ `min_headroom_tokens`.
* [ ] Snapshot persists and answers recall tests.
* [ ] Pins and globs are respected; diffs/decisions preserved.
* [ ] Auto-compact triggers before context exhaustion.
* [ ] No dependency on `token_usage` for correctness.
* [ ] Insta snapshots and unit tests stable/deterministic.
* [ ] Docs updated (README, CODEX.md, AGENTS.md), help text accurate.

---

## 19) Quick-run commands

```
cargo fmt --all
```

```
cargo clippy -- -D warnings
```

```
cargo build -p codex-rs --release
```

```
cargo test -p codex-rs
```

```
hyperfine --warmup 3 "target\\release\\codex.exe /compact --dry-run"
```

---

## 20) CHALLENGE PROTOCOL (for reviewers and future changes)

1. **Material risks/errors**
   * Late, monolithic summaries destroy recall and break workflows.

2. **Critical assumptions**
   * Snapshot + small tail is sufficient to retain task state.
   * Pins/globs cover the majority of “must keep” cases.

3. **Highest-leverage adjustment**
   * Structured snapshot + auto-compact + estimator fallback.

4. **Minimal next steps**
   * Land PR 1 (estimator + dry-run report), then PR 3 (snapshot) and PR 4 (hierarchical compaction).
