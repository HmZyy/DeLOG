# Architecture and coding style

Use this guide when adding a feature to DeLOG. It records the current
ownership boundaries, extension points, coding conventions, and checks to
run before integration.

## Where code belongs

| Owner | Responsibility | Extend here when... |
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
Both adapt to the native `delog-api` contracts. A new remote operation must
keep authorization, route mapping, and app control dispatch in their current
owners rather than introducing a second control path.

Dependencies should point toward the smallest owner of a concept. Put shared
domain types in the owning library, not in `delog-app`; put shared egui widgets
in `delog-app::ui::components`. A helper is useful when two real callers need
the same behavior. Keep variant-specific choices at call sites, especially
labels, routing, sizes, and error context.

## Feature flow

For a new input format: decode in `delog-parsers`, emit through the existing
ingest API, then let snapshots, caches, and views consume it. For a new
dataflow operation: define its graph contract in `delog-flow`, implement the
evaluator there, and add the app editor presentation without duplicating
evaluation rules. For a new application control: define the contract in
`delog-api`, implement it at the app's control boundary, and adapt scripting
or external clients to that same contract. Keep the route table and command
dispatch explicit and test the caller-visible result.

## Routing and diagnostics

Preserve the destination of each existing message unless a separate task
explicitly changes that behavior. `Diag` carries ingest and data-quality
context such as source and timestamp; `PendingLog` feeds the Logging dock.
Follow the adjacent call sites and tests for a new event. Do not make a
shared helper silently choose a different destination. A control API route,
Python command, or UI shortcut must retain its existing path through the
current owner; source-level deduplication is not a reason to reroute it.

## Rust style

- Use Rust 2024, the pinned `rust-toolchain.toml`, `cargo fmt`, and Clippy.
  Pin third-party versions once in `[workspace.dependencies]`; member crates
  use `workspace = true`.
- Prefer small modules with one clear responsibility. Keep public interfaces
  narrow; use a named parameter struct when several calls carry the same
  related options. Extract repeated logic only after the behavior and owner
  are clear. Avoid a generic framework for one caller.
- Preserve typed causes in errors. Use the workspace's `thiserror` derive
  for straightforward `Display` and `Error` implementations; present action
  context at the UI boundary. Error messages use lowercase without a final
  period. Avoid `{:?}` for user-facing errors. When refactoring an existing
  error, pin its message, `Error::source`, conversions, and trait bounds in
  tests first. `#[from]` exposes a cause; add it only when that source contract
  is intended. Keep error types with their owning crate, not a shared catch-all.
- Use domain ID newtypes and `_us`, `_ms`, `_rad`, `_deg`, and `_m` suffixes at
  unit boundaries. `delog-core::time` is the owner of canonical time rules.
- Comments explain why: invariants, precision traps, ordering, and upstream
  quirks. Prefer `//!` for a module contract and `///` for a public item.
  `// SAFETY:` precedes an unsafe block. Do not restate the next line.
- Test names describe behavior. Keep a short unit test beside its code; use
  a sibling `tests.rs` or integration test when a larger fixture or public
  boundary is involved. Test real results and error paths, not source text.

## UI and performance

Use `ui::icons`, `ui::components`, `ui::design_tokens`, and the active theme
for shared controls. A reused component must preserve enabled state, hover
text, size, color, and widget identity where those affect interaction.
Keep UI labels and error context with the calling feature.

In ingest, cache, and render loops, preserve bounded queues, short lock
scopes, and existing allocation patterns. Avoid dynamic dispatch, new
allocations, or synchronization in a hot path just to remove duplicate
syntax. Compare a relevant existing benchmark or profile before and after
changing an algorithm. For structural refactors, first pin externally
visible behavior in tests, then run the same tests on the new implementation.

## Local checks

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --locked --no-default-features -- -D warnings
cargo test --workspace --locked --exclude delog-script -- --test-threads=1
cargo test -p delog-script --locked -- --test-threads=1
```

Run the relevant package's tests while implementing; run the full matrix
before integrating. The single-threaded Rust run also avoids concurrent
headless GPU initialization on systems whose Vulkan loader is not stable
under parallel tests. If a feature adds another language or client, run its
own test suite as well. Route changes require explicit route-contract tests.
