use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use image::RgbaImage;
use wgpu::util::DeviceExt;

use crate::document::{CancellationToken, Resampling};
use crate::error::{AppError, Result};

const MIN_GPU_PIXELS: u64 = 512 * 512;
const MAP_TIMEOUT: Duration = Duration::from_secs(5);

/// A scaling-session GPU cache. The source texture is uploaded once and reused
/// while the user changes preview dimensions or filtering methods.
pub struct GpuScaler {
    source: Arc<RgbaImage>,
    backend: OnceLock<std::result::Result<Mutex<Backend>, String>>,
}

impl GpuScaler {
    #[must_use]
    pub fn new(source: Arc<RgbaImage>) -> Self {
        Self {
            source,
            backend: OnceLock::new(),
        }
    }

    /// Returns `Ok(None)` when CPU scaling is preferable or no hardware GPU is
    /// available. GPU failures therefore never prevent a scale preview.
    pub fn resize(
        &self,
        target_width: u32,
        target_height: u32,
        resampling: Resampling,
        cancellation: &CancellationToken,
    ) -> Result<Option<RgbaImage>> {
        if resampling != Resampling::Bicubic
            || u64::from(target_width) * u64::from(target_height) < MIN_GPU_PIXELS
            || u64::from(self.source.width()) > u64::from(target_width) * 2
            || u64::from(self.source.height()) > u64::from(target_height) * 2
        {
            return Ok(None);
        }
        cancellation.check()?;
        let backend = self.backend.get_or_init(|| {
            Backend::new(&self.source).map(Mutex::new).map_err(|error| {
                tracing::info!(%error, "GPU scaling unavailable; using CPU previews");
                error
            })
        });
        let backend = match backend {
            Ok(backend) => backend,
            Err(_) => return Ok(None),
        };
        let Ok(mut backend) = backend.lock() else {
            tracing::warn!("GPU scaling session was poisoned; using CPU");
            return Ok(None);
        };
        let result = backend.resize(target_width, target_height, cancellation);
        match result {
            Ok(image) => Ok(Some(image)),
            Err(GpuError::Cancelled) => Err(AppError::Cancelled),
            Err(error) => {
                tracing::warn!(%error, "GPU scale preview failed; using CPU");
                Ok(None)
            }
        }
    }
}

struct Backend {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    source: wgpu::Texture,
    source_width: u32,
    source_height: u32,
    max_texture_dimension: u32,
    max_buffer_size: u64,
}

impl Backend {
    fn new(source: &RgbaImage) -> std::result::Result<Self, String> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .map_err(|error| error.to_string())?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            return Err("only a software Vulkan adapter was found".into());
        }
        let limits = adapter.limits();
        if source.width() > limits.max_texture_dimension_2d
            || source.height() > limits.max_texture_dimension_2d
        {
            return Err("source image exceeds the GPU texture limit".into());
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("Diorama scale preview device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|error| error.to_string())?;
        device.on_uncaptured_error(Arc::new(|error| {
            tracing::warn!(%error, "GPU scaling device error");
        }));
        let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Diorama scale preview shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Diorama scale preview pipeline"),
            layout: None,
            module: &shader,
            entry_point: Some("scale"),
            compilation_options: Default::default(),
            cache: None,
        });
        if let Some(error) = pollster::block_on(error_scope.pop()) {
            return Err(error.to_string());
        }
        let source_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Diorama scale source"),
            size: wgpu::Extent3d {
                width: source.width(),
                height: source.height(),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &source_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            source.as_raw(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(source.width() * 4),
                rows_per_image: Some(source.height()),
            },
            source_texture.size(),
        );
        tracing::info!(adapter = %info.name, "GPU scaling previews enabled");
        Ok(Self {
            device,
            queue,
            pipeline,
            source: source_texture,
            source_width: source.width(),
            source_height: source.height(),
            max_texture_dimension: limits.max_texture_dimension_2d,
            max_buffer_size: limits.max_buffer_size,
        })
    }

    fn resize(
        &mut self,
        width: u32,
        height: u32,
        cancellation: &CancellationToken,
    ) -> std::result::Result<RgbaImage, GpuError> {
        cancellation.check().map_err(|_| GpuError::Cancelled)?;
        if width == 0
            || height == 0
            || width > self.max_texture_dimension
            || height > self.max_texture_dimension
        {
            return Err(GpuError::Unavailable("dimensions exceed GPU limits".into()));
        }
        let unpadded_row = u64::from(width) * 4;
        let padded_row = unpadded_row.div_ceil(u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT))
            * u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer_size = padded_row
            .checked_mul(u64::from(height))
            .ok_or_else(|| GpuError::Unavailable("preview is too large".into()))?;
        if buffer_size > self.max_buffer_size {
            return Err(GpuError::Unavailable(
                "preview exceeds GPU buffer limits".into(),
            ));
        }
        let output = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Diorama scale preview output"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let params = [self.source_width, self.source_height, width, height];
        let param_bytes: Vec<u8> = params
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect();
        let uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Diorama scale preview parameters"),
                contents: &param_bytes,
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let source_view = self.source.create_view(&Default::default());
        let output_view = output.create_view(&Default::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Diorama scale preview bindings"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Diorama scale preview readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Diorama scale preview commands"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Diorama scale preview pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(width.div_ceil(16), height.div_ceil(16), 1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(
                        u32::try_from(padded_row)
                            .map_err(|_| GpuError::Unavailable("row is too wide".into()))?,
                    ),
                    rows_per_image: Some(height),
                },
            },
            output.size(),
        );
        let submission = self.queue.submit([encoder.finish()]);
        let slice = staging.slice(..);
        let (sender, receiver) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(MAP_TIMEOUT),
            })
            .map_err(|error| GpuError::Unavailable(error.to_string()))?;
        receiver
            .recv_timeout(MAP_TIMEOUT)
            .map_err(|error| GpuError::Unavailable(error.to_string()))?
            .map_err(|error| GpuError::Unavailable(error.to_string()))?;
        cancellation.check().map_err(|_| GpuError::Cancelled)?;
        let mapped = slice.get_mapped_range();
        let mut pixels = vec![
            0;
            usize::try_from(unpadded_row * u64::from(height)).map_err(|_| {
                GpuError::Unavailable("preview is too large".into())
            })?
        ];
        for (source_row, target_row) in mapped
            .chunks_exact(usize::try_from(padded_row).unwrap_or(usize::MAX))
            .zip(pixels.chunks_exact_mut(usize::try_from(unpadded_row).unwrap_or(usize::MAX)))
        {
            target_row.copy_from_slice(&source_row[..target_row.len()]);
        }
        drop(mapped);
        staging.unmap();
        RgbaImage::from_raw(width, height, pixels)
            .ok_or_else(|| GpuError::Unavailable("invalid GPU output".into()))
    }
}

#[derive(Debug, thiserror::Error)]
enum GpuError {
    #[error("operation cancelled")]
    Cancelled,
    #[error("{0}")]
    Unavailable(String),
}

const SHADER: &str = r#"
struct Params {
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> params: Params;

fn sample_pixel(position: vec2<i32>) -> vec4<f32> {
    let maximum = vec2<i32>(i32(params.source_width) - 1, i32(params.source_height) - 1);
    let color = textureLoad(source, clamp(position, vec2<i32>(0), maximum), 0);
    return vec4<f32>(color.rgb * color.a, color.a);
}

fn finish_color(color: vec4<f32>) -> vec4<f32> {
    let alpha = clamp(color.a, 0.0, 1.0);
    if alpha > 0.000001 {
        return vec4<f32>(clamp(color.rgb / alpha, vec3<f32>(0.0), vec3<f32>(1.0)), alpha);
    }
    return vec4<f32>(0.0);
}

fn cubic_weight(distance: f32) -> f32 {
    let x = abs(distance);
    if x <= 1.0 {
        return 1.5 * x * x * x - 2.5 * x * x + 1.0;
    }
    if x < 2.0 {
        return -0.5 * x * x * x + 2.5 * x * x - 4.0 * x + 2.0;
    }
    return 0.0;
}

@compute @workgroup_size(16, 16)
fn scale(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.target_width || id.y >= params.target_height {
        return;
    }
    let source_position = (vec2<f32>(id.xy) + vec2<f32>(0.5))
        * vec2<f32>(f32(params.source_width) / f32(params.target_width),
                    f32(params.source_height) / f32(params.target_height))
        - vec2<f32>(0.5);
    var color: vec4<f32>;

        let base = vec2<i32>(floor(source_position));
        color = vec4<f32>(0.0);
        var total_weight = 0.0;
        for (var y = -1; y <= 2; y = y + 1) {
            let wy = cubic_weight(source_position.y - f32(base.y + y));
            for (var x = -1; x <= 2; x = x + 1) {
                let weight = cubic_weight(source_position.x - f32(base.x + x)) * wy;
                color += sample_pixel(base + vec2<i32>(x, y)) * weight;
                total_weight += weight;
            }
        }
        color /= total_weight;
    textureStore(output_texture, vec2<i32>(id.xy), finish_color(color));
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn test_image(width: u32, height: u32) -> Arc<RgbaImage> {
        Arc::new(RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([
                ((x * 17 + y * 3) % 256) as u8,
                ((x * 5 + y * 11) % 256) as u8,
                ((x * 7 + y * 13) % 256) as u8,
                ((x * 19 + y * 23) % 256) as u8,
            ])
        }))
    }

    #[test]
    fn filtered_previews_stay_close_to_cpu() {
        let source = test_image(640, 512);
        let cancellation = CancellationToken::default();
        let gpu = GpuScaler::new(source.clone());
        let method = Resampling::Bicubic;
        let Some(actual) = gpu
            .resize(590, 470, method, &cancellation)
            .expect("GPU scaling should not fail")
        else {
            eprintln!("hardware GPU unavailable; skipping comparison");
            return;
        };
        let expected = super::super::resize(&source, 590, 470, method, &cancellation).unwrap();
        let absolute_error: u64 = actual
            .as_raw()
            .iter()
            .zip(expected.as_raw())
            .map(|(actual, expected)| u64::from(actual.abs_diff(*expected)))
            .sum();
        let mean_error = absolute_error as f64 / actual.as_raw().len() as f64;
        assert!(mean_error < 5.0, "{method:?} mean error was {mean_error}");
    }

    #[test]
    fn unsuitable_operations_stay_on_cpu() {
        let source = test_image(640, 512);
        let gpu = GpuScaler::new(source);
        let cancellation = CancellationToken::default();
        assert!(
            gpu.resize(590, 470, Resampling::Nearest, &cancellation)
                .unwrap()
                .is_none()
        );

        assert!(
            gpu.resize(590, 470, Resampling::Lanczos, &cancellation)
                .unwrap()
                .is_none()
        );

        assert!(
            gpu.resize(256, 200, Resampling::Bicubic, &cancellation)
                .unwrap()
                .is_none()
        );
    }
}
