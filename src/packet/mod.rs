mod cpu;
mod wire;

pub use cpu::CpuPacketEngine;
pub use wire::{
    FlowKey, TCP_ACK, TCP_FIN, TCP_PSH, TCP_RST, TCP_SYN, TcpPacketSpec, TcpPacketView,
    build_ipv4_tcp_packet, flow_hash, parse_ipv4_tcp, validate_ipv4_tcp_checksums,
};

use std::{
    collections::HashSet,
    fmt::Write as _,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, ensure};
use bytemuck::{Pod, Zeroable};

pub const MAX_RAW_PACKET_BYTES: usize = 1536;

const PACKET_WORDS: usize = MAX_RAW_PACKET_BYTES.div_ceil(4);
const FLOW_WORDS: usize = 16;
const DEFAULT_FLOW_CAPACITY: usize = 4096;
const DEFAULT_FLOW_PROBE_LIMIT: usize = 32;
const WORKGROUP_SIZE: usize = 64;
const REQUIRED_STORAGE_BUFFERS_PER_SHADER_STAGE: u32 = 5;

const RESPONSE_PLAINTEXT: u32 = 0;
const RESPONSE_HEALTH: u32 = 1;
const RESPONSE_BAD_REQUEST: u32 = 2;
const RESPONSE_NOT_FOUND: u32 = 3;
const RESPONSE_METHOD_NOT_ALLOWED: u32 = 4;

const PACKET_RESPONSES: [&[u8]; 5] = [
    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 14\r\nConnection: keep-alive\r\nServer: gput\r\nX-Gput-Backend: gpu-packet\r\n\r\nHello, World!\n",
    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 3\r\nConnection: keep-alive\r\nServer: gput\r\nX-Gput-Backend: gpu-packet\r\n\r\nok\n",
    b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 12\r\nConnection: keep-alive\r\nServer: gput\r\nX-Gput-Backend: gpu-packet\r\n\r\nbad request\n",
    b"HTTP/1.1 404 Not Found\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 10\r\nConnection: keep-alive\r\nServer: gput\r\nX-Gput-Backend: gpu-packet\r\n\r\nnot found\n",
    b"HTTP/1.1 405 Method Not Allowed\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 19\r\nConnection: keep-alive\r\nServer: gput\r\nX-Gput-Backend: gpu-packet\r\n\r\nmethod not allowed\n",
];

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct PacketMeta {
    len: u32,
    _padding: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct EngineParams {
    packet_count: u32,
    packet_stride_words: u32,
    flow_capacity: u32,
    listen_port: u32,
    flow_probe_limit: u32,
    _padding: [u32; 3],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawPacket {
    bytes: Vec<u8>,
}

impl RawPacket {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self> {
        let bytes = bytes.into();
        ensure!(!bytes.is_empty(), "packet must not be empty");
        ensure!(
            bytes.len() <= MAX_RAW_PACKET_BYTES,
            "packet is {} bytes; maximum is {MAX_RAW_PACKET_BYTES}",
            bytes.len()
        );
        Ok(Self { bytes })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PacketEngineMetrics {
    pub dispatches: u64,
    pub packets: u64,
}

impl PacketEngineMetrics {
    pub fn saturating_sub(self, earlier: Self) -> Self {
        Self {
            dispatches: self.dispatches.saturating_sub(earlier.dispatches),
            packets: self.packets.saturating_sub(earlier.packets),
        }
    }
}

pub trait PacketEngine {
    fn name(&self) -> &'static str;
    fn process_batch(&self, packets: &[RawPacket]) -> Result<Vec<Option<RawPacket>>>;

    fn metrics(&self) -> PacketEngineMetrics {
        PacketEngineMetrics::default()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PacketEngineConfig {
    pub max_batch_size: usize,
    pub flow_capacity: usize,
    pub flow_probe_limit: usize,
    pub listen_port: u16,
}

impl Default for PacketEngineConfig {
    fn default() -> Self {
        Self {
            max_batch_size: 256,
            flow_capacity: DEFAULT_FLOW_CAPACITY,
            flow_probe_limit: DEFAULT_FLOW_PROBE_LIMIT,
            listen_port: 8080,
        }
    }
}

pub struct GpuPacketEngine {
    _instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    input_meta: wgpu::Buffer,
    input_words: wgpu::Buffer,
    output_meta: wgpu::Buffer,
    output_words: wgpu::Buffer,
    _flow_state: wgpu::Buffer,
    readback_meta: wgpu::Buffer,
    readback_words: wgpu::Buffer,
    params: wgpu::Buffer,
    config: PacketEngineConfig,
    adapter_name: String,
    dispatches: AtomicU64,
    packets: AtomicU64,
}

impl GpuPacketEngine {
    pub fn new(config: PacketEngineConfig) -> Result<Self> {
        validate_config(config)?;
        pollster::block_on(Self::new_async(config))
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    async fn new_async(config: PacketEngineConfig) -> Result<Self> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .context("no compute-capable GPU adapter available for packet engine")?;
        let adapter_info = adapter.get_info();
        let downlevel_capabilities = adapter.get_downlevel_capabilities();
        ensure!(
            downlevel_capabilities
                .flags
                .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS),
            "adapter {} does not support compute shaders",
            adapter_info.name
        );

        let mut required_limits = wgpu::Limits::downlevel_defaults();
        required_limits.max_storage_buffers_per_shader_stage =
            REQUIRED_STORAGE_BUFFERS_PER_SHADER_STAGE;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("gput-packet-device"),
                required_features: wgpu::Features::empty(),
                required_limits,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
            })
            .await
            .context("failed to create packet engine device")?;

        let shader_source = packet_shader_source()?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gput-packet-tcp-shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source.as_str().into()),
        });

        let packet_meta_bytes = config
            .max_batch_size
            .checked_mul(std::mem::size_of::<PacketMeta>())
            .context("packet metadata buffer size overflow")?;
        let packet_words_bytes = config
            .max_batch_size
            .checked_mul(PACKET_WORDS)
            .and_then(|words| words.checked_mul(std::mem::size_of::<u32>()))
            .context("packet word buffer size overflow")?;
        let flow_words = config
            .flow_capacity
            .checked_mul(FLOW_WORDS)
            .context("flow word count overflow")?;
        let flow_bytes = flow_words
            .checked_mul(std::mem::size_of::<u32>())
            .context("flow buffer size overflow")?;

        let input_meta = storage_buffer(&device, "packet input metadata", packet_meta_bytes, false);
        let input_words = storage_buffer(&device, "packet input words", packet_words_bytes, false);
        let output_meta =
            storage_buffer(&device, "packet output metadata", packet_meta_bytes, false);
        let output_words =
            storage_buffer(&device, "packet output words", packet_words_bytes, false);
        let flow_state = storage_buffer(&device, "packet TCP flow state", flow_bytes, false);
        let readback_meta =
            storage_buffer(&device, "packet readback metadata", packet_meta_bytes, true);
        let readback_words =
            storage_buffer(&device, "packet readback words", packet_words_bytes, true);
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("packet engine params"),
            size: std::mem::size_of::<EngineParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&flow_state, 0, bytemuck::cast_slice(&vec![0_u32; flow_words]));

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("packet engine bind group layout"),
            entries: &[
                storage_layout(0, true),
                storage_layout(1, true),
                storage_layout(2, false),
                storage_layout(3, false),
                storage_layout(4, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("packet engine bind group"),
            layout: &bind_group_layout,
            entries: &[
                buffer_entry(0, &input_meta),
                buffer_entry(1, &input_words),
                buffer_entry(2, &output_meta),
                buffer_entry(3, &output_words),
                buffer_entry(4, &flow_state),
                buffer_entry(5, &params),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("packet engine pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("packet engine pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        tracing::info!(
            adapter = %adapter_info.name,
            backend = ?adapter_info.backend,
            device_type = ?adapter_info.device_type,
            max_batch_size = config.max_batch_size,
            flow_capacity = config.flow_capacity,
            flow_probe_limit = config.flow_probe_limit,
            listen_port = config.listen_port,
            "using collision-safe GPU-native packet engine"
        );

        Ok(Self {
            _instance: instance,
            device,
            queue,
            pipeline,
            bind_group,
            input_meta,
            input_words,
            output_meta,
            output_words,
            _flow_state: flow_state,
            readback_meta,
            readback_words,
            params,
            config,
            adapter_name: adapter_info.name,
            dispatches: AtomicU64::new(0),
            packets: AtomicU64::new(0),
        })
    }

    fn dispatch_wave(&self, packets: &[&RawPacket]) -> Result<Vec<Option<RawPacket>>> {
        if packets.is_empty() {
            return Ok(Vec::new());
        }
        let packet_count = packets.len();
        let mut metadata = vec![PacketMeta::zeroed(); packet_count];
        let mut words = vec![0_u32; packet_count * PACKET_WORDS];
        for (packet_index, packet) in packets.iter().enumerate() {
            metadata[packet_index].len = packet.bytes.len() as u32;
            let base = packet_index * PACKET_WORDS;
            pack_packet(&packet.bytes, &mut words[base..base + PACKET_WORDS]);
        }

        let params = EngineParams {
            packet_count: packet_count as u32,
            packet_stride_words: PACKET_WORDS as u32,
            flow_capacity: self.config.flow_capacity as u32,
            listen_port: self.config.listen_port.into(),
            flow_probe_limit: self.config.flow_probe_limit as u32,
            _padding: [0; 3],
        };
        self.queue
            .write_buffer(&self.input_meta, 0, bytemuck::cast_slice(&metadata));
        self.queue
            .write_buffer(&self.input_words, 0, bytemuck::cast_slice(&words));
        self.queue
            .write_buffer(&self.params, 0, bytemuck::bytes_of(&params));

        let meta_copy_bytes =
            (packet_count * std::mem::size_of::<PacketMeta>()) as u64;
        let word_copy_bytes =
            (packet_count * PACKET_WORDS * std::mem::size_of::<u32>()) as u64;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("packet engine dispatch"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("packet engine compute pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.dispatch_workgroups(packet_count.div_ceil(WORKGROUP_SIZE) as u32, 1, 1);
        }
        encoder.copy_buffer_to_buffer(
            &self.output_meta,
            0,
            &self.readback_meta,
            0,
            meta_copy_bytes,
        );
        encoder.copy_buffer_to_buffer(
            &self.output_words,
            0,
            &self.readback_words,
            0,
            word_copy_bytes,
        );
        self.queue.submit(Some(encoder.finish()));

        let meta_bytes = map_read(&self.device, &self.readback_meta, meta_copy_bytes)?;
        let word_bytes = map_read(&self.device, &self.readback_words, word_copy_bytes)?;
        let output_meta: &[PacketMeta] = bytemuck::cast_slice(&meta_bytes);
        let output_words: &[u32] = bytemuck::cast_slice(&word_bytes);
        let mut output = Vec::with_capacity(packet_count);

        for (index, meta) in output_meta.iter().enumerate() {
            let len = meta.len as usize;
            if len == 0 {
                output.push(None);
                continue;
            }
            ensure!(
                len <= MAX_RAW_PACKET_BYTES,
                "GPU produced oversized packet of {len} bytes"
            );
            let base = index * PACKET_WORDS;
            output.push(Some(RawPacket::new(unpack_packet(
                &output_words[base..base + PACKET_WORDS],
                len,
            ))?));
        }

        drop(word_bytes);
        drop(meta_bytes);
        self.readback_words.unmap();
        self.readback_meta.unmap();
        self.dispatches.fetch_add(1, Ordering::Relaxed);
        self.packets
            .fetch_add(packet_count as u64, Ordering::Relaxed);
        Ok(output)
    }
}

impl PacketEngine for GpuPacketEngine {
    fn name(&self) -> &'static str {
        "gpu-packet"
    }

    fn process_batch(&self, packets: &[RawPacket]) -> Result<Vec<Option<RawPacket>>> {
        if packets.is_empty() {
            return Ok(Vec::new());
        }
        let waves = schedule_waves(packets, self.config.max_batch_size);
        let mut output = vec![None; packets.len()];
        for wave in waves {
            let inputs = wave.iter().map(|index| &packets[*index]).collect::<Vec<_>>();
            let wave_output = self.dispatch_wave(&inputs)?;
            for (index, packet) in wave.into_iter().zip(wave_output) {
                output[index] = packet;
            }
        }
        Ok(output)
    }

    fn metrics(&self) -> PacketEngineMetrics {
        PacketEngineMetrics {
            dispatches: self.dispatches.load(Ordering::Relaxed),
            packets: self.packets.load(Ordering::Relaxed),
        }
    }
}

pub(crate) fn validate_config(config: PacketEngineConfig) -> Result<()> {
    ensure!(config.max_batch_size > 0, "max batch size must be positive");
    ensure!(config.flow_capacity > 0, "flow capacity must be positive");
    ensure!(
        config.flow_capacity.is_power_of_two(),
        "flow capacity must be a power of two"
    );
    ensure!(config.flow_probe_limit > 0, "flow probe limit must be positive");
    ensure!(
        config.flow_probe_limit <= config.flow_capacity,
        "flow probe limit must not exceed flow capacity"
    );
    ensure!(
        config.max_batch_size <= u32::MAX as usize,
        "max batch size must fit u32"
    );
    ensure!(
        config.flow_capacity <= u32::MAX as usize,
        "flow capacity must fit u32"
    );
    let largest_response = PACKET_RESPONSES
        .iter()
        .map(|response| response.len())
        .max()
        .expect("packet response table is non-empty");
    ensure!(
        40 + largest_response <= MAX_RAW_PACKET_BYTES,
        "largest packet response does not fit the raw packet slot"
    );
    Ok(())
}

pub(crate) fn classify_http_request(payload: &[u8]) -> u32 {
    let Some(line_end) = payload.windows(2).position(|window| window == b"\r\n") else {
        return RESPONSE_BAD_REQUEST;
    };
    let line = &payload[..line_end];
    let mut parts = line.split(|byte| *byte == b' ');
    let Some(method) = parts.next() else {
        return RESPONSE_BAD_REQUEST;
    };
    let Some(target) = parts.next() else {
        return RESPONSE_BAD_REQUEST;
    };
    let Some(version) = parts.next() else {
        return RESPONSE_BAD_REQUEST;
    };
    if parts.next().is_some()
        || target.is_empty()
        || (version != b"HTTP/1.1" && version != b"HTTP/1.0")
    {
        return RESPONSE_BAD_REQUEST;
    }
    if method != b"GET" {
        return RESPONSE_METHOD_NOT_ALLOWED;
    }
    let path = target
        .split(|byte| *byte == b'?')
        .next()
        .unwrap_or(target);
    match path {
        b"/plaintext" => RESPONSE_PLAINTEXT,
        b"/health" => RESPONSE_HEALTH,
        _ => RESPONSE_NOT_FOUND,
    }
}

pub(crate) fn packet_response(response_id: u32) -> &'static [u8] {
    PACKET_RESPONSES
        .get(response_id as usize)
        .copied()
        .unwrap_or(PACKET_RESPONSES[RESPONSE_BAD_REQUEST as usize])
}

fn schedule_waves(packets: &[RawPacket], max_batch_size: usize) -> Vec<Vec<usize>> {
    let keys = packets
        .iter()
        .map(|packet| parse_ipv4_tcp(packet).map(|tcp| tcp.key))
        .collect::<Vec<_>>();
    let mut pending = (0..packets.len()).collect::<Vec<_>>();
    let mut waves = Vec::new();

    while !pending.is_empty() {
        let mut seen = HashSet::new();
        let mut wave = Vec::with_capacity(max_batch_size.min(pending.len()));
        let mut next = Vec::new();
        for index in pending {
            if wave.len() == max_batch_size {
                next.push(index);
                continue;
            }
            if let Some(key) = keys[index]
                && !seen.insert(key)
            {
                next.push(index);
                continue;
            }
            wave.push(index);
        }
        waves.push(wave);
        pending = next;
    }
    waves
}

fn packet_shader_source() -> Result<String> {
    let mut response_bytes = Vec::new();
    let mut response_offsets = Vec::with_capacity(PACKET_RESPONSES.len());
    let mut response_lengths = Vec::with_capacity(PACKET_RESPONSES.len());
    for response in PACKET_RESPONSES {
        response_offsets.push(response_bytes.len() as u32);
        response_lengths.push(response.len() as u32);
        response_bytes.extend_from_slice(response);
    }
    let words = pack_bytes(&response_bytes);
    let mut source = String::new();
    writeln!(source, "const FLOW_WORDS: u32 = {FLOW_WORDS}u;")?;
    writeln!(
        source,
        "const RESPONSE_COUNT: u32 = {}u;",
        PACKET_RESPONSES.len()
    )?;
    writeln!(source, "const RESPONSE_PLAINTEXT: u32 = {RESPONSE_PLAINTEXT}u;")?;
    writeln!(source, "const RESPONSE_HEALTH: u32 = {RESPONSE_HEALTH}u;")?;
    writeln!(
        source,
        "const RESPONSE_BAD_REQUEST: u32 = {RESPONSE_BAD_REQUEST}u;"
    )?;
    writeln!(source, "const RESPONSE_NOT_FOUND: u32 = {RESPONSE_NOT_FOUND}u;")?;
    writeln!(
        source,
        "const RESPONSE_METHOD_NOT_ALLOWED: u32 = {RESPONSE_METHOD_NOT_ALLOWED}u;"
    )?;
    write_u32_array(&mut source, "RESPONSE_OFFSETS", &response_offsets)?;
    write_u32_array(&mut source, "RESPONSE_LENGTHS", &response_lengths)?;
    writeln!(
        source,
        "const RESPONSE_WORDS: array<u32, {}> = array<u32, {}>(",
        words.len(),
        words.len()
    )?;
    for chunk in words.chunks(8) {
        for word in chunk {
            write!(source, "0x{word:08x}u,")?;
        }
        source.push('\n');
    }
    source.push_str(");\n");
    source.push_str(include_str!("packet.wgsl"));
    Ok(source)
}

fn write_u32_array(source: &mut String, name: &str, values: &[u32]) -> Result<()> {
    writeln!(
        source,
        "const {name}: array<u32, {}> = array<u32, {}>(",
        values.len(),
        values.len()
    )?;
    for value in values {
        write!(source, "{value}u,")?;
    }
    source.push_str(");\n");
    Ok(())
}

fn storage_buffer(
    device: &wgpu::Device,
    label: &'static str,
    size: usize,
    readback: bool,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size as u64,
        usage: if readback {
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST
        } else {
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST
        },
        mapped_at_creation: false,
    })
}

fn storage_layout(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn buffer_entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

fn map_read(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    size: u64,
) -> Result<wgpu::BufferView> {
    let slice = buffer.slice(0..size);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device.poll(wgpu::PollType::wait_indefinitely())?;
    receiver
        .recv()
        .context("GPU readback callback disappeared")??;
    Ok(slice.get_mapped_range()?)
}

fn pack_bytes(bytes: &[u8]) -> Vec<u32> {
    let mut words = vec![0_u32; bytes.len().div_ceil(4)];
    for (index, byte) in bytes.iter().copied().enumerate() {
        words[index / 4] |= u32::from(byte) << ((index % 4) * 8);
    }
    words
}

fn pack_packet(bytes: &[u8], words: &mut [u32]) {
    words.fill(0);
    for (index, byte) in bytes.iter().copied().enumerate() {
        words[index / 4] |= u32::from(byte) << ((index % 4) * 8);
    }
}

fn unpack_packet(words: &[u32], len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| ((words[index / 4] >> ((index % 4) * 8)) & 0xff) as u8)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_word_packing_round_trips_unaligned_bytes() {
        let bytes = b"odd packet bytes";
        let mut words = [0_u32; PACKET_WORDS];
        pack_packet(bytes, &mut words);
        assert_eq!(unpack_packet(&words, bytes.len()), bytes);
    }

    #[test]
    fn packet_shader_parses_and_validates() {
        let source = packet_shader_source().expect("packet shader source builds");
        let module = naga::front::wgsl::parse_str(&source).expect("packet shader parses");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .expect("packet shader validates");
    }

    #[test]
    fn every_packet_response_has_an_honest_content_length() {
        for response in PACKET_RESPONSES {
            let separator = response
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .expect("HTTP response has header separator");
            let headers =
                std::str::from_utf8(&response[..separator]).expect("response headers are ASCII");
            let body = &response[separator + 4..];
            let content_length = headers
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .expect("response has content length")
                .parse::<usize>()
                .expect("content length is numeric");
            assert_eq!(content_length, body.len());
        }
    }

    #[test]
    fn scheduler_keeps_same_flow_in_order_across_dispatches() {
        let key = FlowKey {
            src_ip: 1,
            dst_ip: 2,
            src_port: 3,
            dst_port: 4,
        };
        let other = FlowKey {
            src_port: 5,
            ..key
        };
        let packets = [key, key, other]
            .map(|key| {
                build_ipv4_tcp_packet(TcpPacketSpec {
                    key,
                    seq: 1,
                    ack: 0,
                    flags: TCP_SYN,
                    payload: &[],
                })
                .expect("packet builds")
            })
            .to_vec();
        let waves = schedule_waves(&packets, 8);

        assert_eq!(waves, vec![vec![0, 2], vec![1]]);
    }

    #[test]
    fn CPU_request_classifier_matches_packet_routes() {
        assert_eq!(
            classify_http_request(b"GET /plaintext?owl=yes HTTP/1.1\r\n\r\n"),
            RESPONSE_PLAINTEXT
        );
        assert_eq!(
            classify_http_request(b"GET /health HTTP/1.0\r\n\r\n"),
            RESPONSE_HEALTH
        );
        assert_eq!(
            classify_http_request(b"GET /missing HTTP/1.1\r\n\r\n"),
            RESPONSE_NOT_FOUND
        );
        assert_eq!(
            classify_http_request(b"POST /plaintext HTTP/1.1\r\n\r\n"),
            RESPONSE_METHOD_NOT_ALLOWED
        );
        assert_eq!(classify_http_request(b"garbage"), RESPONSE_BAD_REQUEST);
    }
}
