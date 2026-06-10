//! DeLOG renderer: wgpu pipelines, the GPU buffer manager, the 2D plot
//! pass and the 3D scene.
//!
//! Dependency rule (PLAN.md §3.2): this crate is pure wgpu — **no egui
//! types** — so headless benchmarks and golden-image tests can drive it
//! without a window. `delog-app` adapts it through `egui_wgpu` callbacks.
