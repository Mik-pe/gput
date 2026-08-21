#[cfg(target_endian = "big")]
compile_error!("gput's GPU byte packing currently requires a little-endian host");

use std::{
    mem::size_of,
    num::NonZeroU64,
    sync::{Arc, mpsc},
};

use anyhow::{Context, Result, anyhow, bail};
use bytemuck::{Pod, Zeroable};
use tracing::info;

use super::{
    Processor, ProcessorLimits,
    shader_strings::{StringMeta, build_shader_assets},
};
use crate::{
    builtin_router,
    router::{CompiledRouter, GpuRouterLayout, Router},
};

const SHADER_BODY: &str = include_str!("http.wgsl");
const WORKGROUP_SIZE: u32 = 64;
const REQUIRED_STORAGE_BUFFERS_PER_SHADER_STAGE: u32 = 7;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct Params {
    request_stride_words: u32,
    response_stride_words: u32,
    request_count: u32,
    route_count: u32,
    fallback_response_offset: u32,
    bad_request_response_offset: u32,
    method_not_allowed_response_offset: u32,
    _padding: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct RequestMeta {
    input_len: u32,
    _padding_0: u32,
    _padding_1: u32,
    _padding_2: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct ResponseMeta {
    output_len: u32,
    status: u32,
    flags: u32,
    _padding: u32,
}

pub struct GpuProcessor {
    _instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    params_buffer: wgpu::Buffer,
    request_meta_buffer: wgpu::Buffer,
    input_buffer: wgpu::Buffer,
    response_meta_buffer: wgpu::Buffer,
    output_buffer: wgpu::Buffer,
    _string_meta_buffer: wgpu::Buffer,
    _string_words_buffer: wgpu::Buffer,
    _router_words_buffer: wgpu::Buffer,
    response_meta_readback: wgpu::Buffer,
    output_readback: wgpu::Buffer,
    router_layout: GpuRouterLayout,
    max_batch_size: usize,
    max_request_bytes: usize,
    request_stride_words: usize,
    response_stride_words: usize,
}

impl GpuProcessor {
    pub fn new(limits: ProcessorLimits) -> Result<Self> {
        Self::with_router(limits, builtin_router())
    }

    pub fn with_router(limits: ProcessorLimits, router: Router) -> Result<Self> {
        let router = Arc::new(router.compile()?);
        router.validate_gpu_response_slot(limits.response_slot_bytes)?;
        Self::from_compiled(limits, router)
    }

    pub(crate) fn from_compiled(
        limits: ProcessorLimits,
        router: Arc<CompiledRouter>,
    ) -> Result<Self> {
        let shader_assets = build_shader_assets(SHADER_BODY, router.as_ref())?;
        let request_stride_words = words_for_bytes(limits.max_request_bytes)?;
        let response_stride_words = words_for_bytes(limits.response_slot_bytes)?;
        let request_meta_bytes = checked_buffer_bytes(
            "request metadata",
            limits.max_batch_size,
            size_of::<RequestMeta>(),
        )?;
        let input_bytes = checked_buffer_bytes(
            "request input",
            limits.max_batch_size,
            request_stride_words * size_of::<u32>(),
        )?;
        let response_meta_bytes = checked_buffer_bytes(
            "response metadata",
            limits.max_batch_size,
            size_of::<ResponseMeta>(),
        )?;
        let output_bytes = checked_buffer_bytes(
            "response output",
            limits.max_batch_size,
            response_stride_words * size_of::<u32>(),
        )?;
        let string_meta_bytes = checked_buffer_bytes(
            "shader string metadata",
            shader_assets.metadata.len(),
            size_of::<StringMeta>(),
        )?;
        let string_words_bytes = checked_buffer_bytes(
            "shader string arena",
            shader_assets.words.len(),
            size_of::<u32>(),
        )?;
        let router_words_bytes = checked_buffer_bytes(
            "GPU router program",
            router.router_words().len(),
            size_of::<u32>(),
        )?;

        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .context("no suitable GPU adapter was found")?;

        let adapter_info = adapter.get_info();
        let downlevel_capabilities = adapter.get_downlevel_capabilities();
        if !downlevel_capabilities
            .flags
            .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
        {
            bail!(
                "adapter {} does not support compute shaders",
                adapter_info.name
            );
        }

        let mut required_limits = wgpu::Limits::downlevel_defaults();
        required_limits.max_storage_buffers_per_shader_stage =
            REQUIRED_STORAGE_BUFFERS_PER_SHADER_STAGE;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("gput-device"),
            required_features: wgpu::Features::empty(),
            required_limits,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .context("failed to create wgpu device")?;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gput-http-shader"),
            source: wgpu::ShaderSource::Wgsl(shader_assets.source.as_str().into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("gput-bind-group-layout"),
            entries: &[
                buffer_layout_entry(
                    0,
                    wgpu::BufferBindingType::Uniform,
                    false,
                    size_of::<Params>(),
                ),
                buffer_layout_entry(
                    1,
                    wgpu::BufferBindingType::Storage { read_only: true },
                    false,
                    size_of::<RequestMeta>(),
                ),
                buffer_layout_entry(
                    2,
                    wgpu::BufferBindingType::Storage { read_only: true },
                    false,
                    size_of::<u32>(),
                ),
                buffer_layout_entry(
                    3,
                    wgpu::BufferBindingType::Storage { read_only: false },
                    false,
                    size_of::<ResponseMeta>(),
                ),
                buffer_layout_entry(
                    4,
                    wgpu::BufferBindingType::Storage { read_only: false },
                    false,
                    size_of::<u32>(),
                ),
                buffer_layout_entry(
                    5,
                    wgpu::BufferBindingType::Storage { read_only: true },
                    false,
                    size_of::<StringMeta>(),
                ),
                buffer_layout_entry(
                    6,
                    wgpu::BufferBindingType::Storage { read_only: true },
                    false,
                    size_of::<u32>(),
                ),
                buffer_layout_entry(
                    7,
                    wgpu::BufferBindingType::Storage { read_only: true },
                    false,
                    size_of::<u32>(),
                ),
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("gput-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("gput-http-pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("process_requests"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let params_buffer = create_buffer(
            &device,
            "gput-params",
            size_of::<Params>() as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let request_meta_buffer = create_buffer(
            &device,
            "gput-request-meta",
            request_meta_bytes,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let input_buffer = create_buffer(
            &device,
            "gput-input",
            input_bytes,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let response_meta_buffer = create_buffer(
            &device,
            "gput-response-meta",
            response_meta_bytes,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let output_buffer = create_buffer(
            &device,
            "gput-output",
            output_bytes,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let string_meta_buffer = create_buffer(
            &device,
            "gput-string-meta",
            string_meta_bytes,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let string_words_buffer = create_buffer(
            &device,
            "gput-string-arena",
            string_words_bytes,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let router_words_buffer = create_buffer(
            &device,
            "gput-router-program",
            router_words_bytes,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let response_meta_readback = create_buffer(
            &device,
            "gput-response-meta-readback",
            response_meta_bytes,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        let output_readback = create_buffer(
            &device,
            "gput-output-readback",
            output_bytes,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );

        queue.write_buffer(
            &string_meta_buffer,
            0,
            bytemuck::cast_slice(&shader_assets.metadata),
        );
        queue.write_buffer(
            &string_words_buffer,
            0,
            bytemuck::cast_slice(&shader_assets.words),
        );
        queue.write_buffer(
            &router_words_buffer,
            0,
            bytemuck::cast_slice(router.router_words()),
        );

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gput-bind-group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: request_meta_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: input_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: response_meta_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: output_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: string_meta_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: string_words_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: router_words_buffer.as_entire_binding(),
                },
            ],
        });

        let router_layout = router.gpu_layout();
        info!(
            adapter = %adapter_info.name,
            backend = ?adapter_info.backend,
            device_type = ?adapter_info.device_type,
            max_batch_size = limits.max_batch_size,
            request_slot_bytes = request_stride_words * size_of::<u32>(),
            response_slot_bytes = response_stride_words * size_of::<u32>(),
            max_router_response_bytes = router.max_gpu_response_bytes(),
            routes = router_layout.route_count,
            router_words = router.router_words().len(),
            shader_strings = shader_assets.metadata.len(),
            shader_string_bytes = shader_assets.byte_len,
            "using GPU compute processor"
        );

        Ok(Self {
            _instance: instance,
            device,
            queue,
            pipeline,
            bind_group,
            params_buffer,
            request_meta_buffer,
            input_buffer,
            response_meta_buffer,
            output_buffer,
            _string_meta_buffer: string_meta_buffer,
            _string_words_buffer: string_words_buffer,
            _router_words_buffer: router_words_buffer,
            response_meta_readback,
            output_readback,
            router_layout,
            max_batch_size: limits.max_batch_size,
            max_request_bytes: limits.max_request_bytes,
            request_stride_words,
            response_stride_words,
        })
    }

    fn process_gpu_batch(&mut self, requests: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }

        if requests.len() > self.max_batch_size {
            bail!(
                "batch contains {} requests; GPU capacity is {}",
                requests.len(),
                self.max_batch_size
            );
        }

        let request_count = requests.len();
        let mut input_words = vec![0_u32; request_count * self.request_stride_words];
        let mut request_meta = Vec::with_capacity(request_count);

        for (request_index, request) in requests.iter().enumerate() {
            if request.len() > self.max_request_bytes {
                bail!(
                    "request {request_index} is {} bytes; GPU slot capacity is {}",
                    request.len(),
                    self.max_request_bytes
                );
            }

            pack_request_words(
                request,
                &mut input_words[request_index * self.request_stride_words
                    ..(request_index + 1) * self.request_stride_words],
            );
            request_meta.push(RequestMeta {
                input_len: u32::try_from(request.len())
                    .context("request length does not fit u32")?,
                _padding_0: 0,
                _padding_1: 0,
                _padding_2: 0,
            });
        }

        let params = Params {
            request_stride_words: u32::try_from(self.request_stride_words)
                .context("request stride does not fit u32")?,
            response_stride_words: u32::try_from(self.response_stride_words)
                .context("response stride does not fit u32")?,
            request_count: u32::try_from(request_count)
                .context("request count does not fit u32")?,
            route_count: self.router_layout.route_count,
            fallback_response_offset: self.router_layout.fallback_response_offset,
            bad_request_response_offset: self.router_layout.bad_request_response_offset,
            method_not_allowed_response_offset: self
                .router_layout
                .method_not_allowed_response_offset,
            _padding: 0,
        };

        self.queue
            .write_buffer(&self.params_buffer, 0, bytemuck::bytes_of(&params));
        self.queue.write_buffer(
            &self.request_meta_buffer,
            0,
            bytemuck::cast_slice(&request_meta),
        );
        self.queue
            .write_buffer(&self.input_buffer, 0, bytemuck::cast_slice(&input_words));

        let response_meta_copy_bytes = u64::try_from(request_count * size_of::<ResponseMeta>())
            .context("response metadata copy size overflow")?;
        let output_copy_bytes =
            u64::try_from(request_count * self.response_stride_words * size_of::<u32>())
                .context("response output copy size overflow")?;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("gput-batch-encoder"),
            });
        {
            let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("gput-http-compute-pass"),
                timestamp_writes: None,
            });
            compute_pass.set_pipeline(&self.pipeline);
            compute_pass.set_bind_group(0, &self.bind_group, &[]);
            let workgroups = params.request_count.div_ceil(WORKGROUP_SIZE);
            compute_pass.dispatch_workgroups(workgroups, 1, 1);
        }

        encoder.copy_buffer_to_buffer(
            &self.response_meta_buffer,
            0,
            &self.response_meta_readback,
            0,
            response_meta_copy_bytes,
        );
        encoder.copy_buffer_to_buffer(
            &self.output_buffer,
            0,
            &self.output_readback,
            0,
            output_copy_bytes,
        );
        self.queue.submit([encoder.finish()]);

        self.read_responses(request_count, response_meta_copy_bytes, output_copy_bytes)
    }

    fn read_responses(
        &self,
        request_count: usize,
        response_meta_copy_bytes: u64,
        output_copy_bytes: u64,
    ) -> Result<Vec<Vec<u8>>> {
        let meta_slice = self
            .response_meta_readback
            .slice(0..response_meta_copy_bytes);
        let output_slice = self.output_readback.slice(0..output_copy_bytes);
        let (meta_sender, meta_receiver) = mpsc::sync_channel(1);
        let (output_sender, output_receiver) = mpsc::sync_channel(1);

        meta_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = meta_sender.send(result);
        });
        output_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = output_sender.send(result);
        });

        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .context("waiting for GPU batch completion failed")?;

        let meta_map_result = meta_receiver
            .recv()
            .context("response metadata map callback disappeared")?;
        let output_map_result = output_receiver
            .recv()
            .context("response output map callback disappeared")?;

        if let Err(error) = meta_map_result {
            if output_map_result.is_ok() {
                self.output_readback.unmap();
            }
            return Err(anyhow!("mapping response metadata failed: {error}"));
        }

        if let Err(error) = output_map_result {
            self.response_meta_readback.unmap();
            return Err(anyhow!("mapping response output failed: {error}"));
        }

        let meta_data = match meta_slice.get_mapped_range() {
            Ok(data) => data,
            Err(error) => {
                self.output_readback.unmap();
                self.response_meta_readback.unmap();
                return Err(anyhow!("reading mapped response metadata failed: {error}"));
            }
        };
        let output_data = match output_slice.get_mapped_range() {
            Ok(data) => data,
            Err(error) => {
                drop(meta_data);
                self.output_readback.unmap();
                self.response_meta_readback.unmap();
                return Err(anyhow!("reading mapped response output failed: {error}"));
            }
        };

        let decode_result = (|| -> Result<Vec<Vec<u8>>> {
            let response_stride_bytes = self.response_stride_words * size_of::<u32>();
            let mut responses = Vec::with_capacity(request_count);

            for request_index in 0..request_count {
                let meta_start = request_index * size_of::<ResponseMeta>();
                let meta_end = meta_start + size_of::<ResponseMeta>();
                let meta: ResponseMeta =
                    bytemuck::pod_read_unaligned(&meta_data[meta_start..meta_end]);

                if meta.flags != 0 {
                    bail!(
                        "shader failed request {request_index} with status {} and flags {}",
                        meta.status,
                        meta.flags
                    );
                }

                let output_len = usize::try_from(meta.output_len)
                    .context("shader response length does not fit usize")?;
                if output_len == 0 || output_len > response_stride_bytes {
                    bail!(
                        "shader returned invalid response length {output_len} for request {request_index}"
                    );
                }

                let output_start = request_index * response_stride_bytes;
                responses.push(output_data[output_start..output_start + output_len].to_vec());
            }

            Ok(responses)
        })();

        drop(output_data);
        drop(meta_data);
        self.output_readback.unmap();
        self.response_meta_readback.unmap();

        decode_result
    }
}

impl Processor for GpuProcessor {
    fn name(&self) -> &'static str {
        "gpu"
    }

    fn process_batch(&mut self, requests: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        self.process_gpu_batch(requests)
    }
}

fn buffer_layout_entry(
    binding: u32,
    binding_type: wgpu::BufferBindingType,
    has_dynamic_offset: bool,
    minimum_size: usize,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: binding_type,
            has_dynamic_offset,
            min_binding_size: NonZeroU64::new(minimum_size as u64),
        },
        count: None,
    }
}

fn create_buffer(
    device: &wgpu::Device,
    label: &'static str,
    size: u64,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage,
        mapped_at_creation: false,
    })
}

fn words_for_bytes(bytes: usize) -> Result<usize> {
    bytes
        .checked_add(3)
        .map(|value| value / 4)
        .ok_or_else(|| anyhow!("word stride overflow"))
}

fn checked_buffer_bytes(label: &str, count: usize, stride: usize) -> Result<u64> {
    let bytes = count
        .checked_mul(stride)
        .ok_or_else(|| anyhow!("{label} buffer size overflow"))?;
    u64::try_from(bytes).with_context(|| format!("{label} buffer size does not fit u64"))
}

fn pack_request_words(request: &[u8], destination: &mut [u32]) {
    destination.fill(0);

    for (byte_index, byte) in request.iter().copied().enumerate() {
        let word_index = byte_index / 4;
        let shift = (byte_index % 4) * 8;
        destination[word_index] |= u32::from(byte) << shift;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin_router;

    #[test]
    fn packs_bytes_little_endian_for_wgsl_extraction() {
        let mut words = [0_u32; 2];
        pack_request_words(b"GET /", &mut words);

        assert_eq!(words[0], 0x2054_4547);
        assert_eq!(words[1], 0x0000_002f);
    }

    #[test]
    fn composed_shader_parses_and_validates() {
        let router = builtin_router().compile().expect("router compiles");
        let assets = build_shader_assets(SHADER_BODY, &router).expect("shader assets build");
        let module = naga::front::wgsl::parse_str(&assets.source).expect("WGSL must parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("WGSL must validate");
    }
}
