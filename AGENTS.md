# AGENTS.md
Operational contract for Codex in this repository. Read this before touching code.

> Language policy: the human may speak Portuguese, but all artifacts (code, comments, errors, logs, CLI output, docs you generate) **must be English-only**.

---

## 0) How Codex uses this file
- Codex reads AGENTS.md to guide behavior in your repo; treat this like a playbook for agents. See OpenAI’s guidance and the AGENTS.md spec for context. :contentReference[oaicite:0]{index=0}
- The Codex project actively prompts agents to read AGENTS.md and documents expectations around it in releases. Keep this file up to date. :contentReference[oaicite:1]{index=1}

---

## 1) Prime directives
- **One concern per PR.** Keep diffs surgical, reversible, and small.
- **Rust-first.** Work in `codex-rs` and the CLI/TUI. Touch TS only for protocol parity/tooling.
- **KISS.** No function parkour. Prefer singletons only when they cut state churn and remain testable.
- **Error-first.** Explicit errors with actionable context; avoid “magical fallbacks.”
- **Performance brutal.** Measure changes; don’t cargo-cult flags or inlining.

---

## 2) Rust/codex-rs conventions
In `codex-rs` where Rust code lives:
- Crates are prefixed with `codex-` (e.g. `codex-core`). :contentReference[oaicite:2]{index=2}
- Inline variables directly in `format!` placeholders when possible. :contentReference[oaicite:3]{index=3}
- Do **not** add or modify code related to `CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR` or `CODEX_SANDBOX_ENV_VAR`. These envs gate tests/sandbox behavior; assume they’re set appropriately by the CLI and CI. :contentReference[oaicite:4]{index=4}
- Formatting and lints (use repo tasks):
  - Run `just fmt` after changing Rust code. No approval required. :contentReference[oaicite:5]{index=5}
  - Before finalizing, run `just fix -p <project>` to fix lints in the touched crate; only run workspace-wide if you changed shared crates. Ask before running full `just fix`. :contentReference[oaicite:6]{index=6}
- Tests:
  1) Run crate-specific tests for the project you changed (e.g. `codex-tui`).  
  2) If you touched `common/`, `core/`, or `protocol/`, then run the full suite with `cargo test --all-features`. Ask before the full suite; project-specific tests don’t require approval. :contentReference[oaicite:7]{index=7}

**Build defaults (local guidance):**
- Release builds should favor `opt-level=3`, thin LTO, `codegen-units=1`. Use `-C target-cpu=native` locally as opt-in and document portability. Measure p50/p95, allocations, and binary size deltas.

---

## 3) TUI style and code conventions
- Follow `codex-rs/tui/styles.md` for styling rules. :contentReference[oaicite:8]{index=8}
- Prefer ratatui’s `Stylize` helpers for spans/lines over constructing `Style` by hand, unless the style is computed at runtime. This matches recent upstream changes. :contentReference[oaicite:9]{index=9}
  - Basic spans: `"text".into()`  
  - Styled: `"text".red().dim().bold()`  
  - Avoid hardcoded white; prefer default foreground.  
  - Build `Line` with `vec![…].into()` when it stays on one line after rustfmt; otherwise pick the clearer form.
- Wrapping:
  - Use `textwrap::wrap` for plain strings.  
  - For ratatui `Line`, use helpers in `tui/src/wrapping.rs` (e.g., `word_wrap_lines`, `word_wrap_line`).  
  - Indentation: prefer `initial_indent`/`subsequent_indent` options.
- Avoid churn: don’t refactor between equivalent forms without a clear readability or functional gain. Follow file-local conventions. :contentReference[oaicite:10]{index=10}

---

## 4) Shell and portability
- Quote paths that may contain spaces; generate POSIX-friendly commands. Prefer `.` over `source`. Check file existence **at execution time**, not in a preflight step:

```
\[ -f "\${rc\_path}" ] && . "\${rc\_path}" && ( your\_command\_here )
```

- Non-interactive only; one command per line; no TUIs spawned by scripts.
- Sandboxing is OS-dependent (Seatbelt on macOS; Landlock/seccomp or container strategies on Linux). Don’t fight it; write scripts that behave under these constraints. :contentReference[oaicite:11]{index=11}

---

## 5) Filesystem and atomic writes
- Write to `*.tmp`, `fsync` file, `fsync` parent dir, then `rename` atomically.
- Preserve EOL/encoding. Avoid surprise diffs in patching flows.

---

## 6) Tests
### Snapshot tests (TUI)
This repo uses `insta` snapshots in `codex-rs/tui` to validate rendered output. :contentReference[oaicite:12]{index=12}
- Generate updates:
- `cargo test -p codex-tui`
- Inspect pending:
- `cargo insta pending-snapshots -p codex-tui`
- Preview a specific file:
- `cargo insta show -p codex-tui path/to/file.snap.new`
- Accept all (only when intended):
- `cargo insta accept -p codex-tui`
- Install:
- `cargo install cargo-insta`
### Assertions
- Use `pretty_assertions::assert_eq` for clearer diffs in tests. :contentReference[oaicite:13]{index=13}
### Determinism
- Avoid branching logic inside unit tests; inject inputs/env for stable behavior. Use stable IDs (e.g., a nil UUID) where determinism matters (common reviewer expectation). :contentReference[oaicite:14]{index=14}

---

## 7) Protocol and types (Rust ↔ TS)
- When matching OpenAI API, keep required **`snake_case`** on the wire; stage protocol migrations rather than landing large breaking refactors in one PR. These patterns are recognized and accepted upstream. :contentReference[oaicite:15]{index=15}

---

## 8) Performance policy (strict)
- Borrow by default; minimize allocations/copies; preallocate on hot paths; avoid `format!` in tight loops, prefer `write!`/`format_args!`.
- Choose collections for the real workload; benchmark hasher swaps and gate “fast hashers” behind a feature.
- Control inlining and branches only with evidence. Any `unsafe` must have a `// SAFETY:` block and tests targeting invariants.

---

## 9) Sandbox-related code
- Do **not** modify logic keyed by `CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR` or `CODEX_SANDBOX_ENV_VAR`. Tests often use these to short-circuit behavior under agent sandboxes; respect them. :contentReference[oaicite:16]{index=16}

---

## 10) Commit and PR hygiene
- Always work on a new feature branch per change; do not commit directly to `master`.
- Open a Pull Request from your feature branch to `master` for the user to review and merge.
- **Atomic commits.** Run fmt+clippy+tests before committing.
- Message convention: `feat|fix|refactor|docs|chore: short summary`  
Body: What/Why/How, files/lines touched, risks, and perf numbers when relevant.
- PR description must use this template:

```markdown
# What
- [Concrete changes; modules/files touched]

# Why
- [User impact, correctness, or performance motivation; link issues]

# How
- [Minimal approach; shell portability; determinism; atomic writes]

# Tests
- [Unit/golden/property tests; determinism measures]

# Perf
- [Before/after numbers; methodology]

# Follow-ups
- [If staging a migration or larger feature]
```

* CLA must be signed; the bot blocks merges otherwise. ([GitHub][1])

---

## 11) Ready-to-commit checklist

* [ ] Feature branch created; PR to `master` opened for review.
* [ ] One concern only; split if you touched two areas.
* [ ] `just fmt` done; `just fix -p <project>` run (ask before workspace-wide). ([GitHub][2])
* [ ] `cargo test -p <project>` green; ask before full `--all-features` run. ([GitHub][2])
* [ ] TUI code uses ratatui `Stylize` idioms where appropriate. ([GitHub][1])
* [ ] Snapshot tests reviewed/accepted with `cargo-insta` when you intentionally changed output. ([GitHub][2])
* [ ] Shell: quoted paths; POSIX `.`; TOCTOU avoided with on-exec checks.
* [ ] Wire casing correct; migrations staged if applicable.
* [ ] Atomic writes for modified files; EOL/encoding preserved.
* [ ] No Portuguese in artifacts; clear errors with context.

---

## 12) Quick commands (paste & run)

```
just fmt
```

```
just fix -p codex-tui
```

```
cargo test -p codex-tui
```

```
cargo insta pending-snapshots -p codex-tui
```

```
cargo insta accept -p codex-tui
```

```
cargo clippy -- -D warnings
```

```
cargo build -p codex-rs --release
```

```
hyperfine --warmup 3 "target\\release\\codex.exe /compact --dry-run"
```

---

*If you’re “optimizing” without measurements, stop. You’re guessing.*
