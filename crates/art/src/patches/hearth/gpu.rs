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
    /// The firebox (card 373): draft (0 = open fire), ceiling row, flue
    /// radius in cells, unused.
    pub stove: [f32; 4],
    pub wind: [[f32; 4]; 40],
}

/// Ember slots; the shader reads the count from `coarse.w`.
pub const EMBERS: usize = 16;
const LIGHT_CELL: u32 = 4;

/// What [`Sim::picture`] reads back: (fire light rgb, lit smoke) per LED,
/// then 2048 more holding the smokeless fire's luminance, and the ember
/// slots.
pub type Picture = (Vec<[f32; 4]>, Vec<[f32; 4]>);

struct Pass {
    pipeline: wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
}

/// Card 374's per-stage GPU clock: a timestamp at the start and end of
/// every stage's compute pass, read back after each submit. Only for the
/// `#[ignore]` cost tool, on a device opened with `TIMESTAMP_QUERY`.
pub struct Profile {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    names: Vec<&'static str>,
    /// Nanoseconds per stage, every submit since the last [`Profile::take`].
    pub times: Vec<(&'static str, f64)>,
}

#[cfg(test)]
const PROFILE_SLOTS: u32 = 64;

#[cfg(test)]
impl Profile {
    pub fn new(device: &wgpu::Device) -> Profile {
        let set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("hearth stages"),
            ty: wgpu::QueryType::Timestamp,
            count: PROFILE_SLOTS,
        });
        let size = u64::from(PROFILE_SLOTS) * 8;
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hearth stages resolve"),
            size,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hearth stages read"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Profile { set, resolve, read, names: Vec::new(), times: Vec::new() }
    }
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
    /// Card 374's cost tool, when it is measuring.
    pub profile: Option<std::cell::RefCell<Profile>>,
    /// Pressure sweeps per step.
    sweeps: usize,
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
    pub fn new(gpu: &'static Gpu, dims: [u32; 3], sweeps: usize, lut: &[[f32; 4]; 512]) -> Sim {
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
        let parts = buffer("parts", (EMBERS * 64) as u64, storage);
        let out = buffer("out", 2 * 2048 * 16, storage);
        let out_read = buffer("out read", 2 * 2048 * 16, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
        let parts_read =
            buffer("parts read", (EMBERS * 64) as u64, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);

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
        Sim { gpu, dims, coarse, params, bed, vel, scal, vel_next, scal_next, parts, out, out_read, parts_read, passes, profile: None, sweeps }
    }

    fn pass(&self, name: &str) -> &Pass {
        &self.passes.iter().find(|(n, _)| *n == name).expect("an entry point hearth.wgsl has").1
    }

    /// One stage of the work in its own compute pass, timed when profiling.
    fn stage(&self, enc: &mut wgpu::CommandEncoder, label: &'static str, body: impl FnOnce(&mut wgpu::ComputePass)) {
        let mut prof = self.profile.as_ref().map(|p| p.borrow_mut());
        let writes = prof.as_mut().map(|p| {
            let at = 2 * p.names.len() as u32;
            p.names.push(label);
            wgpu::ComputePassTimestampWrites {
                query_set: &p.set,
                beginning_of_pass_write_index: Some(at),
                end_of_pass_write_index: Some(at + 1),
            }
        });
        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(label), timestamp_writes: writes });
        body(&mut cp);
    }

    /// Submit, and when profiling, wait and file each stage's time.
    fn submit(&self, mut enc: wgpu::CommandEncoder) {
        let Some(p) = &self.profile else {
            self.gpu.queue.submit([enc.finish()]);
            return;
        };
        let mut p = p.borrow_mut();
        let n = 2 * p.names.len() as u32;
        if n > 0 {
            enc.resolve_query_set(&p.set, 0..n, &p.resolve, 0);
            enc.copy_buffer_to_buffer(&p.resolve, 0, &p.read, 0, u64::from(n) * 8);
        }
        self.gpu.queue.submit([enc.finish()]);
        if n == 0 {
            return;
        }
        let slice = p.read.slice(..u64::from(n) * 8);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.gpu.device.poll(wgpu::PollType::wait_indefinitely());
        let ticks: Vec<u64> = bytemuck::cast_slice(&slice.get_mapped_range().expect("mapped")).to_vec();
        p.read.unmap();
        let period = f64::from(self.gpu.queue.get_timestamp_period());
        let names = std::mem::take(&mut p.names);
        for (k, name) in names.into_iter().enumerate() {
            let ns = ticks[2 * k + 1].saturating_sub(ticks[2 * k]) as f64 * period;
            p.times.push((name, ns));
        }
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

    /// Put sparks into empty slots: `(slot, [pos+heat, vel+cooling, prev+age, bounces])`.
    pub fn spawn(&self, slot: usize, spark: [[f32; 4]; 4]) {
        self.gpu.queue.write_buffer(&self.parts, (slot * 64) as u64, bytemuck::cast_slice(&spark));
    }

    /// One fixed step of the fluid and the embers.
    pub fn step(&self, params: &Params, bed: &[[f32; 2]]) {
        let q = &self.gpu.queue;
        q.write_buffer(&self.params, 0, bytemuck::bytes_of(params));
        q.write_buffer(&self.bed, 0, bytemuck::cast_slice(bed));
        let cells = u64::from(self.dims[0] * self.dims[1] * self.dims[2]) * 16;
        let g = self.cell_groups();
        let mut enc = self.gpu.device.create_command_encoder(&Default::default());
        self.stage(&mut enc, "advect", |cp| {
            for name in ["adv_fwd", "adv_back_vel", "adv_back_scal", "adv_fix_vel", "adv_fix_scal"] {
                self.run(cp, name, g);
            }
        });
        enc.copy_buffer_to_buffer(&self.vel_next, 0, &self.vel, 0, cells);
        enc.copy_buffer_to_buffer(&self.scal_next, 0, &self.scal, 0, cells);
        self.stage(&mut enc, "combust", |cp| self.run(cp, "combust", g));
        self.stage(&mut enc, "vorticity", |cp| {
            self.run(cp, "curl_of", g);
            self.run(cp, "confine", g);
        });
        self.stage(&mut enc, "divergence", |cp| self.run(cp, "divergence", g));
        let half = [g[0].div_ceil(2), g[1], g[2]];
        self.stage(&mut enc, "pressure", |cp| {
            for _ in 0..self.sweeps {
                self.run(cp, "pres_red", half);
                self.run(cp, "pres_black", half);
            }
        });
        self.stage(&mut enc, "project", |cp| self.run(cp, "gradient", g));
        self.stage(&mut enc, "embers", |cp| self.run(cp, "embers", [1, 1, 1]));
        self.submit(enc);
    }

    /// For tuning and tests: the whole scalar field (temperature, fuel,
    /// smoke, source) read back.
    #[cfg(test)]
    pub fn scalars(&self) -> Vec<[f32; 4]> {
        self.read_field(&self.scal)
    }

    /// For tests: the whole velocity field read back.
    #[cfg(test)]
    pub fn velocities(&self) -> Vec<[f32; 4]> {
        self.read_field(&self.vel)
    }

    #[cfg(test)]
    fn read_field(&self, field: &wgpu::Buffer) -> Vec<[f32; 4]> {
        let size = u64::from(self.dims[0] * self.dims[1] * self.dims[2]) * 16;
        let read = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scal read"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self.gpu.device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(field, 0, &read, 0, size);
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
    pub fn picture(&self, params: &Params) -> Option<Picture> {
        let q = &self.gpu.queue;
        q.write_buffer(&self.params, 0, bytemuck::bytes_of(params));
        let cg = self.coarse.map(|d| d.div_ceil(4));
        let mut enc = self.gpu.device.create_command_encoder(&Default::default());
        self.stage(&mut enc, "light", |cp| {
            for name in ["light_gather", "light_x", "light_y", "light_z"] {
                self.run(cp, name, cg);
            }
        });
        self.stage(&mut enc, "march", |cp| self.run(cp, "march", [8, 4, 1]));
        enc.copy_buffer_to_buffer(&self.out, 0, &self.out_read, 0, 2 * 2048 * 16);
        enc.copy_buffer_to_buffer(&self.parts, 0, &self.parts_read, 0, (EMBERS * 64) as u64);
        self.submit(enc);

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
