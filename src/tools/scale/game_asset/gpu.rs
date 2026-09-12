//! Dedicated wavelet compute, not image resampling. Spectra and spatial working
//! buffers stay on Vulkan; only cropped channel energies return to the CPU.
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use wgpu::util::DeviceExt;

use super::Options;
use crate::document::CancellationToken;

const MAX_FFT: usize = 2048;
const WAIT_LIMIT: Duration = Duration::from_secs(10);
type GpuResult<T> = std::result::Result<T, String>;

pub(super) struct GpuWavelets {
    context: Arc<Context>,
    dimensions: Dimensions,
    spectrum: wgpu::Buffer,
    a: wgpu::Buffer,
    b: wgpu::Buffer,
    energy: wgpu::Buffer,
    staging: wgpu::Buffer,
    scales: usize,
}

#[derive(Clone, Copy)]
pub(super) struct Dimensions {
    pub sw: usize,
    pub sh: usize,
    pub pad: usize,
}

impl Dimensions {
    fn padded(self) -> (usize, usize) {
        (
            (self.sw + 2 * self.pad).next_power_of_two(),
            (self.sh + 2 * self.pad).next_power_of_two(),
        )
    }
}

struct Context {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    fft: wgpu::ComputePipeline,
    filter: wgpu::ComputePipeline,
    energy: wgpu::ComputePipeline,
    twiddles: wgpu::Buffer,
    failed: Arc<AtomicBool>,
}

impl Context {
    fn shared() -> GpuResult<Arc<Self>> {
        static CONTEXT: OnceLock<GpuResult<Arc<Context>>> = OnceLock::new();
        let context = CONTEXT.get_or_init(|| Self::new().map(Arc::new)).clone()?;
        if context.failed.load(Ordering::Relaxed) {
            return Err("wavelet GPU device previously failed".into());
        }
        Ok(context)
    }

    fn new() -> GpuResult<Self> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .map_err(|e| e.to_string())?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            return Err("no hardware Vulkan adapter".into());
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("Diorama Game Asset wavelets"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|e| e.to_string())?;
        let failed = Arc::new(AtomicBool::new(false));
        let failure = failed.clone();
        device.on_uncaptured_error(Arc::new(move |e| {
            failure.store(true, Ordering::Relaxed);
            tracing::warn!(%e, "Game Asset GPU device failed");
        }));
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let entries: Vec<_> = (0..4)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: if binding == 2 {
                        wgpu::BufferBindingType::Uniform
                    } else {
                        wgpu::BufferBindingType::Storage {
                            read_only: binding != 1,
                        }
                    },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            })
            .collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Game Asset wavelet layout"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Game Asset wavelet pipelines"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Game Asset FFT/filter/energy"),
            source: wgpu::ShaderSource::Wgsl(include_str!("wavelet.wgsl").into()),
        });
        let pipeline = |entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let fft = pipeline("fft");
        let filter = pipeline("apply_filter");
        let energy = pipeline("energy");
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(error.to_string());
        }
        let twiddles: Vec<u8> = (0..MAX_FFT / 2)
            .flat_map(|i| {
                let angle = -2.0 * std::f64::consts::PI * i as f64 / MAX_FFT as f64;
                [angle.cos() as f32, angle.sin() as f32]
                    .into_iter()
                    .flat_map(f32::to_le_bytes)
            })
            .collect();
        let twiddles = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Game Asset FFT twiddles"),
            contents: &twiddles,
            usage: wgpu::BufferUsages::STORAGE,
        });
        tracing::info!(adapter = %info.name, "Game Asset GPU wavelets enabled");
        Ok(Self {
            device,
            queue,
            layout,
            fft,
            filter,
            energy,
            twiddles,
            failed,
        })
    }

    fn dispatch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::ComputePipeline,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        params: &[u32; 16],
        groups: (u32, u32),
    ) {
        let bytes: Vec<_> = params.iter().flat_map(|p| p.to_le_bytes()).collect();
        let uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Game Asset wavelet parameters"),
                contents: &bytes,
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let buffers = [input, output, &uniform, &self.twiddles];
        let entries: Vec<_> = buffers
            .iter()
            .enumerate()
            .map(|(i, buffer)| wgpu::BindGroupEntry {
                binding: i as u32,
                resource: buffer.as_entire_binding(),
            })
            .collect();
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Game Asset wavelet bindings"),
            layout: &self.layout,
            entries: &entries,
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Game Asset wavelet compute"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.dispatch_workgroups(groups.0, groups.1, 1);
    }
}

impl GpuWavelets {
    pub fn new(
        lab: &[[f32; 3]],
        dimensions: Dimensions,
        scales: usize,
        memory_budget: u64,
        cancellation: &CancellationToken,
    ) -> GpuResult<Self> {
        cancellation.check().map_err(|e| e.to_string())?;
        if dimensions.sw == 0
            || dimensions.sh == 0
            || dimensions.sw > MAX_FFT
            || dimensions.sh > MAX_FFT
            || dimensions.pad > MAX_FFT
            || lab.len() != dimensions.sw * dimensions.sh
        {
            return Err("invalid GPU wavelet source dimensions".into());
        }
        let (width, height) = dimensions.padded();
        if width > MAX_FFT || height > MAX_FFT || scales == 0 {
            return Err("FFT dimensions are outside the GPU plan".into());
        }
        let complex_bytes = (width * height * 3 * 8) as u64;
        let energy_bytes = ((dimensions.sw * dimensions.sh * 8) as u64)
            .checked_mul(scales as u64)
            .ok_or("too many wavelet scales")?;
        // Include readback and the temporary host upload, not only device buffers.
        let working = energy_bytes
            .checked_mul(3)
            .and_then(|e| e.checked_add(complex_bytes * 4 + (MAX_FFT * 4) as u64))
            .ok_or("wavelet working storage overflow")?;
        if working > memory_budget {
            return Err("GPU wavelets exceed the remaining memory budget".into());
        }
        let context = Context::shared()?;
        let limits = context.device.limits();
        if complex_bytes.max(energy_bytes) > limits.max_storage_buffer_binding_size
            || complex_bytes.max(energy_bytes) > limits.max_buffer_size
        {
            return Err("wavelet buffers exceed GPU limits".into());
        }
        let buffer = |label, size, usage| {
            context.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let storage = wgpu::BufferUsages::STORAGE;
        let spectrum = buffer("Game Asset source spectra", complex_bytes, storage);
        let a = buffer(
            "Game Asset FFT work A",
            complex_bytes,
            storage | wgpu::BufferUsages::COPY_DST,
        );
        let b = buffer("Game Asset FFT work B", complex_bytes, storage);
        let energy = buffer(
            "Game Asset scale energies",
            energy_bytes,
            storage | wgpu::BufferUsages::COPY_SRC,
        );
        let staging = buffer(
            "Game Asset energy readback",
            energy_bytes,
            wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        );
        let reflect = |v: isize, n: usize| {
            let q = v.rem_euclid((2 * n) as isize) as usize;
            if q < n { q } else { 2 * n - 1 - q }
        };
        let mut upload = Vec::with_capacity(complex_bytes as usize);
        for channel in [0, 1, 2] {
            for y in 0..height {
                cancellation.check().map_err(|e| e.to_string())?;
                for x in 0..width {
                    let p = reflect(y as isize - dimensions.pad as isize, dimensions.sh)
                        * dimensions.sw
                        + reflect(x as isize - dimensions.pad as isize, dimensions.sw);
                    upload.extend_from_slice(&lab[p][channel].to_le_bytes());
                    upload.extend_from_slice(&0_f32.to_le_bytes());
                }
            }
        }
        context.queue.write_buffer(&a, 0, &upload);
        let result = Self {
            context,
            dimensions,
            spectrum,
            a,
            b,
            energy,
            staging,
            scales,
        };
        let mut encoder = result
            .context
            .device
            .create_command_encoder(&Default::default());
        let mut params = result.params();
        result.fft(&mut encoder, &result.a, &result.spectrum, &mut params);
        result.context.queue.submit([encoder.finish()]);
        Ok(result)
    }

    fn params(&self) -> [u32; 16] {
        let (width, height) = self.dimensions.padded();
        let mut p = [0; 16];
        p[0] = width as u32;
        p[1] = height as u32;
        p[2] = self.dimensions.sw as u32;
        p[3] = self.dimensions.sh as u32;
        p[4] = self.dimensions.pad as u32;
        p
    }

    fn fft(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        params: &mut [u32; 16],
    ) {
        params[5] = params[0];
        params[7] = 0;
        self.context.dispatch(
            encoder,
            &self.context.fft,
            input,
            &self.b,
            params,
            (params[1], 3),
        );
        params[5] = params[1];
        params[7] = 1;
        self.context.dispatch(
            encoder,
            &self.context.fft,
            &self.b,
            output,
            params,
            (params[0], 3),
        );
    }

    pub fn energies(
        &self,
        tangent: f32,
        wavelengths: &[f32],
        options: &Options,
        cancellation: &CancellationToken,
    ) -> GpuResult<Vec<[f32; 2]>> {
        cancellation.check().map_err(|e| e.to_string())?;
        if self.context.failed.load(Ordering::Relaxed) {
            return Err("GPU device failed".into());
        }
        if wavelengths.len() != self.scales {
            return Err("wavelet scale plan changed".into());
        }
        let mut encoder = self
            .context
            .device
            .create_command_encoder(&Default::default());
        let mut params = self.params();
        params[6] = 1;
        params[8] = tangent.to_bits();
        params[10] = options.log_bandwidth.to_bits();
        params[11] = (options.orientations as f32).to_bits();
        for c in 0..3 {
            params[12 + c] = options.channel_weights[c].to_bits();
        }
        for (scale, &wavelength) in wavelengths.iter().enumerate() {
            cancellation.check().map_err(|e| e.to_string())?;
            params[9] = wavelength.to_bits();
            params[15] = scale as u32 * params[2] * params[3];
            self.context.dispatch(
                &mut encoder,
                &self.context.filter,
                &self.spectrum,
                &self.a,
                &params,
                ((params[0] * params[1]).div_ceil(256), 1),
            );
            self.fft(&mut encoder, &self.a, &self.a, &mut params);
            self.context.dispatch(
                &mut encoder,
                &self.context.energy,
                &self.a,
                &self.energy,
                &params,
                ((params[2] * params[3]).div_ceil(256), 1),
            );
        }
        encoder.copy_buffer_to_buffer(&self.energy, 0, &self.staging, 0, self.staging.size());
        let submission = self.context.queue.submit([encoder.finish()]);
        let slice = self.staging.slice(..);
        let (send, receive) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = send.send(r);
        });
        let started = Instant::now();
        let mapped = loop {
            if cancellation.check().is_err() {
                break Err("operation cancelled".into());
            }
            if started.elapsed() > WAIT_LIMIT {
                break Err("GPU wavelet readback timed out".into());
            }
            match self.context.device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission.clone()),
                timeout: Some(Duration::from_millis(20)),
            }) {
                Ok(_) | Err(wgpu::PollError::Timeout) => {}
                Err(error) => break Err(error.to_string()),
            }
            match receive.try_recv() {
                Ok(result) => break result.map_err(|e| e.to_string()),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(error) => break Err(error.to_string()),
            }
        };
        if let Err(error) = mapped {
            self.staging.unmap();
            if cancellation.check().is_ok() {
                self.context.failed.store(true, Ordering::Relaxed);
            }
            return Err(error);
        }
        let bytes = slice.get_mapped_range();
        let mut result = Vec::with_capacity(bytes.len() / 8);
        for row in bytes.chunks_exact(self.dimensions.sw * 8) {
            if cancellation.check().is_err() {
                drop(bytes);
                self.staging.unmap();
                return Err("operation cancelled".into());
            }
            for value in row.chunks_exact(8) {
                result.push([
                    f32::from_le_bytes(value[..4].try_into().unwrap()),
                    f32::from_le_bytes(value[4..].try_into().unwrap()),
                ]);
            }
        }
        drop(bytes);
        self.staging.unmap();
        if result.iter().flatten().any(|v| !v.is_finite() || *v < 0.0) {
            self.context.failed.store(true, Ordering::Relaxed);
            return Err("GPU produced invalid wavelet energy".into());
        }
        if self.context.failed.load(Ordering::Relaxed) {
            return Err("GPU device failed".into());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsupported_plans_and_cancellation_before_gpu_allocation() {
        let cancellation = CancellationToken::default();
        let lab = vec![[0.0; 3]; 64 * 32];
        let dimensions = Dimensions {
            sw: 64,
            sh: 32,
            pad: 12,
        };
        assert!(GpuWavelets::new(&lab, dimensions, 3, 0, &cancellation).is_err());
        assert!(GpuWavelets::new(&lab, dimensions, usize::MAX, u64::MAX, &cancellation).is_err());
        assert!(
            GpuWavelets::new(
                &lab,
                Dimensions {
                    sw: MAX_FFT + 1,
                    ..dimensions
                },
                3,
                u64::MAX,
                &cancellation
            )
            .is_err()
        );
        assert!(GpuWavelets::new(&[], dimensions, 3, u64::MAX, &cancellation).is_err());
        cancellation.cancel();
        assert!(GpuWavelets::new(&lab, dimensions, 3, u64::MAX, &cancellation).is_err());
    }
}
