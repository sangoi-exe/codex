# MAINTAINERS_REVIEW_BIBLE.md
_What Codex maintainers consistently expect in reviews. Read this **before** writing code or opening a PR._

## 0) Review reality check
- Keep PRs **narrowly scoped** and incremental. If you’re solving two different problems, split into two PRs.
- Rust-first: improvements to the native `codex-rs` path are prioritized over TypeScript unless parity or protocol requires TS changes.
- CLA must be signed; no CLA, no merge.
- A clear, testable “What/Why/How” in the PR description is not optional.

---

## 1) Scope & structure the maintainers reward
- **One concern per PR.** Big refactors should be staged across several PRs.
- **Forward-compatible naming** so future extensions don’t need breaking changes (e.g., prefer `startup_timeout_ms` over a generic `timeout_ms`).
- **Staged migrations** for protocol/type changes: introduce the new path, keep old behavior temporarily, then clean up in a follow-up PR.

---

## 2) Naming, flags, and config semantics
- Prefer **positive** flags over “negative-sense” toggles (`--enable-x` rather than `--no-x` when feasible).
- Avoid “bool soup.” Use enums/newtypes for multi-state behavior.
- Back-compat quirks belong in **config loading/migration**, **not** in hot-path logic.

---

## 3) Shell & portability (review hot-buttons)
- **Quote paths** that may contain spaces; when generating shell commands, ensure proper quoting/escaping.
- Prefer **`.`** over `source` for POSIX portability.
- Avoid time-of-check/time-of-use races: test file existence **at execution**, not earlier. Example idiom:

```
\[ -f "\${rc\_path}" ] && . "\${rc\_path}" && ( your\_command\_here )
```

- Aim for **POSIX-consistent** behavior; avoid zsh/bash-specific leakage unless gated and documented.

---

## 4) Tests: determinism beats cleverness
- Unit tests should be **predictable**: inject environment and inputs; avoid branching logic inside tests.
- Use stable IDs (e.g., a nil/constant UUID) where determinism matters.
- Prefer golden/snapshot tests for CLI text output; keep outputs stable and explicit.

---

## 5) Protocol/types (Rust ↔ TS) and API casing
- When serializing to match the OpenAI API, preserve required **`snake_case`** in the wire format.
- It’s acceptable (preferred) to **stage** protocol changes: temporarily support both old and new messages, then remove legacy paths in a follow-up.
- Keep public surfaces small; prefer well-typed builders for complex options over bool-flag pileups.

---

## 6) Rust guidance (house style distilled)
- **Clippy-clean** builds (treat warnings as errors in CI).
- **Error-first** flow: early returns with `anyhow/thiserror`. No `unwrap/expect` outside tests or one-time boot code.
- Avoid unnecessary allocations/copies: borrow by default, preallocate where hot, and measure changes.
- Concurrency with minimal contention; use `parking_lot` if justified by profiling.
- `unsafe` only as a last resort, smallest scope, with a `// SAFETY:` block explaining invariants and tests that would catch violations.
- Logs are quiet by default; detailed under `--verbose` or `RUST_LOG`. Prefer structured fields over string concat spam.

---

## 7) Filesystem & atomic writes
- For files you create/modify: write to `*.tmp`, `fsync` file, `fsync` parent dir, then atomically `rename` to the final path.
- Preserve EOL/encoding and avoid surprising diffs. Validate space/permissions early when possible.

---

## 8) CLI UX & output rules
- Provide `--dry-run` for operations that could alter state.
- Output should be deterministic, scriptable, and explicit. Avoid mixing progress bars and verbose logs unless gated.
- Exit codes: `0` on success; nonzero on failure with an actionable error line.

---

## 9) Anti-patterns that trigger “changes requested”
- Bundling unrelated changes in one PR.
- Clever shell without proper quoting or with `source` assumptions.
- Tests with hidden branching or non-deterministic IDs.
- Renaming for aesthetics only; churning identifiers without necessity.
- “Optimizations” without benchmarks or that reduce readability.

---

## 10) Ready-to-merge checklist
- [ ] **Scope:** one concern; if you touched two areas, split PRs.
- [ ] **Naming:** forward-compatible; avoid negative-sense flags; prefer typed enums over bool soup.
- [ ] **Config:** back-compat handled during load/migration, not sprinkled in runtime logic.
- [ ] **Shell:** paths quoted; `.` over `source`; file existence checked at execution time.
- [ ] **Tests:** deterministic; no `if` branches; stable IDs; golden tests for CLI output if relevant.
- [ ] **Protocol:** correct casing on the wire; staged migrations where needed.
- [ ] **Rust hygiene:** clippy/fmt clean; no unnecessary allocations/copies; errors with actionable context.
- [ ] **FS safety:** atomic writes; preserve encoding/EOL.
- [ ] **PR body:** “What/Why/How,” files touched, and any staged follow-ups.
- [ ] **CLA:** signed and green.

---

## 11) PR description template (paste this into your PR)

```markdown
# What
- [Concise list of concrete changes; modules/files touched]
- [If staged: what lands now vs. what follows]

# Why
- [User impact, correctness, or performance motivation; link issues]

# How
- [Minimal viable approach; shell portability notes; determinism in tests; atomic writes]
- [Perf notes if applicable: before/after metrics, methodology]

# Tests
- [Unit/golden/properties added or updated; determinism measures]

# Follow-ups
- [Exact next steps if staging a migration or larger feature]
```

---

## 12) Patterns to emulate (seen in approved PRs)

* Incremental steps that unlock downstream work.
* Early convergence on naming and option semantics before touching too many files.
* Protocol changes done in **phases** with temporary dual paths, then cleanup.
* “Trivial” protocol/codegen fixes accompanied by proof that generated output remains identical.

---

## 13) TL;DR for the impatient

* Quote shell paths; use `.` not `source`.
* Tests must be deterministic; inject inputs; stable IDs.
* Keep wire casing correct; stage migrations.
* Borrow > copy; measure “perf” claims.
* One concern per PR; small and mergeable beats grand and blocked.
