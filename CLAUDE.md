# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## State of the repo

DeLOG is being rewritten from scratch in **Rust** (the previous Qt/C++ implementation was removed in commit `d697d61` on the `rework` branch — anything in git history before that is the old codebase and not a reference for new work).

**`PLAN.md` is the single source of truth.** It defines the full architecture, every load-bearing design decision (with rationale and rejected alternatives), the milestone order (§21), and the master checklist (§22). Read PLAN.md §0 (agent protocol) before doing anything, and §4.5 (Zero-Copy Invariants) before touching code that moves sample data.

## What DeLOG is

A desktop analyzer for drone flight logs (ArduPilot `.BIN`, PX4 `.ulg`, MAVLink `.tlog`) and live MAVLink telemetry: GPU-rendered time-series plots, a synchronized 3D vehicle view, and a global timeline — built on `egui`/`eframe` + `wgpu` with an Arrow-based columnar core, targeting 60 FPS over 100M+ samples.

## Checklist discipline (PLAN.md §0 — mandatory)

- Every feature has a stable ID in the §22 checklist (e.g. `CORE-07`). Mark `[~]` when starting, `[x]` when done, `[!]` + one-line reason if blocked — **in the same commit** as the feature.
- Never build something that isn't in the checklist without first adding it there (correct area, new ID; never renumber existing IDs).
- Definition of Done: clippy-clean, unit tests for non-trivial logic, golden/fixture tests for parsers, criterion bench for hot-path changes, no zero-copy invariant violated, checklist updated.

## Commands

```bash
cargo build --workspace
cargo clippy --workspace -- -D warnings   # must be clean (Definition of Done)
cargo fmt --all
cargo test --workspace
cargo test -p delog-core <test_name>      # single test in one crate
cargo bench                               # criterion benches; budgets in PLAN.md §20.4
cargo run -p delog-app
```

All third-party versions are pinned in the workspace `Cargo.toml` `[workspace.dependencies]` table; crates inherit with `workspace = true`.

## Architecture (PLAN.md §3 has the full picture)

Crates under `crates/`, with **absolute** dependency rules (§3.2):

```
delog-app ──► delog-render ──► delog-cache ──► delog-core
    │              │                              ▲
    ├──────────────┼──► delog-stream ──► delog-parsers ──► (core)
    └──────────────┴──► delog-parsers ────────────┘
```

- Data flows downward only. `delog-core` depends on `arrow` + std only. Nothing below `app` may depend on `egui`. The shared MAVLink decoder lives in `delog-parsers::mavlink`; `delog-stream` consumes it (downward edge).
- `delog-render` is **pure wgpu** — no egui types (enables headless golden-image tests). `delog-app` adapts it through `egui_wgpu` callbacks.
- Parsers and stream never see GPU or UI; their only output is `ParsedBatch` + diagnostics into an `IngestSink`.
- Arrow types are vocabulary types of `delog-core`'s API, but `delog-app` must not touch Arrow directly — it goes through core helpers.
- If a change needs an upward dependency, the design is wrong — stop and restructure.

Data flow: parser threads → bounded channel (cap 256) → **single ingest thread** (the only store writer) → immutable `Chunk`s → `StoreSnapshot` published via `ArcSwap` (epoch snapshots, no locks). Plots read through a lazily-built f32 `TraceCache` (the One Copy) + min/max pyramid; GPU draws by vertex pulling from storage buffers — no CPU geometry. §3.3 walks through this end to end.

## Non-negotiable invariants

- **Zero-Copy Invariants (§4.5)** — numbered ZC-1..6; cite them in comments (`// upholds ZC-3`). Exactly one transform copy per plotted field (canonical → f32 render cache). Deliberate exceptions carry `// ZC-EXCEPTION: <reason>` and a counter metric.
- **Time is `i64` microseconds** canonically, end to end; floats exist only in render caches. Tooltips, stats, export, 3D all read canonical `i64` + original dtype.
- **Never hold a lock or borrow across a paint callback or an `.await`.**
- **Never block the UI thread**: any work that could exceed ~16 ms runs off-thread as a job + progress (§19.6).
- Backpressure: file parsers block on a full channel; live decoders drop the batch, increment `ingest_dropped_batches`, and emit a diagnostic (§5).
- Min/max decimation, never LTTB — single-sample spikes are the finding, not noise (§9.5).
- NaN is a gap marker: preserved in caches, rendered as line breaks, never interpolated away.

## Testing (PLAN.md §20)

- Property tests (`proptest`) pin the pyramid to a naive scan; golden fixture tables per parser (`fixtures/` holds small real logs + synthetic generators).
- `cargo-fuzz` targets per frame decoder: no input may panic, OOM, or hang a parser (malformed records are skipped with diagnostics; only framing corruption aborts).
- Headless wgpu golden-image tests drive `delog-render` without a window.
- Perf budgets in §20.4 are soft-asserted in CI — keep them holding when touching hot paths.

## Milestones

Work in the order of §21 (M0 scaffold → M1 core → M2 BIN parser + browser → M3 plot MVP → … → M10 polish). Within a milestone, follow the §22 item order. The first action of the rewrite is **ARC-01** (workspace scaffold).
