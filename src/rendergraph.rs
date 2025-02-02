use rend3::{
    types::SampleCount, util::bind_merge::BindGroupBuilder, DataHandle, DepthHandle, ReadyData,
    RenderGraph, RenderPassDepthTarget, RenderPassTarget, RenderPassTargets,
    RenderTargetDescriptor,
};
use rend3_routine::{
    material::TransparencyType, uniforms, CulledPerMaterial, PbrRenderRoutine, SkyboxRoutine,
    TonemappingRoutine,
};
use wgpu::{
    util::{BufferInitDescriptor, DeviceExt},
    BindGroup, BindGroupLayout, Buffer, Color, Device, PipelineLayoutDescriptor, RenderPipeline,
    RenderPipelineDescriptor, TextureFormat, TextureUsages,
};

use self::wireframe_pass::WireframeRoutine;

pub mod wireframe_pass;

struct PerTransparencyInfo {
    ty: TransparencyType,
    pre_cull: DataHandle<Buffer>,
    cull: DataHandle<CulledPerMaterial>,
}

pub fn add_uniform_bg_creation_to_graph<'node>(
    pbr_routine: &'node PbrRenderRoutine,
    graph: &mut RenderGraph<'node>,
    forward_uniform_bg: DataHandle<BindGroup>,
) {
    let mut builder = graph.add_node("build uniform data");
    let forward_handle = builder.add_data_output(forward_uniform_bg);
    builder.build(
        move |_pt, renderer, _encoder_or_pass, _temps, _ready, graph_data| {
            let mut bgb = BindGroupBuilder::new();

            pbr_routine.samplers.add_to_bg(&mut bgb);

            let uniform_buffer =
                uniforms::create_shader_uniform(uniforms::CreateShaderUniformArgs {
                    device: &renderer.device,
                    camera: graph_data.camera_manager,
                    interfaces: &pbr_routine.interfaces,
                    ambient: pbr_routine.ambient,
                });

            bgb.append_buffer(&uniform_buffer);

            graph_data.directional_light_manager.add_to_bg(&mut bgb);

            let forward_uniform_bg = bgb.build(
                &renderer.device,
                Some("forward uniform bg"),
                &pbr_routine.interfaces.forward_uniform_bgl,
            );

            graph_data.set_data(forward_handle, Some(forward_uniform_bg));
        },
    )
}

pub fn add_default_rendergraph<'node>(
    graph: &mut RenderGraph<'node>,
    _ready: &ReadyData,
    pbr: &'node PbrRenderRoutine,
    _skybox: Option<&'node SkyboxRoutine>,
    tonemapping: &'node TonemappingRoutine,
    _wireframe: &'node WireframeRoutine,
    grid: &'node GridRoutine,
    samples: SampleCount,
) {
    // Setup all of our per-transparency data
    let mut per_transparency = Vec::with_capacity(1);
    for ty in [TransparencyType::Opaque] {
        per_transparency.push(PerTransparencyInfo {
            ty,
            pre_cull: graph.add_data(),
            cull: graph.add_data(),
        })
    }

    // A lot of things don't deal with blending, so lets make a subslice for that situation.
    let per_transparency_no_blend = &per_transparency[..1];

    // Add pre-culling
    for trans in &per_transparency {
        pbr.add_pre_cull_to_graph(graph, trans.ty, trans.pre_cull);
    }

    // Create global bind group information
    let forward_uniform_bg = graph.add_data::<BindGroup>();
    add_uniform_bg_creation_to_graph(&pbr, graph, forward_uniform_bg);

    let grid_uniform_bg = graph.add_data::<BindGroup>();
    grid.create_bind_groups(graph, grid_uniform_bg);

    // Add primary culling
    for trans in &per_transparency {
        pbr.add_culling_to_graph(graph, trans.ty, trans.pre_cull, trans.cull);
    }

    let mut resolution = pbr.render_texture_options.resolution;
    resolution.y /= 2;

    // Make the actual render targets we want to render to.
    let color = graph.add_render_target(RenderTargetDescriptor {
        label: Some("hdr color".into()),
        dim: resolution,//pbr.render_texture_options.resolution,
        samples,
        format: TextureFormat::Rgba16Float,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
    });
    let resolve = samples.needs_resolve().then(|| {
        graph.add_render_target(RenderTargetDescriptor {
            label: Some("hdr resolve".into()),
            dim: resolution,//pbr.render_texture_options.resolution,
            samples: SampleCount::One,
            format: TextureFormat::Rgba16Float,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        })
    });
    let depth = graph.add_render_target(RenderTargetDescriptor {
        label: Some("hdr depth".into()),
        dim: resolution,//pbr.render_texture_options.resolution,
        samples,
        format: TextureFormat::Depth32Float,
        usage: TextureUsages::RENDER_ATTACHMENT,
    });

    // Add depth prepass
    for trans in per_transparency_no_blend {
        pbr.add_prepass_to_graph(
            graph,
            trans.ty,
            color,
            resolve,
            depth,
            forward_uniform_bg,
            trans.cull,
        );
    }

    // Add primary rendering
    for trans in &per_transparency {
        pbr.add_forward_to_graph(
            graph,
            trans.ty,
            color,
            resolve,
            depth,
            forward_uniform_bg,
            trans.cull,
            false,
        );
    }

    grid.add_to_graph(graph, color, depth, resolve, grid_uniform_bg);

    /*
    // Add wireframe rendering
    for trans in &per_transparency {
        pbr.add_forward_to_graph(
            graph,
            trans.ty,
            color,
            resolve,
            depth,
            forward_uniform_bg,
            trans.cull,
            true,
        );
    }
    */

    //wireframe.add_to_graph(graph, color);

    // Make the reference to the surface
    let surface = graph.add_surface_texture();

    tonemapping.add_to_graph(graph, resolve.unwrap_or(color), surface, forward_uniform_bg);
}

pub struct GridRoutine {
    pipeline: RenderPipeline,
    bgl: BindGroupLayout,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable, Default)]
pub struct GridRoutineUniform {
    pub view: [[f32; 4]; 4],
    pub proj: [[f32; 4]; 4],
    pub inv_view: [[f32; 4]; 4],
    pub inv_proj: [[f32; 4]; 4],
}

impl GridRoutine {
    pub fn new(device: &Device) -> Self {
        use wgpu::*;
        let shader = device.create_shader_module(&wgpu::ShaderModuleDescriptor {
            label: Some("Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let uniform_buffer = device.create_buffer(&BufferDescriptor {
            label: Some("Grid uniform buffer"),
            size: std::mem::size_of::<GridRoutineUniform>() as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::MAP_WRITE,
            mapped_at_creation: false,
        });

        let bgl = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("Grid BGL"),
            entries: &[BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::VERTEX_FRAGMENT,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("Grid pipeline layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("Grid Pipeline"),
            layout: Some(&layout),
            vertex: VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
            },
            primitive: PrimitiveState {
                topology: PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: FrontFace::Ccw,
                cull_mode: Some(Face::Back),
                clamp_depth: false,
                polygon_mode: PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(DepthStencilState {
                format: TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: CompareFunction::GreaterEqual,
                stencil: StencilState::default(),
                bias: DepthBiasState::default(),
            }),
            multisample: MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[wgpu::ColorTargetState {
                    format: TextureFormat::Rgba16Float,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                }],
            }),
        });

        Self { pipeline, bgl }
    }

    fn add_to_graph<'node>(
        &'node self,
        graph: &mut RenderGraph<'node>,
        color: rend3::RenderTargetHandle,
        depth: rend3::RenderTargetHandle,
        resolve: Option<rend3::RenderTargetHandle>,
        grid_uniform_bg: DataHandle<BindGroup>,
    ) {
        let mut builder = graph.add_node("Infinite Grid");
        let color_handle = builder.add_render_target_output(color);
        let resolve = builder.add_optional_render_target_output(resolve);
        let depth_handle = builder.add_render_target_output(depth);

        let rpass_handle = builder.add_renderpass(RenderPassTargets {
            targets: vec![RenderPassTarget {
                color: color_handle,
                clear: Color::BLACK,
                resolve: resolve,
            }],
            depth_stencil: Some(RenderPassDepthTarget {
                target: DepthHandle::RenderTarget(depth_handle),
                depth_clear: Some(0.0),
                stencil_clear: None,
            }),
        });

        let grid_uniform_handle = builder.add_data_input(grid_uniform_bg);
        let pt_handle = builder.passthrough_ref(self);

        builder.build(
            move |pt, renderer, encoder_or_pass, temps, ready, graph_data| {
                let this = pt.get(pt_handle);
                let rpass = encoder_or_pass.get_rpass(rpass_handle);
                let grid_uniform_bg = graph_data.get_data(temps, grid_uniform_handle).unwrap();

                rpass.set_bind_group(0, grid_uniform_bg, &[]);
                rpass.set_pipeline(&this.pipeline);
                rpass.draw(0..6, 0..1);
            },
        );
    }

    fn create_bind_groups<'node>(
        &'node self,
        graph: &mut RenderGraph<'node>,
        grid_uniform_bg: DataHandle<BindGroup>,
    ) {
        use wgpu::*;
        let mut builder = graph.add_node("build grid uniforms");
        let output_handle = builder.add_data_output(grid_uniform_bg);
        let pt_handle = builder.passthrough_ref(self);
        builder.build(
            move |pt, renderer, _encoder_or_pass, _temps, _ready, graph_data| {
                let this = pt.get(pt_handle);

                let camera_manager = renderer.camera_manager.read();
                let cam_data = GridRoutineUniform {
                    view: camera_manager.view().to_cols_array_2d(),
                    proj: camera_manager.proj().to_cols_array_2d(),
                    inv_view: camera_manager.view().inverse().to_cols_array_2d(),
                    inv_proj: camera_manager.proj().inverse().to_cols_array_2d(),
                };

                let buffer = renderer.device.create_buffer_init(&BufferInitDescriptor {
                    label: None,
                    contents: bytemuck::cast_slice(&[cam_data]),
                    usage: BufferUsages::UNIFORM,
                });

                let bind_group = renderer.device.create_bind_group(&BindGroupDescriptor {
                    label: Some("Grid BindGroup"),
                    layout: &this.bgl,
                    entries: &[BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                });

                graph_data.set_data(output_handle, Some(bind_group));
            },
        );
    }
}
