# ROADMAP — Web Search Token Budget & Context Accounting

Stabilize Codex behavior when using the `web_search` tool by separating **persisted vs transient** tokens, hard-capping tool payloads, and fixing UX so “remaining context” stops lying.

> Artifacts: English-only. The user may speak Portuguese; translate intent, keep all code/docs in English.

---

## 1) Scope

Deliver a robust, predictable integration for `web_search`:

- Separate **persisted context** (carried to next turn) from **in-flight tool payload** (only for the current turn).
- Add a **Token Budgeter** that caps `web_search` output by result and in total, with aggressive preprocessing and compression.
- Show **two context meters** in TUI and `/status`: “Next turn” (persisted only) and “This turn (with tools)”.
- Persist only **summaries with citations**; never persist raw page dumps into the prompt history.
- Keep 0.30 series model window math intact; don’t regress underlying context calculations.

**Non-goals**
- Changing model, provider, or context window constants.
- Re-architecting TUI outside of the minimal UI for dual meters.
- Rewriting other tools.

---

## 2) Problem statement (observed on 0.30)

- When `web_search` runs, “remaining context” drops to near-zero because the meter **counts the tool’s transient payload** inside that in-flight request.
- On the next assistant turn (no new search), the meter “recovers,” because that transient payload was **not persisted**.
- Users wrongly believe they must compact; they trigger lossy flows unnecessarily. UX is misleading.

---

## 3) Design overview

**Fix accounting + limit the blast radius.**

1) **Dual accounting**
   - Track and display:
     - `Next-turn context`: tokens from system + profile + persisted chat/history (+ baseline).
     - `This-turn (with tools)`: `Next-turn` plus all transient tool payloads.
2) **Token Budgeter for web_search**
   - Hard caps on result count, tokens per result, and total tool tokens.
   - Preprocess (strip HTML/boilerplate), focus windows around matches, deduplicate by URL.
   - Compress to a short extractive summary **with citations**.
   - Persist only the summary, never full raw content.
3) **UX**
   - Two meters in TUI and `/status`:
     - `Next turn: XX% left` (the true budget you carry forward)
     - `This turn (with tools): YY% left` (the temporary peak)
   - Badge: `web payload: ~N tokens (capped)`

---

## 4) Data & accounting model

### 4.1 Structures

```rust
pub struct ContextBudget {
    pub model_context_window: usize,  // e.g., 272_000
    pub baseline_tokens: usize,       // fixed overhead already modeled in 0.30
    pub persisted_input_tokens: usize, // system + profile + persisted history
    pub transient_tool_tokens: usize,  // this-turn-only tool payload
}
```

### 4.2 Percent calculations

```
next_turn_pct = 1.0 - (baseline_tokens + persisted_input_tokens) / model_context_window
this_turn_pct = 1.0 - (baseline_tokens + persisted_input_tokens + transient_tool_tokens) / model_context_window
```

* Display both. Use integer rounding consistent with existing UI.

---

## 5) Token Budgeter (web\_search)

### 5.1 Defaults (configurable)

* `max_results = 3`
* `max_tokens_per_result = 2048`
* `max_total_tokens = 8192`

### 5.2 Preprocessing (deterministic)

* Strip HTML; keep visible text only.
* Drop boilerplate (nav/footer/legal) via simple heuristics.
* Extract **windows** of 200–300 tokens around matched query terms; merge overlapping windows.
* Deduplicate by canonical URL; keep first occurrence.

### 5.3 Compression

* Build a **bullet summary** with source-line citations.
* Keep total tool payload ≤ `max_total_tokens`.
* Persist into history only a compact `ToolSummary` object:

  ```json
  {
    "tool": "web_search",
    "summary": ["bullet 1", "bullet 2", "..."],
    "citations": [{"title":"...", "url":"...", "note":"..."}],
    "token_estimate": 972
  }
  ```
* Keep full raw pages **out of** the prompt. Log raw content only under verbose tracing (not persisted).

---

## 6) UX & CLI/TUI

* **/status** shows:

  * `Next turn: XX% left (persisted only)`
  * `This turn (with tools): YY% left`
  * `web payload this turn: ~N tokens (capped at M)`
* **TUI**:

  * Two slim meters stacked with labels.
  * Badge in the tool panel: `web payload: ~N / M tokens`.

---

## 7) Config (add to user config; overridable via flags)

```toml
[tools.web_search]
enabled = true
max_results = 3
max_tokens_per_result = 2048
max_total_tokens = 8192
persist_raw_pages = false
persist_tool_summary = true
```

Optional runtime flags:

* `--web-max-results <N>`
* `--web-max-tokens-per-result <N>`
* `--web-max-total-tokens <N>`

---

## 8) Safety & constraints

* Do **not** modify sandbox env logic:

  * `CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR`
  * `CODEX_SANDBOX_ENV_VAR`
* English-only artifacts.
* No destructive resets.
* Respect existing context window constants introduced in 0.30.

---

## 9) Tests

### Unit

* Accounting: `next_turn_pct` vs `this_turn_pct` with varied inputs.
* Budgeter:

  * Caps enforced per result and total.
  * Deduplication by URL.
  * Window extraction merges overlaps deterministically.
* Summary persistence vs raw content exclusion.

### Integration

* Session where `web_search` fires:

  * `/status` before search; after search; next turn without search.
  * Verify dual meters display and stability.
* Large pages:

  * Ensure caps hold; summary persists; raw not persisted.
* Regression:

  * 0.30 baseline math unchanged for no-tool turns.

### TUI snapshot (insta)

* New dual-meter line(s) and badge are stable.
* `/status` text snapshot covers both meters and payload line.

---

## 10) Metrics & benchmarks

Measure before/after:

* p50/p95 assistant latency when `web_search` is used.
* Token payload size distribution for tool outputs.
* Binary size and top symbols if code paths expand.

**Commands**

```
hyperfine --warmup 3 "target\\release\\codex.exe /status"
```

```
hyperfine --warmup 3 "target\\release\\codex.exe -e 'search: <query>'"
```

```
cargo bloat -p codex-rs --release -n 20
```

---

## 11) Implementation plan (small, mergeable PRs)

**PR 1 — Dual accounting**

* Add `ContextBudget` and dual percent calculations.
* Expose in `/status` and minimal TUI text.
* Keep existing context math intact; no tool changes yet.

**Acceptance**

* `/status` prints both meters; math verified by unit tests.

---

**PR 2 — Token Budgeter skeleton**

* Implement caps + preprocessing (strip, dedupe, windows).
* Add token estimation (existing estimator or feature-gated tokenizer).
* Show `web payload: ~N / M` in TUI.

**Acceptance**

* Caps enforced in unit tests; payload lines appear; no persistence change yet.

---

**PR 3 — Compression & persistence policy**

* Add extractive summary builder with citations.
* Persist only `ToolSummary` to history; keep raw in verbose logs only.

**Acceptance**

* History shows compact summary; raw excluded; `/status` meters steady across turns.

---

**PR 4 — Polish**

* Flags + config wiring for caps.
* Final TUI polish and insta snapshots.
* Docs in CODEX.md (usage, flags) and AGENTS.md links.

---

## 12) Risks & mitigations

* **Over-aggressive caps hide useful info**
  → allow user overrides; surface counts of truncated items; keep citations.
* **Estimator inaccuracies**
  → conservative margins; display it as an estimate; allow model-side `token_usage` to refine when available.
* **UX clutter**
  → concise labels; single badge; square off with snapshot tests.

---

## 13) Validation checklist (“done when”)

* [ ] `/status` shows **both** meters and correct values across tool/no-tool turns.
* [ ] TUI meters and badge present; no flicker/jitter across interactions.
* [ ] `web_search` payload always ≤ caps; summary persisted; raw excluded from prompt history.
* [ ] No regressions in context window math for non-tool turns.
* [ ] Unit/integration/insta tests green; clippy/fmt clean.
* [ ] Docs updated (CODEX.md usage; AGENTS.md references).

---

## 14) Quick commands (local)

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
hyperfine --warmup 3 "target\\release\\codex.exe /status"
```

---

*If a meter says “0% left” only during a web turn, it isn’t memory loss; it’s transient payload. Fix the accounting. Cap the payload. Move on.*
