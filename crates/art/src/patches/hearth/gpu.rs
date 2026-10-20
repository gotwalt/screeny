//! The GPU side of the hearth: the buffers, one compute pipeline per entry
//! point of `hearth.wgsl`, a fixed step and a picture.
//!
//! Everything is in storage buffers rather than textures: wgpu's downlevel
//! limits (the ones [`Gpu`] asks for) allow no read-write storage textures,
//! and a vec4 buffer with hand-written trilinear sampling is the same maths.
//! The downlevel limit that does shape this file is **four storage buffers
//! per entry point**; every pass below stays within it, which is why the
//! advection is five small dispatches rather than one large one.
//!
//! Each [`Sim::step`] is one submit, so the per-step uniforms written before
//! it land where they should. [`Sim::picture`] gathers the light, marches the
//! rays and reads back 2048 LEDs and the ember slots.

use crate::gpu::Gpu;

/// The step's uniform block, laid out as `Params` in the shader.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Params {
    pub dims: [u32; 4],
    pub coarse: [u32; 4],
    pub a: [f32; 4],
    pub b: [f32; 4],
    pub c: [f32; 4],
    pub d: [f32; 4],
    pub eye: [f32; 4],
    pub right: [f32; 4],
    pub up: [f32; 4],
    pub fwd: [f32; 4],
    pub wind: [[f32; 4]; 40],
}

/// Ember slots; the shader reads the count from `coarse.w`.
pub const EMBERS: usize = 16;
/// Pressure sweeps a step (each a red and a black half).
pub const SWEEPS: usize = 32;
const LIGHT_CELL: u32 = 4;

struct Pass {
    pipeline: wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
}

pub struct Sim {
    gpu: &'static Gpu,
    dims: [u32; 3],
    coarse: [u32; 3],
    params: wgpu::Buffer,
    bed: wgpu::Buffer,
    vel: wgpu::Buffer,
    scal: wgpu::Buffer,
    vel_next: wgpu::Buffer,
    scal_next: wgpu::Buffer,
    parts: wgpu::Buffer,
    out: wgpu::Buffer,
    out_read: wgpu::Buffer,
    parts_read: wgpu::Buffer,
    passes: Vec<(&'static str, Pass)>,
}

/// Which bindings each entry point uses: a default pipeline layout holds only
/// those, so its bind group must name exactly them.
const ENTRIES: &[(&str, &[u32])] = &[
    ("adv_fwd", &[0, 1, 2, 3, 4]),
    ("adv_back_vel", &[0, 1, 3, 5]),
    ("adv_back_scal", &[0, 1, 4, 6]),
    ("adv_fix_vel", &[0, 1, 3, 5]),
    ("adv_fix_scal", &[0, 1, 2, 4, 6]),
    ("combust", &[0, 1, 2, 9]),
    ("curl_of", &[0, 1, 5]),
    ("confine", &[0, 1, 5]),
    ("divergence", &[0, 1, 2, 7]),
    ("pres_red", &[0, 7, 8]),
    ("pres_black", &[0, 7, 8]),
    ("gradient", &[0, 1, 8]),
    ("embers", &[0, 1, 2, 12]),
    ("light_gather", &[0, 2, 10, 14]),
    ("light_x", &[0, 10, 11]),
    ("light_y", &[0, 10, 11]),
    ("light_z", &[0, 10, 11]),
    ("march", &[0, 2, 11, 13, 14]),
];

impl Sim {
    pub fn new(gpu: &'static Gpu, dims: [u32; 3], lut: &[[f32; 4]; 512]) -> Sim {
        let device = &gpu.device;
        let cells = u64::from(dims[0] * dims[1] * dims[2]);
        let coarse = dims.map(|d| d / LIGHT_CELL);
        let coarse_cells = u64::from(coarse[0] * coarse[1] * coarse[2]);
        let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;
        let buffer = |label: &str, size: u64, usage| {
            device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false })
        };
        let params = buffer(
            "hearth params",
            std::mem::size_of::<Params>() as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let lut_buf = buffer("hearth lut", 512 * 16, wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST);
        gpu.queue.write_buffer(&lut_buf, 0, bytemuck::cast_slice(lut));
        let vel = buffer("vel", cells * 16, storage);
        let scal = buffer("scal", cells * 16, storage);
        let vel_next = buffer("vel next", cells * 16, storage);
        let scal_next = buffer("scal next", cells * 16, storage);
        let curl = buffer("curl", cells * 16, storage);
        let scal_back = buffer("scal back", cells * 16, storage);
        let div = buffer("div", cells * 4, storage);
        let pres = buffer("pres", cells * 4, storage);
        let bed = buffer("bed", u64::from(dims[0] * dims[2]) * 8, storage);
        let light = buffer("light", coarse_cells * 4, storage);
        let light2 = buffer("light2", coarse_cells * 4, storage);
        let parts = buffer("parts", (EMBERS * 48) as u64, storage);
        let out = buffer("out", 2048 * 16, storage);
        let out_read = buffer("out read", 2048 * 16, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
        let parts_read =
            buffer("parts read", (EMBERS * 48) as u64, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hearth"),
            source: wgpu::ShaderSource::Wgsl(include_str!("hearth.wgsl").into()),
        });
        let bound: [&wgpu::Buffer; 15] =
            [&params, &vel, &scal, &vel_next, &scal_next, &curl, &scal_back, &div, &pres, &bed, &light, &light2, &parts, &out, &lut_buf];
        let passes = ENTRIES
            .iter()
            .map(|(name, bindings)| {
                let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(name),
                    layout: None,
                    module: &module,
                    entry_point: Some(name),
                    compilation_options: Default::default(),
                    cache: None,
                });
                let entries: Vec<wgpu::BindGroupEntry> = bindings
                    .iter()
                    .map(|b| wgpu::BindGroupEntry { binding: *b, resource: bound[*b as usize].as_entire_binding() })
                    .collect();
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(name),
                    layout: &pipeline.get_bind_group_layout(0),
                    entries: &entries,
                });
                (*name, Pass { pipeline, bind })
            })
            .collect();
        Sim { gpu, dims, coarse, params, bed, vel, scal, vel_next, scal_next, parts, out, out_read, parts_read, passes }
    }

    fn pass(&self, name: &str) -> &Pass {
        &self.passes.iter().find(|(n, _)| *n == name).expect("an entry point hearth.wgsl has").1
    }

    fn run(&self, cp: &mut wgpu::ComputePass, name: &str, groups: [u32; 3]) {
        let p = self.pass(name);
        cp.set_pipeline(&p.pipeline);
        cp.set_bind_group(0, &p.bind, &[]);
        cp.dispatch_workgroups(groups[0], groups[1], groups[2]);
    }

    fn cell_groups(&self) -> [u32; 3] {
        self.dims.map(|d| d.div_ceil(4))
    }

    /// Put sparks into empty slots: `(slot, [pos+heat, vel+cooling, prev])`.
    pub fn spawn(&self, slot: usize, spark: [[f32; 4]; 3]) {
        self.gpu.queue.write_buffer(&self.parts, (slot * 48) as u64, bytemuck::cast_slice(&spark));
    }

    /// One fixed step of the fluid and the embers.
    pub fn step(&self, params: &Params, bed: &[[f32; 2]]) {
        let q = &self.gpu.queue;
        q.write_buffer(&self.params, 0, bytemuck::bytes_of(params));
        q.write_buffer(&self.bed, 0, bytemuck::cast_slice(bed));
        let cells = u64::from(self.dims[0] * self.dims[1] * self.dims[2]) * 16;
        let g = self.cell_groups();
        let mut enc = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut cp = enc.begin_compute_pass(&Default::default());
            for name in ["adv_fwd", "adv_back_vel", "adv_back_scal", "adv_fix_vel", "adv_fix_scal"] {
                self.run(&mut cp, name, g);
            }
        }
        enc.copy_buffer_to_buffer(&self.vel_next, 0, &self.vel, 0, cells);
        enc.copy_buffer_to_buffer(&self.scal_next, 0, &self.scal, 0, cells);
        {
            let mut cp = enc.begin_compute_pass(&Default::default());
            for name in ["combust", "curl_of", "confine", "divergence"] {
                self.run(&mut cp, name, g);
            }
            let half = [g[0].div_ceil(2), g[1], g[2]];
            for _ in 0..SWEEPS {
                self.run(&mut cp, "pres_red", half);
                self.run(&mut cp, "pres_black", half);
            }
            self.run(&mut cp, "gradient", g);
            self.run(&mut cp, "embers", [1, 1, 1]);
        }
        q.submit([enc.finish()]);
    }

    /// For tuning: the whole scalar field (temperature, fuel, smoke, source)
    /// read back.
    #[cfg(test)]
    pub fn scalars(&self) -> Vec<[f32; 4]> {
        let size = u64::from(self.dims[0] * self.dims[1] * self.dims[2]) * 16;
        let read = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scal read"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self.gpu.device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(&self.scal, 0, &read, 0, size);
        self.gpu.queue.submit([enc.finish()]);
        let slice = read.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.gpu.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        let out = bytemuck::cast_slice(&slice.get_mapped_range().expect("mapped")).to_vec();
        read.unmap();
        out
    }

    /// The fire's light per LED (rgb) and lit smoke (a), and the ember
    /// slots, read back.
    pub fn picture(&self, params: &Params) -> Option<(Vec<[f32; 4]>, Vec<[f32; 4]>)> {
        let q = &self.gpu.queue;
        q.write_buffer(&self.params, 0, bytemuck::bytes_of(params));
        let cg = self.coarse.map(|d| d.div_ceil(4));
        let mut enc = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut cp = enc.begin_compute_pass(&Default::default());
            for name in ["light_gather", "light_x", "light_y", "light_z"] {
                self.run(&mut cp, name, cg);
            }
            self.run(&mut cp, "march", [8, 4, 1]);
        }
        enc.copy_buffer_to_buffer(&self.out, 0, &self.out_read, 0, 2048 * 16);
        enc.copy_buffer_to_buffer(&self.parts, 0, &self.parts_read, 0, (EMBERS * 48) as u64);
        q.submit([enc.finish()]);

        let (a, b) = (self.out_read.slice(..), self.parts_read.slice(..));
        a.map_async(wgpu::MapMode::Read, |_| {});
        b.map_async(wgpu::MapMode::Read, |_| {});
        if let Err(e) = self.gpu.device.poll(wgpu::PollType::wait_indefinitely()) {
            eprintln!("screeny-art: hearth: gpu poll failed: {e}");
            return None;
        }
        let read = |s: wgpu::BufferSlice| -> Option<Vec<[f32; 4]>> {
            match s.get_mapped_range() {
                Ok(data) => Some(bytemuck::cast_slice(&data).to_vec()),
                Err(e) => {
                    eprintln!("screeny-art: hearth: read-back failed: {e}");
                    None
                }
            }
        };
        let (leds, sparks) = (read(a), read(b));
        self.out_read.unmap();
        self.parts_read.unmap();
        Some((leds?, sparks?))
    }
}
