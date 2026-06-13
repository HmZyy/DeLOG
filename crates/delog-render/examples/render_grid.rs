//! Visual verification for the 3D offscreen target (GPU-20) and the infinite
//! ground grid + axes (GPU-21): renders the grid headlessly from two camera
//! angles and writes PNGs you can open. There is no in-app scene pane yet
//! (TDV-01), so this is the surface where the rendered output is observable.
//!
//! Run: `cargo run -p delog-render --example render_grid`
//! Output: `/tmp/delog_grid_perspective.png`, `/tmp/delog_grid_topdown.png`

use std::fs::File;
use std::io::BufWriter;

use delog_render::{Grid3dPipeline, GridUniform, RenderContext, Scene3dTarget};
use glam::{Mat4, Vec3};

fn render(ctx: &RenderContext, w: u32, h: u32, eye: Vec3, up: Vec3) -> Vec<u8> {
    let target = Scene3dTarget::new(ctx.clone(), w, h);
    let grid = Grid3dPipeline::new(
        ctx,
        target.color_format(),
        target.depth_format(),
        target.sample_count(),
    );

    let proj = Mat4::perspective_rh(55f32.to_radians(), w as f32 / h as f32, 0.1, 500.0);
    let view = Mat4::look_at_rh(eye, Vec3::ZERO, up);
    let view_proj = proj * view;
    grid.set_uniform(
        ctx,
        &GridUniform::new(
            view_proj.to_cols_array_2d(),
            view_proj.inverse().to_cols_array_2d(),
            eye.to_array(),
            1.0,
            20.0,
            120.0,
            true,
            false,
        ),
    );

    // A dark slate background so grid lines and colored axes read clearly.
    let clear = wgpu::Color {
        r: 18.0 / 255.0,
        g: 20.0 / 255.0,
        b: 26.0 / 255.0,
        a: 1.0,
    };
    let mut enc = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = target.begin_pass(&mut enc, clear);
        grid.draw(&mut pass);
    }
    ctx.queue().submit([enc.finish()]);
    ctx.device()
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();

    target.read_rgba().pixels
}

fn write_png(path: &str, w: u32, h: u32, rgba: &[u8]) {
    let file = File::create(path).unwrap();
    let mut encoder = png::Encoder::new(BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(rgba).unwrap();
    println!("wrote {path}");
}

fn main() {
    let Some(ctx) = RenderContext::headless() else {
        eprintln!("no wgpu adapter available — cannot render");
        std::process::exit(1);
    };
    let (w, h) = (960u32, 720u32);

    // Classic floor perspective: camera above and behind, looking at origin.
    let persp = render(&ctx, w, h, Vec3::new(10.0, 8.0, 16.0), Vec3::Y);
    write_png("/tmp/delog_grid_perspective.png", w, h, &persp);

    // Near top-down: the red X (East) and blue Z (South) axes cross at origin.
    let top = render(&ctx, w, h, Vec3::new(0.5, 22.0, 0.5), Vec3::Z);
    write_png("/tmp/delog_grid_topdown.png", w, h, &top);

    // Probe: extreme wide aspect — cells must stay square (aspect handled).
    let wide = render(&ctx, 1280, 360, Vec3::new(0.5, 22.0, 0.5), Vec3::Z);
    write_png("/tmp/delog_grid_wide.png", 1280, 360, &wide);
}
