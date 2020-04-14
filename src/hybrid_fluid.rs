use crate::wgpu_utils::binding_builder::*;
use crate::wgpu_utils::pipelines::*;
use crate::wgpu_utils::shader::*;
use crate::wgpu_utils::uniformbuffer::*;
use crate::wgpu_utils::*;
use rand::prelude::*;
use std::{path::Path, rc::Rc};
use uniformbuffer::PaddedVector3;

#[repr(C)]
#[derive(Clone, Copy)]
struct SimulationPropertiesUniformBufferContent {
    num_particles: u32,
    padding0: f32,
    padding1: f32,
    padding2: f32,
}

pub struct HybridFluid {
    //gravity: cgmath::Vector3<f32>, // global gravity force in m/s² (== N/kg)
    grid_dimension: wgpu::Extent3d,

    particles: wgpu::Buffer,
    simulation_properties_uniformbuffer: UniformBuffer<SimulationPropertiesUniformBufferContent>,

    bind_group_uniform: wgpu::BindGroup,
    bind_group_update_particles_and_grid: wgpu::BindGroup,
    bind_group_update_particles: wgpu::BindGroup,
    bind_group_compute_divergence: wgpu::BindGroup,
    bind_group_pressure_write_0: wgpu::BindGroup,
    bind_group_pressure_write_1: wgpu::BindGroup,

    pipeline_clear_grids: ReloadableComputePipeline,
    pipeline_build_llgrid: ReloadableComputePipeline,
    pipeline_build_vgrid: ReloadableComputePipeline,
    pipeline_compute_divergence: ReloadableComputePipeline,
    pipeline_particle_update: ReloadableComputePipeline,

    num_particles: u32,
    max_num_particles: u32,
}

// todo: probably want to split this up into several buffers
#[repr(C)]
#[derive(Clone, Copy)]
struct Particle {
    // Particle positions are in grid space to simplify shader computation
    // (no scaling/translation needed until we're rendering or interacting with other objects!)
    position: cgmath::Point3<f32>,
    linked_list_next: u32,
    velocity: PaddedVector3,
}

impl HybridFluid {
    // particles are distributed 2x2x2 within a single gridcell
    // (seems to be widely accepted as the default)
    const PARTICLES_PER_GRID_CELL: u32 = 8;

    pub fn new(
        device: &wgpu::Device,
        grid_dimension: wgpu::Extent3d,
        max_num_particles: u32,
        shader_dir: &ShaderDirectory,
        per_frame_bind_group_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        // Layouts
        let group_layout_uniform = BindGroupLayoutBuilder::new()
            .next_binding_compute(binding_glsl::uniform())
            .create(device, "BindGroupLayout: HybridFluid Uniform");
        let group_layout_update_particles_and_grid = BindGroupLayoutBuilder::new()
            .next_binding_compute(binding_glsl::buffer(false)) // particles
            .next_binding_compute(binding_glsl::image3d(wgpu::TextureFormat::Rgba32Float, false)) // vgrid
            .next_binding_compute(binding_glsl::uimage3d(wgpu::TextureFormat::R32Uint, false)) // llgrid
            .create(device, "BindGroupLayout: Update Particles and/or Velocity Grid");
        let group_layout_update_particles = BindGroupLayoutBuilder::new()
            .next_binding_compute(binding_glsl::buffer(false)) // particles
            .next_binding_compute(binding_glsl::texture3D()) // vgrid
            .create(device, "BindGroupLayout: Update Particles and/or Velocity Grid");
        let group_layout_pressure_solve = BindGroupLayoutBuilder::new()
            .next_binding_compute(binding_glsl::texture3D()) // vgrid
            .next_binding_compute(binding_glsl::texture3D()) // dummy or divergence
            .next_binding_compute(binding_glsl::texture3D()) // pressure
            .next_binding_compute(binding_glsl::image3d(wgpu::TextureFormat::R32Float, false)) // pressure or divergence
            .create(device, "BindGroupLayout: Pressure solve volumes");

        // Resources
        let simulation_properties_uniformbuffer = UniformBuffer::new(device);
        let particle_buffer_size = max_num_particles as u64 * std::mem::size_of::<Particle>() as u64;
        let particles = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Buffer: ParticleBuffer"),
            size: particle_buffer_size,
            usage: wgpu::BufferUsage::STORAGE | wgpu::BufferUsage::STORAGE_READ | wgpu::BufferUsage::COPY_DST,
        });
        let create_volume_texture_descriptor = |label: &'static str, format: wgpu::TextureFormat| -> wgpu::TextureDescriptor {
            wgpu::TextureDescriptor {
                label: Some(label),
                size: grid_dimension,
                array_layer_count: 1,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D3,
                format,
                usage: wgpu::TextureUsage::SAMPLED | wgpu::TextureUsage::STORAGE,
            }
        };
        let volume_velocity = device.create_texture(&create_volume_texture_descriptor("Velocity Volume", wgpu::TextureFormat::Rgba32Float));
        let volume_linked_lists = device.create_texture(&create_volume_texture_descriptor("Linked Lists Volume", wgpu::TextureFormat::R32Uint));
        let volume_divergence = device.create_texture(&create_volume_texture_descriptor("Velocity Volume", wgpu::TextureFormat::R32Float)); // TODO: could reuse data from volume_linked_lists
        let volume_pressure0 = device.create_texture(&create_volume_texture_descriptor("Pressure Volume 0", wgpu::TextureFormat::R32Float));
        let volume_pressure1 = device.create_texture(&create_volume_texture_descriptor("Pressure Volume 1", wgpu::TextureFormat::R32Float));

        // Resource views
        let volume_velocity_view = volume_velocity.create_default_view();
        let volume_linked_lists_view = volume_linked_lists.create_default_view();
        let volume_divergence_view = volume_divergence.create_default_view();
        let volume_pressure0_view = volume_pressure0.create_default_view();
        let volume_pressure1_view = volume_pressure1.create_default_view();

        // Bind groups.
        let bind_group_uniform = BindGroupBuilder::new(&group_layout_uniform)
            .resource(simulation_properties_uniformbuffer.binding_resource())
            .create(device, "BindGroup: HybridFluid Uniform");
        let bind_group_update_particles_and_grid = BindGroupBuilder::new(&group_layout_update_particles_and_grid)
            .buffer(&particles, 0..particle_buffer_size)
            .texture(&volume_velocity_view)
            .texture(&volume_linked_lists_view)
            .create(device, "BindGroup: Update Particles and/or Velocity Grid");
        let bind_group_update_particles = BindGroupBuilder::new(&group_layout_update_particles)
            .buffer(&particles, 0..particle_buffer_size)
            .texture(&volume_velocity_view)
            .create(device, "BindGroup: Update Particles");
        let bind_group_compute_divergence = BindGroupBuilder::new(&group_layout_pressure_solve)
            .texture(&volume_velocity_view)
            .texture(&volume_pressure0_view)
            .texture(&volume_pressure1_view)
            .texture(&volume_divergence_view)
            .create(device, "BindGroup: Compute Divergence");
        let bind_group_pressure_write_0 = BindGroupBuilder::new(&group_layout_pressure_solve)
            .texture(&volume_velocity_view)
            .texture(&volume_divergence_view)
            .texture(&volume_pressure1_view)
            .texture(&volume_pressure0_view)
            .create(device, "BindGroup: Pressure write 0");
        let bind_group_pressure_write_1 = BindGroupBuilder::new(&group_layout_pressure_solve)
            .texture(&volume_velocity_view)
            .texture(&volume_divergence_view)
            .texture(&volume_pressure1_view)
            .texture(&volume_pressure0_view)
            .create(device, "BindGroup: Pressure write 1");

        // pipeline layouts.
        // Note that layouts directly correspond to DX12 root signatures.
        // We want to avoid having many of them and share as much as we can, but since WebGPU needs to set barriers for everything that is not readonly it's a tricky tradeoff.
        // Considering that all pipelines here require UAV barriers anyways a few more or less won't make too much difference (... is that true?).
        // Therefore we're compromising for less layouts & easier to maintain code (also less binding changes 🤔)
        // TODO: This setup is super coarse now. Need to figure out actual impact and see if splitting down makes sense.
        let layout_update_particles_and_grid = Rc::new(device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            bind_group_layouts: &[
                per_frame_bind_group_layout,
                &group_layout_uniform.layout,
                &group_layout_update_particles_and_grid.layout,
            ],
        }));
        let layout_pressure_solve = Rc::new(device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            bind_group_layouts: &[
                per_frame_bind_group_layout,
                &group_layout_uniform.layout,
                &group_layout_pressure_solve.layout,
            ],
        }));
        let layout_update_particles = Rc::new(device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            bind_group_layouts: &[
                per_frame_bind_group_layout,
                &group_layout_uniform.layout,
                &group_layout_update_particles.layout,
            ],
        }));

        let pipeline_clear_grids =
            ReloadableComputePipeline::new(device, &layout_update_particles_and_grid, shader_dir, Path::new("clear_grids.comp"));
        let pipeline_build_llgrid =
            ReloadableComputePipeline::new(device, &layout_update_particles_and_grid, shader_dir, Path::new("build_llgrid.comp"));
        let pipeline_build_vgrid =
            ReloadableComputePipeline::new(device, &layout_update_particles_and_grid, shader_dir, Path::new("build_vgrid.comp"));
        let pipeline_compute_divergence =
            ReloadableComputePipeline::new(device, &layout_pressure_solve, shader_dir, Path::new("compute_divergence.comp"));
        let pipeline_particle_update =
            ReloadableComputePipeline::new(device, &layout_update_particles, shader_dir, Path::new("particle_update.comp"));

        HybridFluid {
            //gravity: cgmath::Vector3::new(0.0, -9.81, 0.0), // there needs to be some grid->world relation
            grid_dimension,

            particles,
            simulation_properties_uniformbuffer,

            bind_group_uniform,
            bind_group_update_particles_and_grid,
            bind_group_update_particles,
            bind_group_compute_divergence,
            bind_group_pressure_write_0,
            bind_group_pressure_write_1,

            pipeline_clear_grids,
            pipeline_build_llgrid,
            pipeline_build_vgrid,
            pipeline_compute_divergence,
            pipeline_particle_update,

            num_particles: 0,
            max_num_particles,
        }
    }

    fn clamp_to_grid(&self, grid_cor: cgmath::Point3<f32>) -> cgmath::Point3<u32> {
        cgmath::Point3::new(
            self.grid_dimension.width.min(grid_cor.x as u32),
            self.grid_dimension.height.min(grid_cor.y as u32),
            self.grid_dimension.depth.min(grid_cor.z as u32),
        )
    }

    pub fn try_reload_shaders(&mut self, device: &wgpu::Device, shader_dir: &ShaderDirectory) {
        let _ = self.pipeline_clear_grids.try_reload_shader(device, shader_dir);
        let _ = self.pipeline_build_llgrid.try_reload_shader(device, shader_dir);
        let _ = self.pipeline_build_vgrid.try_reload_shader(device, shader_dir);
        let _ = self.pipeline_compute_divergence.try_reload_shader(device, shader_dir);
        let _ = self.pipeline_particle_update.try_reload_shader(device, shader_dir);
    }

    // Clears all state (i.e. removes fluid particles)
    pub fn reset(&mut self) {
        self.num_particles = 0;
    }

    // Adds a cube of fluid. Coordinates are in grid space! Very slow operation!
    pub fn add_fluid_cube(
        &mut self,
        device: &wgpu::Device,
        init_encoder: &mut wgpu::CommandEncoder,
        min_grid: cgmath::Point3<f32>,
        max_grid: cgmath::Point3<f32>,
    ) {
        // align to whole cells for simplicity.
        let min_grid = self.clamp_to_grid(min_grid);
        let max_grid = self.clamp_to_grid(max_grid);
        let extent_cell = max_grid - min_grid;

        let num_new_particles = self
            .max_num_particles
            .min(((max_grid.x - min_grid.x) * (max_grid.y - min_grid.y) * (max_grid.z - min_grid.z) * Self::PARTICLES_PER_GRID_CELL) as u32);

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let particle_buffer_mapping = device.create_buffer_mapped(&wgpu::BufferDescriptor {
            label: Some("Buffer: Particle Update"),
            size: num_new_particles as u64 * particle_size,
            usage: wgpu::BufferUsage::COPY_SRC,
        });

        // Fill buffer with particle data
        let mut rng: rand::rngs::SmallRng = rand::SeedableRng::seed_from_u64(num_new_particles as u64);
        let new_particles =
            unsafe { std::slice::from_raw_parts_mut(particle_buffer_mapping.data.as_mut_ptr() as *mut Particle, num_new_particles as usize) };
        for (i, particle) in new_particles.iter_mut().enumerate() {
            //let sample_idx = i as u32 % Self::PARTICLES_PER_GRID_CELL;
            let cell = cgmath::Point3::new(
                (min_grid.x + i as u32 / Self::PARTICLES_PER_GRID_CELL % extent_cell.x) as f32,
                (min_grid.y + i as u32 / Self::PARTICLES_PER_GRID_CELL / extent_cell.x % extent_cell.y) as f32,
                (min_grid.z + i as u32 / Self::PARTICLES_PER_GRID_CELL / extent_cell.x / extent_cell.y) as f32,
            );
            let position = cell + rng.gen::<cgmath::Vector3<f32>>();
            *particle = Particle {
                position,
                linked_list_next: 0xFFFFFFFF,
                velocity: cgmath::vec3(0.0, 0.0, 0.0).into(),
            };
        }

        init_encoder.copy_buffer_to_buffer(
            &particle_buffer_mapping.finish(),
            0,
            &self.particles,
            self.num_particles as u64 * particle_size,
            num_new_particles as u64 * particle_size,
        );
        self.num_particles += num_new_particles;

        self.update_simulation_properties_uniformbuffer(device, init_encoder);
    }

    fn update_simulation_properties_uniformbuffer(&mut self, device: &wgpu::Device, init_encoder: &mut wgpu::CommandEncoder) {
        self.simulation_properties_uniformbuffer.update_content(
            init_encoder,
            device,
            SimulationPropertiesUniformBufferContent {
                num_particles: self.num_particles,
                padding0: 0.0,
                padding1: 0.0,
                padding2: 0.0,
            },
        );
    }

    pub fn num_particles(&self) -> u32 {
        self.num_particles
    }

    pub fn particle_binding_resource(&self) -> wgpu::BindingResource {
        wgpu::BindingResource::Buffer {
            buffer: &self.particles,
            range: 0..self.particle_buffer_size(),
        }
    }

    pub fn particle_buffer_size(&self) -> u64 {
        self.max_num_particles as u64 * std::mem::size_of::<Particle>() as u64
    }

    // todo: timing
    pub fn step<'a>(&'a self, cpass: &mut wgpu::ComputePass<'a>) {
        const COMPUTE_LOCAL_SIZE_FLUID: wgpu::Extent3d = wgpu::Extent3d {
            width: 8,
            height: 8,
            depth: 8,
        };
        const COMPUTE_LOCAL_SIZE_PARTICLES: u32 = 512;

        let grid_work_groups = wgpu::Extent3d {
            width: self.grid_dimension.width / COMPUTE_LOCAL_SIZE_FLUID.width,
            height: self.grid_dimension.height / COMPUTE_LOCAL_SIZE_FLUID.height,
            depth: self.grid_dimension.depth / COMPUTE_LOCAL_SIZE_FLUID.depth,
        };
        let particle_work_groups = (self.num_particles as u32 + COMPUTE_LOCAL_SIZE_PARTICLES - 1) / COMPUTE_LOCAL_SIZE_PARTICLES;

        cpass.set_bind_group(1, &self.bind_group_uniform, &[]);

        // grouped by layouts.
        {
            cpass.set_bind_group(2, &self.bind_group_update_particles_and_grid, &[]);

            // clear front velocity and linkedlist grid
            // It's either this or a loop over encoder.begin_render_pass which then also requires a myriad of texture views...
            // (might still be faster because RT clear operations are usually very quick :/)
            cpass.set_pipeline(self.pipeline_clear_grids.pipeline());
            cpass.dispatch(grid_work_groups.width, grid_work_groups.height, grid_work_groups.depth);

            // Create particle linked lists and write heads in dual grids
            // Transfer velocities to grid. (write grid, read particles)
            cpass.set_pipeline(self.pipeline_build_llgrid.pipeline());
            cpass.dispatch(particle_work_groups, 1, 1);

            // Gather velocities in velocity grid and apply global forces.
            cpass.set_pipeline(self.pipeline_build_vgrid.pipeline());
            cpass.dispatch(grid_work_groups.width, grid_work_groups.height, grid_work_groups.depth);
        }
        {
            // Compute divergence
            cpass.set_pipeline(self.pipeline_compute_divergence.pipeline());
            cpass.set_bind_group(2, &self.bind_group_compute_divergence, &[]);
            cpass.dispatch(grid_work_groups.width, grid_work_groups.height, grid_work_groups.depth);

            // Clear pressure grid.
            // (optional, todo) Precondition pressure
            cpass.set_bind_group(2, &self.bind_group_pressure_write_0, &[]);

            // Pressure solve
            cpass.set_bind_group(2, &self.bind_group_pressure_write_1, &[]);
        }
        {
            cpass.set_bind_group(2, &self.bind_group_update_particles, &[]);

            // Make velocity grid divergence free
            // TODO

            // Transfer velocities to particles.
            cpass.set_pipeline(self.pipeline_particle_update.pipeline());
            cpass.dispatch(particle_work_groups, 1, 1);

            // Advect particles.  (write particles)
        }
    }
}
