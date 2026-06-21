//! DeLOG renderer: wgpu pipelines, the GPU buffer manager, the 2D plot
//! pass and the 3D scene.
//!
//! Dependency rule: this crate is pure wgpu — **no egui
//! types** — so headless benchmarks and golden-image tests can drive it
//! without a window. `delog-app` adapts it through `egui_wgpu` callbacks.
//!
//! ## Embedded assets
//!
//! Everything the renderer ships lives under the workspace `assets/` directory
//! and is embedded at compile time — there is no runtime asset directory to
//! locate, so a stripped single binary always renders:
//!
//! - **Palette** — [`palette`], `include!`d from `assets/palette.rs`.
//! - **Shaders** — WGSL sources are pulled in with `include_str!` at each
//!   pipeline's construction site (e.g. the `line_pull` shader via
//!   `include_str!(".../assets/shaders/line_pull.wgsl")`).
//! - **Models** — vehicle GLBs are pulled in with `include_bytes!` by the model
//!   registry (quad / fixed-wing / delta / marker), with a
//!   procedural cone as the unconditional fallback.

pub mod buffers;
pub mod context;
pub mod errors;
pub mod grid3d;
pub mod line;
pub mod mesh;
pub mod minmax;
pub mod palette;
pub mod scatter;
pub mod scene_target;
pub mod step;
pub mod target;
pub mod traj3d;
pub mod uniforms;

pub use buffers::{BufferManager, UploadStat};
pub use context::RenderContext;
pub use errors::GpuErrorHub;
pub use grid3d::{Grid3dPipeline, GridUniform};
pub use line::LinePipeline;
pub use mesh::{MeshCpu, MeshError, MeshGpu, MeshPipeline, MeshUniform, Vertex, load_glb};
pub use minmax::{COLUMN_STRIDE, MinMaxColPipeline};
pub use scatter::ScatterPipeline;
pub use scene_target::{COLOR_FORMAT, DEPTH_FORMAT, SAMPLE_COUNT, Scene3dTarget};
pub use step::StepPipeline;
pub use target::{OffscreenTarget, RgbaImage};
pub use traj3d::{Traj3dPipeline, Traj3dUniform};
pub use uniforms::{PlotUniform, UniformRing};
