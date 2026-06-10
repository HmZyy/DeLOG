# `assets/` — compile-time embedded resources

Everything DeLOG ships in its binary lives here and is embedded at build time
(PLAN.md §3.1). There is no runtime asset directory to discover, so a stripped
single binary always renders.

| Path             | Embedded via    | Owner / consumer                          | Lands in |
| ---------------- | --------------- | ----------------------------------------- | -------- |
| `palette.rs`     | `include!`      | `delog-render::palette` (re-exported to app) | ARC-06   |
| `shaders/*.wgsl` | `include_str!`  | `delog-render` pipeline constructors      | GPU-05 + |
| `models/*.glb`   | `include_bytes!`| `delog-render` model registry             | TDV-08   |

## Conventions

- **Palette** (`palette.rs`) is the single source of truth for trace colors
  across plots, legend dots and 3D paths. It is `include!`d rather than declared
  as a module so the same constants compile into whichever crate needs them
  without an upward crate dependency. Keep it `include!`-safe: no leading `//!`
  inner-doc lines.
- **Shaders** are `include_str!`'d at the pipeline that owns them, not loaded at
  runtime, so a shader typo is a compile/test failure (headless golden-image
  tests, PLAN.md §20.3) rather than a black frame in the field.
- **Models** are `include_bytes!`'d by the model registry. A procedural cone is
  the unconditional fallback (PLAN.md §10.3), so a missing or corrupt GLB can
  never blank the 3D scene.
