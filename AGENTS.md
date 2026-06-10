# Repository Guidelines

## Project Structure & Module Organization

DeLOG is being rewritten from scratch in Rust. `PLAN.md` is the source of truth for architecture, milestones, and the master checklist; read section 0 before starting work. The current repository is documentation-only until the workspace scaffold is added.

Planned layout:

```text
Cargo.toml              # workspace and pinned dependencies
crates/delog-core/      # IDs, time model, Arrow store, snapshots, diagnostics
crates/delog-parsers/   # AP BIN, ULog, tlog, and future CSV parsers
crates/delog-stream/    # live MAVLink links and recording
crates/delog-cache/     # f32 render caches and min/max pyramids
crates/delog-render/    # pure wgpu renderer
crates/delog-app/       # eframe/egui UI shell and glue
assets/                 # GLB models, WGSL shaders, icons, palette
fixtures/               # small real logs and synthetic test generators
benches/                # Criterion benchmark harnesses
```

Keep dependency direction exactly as specified in `PLAN.md` section 3.2.

## Build, Test, and Development Commands

After the Rust workspace exists, use:

```bash
cargo build --workspace
cargo fmt --all
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo test -p delog-core <test_name>
cargo bench
cargo run -p delog-app
```

`clippy` must be warning-free for completed checklist work. Run `cargo bench` when changing parser, ingest, cache, query, GPU upload, or paint hot paths.

## Coding Style & Naming Conventions

Use stable Rust, edition 2024. Follow `rustfmt` defaults. Prefer small, explicit modules named by domain behavior, for example `time`, `snapshot`, `pyramid`, or `bin_parser`. Crate names use the `delog-*` pattern.

Preserve zero-copy invariants from `PLAN.md` section 4.5. Cite deliberate invariant-sensitive code with comments such as `// upholds ZC-3`; justified exceptions require `// ZC-EXCEPTION: <reason>` and a metric.

## Testing Guidelines

Add unit tests for non-trivial logic, parser golden tests using `fixtures/`, `proptest` coverage for min/max pyramid behavior, fuzz targets for frame decoders, and headless `wgpu` golden-image tests for renderer behavior. Test names should describe the behavior under test, e.g. `preserves_nan_gaps_in_trace_cache`.

## Commit & Pull Request Guidelines

Recent history uses short imperative commit messages, for example `Render plot lines with OpenGL buffers` and `Populate performance metrics dock`. Keep commits focused and update the relevant `PLAN.md` checklist item in the same commit as the implementation.

Pull requests should include a concise description, linked checklist IDs, test/benchmark results, and screenshots or recordings for UI changes. Call out any zero-copy exceptions, performance budget changes, or dependency-direction concerns explicitly.
