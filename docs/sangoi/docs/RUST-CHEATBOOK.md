# docs/codex/RUST-CHEATBOOK.md

> Canonical playbook for codex-rs contributors. Read this first, then act. Keep code fast, small, and boring.

## 0) Ground rules

* English-only for identifiers, comments, error messages, logs, CLI output.
* KISS. No function parkour. Prefer singletons only when they reduce state churn.
* Don’t rename public API/identifiers unless strictly necessary and documented.
* Error-first: fail fast with explicit context. No “magical fallbacks.”
* Measure before bragging. Every “perf” claim needs numbers.

---

## 1) Build profiles and compile-time tuning

**Cargo.toml** (suggested):

```toml
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
strip = "symbols"
panic = "abort"

[profile.dev]
debug = 1
opt-level = 0
lto = false
```

Optional portability knobs:

* Local dev: `RUSTFLAGS="-C target-cpu=native"` for max perf on your box.
* PGO: guard behind a dedicated profile/feature and document exact steps/workload.

---

## 2) Allocation, copies, and strings

* Prefer `&str`, `&[T]`, iterators; avoid `to_string()`, `to_vec()`, `clone()` unless necessary.
* Preallocate with `with_capacity`/`reserve_exact` on hot paths.
* Consider `SmallVec`, `SmallString`/`SmartString` for tiny, frequent buffers (only with benchmark evidence).
* Use `Cow<'_, str>` to borrow by default; own only on mutation.
* Avoid `format!` in loops; use `write!`/`format_args!` into preallocated buffers.

**Don’t:**

* Build large strings just to slice them later.
* Copy `PathBuf`/`String` around when `&Path`/`&str` suffice.

---

## 3) Collections and hashing

* Default `HashMap` is fine; prefer `BTreeMap` for predictable order or small keys with cache-friendly scans.
* `indexmap` only if order is semantically required.
* Fast hashers (`fxhash`/`ahash`) only behind `feature = "fast-hash"` and only with benchmark proof. Never as default on untrusted input.

---

## 4) Errors and result flow

* Library-like errors: `thiserror` enums. CLI edges: `anyhow::Result` with `.context("what failed and why")`.
* No `unwrap()/expect()` outside tests and one-time boot code.
* Use `bail!`/`ensure!` for guardrails.
* Error messages: actionable, include inputs and key state (redact secrets).

**Template:**

```rust
use anyhow::{Result, Context, bail, ensure};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("invalid schema: {0}")]
    InvalidSchema(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn load_snapshot(p: &std::path::Path) -> Result<SummaryV1> {
    let data = std::fs::read(p).with_context(|| format!("reading snapshot file: {}", p.display()))?;
    let s: SummaryV1 = serde_json::from_slice(&data)
        .context("parsing SummaryV1 JSON")?;
    Ok(s)
}
```

---

## 5) Logging, tracing, and diagnostics

* Use `tracing` with targets and levels; quiet by default, verbose under `--verbose` or `RUST_LOG`.
* Annotate hot functions with `#[tracing::instrument(skip(huge_struct))]` only when useful; avoid noise.
* Prefer structured fields: `info!(task=%id, "message")` instead of string concat spam.

---

## 6) CLI UX (clap) and output rules

* Flags explicit and typed; avoid boolean soup. Use enums/newtypes.
* Always provide `--dry-run` for potentially destructive ops.
* Deterministic, testable output. No randomness unless seeded and printed.
* Exit codes: 0 success, nonzero with clear error lines.

**Progress bars:** `indicatif` with sane refresh rate. Never spam logs and progress simultaneously.

---

## 7) Filesystem and atomic writes

* Write to `file.tmp`, `fsync` the file, `fsync` parent dir, then `std::fs::rename` to final.
* Preserve EOL/encoding on patching. Validate space and permission errors up front when possible.

**Skeleton:**

```rust
use std::{fs, io::Write, path::Path};

pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    // fsync parent dir for durability
    if let Some(dir) = path.parent() {
        fs::OpenOptions::new().read(true).open(dir)?.sync_all()?;
    }
    fs::rename(tmp, path)?;
    Ok(())
}
```

---

## 8) Concurrency

* CPU-bound parallelism: prefer `rayon` or scoped threads; avoid global contention.
* Use `parking_lot` locks if lock-heavy and justified by profiling.
* Never fake Send/Sync. If you must, document invariants and test race-y paths.

---

## 9) Unsafe code policy

* Last resort only; smallest possible scope.
* Mandatory `// SAFETY:` doc block explaining invariants and why it’s correct.
* Add tests that would explode if invariants break (Miri-friendly when possible).

---

## 10) Profiling and benchmarking

**One-liners:**

```
cargo bench
```

```
cargo llvm-lines -p codex-rs
```

```
cargo bloat -p codex-rs --release -n 20
```

```
cargo flamegraph -p codex-rs --bench <name>
```

```
hyperfine "target\\release\\codex.exe /compact --dry-run"
```

Measure p50/p95 latency, allocations (if available), and RSS/WS. Commit numbers in the PR description.

---

## 11) Feature flags and build-time toggles

* Perf toys behind `--features perf` or `FAST_HASH`, off by default.
* Keep a portable default build. Non-portable opts (PGO, `target-cpu=native`) opt-in only.

---

## 12) API design patterns

* Use newtypes for typed IDs instead of strings.
* Builders for optional params; sane defaults.
* No implicit globals; if singleton, make it explicit and testable.

---

## 13) Test strategy

* Unit tests for hot logic and error edges.
* Golden tests for CLI output snapshots (stable files).
* Property tests (`proptest`) for parsers and reducers.
* Regression tests for “recall after compaction” semantics.

---

## 14) Lints, CI, hygiene

**One-liners:**

```
cargo fmt --all
```

```
cargo clippy -- -D warnings
```

```
cargo udeps -p codex-rs
```

```
cargo deny check
```

```
cargo outdated -wR
```

---

## 15) Determinism & reproducibility

* Sort outputs when order is not semantically meaningful.
* Seed RNG where needed and print the seed.
* Avoid wall-clock in tests; use fixed timestamps or inject a clock.

---

## 16) Review/PR checklist

* [ ] Clippy clean, fmt applied.
* [ ] Bench before/after with numbers in PR body.
* [ ] No Portuguese in code/comments/logs/output.
* [ ] Atomic writes for files touched.
* [ ] Error messages actionable.
* [ ] Flags documented in CODEX.md.
* [ ] Tests for new branches and error cases.
* [ ] No gratuitous renames; migration notes if any.

---

## 17) Don’ts

* Don’t enable “fast hashers” globally.
* Don’t slap `#[inline(always)]` everywhere.
* Don’t use `unsafe` to silence the borrow checker.
* Don’t log secrets or dump giant blobs.
* Don’t rely on `token_usage` presence without a local estimator fallback.

---

## 18) Quick snippets

**Result alias:**

```rust
pub type Result<T> = anyhow::Result<T>;
```

**Guard macros:**

```rust
macro_rules! check {
    ($cond:expr, $($arg:tt)*) => {
        if !$cond { anyhow::bail!($($arg)*); }
    }
}
```

**Borrow-friendly JSON:**

```rust
#[derive(serde::Deserialize)]
struct Foo<'a> {
    #[serde(borrow)]
    name: std::borrow::Cow<'a, str>,
}
```

**Time-limited work (coarse):**

```rust
// use a deadline and poll; avoid blocking indefinitely in CLI paths
```

---

Use this cheatbook as constraints, not decoration. If you “optimize” without measuring, you’re guessing.

---

## 19) Minimal perf measurement recipe (example)

```
hyperfine --warmup 3 "target\\release\\codex.exe /compact --dry-run"
```

```
cargo bloat -p codex-rs --release -n 15
```

Record deltas in PR: p50/p95, alloc count (if measured), binary size, and hotspots from flamegraph.

---

## 20) Atomic change script (local)

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