# Architecture and coding style

DeLOG features follow the ownership boundaries, extension points, coding
conventions, and integration checks defined here.

## Where code belongs

| Owner | Responsibility | Changes owned here |
| --- | --- | --- |
| `delog-core` | IDs, canonical microsecond time, immutable chunks and snapshots, ingest, diagnostics | data shape or storage semantics change |
| `delog-parsers` | file format detection and decoding | a new file format is read |
| `delog-stream` | live links, frame reading, and recording | a live transport changes |
| `delog-flow` | graph model, commands, evaluation, and publishing | a new dataflow node or operation is added |
| `delog-api` | stable native catalog and application-control contracts | a caller-visible control operation is added |
| `delog-script` | Python execution and adapters to native contracts | embedded scripting gains a capability |
| `delog-cache` | derived render caches and decimation | plot data preparation changes |
| `delog-render` | wgpu resources and draw passes | GPU rendering changes |
| `delog-parquet-format` | versioned structured Parquet schema | interchange format changes |
| `delog-app` | egui UI, application orchestration, persistence, and user-visible destinations | a view or application action changes |

When the external Python API branch is present, `delog-remote` owns its
authenticated loopback protocol and explicit HTTP routes;
`python/delog-client` owns Python transport, models, and client ergonomics.
Both adapt to the native `delog-api` contracts. Remote operations keep
authorization, route mapping, and app control dispatch in their current
owners; a second control path is not introduced.

Dependencies point toward the smallest owner of a concept. Shared domain
types belong in their owning library, not in `delog-app`; shared egui widgets
belong in `delog-app::ui::components`. A helper is shared when two real
callers need the same behavior. Variant-specific choices remain at call
sites, especially labels, routing, sizes, and error context.

## Feature flow

New input formats are decoded in `delog-parsers` and emitted through the
existing ingest API; snapshots, caches, and views consume that output. New
dataflow operations have their graph contract and evaluator in `delog-flow`;
the app editor presents them without duplicating evaluation rules. New
application controls have their contract in `delog-api` and their
implementation at the app's control boundary; scripting and external clients
adapt to that contract. Route tables and command dispatch remain explicit,
and caller-visible results have tests.

## Routing and diagnostics

The destination of each existing message remains unchanged unless a separate
task explicitly changes that behavior. `Diag` carries ingest and data-quality
context such as source and timestamp; `PendingLog` feeds the Logging dock.
New events follow adjacent call sites and tests. Shared helpers never
silently select a different destination. Control API routes, Python commands,
and UI shortcuts retain their existing paths through the current owner;
source-level deduplication never reroutes them.

## Rust style

- Rust 2024, the pinned `rust-toolchain.toml`, `cargo fmt`, and Clippy are the
  workspace standards. Third-party versions are pinned once in
  `[workspace.dependencies]`; member crates use `workspace = true`.
- Modules have one clear responsibility and public interfaces remain narrow.
  A named parameter struct carries related options across repeated calls.
  Repeated logic is extracted only when its behavior and owner are clear; a
  single caller does not justify a generic framework.
- Errors retain typed causes. The workspace's `thiserror` derive handles
  straightforward `Display` and `Error` implementations; the UI boundary
  supplies action context. Error messages are lowercase without a final
  period, and user-facing errors use `Display`, not `{:?}`. Before an existing
  error is refactored, tests pin its message, `Error::source`, conversions,
  and trait bounds. `#[from]` exposes a cause and appears only when that
  source contract is intended. Error types remain in their owning crates,
  never in a shared catch-all.
- Domain IDs use newtypes. Unit boundaries carry `_us`, `_ms`, `_rad`, `_deg`,
  and `_m` suffixes; `delog-core::time` owns canonical time rules.
- Comments explain invariants, precision traps, ordering, upstream quirks,
  and other reasons behind code. Module contracts use `//!`; public items use
  `///`. Unsafe blocks have a preceding `// SAFETY:` explanation. Comments
  do not restate the next line.
- Test names describe behavior. Short unit tests sit beside their code;
  larger fixtures and public boundaries use a sibling `tests.rs` or an
  integration test. Tests assert real results and error paths, not source text.

## UI and performance

Shared controls use `ui::icons`, `ui::components`, `ui::design_tokens`, and the
active theme. Reused components preserve enabled state, hover text, size,
color, and widget identity wherever those affect interaction. UI labels and
error context remain with the calling feature.

In ingest, cache, and render loops, queues remain bounded, lock scopes remain
short, and existing allocation patterns remain unchanged. Removing duplicate
syntax never adds dynamic dispatch, allocations, or synchronization to a hot
path. Algorithm changes have a relevant before-and-after benchmark or
profile. Structural refactors have tests that pin externally visible behavior
before the change and run again afterward.

## Local checks

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --locked --no-default-features -- -D warnings
cargo test --workspace --locked --exclude delog-script -- --test-threads=1
cargo test -p delog-script --locked -- --test-threads=1
```

The relevant package's tests run during implementation; the full matrix runs
before integration. The single-threaded Rust run also avoids concurrent
headless GPU initialization on systems whose Vulkan loader is not stable
under parallel tests. Each additional language or client has its own test
suite. Route changes have explicit route-contract tests.
