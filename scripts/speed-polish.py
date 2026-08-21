#!/usr/bin/env python3
from pathlib import Path
from textwrap import dedent


def read(path: str) -> str:
    return Path(path).read_text()


def write(path: str, source: str) -> None:
    Path(path).write_text(source)


def replace(source: str, old: str, new: str, label: str) -> str:
    old = dedent(old)
    new = dedent(new)
    if old not in source:
        raise SystemExit(f"expected block not found: {label}")
    return source.replace(old, new, 1)


packet_path = "src/packet/mod.rs"
source = read(packet_path)
source = replace(
    source,
    """
    mod cpu;
    """,
    """
    #[cfg(target_endian = "big")]
    compile_error!("gput packet packing currently requires a little-endian host");

    mod cpu;
    """,
    "packet endian guard",
)
source = replace(
    source,
    """
        sync::atomic::{AtomicU64, Ordering},
    """,
    """
        sync::{
            Mutex,
            atomic::{AtomicU64, Ordering},
        },
    """,
    "packet scratch mutex import",
)
source = replace(
    source,
    """
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RawPacket {
    """,
    """
    #[derive(Default)]
    struct PacketScratch {
        metadata: Vec<PacketMeta>,
        words: Vec<u32>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RawPacket {
    """,
    "packet scratch type",
)
source = replace(
    source,
    """
        packets: AtomicU64,
    }
    """,
    """
        packets: AtomicU64,
        scratch: Mutex<PacketScratch>,
    }
    """,
    "packet scratch field",
)
source = replace(
    source,
    """
                packets: AtomicU64::new(0),
            })
    """,
    """
                packets: AtomicU64::new(0),
                scratch: Mutex::new(PacketScratch::default()),
            })
    """,
    "packet scratch initialization",
)
source = replace(
    source,
    """
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
    """,
    """
        fn dispatch_wave(
            &self,
            packets: &[RawPacket],
            wave: &[usize],
            scratch: &mut PacketScratch,
        ) -> Result<Vec<Option<RawPacket>>> {
            if wave.is_empty() {
                return Ok(Vec::new());
            }
            let packet_count = wave.len();
            scratch.metadata.resize(packet_count, PacketMeta::zeroed());
            scratch.words.resize(packet_count * PACKET_WORDS, 0);
            for (packet_index, source_index) in wave.iter().copied().enumerate() {
                let packet = &packets[source_index];
                scratch.metadata[packet_index].len = packet.bytes.len() as u32;
                let base = packet_index * PACKET_WORDS;
                pack_packet(
                    &packet.bytes,
                    &mut scratch.words[base..base + PACKET_WORDS],
                );
            }
    """,
    "packet staging allocations",
)
source = source.replace(
    "bytemuck::cast_slice(&metadata)",
    "bytemuck::cast_slice(&scratch.metadata)",
    1,
)
source = source.replace(
    "bytemuck::cast_slice(&words)",
    "bytemuck::cast_slice(&scratch.words)",
    1,
)
source = replace(
    source,
    """
            let meta_bytes = map_read(&self.device, &self.readback_meta, meta_copy_bytes)?;
            let word_bytes = map_read(&self.device, &self.readback_words, word_copy_bytes)?;
    """,
    """
            let (meta_bytes, word_bytes) = map_two_reads(
                &self.device,
                &self.readback_meta,
                meta_copy_bytes,
                &self.readback_words,
                word_copy_bytes,
            )?;
    """,
    "packet readback call",
)
source = replace(
    source,
    """
            let waves = schedule_waves(packets, self.config.max_batch_size);
            let mut output = vec![None; packets.len()];
            for wave in waves {
                let inputs = wave
                    .iter()
                    .map(|index| &packets[*index])
                    .collect::<Vec<_>>();
                let wave_output = self.dispatch_wave(&inputs)?;
                for (index, packet) in wave.into_iter().zip(wave_output) {
                    output[index] = packet;
                }
            }
    """,
    """
            let waves = schedule_waves(packets, self.config.max_batch_size);
            let mut output = vec![None; packets.len()];
            let mut scratch = self
                .scratch
                .lock()
                .map_err(|_| anyhow::anyhow!("GPU packet scratch buffer poisoned"))?;
            for wave in waves {
                let wave_output = self.dispatch_wave(packets, &wave, &mut scratch)?;
                for (index, packet) in wave.into_iter().zip(wave_output) {
                    output[index] = packet;
                }
            }
    """,
    "packet dispatch wave allocation",
)
source = replace(
    source,
    """
    fn map_read(device: &wgpu::Device, buffer: &wgpu::Buffer, size: u64) -> Result<wgpu::BufferView> {
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
    """,
    """
    fn map_two_reads(
        device: &wgpu::Device,
        first_buffer: &wgpu::Buffer,
        first_size: u64,
        second_buffer: &wgpu::Buffer,
        second_size: u64,
    ) -> Result<(wgpu::BufferView, wgpu::BufferView)> {
        let first_slice = first_buffer.slice(0..first_size);
        let second_slice = second_buffer.slice(0..second_size);
        let (first_sender, first_receiver) = std::sync::mpsc::sync_channel(1);
        let (second_sender, second_receiver) = std::sync::mpsc::sync_channel(1);
        first_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = first_sender.send(result);
        });
        second_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = second_sender.send(result);
        });
        device.poll(wgpu::PollType::wait_indefinitely())?;

        let first_result = first_receiver
            .recv()
            .context("first GPU readback callback disappeared")?;
        let second_result = second_receiver
            .recv()
            .context("second GPU readback callback disappeared")?;
        if let Err(error) = first_result {
            if second_result.is_ok() {
                second_buffer.unmap();
            }
            anyhow::bail!("mapping first GPU readback failed: {error}");
        }
        if let Err(error) = second_result {
            first_buffer.unmap();
            anyhow::bail!("mapping second GPU readback failed: {error}");
        }

        let first = first_slice.get_mapped_range()?;
        let second = match second_slice.get_mapped_range() {
            Ok(second) => second,
            Err(error) => {
                drop(first);
                first_buffer.unmap();
                second_buffer.unmap();
                return Err(error.into());
            }
        };
        Ok((first, second))
    }
    """,
    "parallel packet readbacks",
)
source = replace(
    source,
    """
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
    """,
    """
    fn pack_packet(bytes: &[u8], words: &mut [u32]) {
        let destination: &mut [u8] = bytemuck::cast_slice_mut(words);
        destination[..bytes.len()].copy_from_slice(bytes);
    }

    fn unpack_packet(words: &[u32], len: usize) -> Vec<u8> {
        let bytes: &[u8] = bytemuck::cast_slice(words);
        bytes[..len].to_vec()
    }
    """,
    "packet memcpy packing",
)
write(packet_path, source)

processor_path = "src/processor/gpu.rs"
source = read(processor_path)
source = replace(
    source,
    """
        response_stride_words: usize,
    }
    """,
    """
        response_stride_words: usize,
        request_meta_scratch: Vec<RequestMeta>,
        input_words_scratch: Vec<u32>,
    }
    """,
    "HTTP processor scratch fields",
)
source = replace(
    source,
    """
                response_stride_words,
            })
    """,
    """
                response_stride_words,
                request_meta_scratch: Vec::with_capacity(limits.max_batch_size),
                input_words_scratch: Vec::with_capacity(
                    limits.max_batch_size * request_stride_words,
                ),
            })
    """,
    "HTTP processor scratch initialization",
)
source = replace(
    source,
    """
            let request_count = requests.len();
            let mut input_words = vec![0_u32; request_count * self.request_stride_words];
            let mut request_meta = Vec::with_capacity(request_count);
    """,
    """
            let request_count = requests.len();
            self.input_words_scratch
                .resize(request_count * self.request_stride_words, 0);
            self.request_meta_scratch.clear();
    """,
    "HTTP processor staging allocations",
)
source = source.replace("&mut input_words[", "&mut self.input_words_scratch[", 1)
source = source.replace(
    "            request_meta.push(RequestMeta {",
    "            self.request_meta_scratch.push(RequestMeta {",
    1,
)
source = source.replace(
    "bytemuck::cast_slice(&request_meta)",
    "bytemuck::cast_slice(&self.request_meta_scratch)",
    1,
)
source = source.replace(
    "bytemuck::cast_slice(&input_words)",
    "bytemuck::cast_slice(&self.input_words_scratch)",
    1,
)
source = replace(
    source,
    """
    fn pack_request_words(request: &[u8], destination: &mut [u32]) {
        destination.fill(0);

        for (byte_index, byte) in request.iter().copied().enumerate() {
            let word_index = byte_index / 4;
            let shift = (byte_index % 4) * 8;
            destination[word_index] |= u32::from(byte) << shift;
        }
    }
    """,
    """
    fn pack_request_words(request: &[u8], destination: &mut [u32]) {
        let destination: &mut [u8] = bytemuck::cast_slice_mut(destination);
        destination[..request.len()].copy_from_slice(request);
    }
    """,
    "HTTP memcpy packing",
)
write(processor_path, source)

network_path = "src/network.rs"
source = read(network_path)
source = replace(
    source,
    """
                let remainder = pending.split_off(header_len);
                let request = std::mem::replace(pending, remainder);
                return Ok(Some(request));
    """,
    """
                let request = pending.drain(..header_len).collect::<Vec<_>>();
                return Ok(Some(request));
    """,
    "persistent connection buffer reuse",
)
write(network_path, source)

batcher_path = "src/batcher.rs"
source = read(batcher_path)
source = replace(
    source,
    """
    fn worker_loop(
        mut processor: Box<dyn Processor>,
        receiver: Receiver<Job>,
        config: BatcherConfig,
        metrics: Arc<BatcherMetrics>,
    ) {
        while let Ok(first) = receiver.recv() {
    """,
    """
    fn worker_loop(
        mut processor: Box<dyn Processor>,
        receiver: Receiver<Job>,
        config: BatcherConfig,
        metrics: Arc<BatcherMetrics>,
    ) {
        let mut jobs = Vec::with_capacity(config.max_batch_size);
        let mut requests = Vec::with_capacity(config.max_batch_size);

        while let Ok(first) = receiver.recv() {
    """,
    "batcher reusable vectors",
)
source = replace(
    source,
    """
            let mut jobs = Vec::with_capacity(config.max_batch_size);
            jobs.push(first);
    """,
    """
            jobs.clear();
            requests.clear();
            jobs.push(first);
    """,
    "batcher vector reset",
)
source = replace(
    source,
    """
            let result = {
                let requests = jobs
                    .iter()
                    .map(|job| job.request.as_slice())
                    .collect::<Vec<_>>();
                processor.process_batch(&requests)
            };
    """,
    """
            requests.extend(jobs.iter().map(|job| job.request.as_slice()));
            let result = processor.process_batch(&requests);
    """,
    "batcher request slice reuse",
)
source = source.replace(
    "for (job, response) in jobs.into_iter().zip(responses)",
    "for (job, response) in jobs.drain(..).zip(responses)",
    1,
)
source = source.replace(
    "fail_batch(jobs, message, &metrics);",
    "fail_batch(&mut jobs, message, &metrics);",
    1,
)
source = source.replace(
    "fail_batch(jobs, processing_error.to_string().into(), &metrics);",
    "fail_batch(&mut jobs, processing_error.to_string().into(), &metrics);",
    1,
)
source = source.replace(
    "fn fail_batch(jobs: Vec<Job>, message: Arc<str>, metrics: &BatcherMetrics) {",
    "fn fail_batch(jobs: &mut Vec<Job>, message: Arc<str>, metrics: &BatcherMetrics) {",
    1,
)
source = source.replace("    for job in jobs {", "    for job in jobs.drain(..) {", 1)
write(batcher_path, source)

cpu_path = "src/packet/cpu.rs"
source = read(cpu_path)
source = replace(
    source,
    """
        sync::{
            Mutex,
            atomic::{AtomicU64, Ordering},
        },
    """,
    """
        sync::{
            Mutex, OnceLock,
            atomic::{AtomicU64, Ordering},
        },
    """,
    "CPU response cache import",
)
source = source.replace("                &response,", "                response,", 1)
source = source.replace("        &response,", "        response,", 1)
source = replace(
    source,
    """
    fn cpu_response(response_id: u32) -> Vec<u8> {
        let mut response = packet_response(response_id).to_vec();
        if let Some(offset) = response
            .windows(b"gpu-packet".len())
            .position(|window| window == b"gpu-packet")
        {
            response[offset..offset + b"cpu-packet".len()].copy_from_slice(b"cpu-packet");
        }
        response
    }
    """,
    """
    fn cpu_response(response_id: u32) -> &'static [u8] {
        static RESPONSES: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
        let responses = RESPONSES.get_or_init(|| {
            (0..super::PACKET_RESPONSES.len())
                .map(|index| {
                    let mut response = packet_response(index as u32).to_vec();
                    if let Some(offset) = response
                        .windows(b"gpu-packet".len())
                        .position(|window| window == b"gpu-packet")
                    {
                        response[offset..offset + b"cpu-packet".len()]
                            .copy_from_slice(b"cpu-packet");
                    }
                    response
                })
                .collect()
        });
        responses
            .get(response_id as usize)
            .unwrap_or(&responses[super::RESPONSE_BAD_REQUEST as usize])
            .as_slice()
    }
    """,
    "CPU response cache",
)
write(cpu_path, source)

ci_path = ".github/workflows/ci.yml"
source = read(ci_path)
source = replace(
    source,
    """
                    run_check "GPU TCP retransmission, routing and collision proof" \\
                      env \\
    """,
    """
                    run_check "GPU doctor through Lavapipe" \\
                      env \\
                        WGPU_BACKEND=vulkan \\
                        VK_DRIVER_FILES="$lvp_icd" \\
                        VK_ICD_FILENAMES="$lvp_icd" \\
                        XDG_RUNTIME_DIR="$runtime_dir" \\
                        LIBGL_ALWAYS_SOFTWARE=true \\
                        GALLIUM_DRIVER=llvmpipe \\
                      target/debug/gput-doctor

                    run_check "GPU TCP retransmission, routing and collision proof" \\
                      env \\
    """,
    "doctor in permanent CI",
)
write(ci_path, source)
