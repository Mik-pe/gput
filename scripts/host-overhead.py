#!/usr/bin/env python3
from pathlib import Path


def replace_once(source: str, old: str, new: str, label: str) -> str:
    if old not in source:
        raise SystemExit(f"expected block not found: {label}")
    return source.replace(old, new, 1)


packet_path = Path("src/packet/mod.rs")
packet = packet_path.read_text()
packet = replace_once(
    packet,
    """#[derive(Default)]
struct PacketScratch {
    metadata: Vec<PacketMeta>,
    words: Vec<u32>,
}
""",
    """#[derive(Default)]
struct PacketScratch {
    metadata: Vec<PacketMeta>,
    words: Vec<u32>,
    keys: Vec<Option<FlowKey>>,
    pending: Vec<usize>,
    deferred: Vec<usize>,
    wave: Vec<usize>,
    seen: HashSet<FlowKey>,
}
""",
    "packet scheduler scratch",
)
packet = replace_once(
    packet,
    """    fn dispatch_wave(
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
        scratch.words.resize(packet_count * INPUT_PACKET_WORDS, 0);
        for (packet_index, source_index) in wave.iter().copied().enumerate() {
            let packet = &packets[source_index];
            scratch.metadata[packet_index].len = packet.bytes.len() as u32;
            let base = packet_index * INPUT_PACKET_WORDS;
            pack_packet(
                &packet.bytes,
                &mut scratch.words[base..base + INPUT_PACKET_WORDS],
            );
        }
""",
    """    fn dispatch_wave(
        &self,
        packets: &[RawPacket],
        wave: &[usize],
        metadata: &mut Vec<PacketMeta>,
        words: &mut Vec<u32>,
    ) -> Result<Vec<Option<RawPacket>>> {
        if wave.is_empty() {
            return Ok(Vec::new());
        }
        let packet_count = wave.len();
        metadata.resize(packet_count, PacketMeta::zeroed());
        words.resize(packet_count * INPUT_PACKET_WORDS, 0);
        for (packet_index, source_index) in wave.iter().copied().enumerate() {
            let packet = &packets[source_index];
            metadata[packet_index].len = packet.bytes.len() as u32;
            let base = packet_index * INPUT_PACKET_WORDS;
            pack_packet(
                &packet.bytes,
                &mut words[base..base + INPUT_PACKET_WORDS],
            );
        }
""",
    "packet dispatch scratch split",
)
packet = packet.replace(
    "bytemuck::cast_slice(&scratch.metadata)",
    "bytemuck::cast_slice(metadata)",
    1,
)
packet = packet.replace(
    "bytemuck::cast_slice(&scratch.words)",
    "bytemuck::cast_slice(words)",
    1,
)
packet = replace_once(
    packet,
    """        let waves = schedule_waves(packets, self.config.max_batch_size);
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
        Ok(output)
""",
    """        let mut output = vec![None; packets.len()];
        let mut scratch = self
            .scratch
            .lock()
            .map_err(|_| anyhow::anyhow!("GPU packet scratch buffer poisoned"))?;
        scratch.keys.clear();
        scratch
            .keys
            .extend(packets.iter().map(|packet| parse_ipv4_tcp(packet).map(|tcp| tcp.key)));
        scratch.pending.clear();
        scratch.pending.extend(0..packets.len());

        while !scratch.pending.is_empty() {
            {
                let PacketScratch {
                    keys,
                    pending,
                    deferred,
                    wave,
                    seen,
                    ..
                } = &mut *scratch;
                fill_next_wave(
                    keys,
                    pending,
                    self.config.max_batch_size,
                    seen,
                    wave,
                    deferred,
                );
            }

            let wave_output = {
                let PacketScratch {
                    metadata,
                    words,
                    wave,
                    ..
                } = &mut *scratch;
                self.dispatch_wave(packets, wave, metadata, words)?
            };
            for (&index, packet) in scratch.wave.iter().zip(wave_output) {
                output[index] = packet;
            }
            std::mem::swap(&mut scratch.pending, &mut scratch.deferred);
        }
        Ok(output)
""",
    "allocation-free packet scheduling",
)
packet = replace_once(
    packet,
    """fn schedule_waves(packets: &[RawPacket], max_batch_size: usize) -> Vec<Vec<usize>> {
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
""",
    """fn fill_next_wave(
    keys: &[Option<FlowKey>],
    pending: &[usize],
    max_batch_size: usize,
    seen: &mut HashSet<FlowKey>,
    wave: &mut Vec<usize>,
    deferred: &mut Vec<usize>,
) {
    seen.clear();
    wave.clear();
    deferred.clear();

    for &index in pending {
        if wave.len() == max_batch_size {
            deferred.push(index);
            continue;
        }
        if let Some(key) = keys[index]
            && !seen.insert(key)
        {
            deferred.push(index);
            continue;
        }
        wave.push(index);
    }
}
""",
    "reusable wave scheduler",
)
packet = replace_once(
    packet,
    """        let waves = schedule_waves(&packets, 8);

        assert_eq!(waves, vec![vec![0, 2], vec![1]]);
""",
    """        let keys = packets
            .iter()
            .map(|packet| parse_ipv4_tcp(packet).map(|tcp| tcp.key))
            .collect::<Vec<_>>();
        let mut seen = HashSet::new();
        let mut pending = vec![0, 1, 2];
        let mut deferred = Vec::new();
        let mut wave = Vec::new();

        fill_next_wave(
            &keys,
            &pending,
            8,
            &mut seen,
            &mut wave,
            &mut deferred,
        );
        assert_eq!(wave, vec![0, 2]);
        std::mem::swap(&mut pending, &mut deferred);
        fill_next_wave(
            &keys,
            &pending,
            8,
            &mut seen,
            &mut wave,
            &mut deferred,
        );
        assert_eq!(wave, vec![1]);
""",
    "scheduler unit test",
)
packet_path.write_text(packet)

http_path = Path("src/processor/gpu.rs")
http = http_path.read_text()
http = replace_once(
    http,
    """    response_meta_readback: wgpu::Buffer,
    output_readback: wgpu::Buffer,
    router_layout: GpuRouterLayout,
""",
    """    readback_buffer: wgpu::Buffer,
    router_layout: GpuRouterLayout,
""",
    "HTTP combined readback field",
)
http = replace_once(
    http,
    """        let shader_assets = build_shader_assets(SHADER_BODY, router.as_ref())?;
        let request_stride_words = words_for_bytes(limits.max_request_bytes)?;
        let response_stride_words = words_for_bytes(limits.response_slot_bytes)?;
""",
    """        router.validate_gpu_response_slot(limits.response_slot_bytes)?;
        let shader_assets = build_shader_assets(SHADER_BODY, router.as_ref())?;
        let request_stride_words = words_for_bytes(limits.max_request_bytes)?;
        let configured_response_stride_words = words_for_bytes(limits.response_slot_bytes)?;
        let response_stride_words = words_for_bytes(router.max_gpu_response_bytes())?;
""",
    "router-sized HTTP response slots",
)
http = replace_once(
    http,
    """        let output_bytes = checked_buffer_bytes(
            "response output",
            limits.max_batch_size,
            response_stride_words * size_of::<u32>(),
        )?;
        let string_meta_bytes = checked_buffer_bytes(
""",
    """        let output_bytes = checked_buffer_bytes(
            "response output",
            limits.max_batch_size,
            response_stride_words * size_of::<u32>(),
        )?;
        let readback_bytes = response_meta_bytes
            .checked_add(output_bytes)
            .context("combined HTTP readback size overflow")?;
        let string_meta_bytes = checked_buffer_bytes(
""",
    "HTTP combined readback size",
)
http = replace_once(
    http,
    """        let response_meta_readback = create_buffer(
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
""",
    """        let readback_buffer = create_buffer(
            &device,
            "gput-combined-readback",
            readback_bytes,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
""",
    "HTTP readback buffer creation",
)
http = replace_once(
    http,
    """            response_slot_bytes = response_stride_words * size_of::<u32>(),
            max_router_response_bytes = router.max_gpu_response_bytes(),
""",
    """            configured_response_slot_bytes = configured_response_stride_words * size_of::<u32>(),
            response_slot_bytes = response_stride_words * size_of::<u32>(),
            max_router_response_bytes = router.max_gpu_response_bytes(),
""",
    "HTTP response slot telemetry",
)
http = replace_once(
    http,
    """            _router_words_buffer: router_words_buffer,
            response_meta_readback,
            output_readback,
            router_layout,
""",
    """            _router_words_buffer: router_words_buffer,
            readback_buffer,
            router_layout,
""",
    "HTTP readback initialization",
)
http = replace_once(
    http,
    """        encoder.copy_buffer_to_buffer(
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
""",
    """        encoder.copy_buffer_to_buffer(
            &self.response_meta_buffer,
            0,
            &self.readback_buffer,
            0,
            response_meta_copy_bytes,
        );
        encoder.copy_buffer_to_buffer(
            &self.output_buffer,
            0,
            &self.readback_buffer,
            response_meta_copy_bytes,
            output_copy_bytes,
        );
        self.queue.submit([encoder.finish()]);

        self.read_responses(request_count, response_meta_copy_bytes, output_copy_bytes)
""",
    "HTTP combined readback copies",
)
read_start = http.index("    fn read_responses(")
read_end = http.index("\n}\n\nimpl Processor", read_start)
new_read = """    fn read_responses(
        &self,
        request_count: usize,
        response_meta_copy_bytes: u64,
        output_copy_bytes: u64,
    ) -> Result<Vec<Vec<u8>>> {
        let readback_bytes = response_meta_copy_bytes
            .checked_add(output_copy_bytes)
            .context("HTTP readback copy size overflow")?;
        let slice = self.readback_buffer.slice(0..readback_bytes);
        let (sender, receiver) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .context("waiting for GPU batch completion failed")?;
        receiver
            .recv()
            .context("HTTP readback callback disappeared")??;

        let data = slice
            .get_mapped_range()
            .context("reading mapped HTTP responses failed")?;
        let decode_result = (|| -> Result<Vec<Vec<u8>>> {
            let (meta_data, output_data) = data.split_at(response_meta_copy_bytes as usize);
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

        drop(data);
        self.readback_buffer.unmap();
        decode_result
    }
"""
http = http[:read_start] + new_read + http[read_end:]
http = replace_once(
    http,
    """    fn packs_bytes_little_endian_for_wgsl_extraction() {
""",
    """    fn router_response_slots_ignore_unused_configured_padding() {
        let router = builtin_router().compile().expect("built-in router compiles");
        let exact = words_for_bytes(router.max_gpu_response_bytes()).expect("exact stride");
        let configured = words_for_bytes(512).expect("configured stride");

        assert!(exact <= configured);
        assert_eq!(exact * size_of::<u32>(), router.max_gpu_response_bytes().next_multiple_of(4));
    }

    #[test]
    fn packs_bytes_little_endian_for_wgsl_extraction() {
""",
    "HTTP exact response slot test",
)
http_path.write_text(http)
