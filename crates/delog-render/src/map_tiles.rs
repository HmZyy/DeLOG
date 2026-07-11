//! Textured map tiles placed on arbitrary render-space quadrilaterals.

use crate::RenderContext;
use std::collections::{HashMap, HashSet};

const TILE_SIZE: u32 = 256;
const LAYER_COUNT: u32 = 128;

#[derive(Clone, Copy, Debug)]
pub struct MapTileUpload<'a> {
    pub key: u64,
    pub rgba: &'a [u8],
    pub corners: [[f32; 3]; 4],
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MapTileError {
    #[error("tile RGBA data must contain exactly 256 x 256 x 4 bytes")]
    InvalidImageSize,
    #[error("all 128 map tile texture layers are occupied")]
    Full,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    uv: [f32; 2],
    layer: u32,
}

struct Tile {
    layer: u32,
    vertices: wgpu::Buffer,
}

pub struct MapTilePipeline {
    ctx: RenderContext,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    texture: wgpu::Texture,
    uniform: wgpu::Buffer,
    tiles: HashMap<u64, Tile>,
    free_layers: Vec<u32>,
}

impl MapTilePipeline {
    pub fn new(
        ctx: &RenderContext,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        sample_count: u32,
    ) -> Self {
        let device = ctx.device();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("delog-map-tiles-array"),
            size: wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: LAYER_COUNT,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("delog-map-tiles-array-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("delog-map-tiles-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("delog-map-tiles-view-proj"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("delog-map-tiles-bind-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("delog-map-tiles-bind-group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("delog-map-tiles.wgsl"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../../../assets/shaders/map_tiles.wgsl").into(),
            ),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("delog-map-tiles-pipeline-layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("delog-map-tiles-pipeline"), layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader, entry_point: Some("vs_main"), compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Uint32],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader, entry_point: Some("fs_main"), compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: color_format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState { format: depth_format, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::LessEqual), stencil: Default::default(), bias: Default::default() }),
            multisample: wgpu::MultisampleState { count: sample_count, ..Default::default() },
            multiview_mask: None, cache: None,
        });
        let identity = glam_identity();
        ctx.queue()
            .write_buffer(&uniform, 0, bytemuck::cast_slice(&identity));
        Self {
            ctx: ctx.clone(),
            pipeline,
            bind_group,
            texture,
            uniform,
            tiles: HashMap::new(),
            free_layers: (0..LAYER_COUNT).rev().collect(),
        }
    }

    pub fn upload(&mut self, upload: MapTileUpload<'_>) -> Result<(), MapTileError> {
        if upload.rgba.len() != (TILE_SIZE * TILE_SIZE * 4) as usize {
            return Err(MapTileError::InvalidImageSize);
        }
        let layer = self
            .tiles
            .get(&upload.key)
            .map(|tile| tile.layer)
            .or_else(|| self.free_layers.pop())
            .ok_or(MapTileError::Full)?;
        self.ctx.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            upload.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(TILE_SIZE * 4),
                rows_per_image: Some(TILE_SIZE),
            },
            wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
        let c = upload.corners;
        let vertices = [
            Vertex {
                position: c[0],
                uv: [0.0, 1.0],
                layer,
            },
            Vertex {
                position: c[1],
                uv: [1.0, 1.0],
                layer,
            },
            Vertex {
                position: c[2],
                uv: [1.0, 0.0],
                layer,
            },
            Vertex {
                position: c[0],
                uv: [0.0, 1.0],
                layer,
            },
            Vertex {
                position: c[2],
                uv: [1.0, 0.0],
                layer,
            },
            Vertex {
                position: c[3],
                uv: [0.0, 0.0],
                layer,
            },
        ];
        let buffer = self.ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("delog-map-tile-vertices"),
            size: std::mem::size_of_val(&vertices) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.ctx
            .queue()
            .write_buffer(&buffer, 0, bytemuck::cast_slice(&vertices));
        self.tiles.insert(
            upload.key,
            Tile {
                layer,
                vertices: buffer,
            },
        );
        Ok(())
    }

    pub fn retain(&mut self, keys: impl IntoIterator<Item = u64>) {
        let keep: HashSet<u64> = keys.into_iter().collect();
        self.tiles.retain(|key, tile| {
            if keep.contains(key) {
                true
            } else {
                self.free_layers.push(tile.layer);
                false
            }
        });
    }

    pub fn set_view_proj(&self, view_proj: [[f32; 4]; 4]) {
        self.ctx
            .queue()
            .write_buffer(&self.uniform, 0, bytemuck::cast_slice(&view_proj));
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        for tile in self.tiles.values() {
            pass.set_vertex_buffer(0, tile.vertices.slice(..));
            pass.draw(0..6, 0..1);
        }
    }
}

fn glam_identity() -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

#[cfg(test)]
mod tests {
    use super::{MapTilePipeline, MapTileUpload};
    use crate::{Grid3dPipeline, GridUniform, RenderContext, Scene3dTarget};

    struct NearTrianglePipeline(wgpu::RenderPipeline);

    impl NearTrianglePipeline {
        fn new(ctx: &RenderContext) -> Self {
            let shader = ctx
                .device()
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("map-tile-depth-test-triangle"),
                    source: wgpu::ShaderSource::Wgsl(
                        r#"
@vertex
fn vs(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-0.4, -0.4),
        vec2<f32>(0.4, -0.4),
        vec2<f32>(0.0, 0.4),
    );
    return vec4<f32>(positions[vertex], 0.2, 1.0);
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 1.0, 0.0, 1.0);
}
"#
                        .into(),
                    ),
                });
            let layout = ctx
                .device()
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("map-tile-depth-test-triangle-layout"),
                    bind_group_layouts: &[],
                    immediate_size: 0,
                });
            Self(
                ctx.device()
                    .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: Some("map-tile-depth-test-triangle-pipeline"),
                        layout: Some(&layout),
                        vertex: wgpu::VertexState {
                            module: &shader,
                            entry_point: Some("vs"),
                            compilation_options: Default::default(),
                            buffers: &[],
                        },
                        fragment: Some(wgpu::FragmentState {
                            module: &shader,
                            entry_point: Some("fs"),
                            compilation_options: Default::default(),
                            targets: &[Some(wgpu::ColorTargetState {
                                format: crate::COLOR_FORMAT,
                                blend: None,
                                write_mask: wgpu::ColorWrites::ALL,
                            })],
                        }),
                        primitive: wgpu::PrimitiveState::default(),
                        depth_stencil: Some(wgpu::DepthStencilState {
                            format: crate::DEPTH_FORMAT,
                            depth_write_enabled: Some(true),
                            depth_compare: Some(wgpu::CompareFunction::Less),
                            stencil: Default::default(),
                            bias: Default::default(),
                        }),
                        multisample: wgpu::MultisampleState {
                            count: crate::SAMPLE_COUNT,
                            ..Default::default()
                        },
                        multiview_mask: None,
                        cache: None,
                    }),
            )
        }

        fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
            pass.set_pipeline(&self.0);
            pass.draw(0..3, 0..1);
        }
    }

    fn solid(rgba: [u8; 4]) -> Vec<u8> {
        rgba.repeat(256 * 256)
    }

    fn render(tiles: &[MapTileUpload<'_>], view_proj: glam::Mat4) -> crate::RgbaImage {
        let ctx = RenderContext::headless().expect("headless adapter");
        let target = Scene3dTarget::new(ctx.clone(), 64, 64);
        let mut pipeline = MapTilePipeline::new(
            &ctx,
            target.color_format(),
            target.depth_format(),
            target.sample_count(),
        );
        for tile in tiles {
            pipeline.upload(*tile).unwrap();
        }
        pipeline.set_view_proj(view_proj.to_cols_array_2d());
        let mut encoder = ctx.device().create_command_encoder(&Default::default());
        {
            let mut pass = target.begin_pass(&mut encoder, wgpu::Color::BLACK);
            pipeline.draw(&mut pass);
        }
        ctx.queue().submit([encoder.finish()]);
        ctx.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        target.read_rgba()
    }

    #[test]
    fn map_tiles_upload_draw_and_ground_placement() {
        let red = solid([255, 0, 0, 255]);
        let blue = solid([0, 0, 255, 255]);
        let view_proj = glam::Mat4::perspective_rh(60f32.to_radians(), 1.0, 0.1, 100.0)
            * glam::Mat4::look_at_rh(
                glam::Vec3::new(0.0, 3.0, 3.0),
                glam::Vec3::ZERO,
                glam::Vec3::Y,
            );
        let image = render(
            &[
                MapTileUpload {
                    key: 1,
                    rgba: &red,
                    corners: [
                        [-2.0, 0.0, 1.0],
                        [0.0, 0.0, 1.0],
                        [0.0, 0.0, -1.0],
                        [-2.0, 0.0, -1.0],
                    ],
                },
                MapTileUpload {
                    key: 2,
                    rgba: &blue,
                    corners: [
                        [0.0, 0.0, 1.0],
                        [2.0, 0.0, 1.0],
                        [2.0, 0.0, -1.0],
                        [0.0, 0.0, -1.0],
                    ],
                },
            ],
            view_proj,
        );
        assert!(image.matches(24, 36, [255, 0, 0, 255], 4));
        assert!(image.matches(40, 36, [0, 0, 255, 255], 4));
    }

    #[test]
    fn map_tiles_retain_reuses_freed_layer() {
        let ctx = RenderContext::headless().expect("headless adapter");
        let mut pipeline = MapTilePipeline::new(
            &ctx,
            crate::COLOR_FORMAT,
            crate::DEPTH_FORMAT,
            crate::SAMPLE_COUNT,
        );
        let pixels = solid([255, 0, 0, 255]);
        let corners = [
            [-1.0, -1.0, 0.5],
            [1.0, -1.0, 0.5],
            [1.0, 1.0, 0.5],
            [-1.0, 1.0, 0.5],
        ];
        for key in 0..128 {
            pipeline
                .upload(MapTileUpload {
                    key,
                    rgba: &pixels,
                    corners,
                })
                .unwrap();
        }
        pipeline.retain([127]);
        pipeline
            .upload(MapTileUpload {
                key: 128,
                rgba: &pixels,
                corners,
            })
            .expect("freed layer is reusable");
    }

    #[test]
    fn map_tile_depth_composes_with_nearer_triangle_independent_of_hash_order() {
        let red = solid([255, 0, 0, 255]);
        let blue = solid([0, 0, 255, 255]);
        let full_screen = |depth| {
            [
                [-1.0, -1.0, depth],
                [1.0, -1.0, depth],
                [1.0, 1.0, depth],
                [-1.0, 1.0, depth],
            ]
        };

        for reverse in [false, true] {
            let ctx = RenderContext::headless().expect("headless adapter");
            let target = Scene3dTarget::new(ctx.clone(), 64, 64);
            let mut tiles = MapTilePipeline::new(
                &ctx,
                target.color_format(),
                target.depth_format(),
                target.sample_count(),
            );
            let uploads = [
                MapTileUpload {
                    key: if reverse { 2 } else { 1 },
                    rgba: &red,
                    corners: full_screen(0.8),
                },
                MapTileUpload {
                    key: if reverse { 1 } else { 2 },
                    rgba: &blue,
                    corners: full_screen(0.6),
                },
            ];
            let insertion_order = if reverse { [1, 0] } else { [0, 1] };
            for index in insertion_order {
                tiles.upload(uploads[index]).unwrap();
            }
            tiles.set_view_proj(glam::Mat4::IDENTITY.to_cols_array_2d());
            let triangle = NearTrianglePipeline::new(&ctx);
            let mut encoder = ctx.device().create_command_encoder(&Default::default());
            {
                let mut pass = target.begin_pass(&mut encoder, wgpu::Color::BLACK);
                triangle.draw(&mut pass);
                tiles.draw(&mut pass);
            }
            ctx.queue().submit([encoder.finish()]);
            ctx.device()
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            let image = target.read_rgba();
            assert!(image.matches(32, 32, [0, 255, 0, 255], 4));
            assert!(image.matches(16, 32, [0, 0, 255, 255], 4));
        }
    }

    #[test]
    fn scene_draw_order_places_tiles_before_depth_composed_overlays() {
        let red = solid([255, 0, 0, 255]);
        let ctx = RenderContext::headless().expect("headless adapter");
        let target = Scene3dTarget::new(ctx.clone(), 64, 64);
        let mut tiles = MapTilePipeline::new(
            &ctx,
            target.color_format(),
            target.depth_format(),
            target.sample_count(),
        );
        tiles
            .upload(MapTileUpload {
                key: 1,
                rgba: &red,
                corners: [
                    [-1.0, -1.0, 0.8],
                    [1.0, -1.0, 0.8],
                    [1.0, 1.0, 0.8],
                    [-1.0, 1.0, 0.8],
                ],
            })
            .unwrap();
        tiles.set_view_proj(glam::Mat4::IDENTITY.to_cols_array_2d());
        let grid = Grid3dPipeline::new(
            &ctx,
            target.color_format(),
            target.depth_format(),
            target.sample_count(),
        );
        grid.set_uniform(
            &ctx,
            &GridUniform::new(
                glam::Mat4::IDENTITY.to_cols_array_2d(),
                glam::Mat4::IDENTITY.to_cols_array_2d(),
                [0.0, 1.0, 0.0],
                1.0,
                10.0,
                100.0,
                false,
                false,
            ),
        );
        let trajectory_or_vehicle = NearTrianglePipeline::new(&ctx);
        let mut encoder = ctx.device().create_command_encoder(&Default::default());
        {
            let mut pass = target.begin_pass(&mut encoder, wgpu::Color::BLACK);
            tiles.draw(&mut pass);
            grid.draw(&mut pass);
            trajectory_or_vehicle.draw(&mut pass);
        }
        ctx.queue().submit([encoder.finish()]);
        ctx.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let image = target.read_rgba();
        assert!(image.matches(32, 32, [0, 255, 0, 255], 4));
        assert!(image.matches(8, 32, [255, 0, 0, 255], 4));
    }
}
