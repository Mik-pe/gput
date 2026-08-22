#!/usr/bin/env python3
from pathlib import Path


def replace_once(source: str, old: str, new: str, label: str) -> str:
    if old not in source:
        raise SystemExit(f"expected block not found: {label}")
    return source.replace(old, new, 1)


packet_path = Path("src/packet/mod.rs")
source = packet_path.read_text()
source = replace_once(
    source,
    '''    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
''',
    '''    pub fn copy_from_slice(bytes: &[u8]) -> Result<Self> {
        Self::new(bytes.to_vec())
    }

    pub fn replace_from_slice(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(!bytes.is_empty(), "packet must not be empty");
        ensure!(
            bytes.len() <= MAX_RAW_PACKET_BYTES,
            "packet is {} bytes; maximum is {MAX_RAW_PACKET_BYTES}",
            bytes.len()
        );
        self.bytes.clear();
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
''',
    "reusable raw packet API",
)
source = replace_once(
    source,
    '''pub trait PacketEngine {
    fn name(&self) -> &'static str;
    fn process_batch_into(
''',
    '''/// Receives packet bytes while they are still owned by the engine.
///
/// The slice is valid only for the duration of `packet`. Transports can use
/// this to send mapped GPU output without first building an intermediate
/// `RawPacket` for every response.
pub trait PacketSink {
    fn packet(&mut self, source_index: usize, packet: Option<&[u8]>) -> Result<()>;
}

impl<F> PacketSink for F
where
    F: FnMut(usize, Option<&[u8]>) -> Result<()>,
{
    fn packet(&mut self, source_index: usize, packet: Option<&[u8]>) -> Result<()> {
        self(source_index, packet)
    }
}

pub trait PacketEngine {
    fn name(&self) -> &'static str;
    fn process_batch_into(
''',
    "packet sink trait",
)
source = replace_once(
    source,
    '''    fn process_batch(&self, packets: &[RawPacket]) -> Result<Vec<Option<RawPacket>>> {
        let mut output = Vec::new();
        self.process_batch_into(packets, &mut output)?;
        Ok(output)
    }

    fn metrics(&self) -> PacketEngineMetrics {
''',
    '''    fn process_batch(&self, packets: &[RawPacket]) -> Result<Vec<Option<RawPacket>>> {
        let mut output = Vec::new();
        self.process_batch_into(packets, &mut output)?;
        Ok(output)
    }

    fn process_batch_to(
        &self,
        packets: &[RawPacket],
        sink: &mut dyn PacketSink,
    ) -> Result<()> {
        let mut output = Vec::new();
        self.process_batch_into(packets, &mut output)?;
        for (source_index, packet) in output.iter().enumerate() {
            sink.packet(source_index, packet.as_ref().map(RawPacket::as_bytes))?;
        }
        Ok(())
    }

    fn metrics(&self) -> PacketEngineMetrics {
''',
    "default borrowed output adapter",
)
source = replace_once(
    source,
    '''    fn dispatch_wave(
        &self,
        packets: &[RawPacket],
        wave: &[usize],
        metadata: &mut Vec<PacketMeta>,
        output: &mut [Option<RawPacket>],
    ) -> Result<()> {
''',
    '''    fn dispatch_wave(
        &self,
        packets: &[RawPacket],
        wave: &[usize],
        metadata: &mut Vec<PacketMeta>,
        sink: &mut dyn PacketSink,
    ) -> Result<()> {
''',
    "borrowed dispatch sink",
)
source = source.replace(
    '''            let result = decode_wave_outputs(
                output_meta,
                output_words,
                wave,
                self.output_stride_words,
                output,
            );
''',
    '''            let result = emit_wave_outputs(
                output_meta,
                output_words,
                wave,
                self.output_stride_words,
                sink,
            );
''',
)
if "decode_wave_outputs(" in source:
    raise SystemExit("not every GPU decode call was converted to borrowed output")
source = replace_once(
    source,
    '''    fn process_batch_into(
        &self,
        packets: &[RawPacket],
        output: &mut Vec<Option<RawPacket>>,
    ) -> Result<()> {
        if packets.is_empty() {
            output.clear();
            return Ok(());
        }
        let schedule_started = Instant::now();
        let mut schedule_elapsed = Duration::ZERO;
        output.truncate(packets.len());
        output.resize_with(packets.len(), || None);
''',
    '''    fn process_batch_into(
        &self,
        packets: &[RawPacket],
        output: &mut Vec<Option<RawPacket>>,
    ) -> Result<()> {
        output.truncate(packets.len());
        output.resize_with(packets.len(), || None);
        let mut sink = |source_index: usize, packet: Option<&[u8]>| -> Result<()> {
            let slot = &mut output[source_index];
            match packet {
                Some(packet) => assign_packet_bytes(slot, packet),
                None => {
                    *slot = None;
                    Ok(())
                }
            }
        };
        self.process_batch_to(packets, &mut sink)
    }

    fn process_batch_to(
        &self,
        packets: &[RawPacket],
        sink: &mut dyn PacketSink,
    ) -> Result<()> {
        if packets.is_empty() {
            return Ok(());
        }
        let schedule_started = Instant::now();
        let mut schedule_elapsed = Duration::ZERO;
''',
    "GPU borrowed output implementation",
)
source = source.replace(
    "self.dispatch_wave(packets, wave, metadata, output)?;",
    "self.dispatch_wave(packets, wave, metadata, sink)?;",
)
source = replace_once(
    source,
    '''fn assign_packet_words(slot: &mut Option<RawPacket>, words: &[u32], len: usize) -> Result<()> {
    ensure!(
        len > 0 && len <= MAX_RAW_PACKET_BYTES,
        "packet output length {len} is outside the raw packet bounds"
    );
    let source: &[u8] = bytemuck::cast_slice(words);
    if let Some(packet) = slot {
        packet.bytes.clear();
        packet.bytes.extend_from_slice(&source[..len]);
    } else {
        *slot = Some(RawPacket::new(source[..len].to_vec())?);
    }
    Ok(())
}

fn decode_wave_outputs(
    metadata: &[u32],
    words: &[u32],
    wave: &[usize],
    output_stride_words: usize,
    output: &mut [Option<RawPacket>],
) -> Result<()> {
    for (index, (&len, &source_index)) in metadata.iter().zip(wave.iter()).enumerate() {
        let len = len as usize;
        if len == 0 {
            output[source_index] = None;
            continue;
        }
        ensure!(
            len <= output_stride_words * std::mem::size_of::<u32>(),
            "GPU produced packet of {len} bytes outside the tight response slot"
        );
        let base = index * output_stride_words;
        let packet_words = &words[base..base + output_stride_words];
        assign_packet_words(&mut output[source_index], packet_words, len)?;
    }
    Ok(())
}
''',
    '''fn assign_packet_bytes(slot: &mut Option<RawPacket>, bytes: &[u8]) -> Result<()> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_RAW_PACKET_BYTES,
        "packet output length {} is outside the raw packet bounds",
        bytes.len()
    );
    if let Some(packet) = slot {
        packet.replace_from_slice(bytes)?;
    } else {
        *slot = Some(RawPacket::copy_from_slice(bytes)?);
    }
    Ok(())
}

fn emit_wave_outputs(
    metadata: &[u32],
    words: &[u32],
    wave: &[usize],
    output_stride_words: usize,
    sink: &mut dyn PacketSink,
) -> Result<()> {
    let bytes: &[u8] = bytemuck::cast_slice(words);
    let output_stride_bytes = output_stride_words * std::mem::size_of::<u32>();
    for (index, (&len, &source_index)) in metadata.iter().zip(wave.iter()).enumerate() {
        let len = len as usize;
        if len == 0 {
            sink.packet(source_index, None)?;
            continue;
        }
        ensure!(
            len <= output_stride_bytes,
            "GPU produced packet of {len} bytes outside the tight response slot"
        );
        let base = index * output_stride_bytes;
        sink.packet(source_index, Some(&bytes[base..base + len]))?;
    }
    Ok(())
}
''',
    "borrowed output decoder",
)
source = replace_once(
    source,
    '''    #[test]
    fn packet_word_packing_round_trips_unaligned_bytes() {
''',
    '''    #[test]
    fn raw_packet_storage_can_be_reused_without_changing_its_contract() {
        let mut packet = RawPacket::copy_from_slice(b"first").expect("packet builds");
        packet
            .replace_from_slice(b"second packet")
            .expect("packet is replaced");
        assert_eq!(packet.as_bytes(), b"second packet");
    }

    #[test]
    fn packet_word_packing_round_trips_unaligned_bytes() {
''',
    "raw packet reuse test",
)
packet_path.write_text(source)

packetd_path = Path("src/bin/gput-packetd.rs")
source = packetd_path.read_text()
source = replace_once(
    source,
    '''    let mut batch = Vec::with_capacity(cli.batch_capacity);
    let mut responses = Vec::with_capacity(cli.batch_capacity);
''',
    '''    let mut batch = Vec::with_capacity(cli.batch_capacity);
    let mut spare_packets = Vec::with_capacity(cli.batch_capacity);
''',
    "packetd response staging removal",
)
source = replace_once(
    source,
    '''    let mut peak_batch = 0_usize;

    loop {
        batch.clear();
''',
    '''    let mut peak_batch = 0_usize;
    let mut receive_allocations = 0_u64;
    let mut receive_reuses = 0_u64;

    loop {
        spare_packets.extend(batch.drain(..));
''',
    "packetd packet pool initialization",
)
source = replace_once(
    source,
    '''        batch.push(RawPacket::new(buffer[..first_len].to_vec())?);
''',
    '''        if push_received_packet(
            &mut batch,
            &mut spare_packets,
            &buffer[..first_len],
        )? {
            receive_reuses = receive_reuses.saturating_add(1);
        } else {
            receive_allocations = receive_allocations.saturating_add(1);
        }
''',
    "first pooled TUN packet",
)
source = replace_once(
    source,
    '''                    batch.push(RawPacket::new(buffer[..packet_len].to_vec())?);
''',
    '''                    if push_received_packet(
                        &mut batch,
                        &mut spare_packets,
                        &buffer[..packet_len],
                    )? {
                        receive_reuses = receive_reuses.saturating_add(1);
                    } else {
                        receive_allocations = receive_allocations.saturating_add(1);
                    }
''',
    "subsequent pooled TUN packet",
)
source = replace_once(
    source,
    '''        selected.engine.process_batch_into(&batch, &mut responses)?;
        for response in responses.iter().flatten() {
            let sent = device
                .send(response.as_bytes())
                .context("failed to inject engine response into TUN")?;
            ensure!(
                sent == response.as_bytes().len(),
                "TUN accepted {sent} of {} response bytes",
                response.as_bytes().len()
            );
            packets_out = packets_out.saturating_add(1);
        }
''',
    '''        let mut send_response = |_source_index: usize, response: Option<&[u8]>| -> Result<()> {
            let Some(response) = response else {
                return Ok(());
            };
            let sent = device
                .send(response)
                .context("failed to inject borrowed engine response into TUN")?;
            ensure!(
                sent == response.len(),
                "TUN accepted {sent} of {} response bytes",
                response.len()
            );
            packets_out = packets_out.saturating_add(1);
            Ok(())
        };
        selected
            .engine
            .process_batch_to(&batch, &mut send_response)?;
''',
    "borrowed TUN output",
)
source = replace_once(
    source,
    '''                "gput packet stats: engine={engine_name} in={packets_in} out={packets_out} pps={:.0} batches={batches} avg_batch={average_batch:.2} peak_batch={peak_batch} engine_dispatches={} packets/dispatch={packets_per_dispatch:.2}",
                packets_in as f64 / elapsed.max(f64::EPSILON),
                metrics.dispatches,
''',
    '''                "gput packet stats: engine={engine_name} in={packets_in} out={packets_out} pps={:.0} batches={batches} avg_batch={average_batch:.2} peak_batch={peak_batch} rx_allocations={receive_allocations} rx_reuses={receive_reuses} engine_dispatches={} packets/dispatch={packets_per_dispatch:.2}",
                packets_in as f64 / elapsed.max(f64::EPSILON),
                metrics.dispatches,
''',
    "packetd pool telemetry",
)
source = replace_once(
    source,
    '''fn select_engine(
''',
    '''fn push_received_packet(
    batch: &mut Vec<RawPacket>,
    spare_packets: &mut Vec<RawPacket>,
    bytes: &[u8],
) -> Result<bool> {
    if let Some(mut packet) = spare_packets.pop() {
        packet.replace_from_slice(bytes)?;
        batch.push(packet);
        Ok(true)
    } else {
        batch.push(RawPacket::copy_from_slice(bytes)?);
        Ok(false)
    }
}

fn select_engine(
''',
    "packetd pool helper",
)
packetd_path.write_text(source)

readme_path = Path("README.md")
source = readme_path.read_text()
source = replace_once(
    source,
    '''- batched TUN ingress with packet/dispatch telemetry
''',
    '''- batched TUN ingress with packet/dispatch telemetry
- pooled TUN receive packets and borrowed mapped-output delivery, avoiding two host-side allocation/copy detours
''',
    "README zero-copy transport bullet",
)
readme_path.write_text(source)

docs_path = Path("docs/GPU_NETWORKING.md")
source = docs_path.read_text()
source = replace_once(
    source,
    '''Input packets are packed directly into `wgpu` upload staging memory. The engine does not build a second contiguous host `Vec` merely to copy it into the staging allocation one moment later.
''',
    '''Input packets are packed directly into `wgpu` upload staging memory. The engine does not build a second contiguous host `Vec` merely to copy it into the staging allocation one moment later.

Packet outputs also have a borrowed delivery API. A transport may consume each generated packet while the GPU output remains mapped, so `gput-packetd` writes response bytes directly to TUN instead of first copying every response into an intermediate `RawPacket`. Incoming TUN packet allocations are pooled and reused across batches. The kernel boundary still copies; the avoidable furniture-moving inside gput no longer does.
''',
    "GPU networking borrowed output docs",
)
docs_path.write_text(source)
