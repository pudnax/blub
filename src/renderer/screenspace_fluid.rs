use crate::hybrid_fluid::*;
use crate::render_output::hdr_backbuffer::HdrBackbuffer;
use crate::render_output::screen::Screen;
use crate::wgpu_utils::pipelines::*;
use crate::wgpu_utils::{
    self,
    binding_builder::{BindGroupBuilder, BindGroupLayoutBuilder, BindGroupLayoutWithDesc},
    binding_glsl,
    shader::*,
};
use std::path::{Path, PathBuf};
use std::rc::Rc;

struct ScreenDependentProperties {
    texture_view_fluid_view: [wgpu::TextureView; 2],
    texture_view_fluid_thickness: [wgpu::TextureView; 2],
    bind_group_narrow_range_filter: [wgpu::BindGroup; 2],
    bind_group_thickness_filter: [wgpu::BindGroup; 2],
    bind_group_compose: wgpu::BindGroup,
    target_textures_resolution: wgpu::Extent3d,
}

struct ScreenIndependentProperties {
    pipeline_render_particles: RenderPipelineHandle,

    pipeline_narrow_range_filter_1d: ComputePipelineHandle,
    pipeline_narrow_range_filter_2d: ComputePipelineHandle,
    group_layout_narrow_range_filter: BindGroupLayoutWithDesc,

    pipeline_thickness_filter: ComputePipelineHandle,
    group_layout_thickness_filter: BindGroupLayoutWithDesc,

    pipeline_fluid: ComputePipelineHandle,
    group_layout_compose: BindGroupLayoutWithDesc,
}

pub struct ScreenSpaceFluid {
    screen_independent: ScreenIndependentProperties,
    screen_dependent: ScreenDependentProperties,
}

impl ScreenSpaceFluid {
    const FORMAT_FLUID_DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
    const FORMAT_FLUID_THICKNESS: wgpu::TextureFormat = wgpu::TextureFormat::R16Float; // TODO: Smaller?

    pub fn new(
        device: &wgpu::Device,
        shader_dir: &ShaderDirectory,
        pipeline_manager: &mut PipelineManager,
        per_frame_bind_group_layout: &wgpu::BindGroupLayout,
        fluid_renderer_group_layout: &wgpu::BindGroupLayout,
        backbuffer: &HdrBackbuffer,
    ) -> ScreenSpaceFluid {
        let group_layout_narrow_range_filter = BindGroupLayoutBuilder::new()
            .next_binding_compute(binding_glsl::image2d(Self::FORMAT_FLUID_DEPTH, false)) // Fluid depth target
            .next_binding_compute(binding_glsl::texture2D()) // Fluid depth source
            .create(device, "BindGroupLayout: Narrow Range Filter");
        let group_layout_thickness_filter = BindGroupLayoutBuilder::new()
            .next_binding_compute(binding_glsl::image2d(Self::FORMAT_FLUID_THICKNESS, false)) // Fluid depth target
            .next_binding_compute(binding_glsl::texture2D()) // Fluid depth source
            .create(device, "BindGroupLayout: Narrow Range Filter");

        let group_layout_compose = BindGroupLayoutBuilder::new()
            .next_binding_compute(binding_glsl::texture2D()) // Fluid depth
            .next_binding_compute(binding_glsl::texture2D()) // Fluid thickness
            .next_binding_compute(binding_glsl::image2d(HdrBackbuffer::FORMAT, false)) // hdr backbuffer
            .create(device, "BindGroupLayout: SSFluid, Final fluid/Compose");

        let pipeline_render_particles = pipeline_manager.create_render_pipeline(
            device,
            shader_dir,
            RenderPipelineCreationDesc {
                layout: Rc::new(device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    bind_group_layouts: &[&per_frame_bind_group_layout, &fluid_renderer_group_layout],
                    push_constant_ranges: &[],
                })),
                vertex_shader_relative_path: PathBuf::from("screenspace_fluid/particles.vert"),
                fragment_shader_relative_path: Some(PathBuf::from("screenspace_fluid/particles.frag")),
                rasterization_state: Some(rasterization_state::culling_none()),
                primitive_topology: wgpu::PrimitiveTopology::TriangleStrip,
                color_states: vec![
                    wgpu::ColorStateDescriptor {
                        format: Self::FORMAT_FLUID_DEPTH,
                        color_blend: wgpu::BlendDescriptor {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Min,
                        },
                        alpha_blend: wgpu::BlendDescriptor::REPLACE,
                        write_mask: wgpu::ColorWrite::ALL,
                    },
                    wgpu::ColorStateDescriptor {
                        format: Self::FORMAT_FLUID_THICKNESS,
                        color_blend: wgpu::BlendDescriptor {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha_blend: wgpu::BlendDescriptor::REPLACE,
                        write_mask: wgpu::ColorWrite::ALL,
                    },
                ],
                depth_stencil_state: Some(wgpu::DepthStencilStateDescriptor {
                    format: Screen::FORMAT_DEPTH,
                    depth_write_enabled: false,
                    depth_compare: wgpu::CompareFunction::LessEqual,
                    stencil_front: wgpu::StencilStateFaceDescriptor::IGNORE,
                    stencil_back: wgpu::StencilStateFaceDescriptor::IGNORE,
                    stencil_read_mask: 0,
                    stencil_write_mask: 0,
                }),
                vertex_state: wgpu::VertexStateDescriptor {
                    index_format: wgpu::IndexFormat::Uint16,
                    vertex_buffers: &[],
                },
                sample_count: 1,
                sample_mask: !0,
                alpha_to_coverage_enabled: false,
            },
        );

        // Use same push constant range for all compute pipelines to improve internal Vulkan pipeline compatibility.
        let push_constant_ranges = &[wgpu::PushConstantRange {
            stages: wgpu::ShaderStage::COMPUTE,
            range: 0..4,
        }];

        let layout_narrow_range_filter = Rc::new(device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            bind_group_layouts: &[
                &per_frame_bind_group_layout,
                &fluid_renderer_group_layout,
                &group_layout_narrow_range_filter.layout,
            ],
            push_constant_ranges,
        }));
        let pipeline_narrow_range_filter_1d = pipeline_manager.create_compute_pipeline(
            device,
            shader_dir,
            ComputePipelineCreationDesc::new(
                layout_narrow_range_filter.clone(),
                Path::new("screenspace_fluid/narrow_range_filter_1d.comp"),
            ),
        );
        let pipeline_narrow_range_filter_2d = pipeline_manager.create_compute_pipeline(
            device,
            shader_dir,
            ComputePipelineCreationDesc::new(
                layout_narrow_range_filter.clone(),
                Path::new("screenspace_fluid/narrow_range_filter_2d.comp"),
            ),
        );

        let layout_thickness_filter = Rc::new(device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            bind_group_layouts: &[
                &per_frame_bind_group_layout,
                &fluid_renderer_group_layout,
                &group_layout_thickness_filter.layout,
            ],
            push_constant_ranges,
        }));
        let pipeline_thickness_filter = pipeline_manager.create_compute_pipeline(
            device,
            shader_dir,
            ComputePipelineCreationDesc::new(layout_thickness_filter.clone(), Path::new("screenspace_fluid/thickness_filter.comp")),
        );

        let pipeline_fluid = pipeline_manager.create_compute_pipeline(
            device,
            shader_dir,
            ComputePipelineCreationDesc::new(
                Rc::new(device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    bind_group_layouts: &[&per_frame_bind_group_layout, &fluid_renderer_group_layout, &group_layout_compose.layout],
                    push_constant_ranges,
                })),
                Path::new("screenspace_fluid/fluid.comp"),
            ),
        );

        let screen_independent = ScreenIndependentProperties {
            pipeline_render_particles,

            pipeline_narrow_range_filter_1d,
            pipeline_narrow_range_filter_2d,
            group_layout_narrow_range_filter,

            pipeline_thickness_filter,
            group_layout_thickness_filter,

            pipeline_fluid,
            group_layout_compose,
        };

        let screen_dependent = Self::create_screen_dependent_properties(&screen_independent, device, backbuffer);

        ScreenSpaceFluid {
            screen_dependent,
            screen_independent,
        }
    }

    fn create_screen_dependent_properties(
        screen_independent: &ScreenIndependentProperties,
        device: &wgpu::Device,
        backbuffer: &HdrBackbuffer,
    ) -> ScreenDependentProperties {
        let target_textures_resolution = wgpu::Extent3d {
            width: backbuffer.resolution().width,
            height: backbuffer.resolution().height,
            depth: 1,
        };
        let texture_fluid_depth = [
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Texture: Fluid Depth 1 (render target)"),
                size: target_textures_resolution,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: Self::FORMAT_FLUID_DEPTH,
                usage: wgpu::TextureUsage::OUTPUT_ATTACHMENT | wgpu::TextureUsage::STORAGE | wgpu::TextureUsage::SAMPLED,
            }),
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Texture: Fluid Depth 2 (blur target)"),
                size: target_textures_resolution,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: Self::FORMAT_FLUID_DEPTH,
                usage: wgpu::TextureUsage::OUTPUT_ATTACHMENT | wgpu::TextureUsage::STORAGE | wgpu::TextureUsage::SAMPLED,
            }),
        ];
        let texture_view_fluid_view = [texture_fluid_depth[0].create_default_view(), texture_fluid_depth[1].create_default_view()];

        let texture_fluid_thickness = [
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Texture: Fluid Thickness 1 (render target)"),
                size: target_textures_resolution,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: Self::FORMAT_FLUID_THICKNESS,
                usage: wgpu::TextureUsage::OUTPUT_ATTACHMENT | wgpu::TextureUsage::STORAGE | wgpu::TextureUsage::SAMPLED,
            }),
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Texture: Fluid Thickness 2 (blur target)"),
                size: target_textures_resolution,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: Self::FORMAT_FLUID_THICKNESS,
                usage: wgpu::TextureUsage::OUTPUT_ATTACHMENT | wgpu::TextureUsage::STORAGE | wgpu::TextureUsage::SAMPLED,
            }),
        ];
        let texture_view_fluid_thickness = [
            texture_fluid_thickness[0].create_default_view(),
            texture_fluid_thickness[1].create_default_view(),
        ];

        let bind_group_narrow_range_filter = [
            BindGroupBuilder::new(&screen_independent.group_layout_narrow_range_filter)
                .texture(&texture_view_fluid_view[1])
                .texture(&texture_view_fluid_view[0])
                .create(device, "BindGroup: Narrow Range filter 1"),
            BindGroupBuilder::new(&screen_independent.group_layout_narrow_range_filter)
                .texture(&texture_view_fluid_view[0])
                .texture(&texture_view_fluid_view[1])
                .create(device, "BindGroup: Narrow Range filter 2"),
        ];
        let bind_group_thickness_filter = [
            BindGroupBuilder::new(&screen_independent.group_layout_thickness_filter)
                .texture(&texture_view_fluid_thickness[1])
                .texture(&texture_view_fluid_thickness[0])
                .create(device, "BindGroup: Thickness Filter 1"),
            BindGroupBuilder::new(&screen_independent.group_layout_thickness_filter)
                .texture(&texture_view_fluid_thickness[0])
                .texture(&texture_view_fluid_thickness[1])
                .create(device, "BindGroup: Thickness Filter 2"),
        ];
        let bind_group_compose = BindGroupBuilder::new(&screen_independent.group_layout_compose)
            .texture(&texture_view_fluid_view[1])
            .texture(&texture_view_fluid_thickness[0])
            .texture(&backbuffer.texture_view())
            .create(device, "BindGroup: SSFluid, Final Compose");

        ScreenDependentProperties {
            texture_view_fluid_view,
            texture_view_fluid_thickness,
            target_textures_resolution,
            bind_group_narrow_range_filter,
            bind_group_thickness_filter,
            bind_group_compose,
        }
    }

    pub fn on_window_resize(&mut self, device: &wgpu::Device, backbuffer: &HdrBackbuffer) {
        self.screen_dependent = Self::create_screen_dependent_properties(&self.screen_independent, device, backbuffer);
    }

    pub fn draw<'a>(
        &'a self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline_manager: &'a PipelineManager,
        depthbuffer: &wgpu::TextureView,
        per_frame_bind_group: &wgpu::BindGroup,
        fluid: &HybridFluid,
    ) {
        // Set some depth value that is beyond the far plane. (could do infinity, but don't trust this is passed down correctly)
        let depth_clear_color = wgpu::Color {
            r: 999999.0,
            g: 999999.0,
            b: 999999.0,
            a: 999999.0,
        };

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[
                    wgpu::RenderPassColorAttachmentDescriptor {
                        attachment: &self.screen_dependent.texture_view_fluid_view[0],
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(depth_clear_color),
                            store: true,
                        },
                    },
                    wgpu::RenderPassColorAttachmentDescriptor {
                        attachment: &self.screen_dependent.texture_view_fluid_thickness[0],
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: true,
                        },
                    },
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachmentDescriptor {
                    attachment: depthbuffer,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: false,
                    }),
                    stencil_ops: None,
                }),
            });
            rpass.push_debug_group("screen space fluid, particles");

            rpass.set_bind_group(0, &per_frame_bind_group, &[]);
            rpass.set_bind_group(1, fluid.bind_group_renderer(), &[]);
            rpass.set_pipeline(pipeline_manager.get_render(&self.screen_independent.pipeline_render_particles));
            rpass.draw(0..4, 0..fluid.num_particles());

            rpass.pop_debug_group();
        }

        // Clear the intermediate blur targets
        {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[wgpu::RenderPassColorAttachmentDescriptor {
                    attachment: &self.screen_dependent.texture_view_fluid_view[1],
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(depth_clear_color),
                        store: true,
                    },
                }],
                depth_stencil_attachment: None,
            });
        }
        {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[wgpu::RenderPassColorAttachmentDescriptor {
                    attachment: &self.screen_dependent.texture_view_fluid_thickness[1],
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: true,
                    },
                }],
                depth_stencil_attachment: None,
            });
        }
        {
            let mut cpass = encoder.begin_compute_pass();
            cpass.set_bind_group(0, &per_frame_bind_group, &[]);
            cpass.set_bind_group(1, fluid.bind_group_renderer(), &[]);

            const LOCAL_SIZE_FILTER_1D: wgpu::Extent3d = wgpu::Extent3d {
                width: 64,
                height: 1,
                depth: 1,
            };
            let work_group_filter_1d = wgpu_utils::compute_group_size(self.screen_dependent.target_textures_resolution, LOCAL_SIZE_FILTER_1D);

            // Depth Filter
            {
                // Filter 1D
                {
                    cpass.set_pipeline(pipeline_manager.get_compute(&self.screen_independent.pipeline_narrow_range_filter_1d));

                    // Filter Y
                    cpass.set_bind_group(2, &self.screen_dependent.bind_group_narrow_range_filter[0], &[]);
                    cpass.set_push_constants(0, &[1]);
                    cpass.dispatch(work_group_filter_1d.width, work_group_filter_1d.height, work_group_filter_1d.depth);
                    // Filter X - note that since filter is not really separable, order makes a difference. Found this order visually more pleasing.
                    cpass.set_bind_group(2, &self.screen_dependent.bind_group_narrow_range_filter[1], &[]);
                    cpass.set_push_constants(0, &[0]);
                    cpass.dispatch(work_group_filter_1d.width, work_group_filter_1d.height, work_group_filter_1d.depth);
                }
                // Filter 2D
                {
                    cpass.set_pipeline(pipeline_manager.get_compute(&self.screen_independent.pipeline_narrow_range_filter_2d));
                    cpass.set_bind_group(2, &self.screen_dependent.bind_group_narrow_range_filter[0], &[]);
                    const LOCAL_SIZE_FILTER_2D: wgpu::Extent3d = wgpu::Extent3d {
                        width: 32,
                        height: 32,
                        depth: 1,
                    };
                    let work_group = wgpu_utils::compute_group_size(self.screen_dependent.target_textures_resolution, LOCAL_SIZE_FILTER_2D);
                    cpass.dispatch(work_group.width, work_group.height, work_group.depth);
                }
            }
            // Thickness filter
            {
                cpass.set_pipeline(pipeline_manager.get_compute(&self.screen_independent.pipeline_thickness_filter));

                // Filter Y
                cpass.set_bind_group(2, &self.screen_dependent.bind_group_thickness_filter[0], &[]);
                cpass.set_push_constants(0, &[1]);
                cpass.dispatch(work_group_filter_1d.width, work_group_filter_1d.height, work_group_filter_1d.depth);
                // Filter X
                cpass.set_bind_group(2, &self.screen_dependent.bind_group_thickness_filter[1], &[]);
                cpass.set_push_constants(0, &[0]);
                cpass.dispatch(work_group_filter_1d.width, work_group_filter_1d.height, work_group_filter_1d.depth);
            }

            const LOCAL_SIZE_COMPOSE: wgpu::Extent3d = wgpu::Extent3d {
                width: 32,
                height: 32,
                depth: 1,
            };
            cpass.set_bind_group(2, &self.screen_dependent.bind_group_compose, &[]);
            cpass.set_pipeline(pipeline_manager.get_compute(&self.screen_independent.pipeline_fluid));
            let work_group = wgpu_utils::compute_group_size(self.screen_dependent.target_textures_resolution, LOCAL_SIZE_COMPOSE);
            cpass.dispatch(work_group.width, work_group.height, work_group.depth);
        }
    }
}
