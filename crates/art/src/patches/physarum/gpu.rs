//! The colony on the GPU: agents, two trail buffers in turn, the deposits
//! banked atomically, and a 64 x 32 exposure read back once a frame.

use crate::gpu::Gpu;

/// The uniform `physarum.wgsl` reads. Scalars only, so the layout is the
/// same on every backend.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Params {
    pub w: u32,
    pub h: u32,
    pub n: u32,
    pub seed: u32,
    /// Sensor distance, sensor angle, rotation angle, move distance: each a
    /// base, an amplitude and an exponent on the sensed trail.
    pub laws: [f32; 12],
    pub deposit: f32,
    pub decay: f32,
    pub diffuse: f32,
    pub inv_ref: f32,
    pub cx: f32,
    pub cy: f32,
    pub scale: f32,
    pub first: u32,
    pub wobble: f32,
    pub respawn: f32,
    pub cap: f32,
    pub flow: f32,
    pub time: f32,
    pub pad: [f32; 2],
}

/// A uniform slot: `Params` padded to the copy alignment every backend
/// takes.
const SLOT: u64 = 256;
/// The most substeps one frame may hold.
pub const MAX_SUBSTEPS: usize = 32;

const _: () = assert!(std::mem::size_of::<Params>() as u64 <= SLOT);

struct Pass {
    pipeline: wgpu::ComputePipeline,
    /// One per trail parity: which buffer is read, which is written.
    binds: [wgpu::BindGroup; 2],
}

pub struct Sim {
    gpu: &'static Gpu,
    w: u32,
    h: u32,
    n: u32,
    params: wgpu::Buffer,
    /// Every substep's params for a frame, one slot each, copied into
    /// `params` between passes: a frame is one submission.
    slots: wgpu::Buffer,
    leds: wgpu::Buffer,
    /// The exposure read back, two in turn: a frame returns the one before
    /// it, so the GPU steps this frame while the CPU paints the last.
    leds_read: [wgpu::Buffer; 2],
    pending: Option<(usize, wgpu::SubmissionIndex)>,
    turn: usize,
    step: Pass,
    diffuse: Pass,
    picture: Pass,
    /// Which trail buffer holds the current field.
    parity: usize,
}

impl Sim {
    /// A `w` x `h` world with `agents` scattered over it at random.
    pub fn new(gpu: &'static Gpu, w: u32, h: u32, agents: &[[f32; 4]]) -> Sim {
        let device = &gpu.device;
        use wgpu::BufferUsages as U;
        let buffer =
            |label, size: u64, usage| device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false });
        let cells = u64::from(w * h) * 4;
        let params = buffer("physarum params", SLOT, U::UNIFORM | U::COPY_DST);
        let agent_buf = buffer("physarum agents", agents.len() as u64 * 16, U::STORAGE | U::COPY_DST);
        gpu.queue.write_buffer(&agent_buf, 0, bytemuck::cast_slice(agents));
        let trails = [buffer("physarum trail a", cells, U::STORAGE), buffer("physarum trail b", cells, U::STORAGE)];
        let dep = buffer("physarum deposits", cells, U::STORAGE);
        let leds = buffer("physarum leds", 2048 * 4, U::STORAGE | U::COPY_SRC);
        let leds_read = [0, 1].map(|_| buffer("physarum leds read", 2048 * 4, U::MAP_READ | U::COPY_DST));
        let slots = buffer("physarum slots", SLOT * MAX_SUBSTEPS as u64, U::COPY_SRC | U::COPY_DST);

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("physarum"),
            source: wgpu::ShaderSource::Wgsl(include_str!("physarum.wgsl").into()),
        });
        // Binding numbers as in the shader: 0 params, 1 agents, 2 trail
        // (read), 3 deposits, 4 trail (written), 5 leds.
        let pass = |name: &str, bindings: &[u32]| {
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(name),
                layout: None,
                module: &module,
                entry_point: Some(name),
                compilation_options: Default::default(),
                cache: None,
            });
            let bind = |parity: usize| {
                let bound = [&params, &agent_buf, &trails[parity], &dep, &trails[1 - parity], &leds];
                let entries: Vec<wgpu::BindGroupEntry> =
                    bindings.iter().map(|b| wgpu::BindGroupEntry { binding: *b, resource: bound[*b as usize].as_entire_binding() }).collect();
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(name),
                    layout: &pipeline.get_bind_group_layout(0),
                    entries: &entries,
                })
            };
            let binds = [bind(0), bind(1)];
            Pass { pipeline, binds }
        };
        let step = pass("step", &[0, 1, 2, 3]);
        let diffuse = pass("diffuse", &[0, 2, 3, 4]);
        let picture = pass("picture", &[0, 2, 5]);
        Sim { gpu, w, h, n: agents.len() as u32, params, slots, leds, leds_read, pending: None, turn: 0, step, diffuse, picture, parity: 0 }
    }

    pub fn agents(&self) -> u32 {
        self.n
    }

    /// A frame's steps of the colony, one per `params`, the camera exposed
    /// after each. Returns the exposure of the frame *before* this one (the
    /// first frame waits for its own): one value per LED, summed over its
    /// substeps, relative to the world's mean trail.
    pub fn frame(&mut self, params: &[Params]) -> Option<Vec<f32>> {
        let params = &params[..params.len().min(MAX_SUBSTEPS)];
        let q = &self.gpu.queue;
        let mut bytes = vec![0u8; SLOT as usize * params.len()];
        for (k, p) in params.iter().enumerate() {
            let p = Params { first: u32::from(k == 0), ..*p };
            bytes[k * SLOT as usize..][..std::mem::size_of::<Params>()].copy_from_slice(bytemuck::bytes_of(&p));
        }
        q.write_buffer(&self.slots, 0, &bytes);
        let mut enc = self.gpu.device.create_command_encoder(&Default::default());
        for k in 0..params.len() {
            enc.copy_buffer_to_buffer(&self.slots, k as u64 * SLOT, &self.params, 0, SLOT);
            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("physarum"), timestamp_writes: None });
            let mut run = |pass: &Pass, parity: usize, groups: [u32; 3]| {
                cp.set_pipeline(&pass.pipeline);
                cp.set_bind_group(0, &pass.binds[parity], &[]);
                cp.dispatch_workgroups(groups[0], groups[1], groups[2]);
            };
            run(&self.step, self.parity, [self.n.div_ceil(256), 1, 1]);
            run(&self.diffuse, self.parity, [self.w.div_ceil(16), self.h.div_ceil(16), 1]);
            run(&self.picture, 1 - self.parity, [8, 4, 1]);
            drop(cp);
            self.parity = 1 - self.parity;
        }
        let turn = self.turn;
        self.turn = 1 - turn;
        enc.copy_buffer_to_buffer(&self.leds, 0, &self.leds_read[turn], 0, 2048 * 4);
        let index = q.submit([enc.finish()]);
        let (read, index) = self.pending.replace((turn, index.clone())).unwrap_or((turn, index));
        self.read(read, index)
    }

    fn read(&self, which: usize, index: wgpu::SubmissionIndex) -> Option<Vec<f32>> {
        let buf = &self.leds_read[which];
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        if let Err(e) = self.gpu.device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: None }) {
            eprintln!("screeny-art: physarum: gpu poll failed: {e}");
            return None;
        }
        let out = match slice.get_mapped_range() {
            Ok(data) => Some(bytemuck::cast_slice(&data).to_vec()),
            Err(e) => {
                eprintln!("screeny-art: physarum: read-back failed: {e}");
                None
            }
        };
        buf.unmap();
        out
    }
}
