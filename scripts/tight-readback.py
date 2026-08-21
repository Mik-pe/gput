#!/usr/bin/env python3
from pathlib import Path


def replace_once(source: str, old: str, new: str, label: str) -> str:
    if old not in source:
        raise SystemExit(f"expected block not found: {label}")
    return source.replace(old, new, 1)


packet_path = Path("src/packet/mod.rs")
source = packet_path.read_text()
source = source.replace("PACKET_WORDS", "INPUT_PACKET_WORDS")
source = replace_once(
    source,
    """struct EngineParams {
    packet_count: u32,
    packet_stride_words: u32,
    flow_capacity: u32,
    listen_port: u32,
    flow_probe_limit: u32,
    _padding: [u32; 3],
}
""",
    """struct EngineParams {
    packet_count: u32,
    input_stride_words: u32,
    output_stride_words: u32,
    flow_capacity: u32,
    listen_port: u32,
    flow_probe_limit: u32,
    _padding: [u32; 2],
}
""",
    "Rust packet engine parameters",
)
source = replace_once(
    source,
    """    readback_meta: wgpu::Buffer,
    readback_words: wgpu::Buffer,
    params: wgpu::Buffer,
    config: PacketEngineConfig,
    adapter_name: String,
""",
    """    readback: wgpu::Buffer,
    params: wgpu::Buffer,
    config: PacketEngineConfig,
    output_stride_words: usize,
    adapter_name: String,
""",
    "packet engine readback fields",
)
source = replace_once(
    source,
    """        let packet_meta_bytes = config
            .max_batch_size
            .checked_mul(std::mem::size_of::<PacketMeta>())
            .context("packet metadata buffer size overflow")?;
        let packet_words_bytes = config
            .max_batch_size
            .checked_mul(INPUT_PACKET_WORDS)
            .and_then(|words| words.checked_mul(std::mem::size_of::<u32>()))
            .context("packet word buffer size overflow")?;
        let flow_words = config
""",
    """        let packet_meta_bytes = config
            .max_batch_size
            .checked_mul(std::mem::size_of::<PacketMeta>())
            .context("packet metadata buffer size overflow")?;
        let input_words_bytes = config
            .max_batch_size
            .checked_mul(INPUT_PACKET_WORDS)
            .and_then(|words| words.checked_mul(std::mem::size_of::<u32>()))
            .context("packet input buffer size overflow")?;
        let output_stride_words = max_response_packet_bytes().div_ceil(std::mem::size_of::<u32>());
        let output_words_bytes = config
            .max_batch_size
            .checked_mul(output_stride_words)
            .and_then(|words| words.checked_mul(std::mem::size_of::<u32>()))
            .context("packet output buffer size overflow")?;
        let readback_bytes = packet_meta_bytes
            .checked_add(output_words_bytes)
            .context("combined packet readback size overflow")?;
        let flow_words = config
""",
    "packet buffer sizes",
)
source = replace_once(
    source,
    """        let input_meta = storage_buffer(&device, "packet input metadata", packet_meta_bytes, false);
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
""",
    """        let input_meta = storage_buffer(&device, "packet input metadata", packet_meta_bytes, false);
        let input_words = storage_buffer(&device, "packet input words", input_words_bytes, false);
        let output_meta =
            storage_buffer(&device, "packet output metadata", packet_meta_bytes, false);
        let output_words =
            storage_buffer(&device, "packet output words", output_words_bytes, false);
        let flow_state = storage_buffer(&device, "packet TCP flow state", flow_bytes, false);
        let readback = storage_buffer(
            &device,
            "combined packet readback",
            readback_bytes,
            true,
        );
""",
    "packet buffer creation",
)
source = replace_once(
    source,
    """            listen_port = config.listen_port,
            "using collision-safe GPU-native packet engine"
""",
    """            listen_port = config.listen_port,
            input_slot_bytes = INPUT_PACKET_WORDS * std::mem::size_of::<u32>(),
            output_slot_bytes = output_stride_words * std::mem::size_of::<u32>(),
            "using collision-safe GPU-native packet engine"
""",
    "packet engine startup telemetry",
)
source = replace_once(
    source,
    """            _flow_state: flow_state,
            readback_meta,
            readback_words,
            params,
            config,
            adapter_name: adapter_info.name,
""",
    """            _flow_state: flow_state,
            readback,
            params,
            config,
            output_stride_words,
            adapter_name: adapter_info.name,
""",
    "packet engine field initialization",
)
source = replace_once(
    source,
    """        let params = EngineParams {
            packet_count: packet_count as u32,
            packet_stride_words: INPUT_PACKET_WORDS as u32,
            flow_capacity: self.config.flow_capacity as u32,
            listen_port: self.config.listen_port.into(),
            flow_probe_limit: self.config.flow_probe_limit as u32,
            _padding: [0; 3],
        };
""",
    """        let params = EngineParams {
            packet_count: packet_count as u32,
            input_stride_words: INPUT_PACKET_WORDS as u32,
            output_stride_words: self.output_stride_words as u32,
            flow_capacity: self.config.flow_capacity as u32,
            listen_port: self.config.listen_port.into(),
            flow_probe_limit: self.config.flow_probe_limit as u32,
            _padding: [0; 2],
        };
""",
    "packet dispatch parameters",
)
source = replace_once(
    source,
    """        let meta_copy_bytes = (packet_count * std::mem::size_of::<PacketMeta>()) as u64;
        let word_copy_bytes =
            (packet_count * INPUT_PACKET_WORDS * std::mem::size_of::<u32>()) as u64;
""",
    """        let meta_copy_bytes = (packet_count * std::mem::size_of::<PacketMeta>()) as u64;
        let word_copy_bytes =
            (packet_count * self.output_stride_words * std::mem::size_of::<u32>()) as u64;
        let readback_copy_bytes = meta_copy_bytes + word_copy_bytes;
""",
    "packet copy sizes",
)
source = replace_once(
    source,
    """        encoder.copy_buffer_to_buffer(
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

        let (meta_bytes, word_bytes) = map_two_reads(
            &self.device,
            &self.readback_meta,
            meta_copy_bytes,
            &self.readback_words,
            word_copy_bytes,
        )?;
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
            let base = index * INPUT_PACKET_WORDS;
            output.push(Some(RawPacket::new(unpack_packet(
                &output_words[base..base + INPUT_PACKET_WORDS],
                len,
            ))?));
        }

        drop(word_bytes);
        drop(meta_bytes);
        self.readback_words.unmap();
        self.readback_meta.unmap();
""",
    """        encoder.copy_buffer_to_buffer(
            &self.output_meta,
            0,
            &self.readback,
            0,
            meta_copy_bytes,
        );
        encoder.copy_buffer_to_buffer(
            &self.output_words,
            0,
            &self.readback,
            meta_copy_bytes,
            word_copy_bytes,
        );
        self.queue.submit(Some(encoder.finish()));

        let readback = map_read(&self.device, &self.readback, readback_copy_bytes)?;
        let output = {
            let (meta_bytes, word_bytes) = readback.split_at(meta_copy_bytes as usize);
            let output_meta: &[PacketMeta] = bytemuck::cast_slice(meta_bytes);
            let output_words: &[u32] = bytemuck::cast_slice(word_bytes);
            let mut output = Vec::with_capacity(packet_count);

            for (index, meta) in output_meta.iter().enumerate() {
                let len = meta.len as usize;
                if len == 0 {
                    output.push(None);
                    continue;
                }
                ensure!(
                    len <= self.output_stride_words * std::mem::size_of::<u32>(),
                    "GPU produced packet of {len} bytes outside the tight response slot"
                );
                let base = index * self.output_stride_words;
                output.push(Some(RawPacket::new(unpack_packet(
                    &output_words[base..base + self.output_stride_words],
                    len,
                ))?));
            }
            output
        };

        drop(readback);
        self.readback.unmap();
""",
    "combined packet readback",
)
source = replace_once(
    source,
    """    let largest_response = PACKET_RESPONSES
        .iter()
        .map(|response| response.len())
        .max()
        .expect("packet response table is non-empty");
    ensure!(
        40 + largest_response <= MAX_RAW_PACKET_BYTES,
        "largest packet response does not fit the raw packet slot"
    );
""",
    """    ensure!(
        max_response_packet_bytes() <= MAX_RAW_PACKET_BYTES,
        "largest packet response does not fit the raw packet slot"
    );
""",
    "packet response validation",
)
source = replace_once(
    source,
    """pub(crate) fn packet_response(response_id: u32) -> &'static [u8] {
    PACKET_RESPONSES
        .get(response_id as usize)
        .copied()
        .unwrap_or(PACKET_RESPONSES[RESPONSE_BAD_REQUEST as usize])
}
""",
    """pub(crate) fn packet_response(response_id: u32) -> &'static [u8] {
    PACKET_RESPONSES
        .get(response_id as usize)
        .copied()
        .unwrap_or(PACKET_RESPONSES[RESPONSE_BAD_REQUEST as usize])
}

fn max_response_packet_bytes() -> usize {
    40 + PACKET_RESPONSES
        .iter()
        .map(|response| response.len())
        .max()
        .expect("packet response table is non-empty")
}
""",
    "maximum response packet size helper",
)
map_start = source.index("fn map_two_reads(")
map_end = source.index("\nfn pack_bytes", map_start)
source = source[:map_start] + """fn map_read(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    size: u64,
) -> Result<wgpu::BufferView> {
    let slice = buffer.slice(0..size);
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device.poll(wgpu::PollType::wait_indefinitely())?;
    receiver
        .recv()
        .context("GPU packet readback callback disappeared")??;
    Ok(slice.get_mapped_range()?)
}
""" + source[map_end:]
source = replace_once(
    source,
    """    fn packet_word_packing_round_trips_unaligned_bytes() {
        let bytes = b"odd packet bytes";
        let mut words = [0_u32; INPUT_PACKET_WORDS];
        pack_packet(bytes, &mut words);
        assert_eq!(unpack_packet(&words, bytes.len()), bytes);
    }
""",
    """    fn packet_word_packing_round_trips_unaligned_bytes() {
        let bytes = b"odd packet bytes";
        let mut words = [0_u32; INPUT_PACKET_WORDS];
        pack_packet(bytes, &mut words);
        assert_eq!(unpack_packet(&words, bytes.len()), bytes);
    }

    #[test]
    fn response_slots_are_tight_instead_of_mtu_sized_furniture_vans() {
        let response_bytes = max_response_packet_bytes();
        assert!(response_bytes < MAX_RAW_PACKET_BYTES / 2);
        assert_eq!(
            response_bytes.div_ceil(std::mem::size_of::<u32>()),
            56
        );
    }
""",
    "tight response slot test",
)
packet_path.write_text(source)

shader_path = Path("src/packet/packet.wgsl")
shader = shader_path.read_text()
shader = replace_once(
    shader,
    """struct EngineParams {
    packet_count: u32,
    packet_stride_words: u32,
    flow_capacity: u32,
    listen_port: u32,
    flow_probe_limit: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}
""",
    """struct EngineParams {
    packet_count: u32,
    input_stride_words: u32,
    output_stride_words: u32,
    flow_capacity: u32,
    listen_port: u32,
    flow_probe_limit: u32,
    pad0: u32,
    pad1: u32,
}
""",
    "WGSL packet engine parameters",
)
shader = shader.replace(
    "let base = packet_index * params.packet_stride_words;",
    "let base = packet_index * params.input_stride_words;",
    1,
)
shader = shader.replace(
    "let base = packet_index * params.packet_stride_words;",
    "let base = packet_index * params.output_stride_words;",
    1,
)
shader = shader.replace(
    "let base = packet_index * params.packet_stride_words;",
    "let base = packet_index * params.output_stride_words;",
    1,
)
if "params.packet_stride_words" in shader:
    raise SystemExit("legacy packet stride remained in WGSL")
shader_path.write_text(shader)
