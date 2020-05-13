use crate::hybrid_fluid::HybridFluid;
use crate::shader::ShaderDirectory;
use crate::wgpu_utils::pipelines::*;
use std::{path::Path, rc::Rc};

pub struct VolumeRenderer {
    velocity_render_pipeline: RenderPipelineHandle,
    divergence_render_pipeline_desc: RenderPipelineHandle,
}

impl VolumeRenderer {
    pub fn new(
        device: &wgpu::Device,
        shader_dir: &ShaderDirectory,
        pipeline_manager: &mut PipelineManager,
        per_frame_bind_group_layout: &wgpu::BindGroupLayout,
        fluid_renderer_group_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let layout = Rc::new(device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            bind_group_layouts: &[&per_frame_bind_group_layout, &fluid_renderer_group_layout],
        }));

        let mut velocity_render_pipeline_desc = RenderPipelineCreationDesc::new(
            layout.clone(),
            Path::new("volume_visualization_velocity.vert"),
            Some(Path::new("vertex_color.frag")),
        );
        velocity_render_pipeline_desc.primitive_topology = wgpu::PrimitiveTopology::LineList;

        let divergence_render_pipeline_desc = RenderPipelineCreationDesc::new(
            layout.clone(),
            Path::new("volume_visualization_divergence.vert"),
            Some(Path::new("sphere_particles.frag")),
        );

        VolumeRenderer {
            velocity_render_pipeline: pipeline_manager.create_render_pipeline(device, shader_dir, velocity_render_pipeline_desc),
            divergence_render_pipeline_desc: pipeline_manager.create_render_pipeline(device, shader_dir, divergence_render_pipeline_desc),
        }
    }

    pub fn draw_volume_velocities<'a>(&'a self, rpass: &mut wgpu::RenderPass<'a>, pipeline_manager: &'a PipelineManager, fluid: &'a HybridFluid) {
        rpass.set_pipeline(pipeline_manager.get_render(&self.velocity_render_pipeline));
        rpass.set_bind_group(1, fluid.bind_group_renderer(), &[]);
        let num_grid_cells = fluid.grid_dimension().width * fluid.grid_dimension().height * fluid.grid_dimension().depth;
        rpass.draw(0..2, 0..num_grid_cells);
    }

    pub fn draw_volume_divergence<'a>(&'a self, rpass: &mut wgpu::RenderPass<'a>, pipeline_manager: &'a PipelineManager, fluid: &'a HybridFluid) {
        rpass.set_pipeline(pipeline_manager.get_render(&self.divergence_render_pipeline_desc));
        rpass.set_bind_group(1, fluid.bind_group_renderer(), &[]);
        let num_grid_cells = fluid.grid_dimension().width * fluid.grid_dimension().height * fluid.grid_dimension().depth;
        rpass.draw(0..6, 0..num_grid_cells);
    }
}
