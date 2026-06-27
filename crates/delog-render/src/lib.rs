//! DeLOG renderer: wgpu pipelines, the GPU buffer manager, the 2D plot
//! pass and the 3D scene.
//!
//! Invariant: this crate is pure wgpu (no egui types), so headless benchmarks
//! and golden-image tests can drive it without a window. All shipped assets
//! (palette, shaders, models) are embedded at compile time.

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
