pub fn compute_group_size(resource_size: wgpu::Extent3d, group_local_size: wgpu::Extent3d) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: (resource_size.width + group_local_size.width - 1) / group_local_size.width,
        height: (resource_size.height + group_local_size.height - 1) / group_local_size.height,
        depth: (resource_size.depth + group_local_size.depth - 1) / group_local_size.depth,
    }
}

pub fn compute_group_size_1d(resource_size: u32, group_local_size: u32) -> u32 {
    (resource_size + group_local_size - 1) / group_local_size
}
