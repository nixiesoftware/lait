//! Transport-stream and playlist conformance, checked from the bytes the
//! producer serves. Test-only: the walker here is what a strict player does.
//!
//! Two defects were found by hand-parsing a segment in Python — continuity
//! counters that restarted at every segment, and a negative PCR that wrapped
//! to the top of the 33-bit clock at the start of every part. Both are fixed
//! in the packager; this is what keeps them fixed. Nothing here decodes a
//! picture: it walks packets, PES headers and Annex-B start codes, which is
//! exactly the level both defects lived at.

use std::collections::BTreeMap;
use std::fmt;

use super::HlsSegment;

const TS_PACKET_BYTES: usize = 188;
const SYNC_BYTE: u8 = 0x47;
const PAT_PID: u16 = 0;
/// The 90 kHz clock's range: PTS, DTS and the PCR base are 33 bits.
const CLOCK_BITS: u64 = 1 << 33;
/// How close to the wrap a clock is taken for a wrapped negative rather
/// than a stream that has genuinely run 26 hours.
const WRAP_GUARD_90K: u64 = 90_000;
/// The slack between what EXTINF says and what the timestamps span: one
/// frame at a generous rate, in milliseconds.
const DURATION_SLACK_MS: u64 = 45;

const NAL_NON_IDR: u8 = 1;
const NAL_IDR: u8 = 5;
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;

/// What was found wrong, named so a test can assert the kind and a person
/// can read the detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Violation {
    pub(crate) kind: Kind,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A packet that is not 188 bytes or does not start with the sync byte.
    Packet,
    /// PAT or PMT is missing from a segment, or the PMT names no PCR PID.
    ProgramTables,
    /// A continuity counter that did not increment mod 16 on a payload
    /// packet, within a segment or across a seam that is not a discontinuity.
    Continuity,
    /// No PCR on the PCR PID.
    PcrMissing,
    /// A PCR of zero or in the wrap region — the negative-clock defect.
    PcrRange,
    /// A PCR that went backwards.
    PcrOrder,
    /// A PCR ahead of the DTS of the access unit it was written before.
    PcrAheadOfDts,
    /// A PES header the walker could not read, or one with marker bits
    /// missing from its timestamps.
    Pes,
    /// A decode timestamp that did not advance.
    TimestampOrder,
    /// A presentation timestamp behind its decode timestamp.
    PtsBeforeDts,
    /// The first video access unit of a segment is not an IDR led by SPS
    /// and PPS.
    KeyFrame,
    /// EXTINF disagrees with the timestamps by more than a frame.
    Duration,
    /// The playlist's fixed header lines.
    Playlist,
    /// A playlist reload that contradicts the one before it.
    Reload,
}

impl fmt::Display for Violation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.detail)
    }
}

fn violation(kind: Kind, detail: impl Into<String>) -> Violation {
    Violation {
        kind,
        detail: detail.into(),
    }
}

// ---------------------------------------------------------------------------
// Transport packets
// ---------------------------------------------------------------------------

/// One transport packet, as much of it as the checks read.
#[derive(Debug, Clone)]
pub(crate) struct Packet {
    pub(crate) index: usize,
    pub(crate) pid: u16,
    pub(crate) continuity: u8,
    pub(crate) payload_unit_start: bool,
    pub(crate) has_payload: bool,
    pub(crate) discontinuity_indicator: bool,
    pub(crate) random_access: bool,
    /// The PCR in 27 MHz ticks (base × 300 + extension), when carried.
    pub(crate) pcr: Option<u64>,
    /// The payload bytes after the header and any adaptation field.
    pub(crate) payload: Vec<u8>,
}

/// Split segment bytes into packets. A malformed packet is reported and
/// skipped; the walk goes on so one bad byte does not hide everything after.
pub(crate) fn packets(bytes: &[u8]) -> (Vec<Packet>, Vec<Violation>) {
    let mut out = Vec::new();
    let mut violations = Vec::new();
    if bytes.len() % TS_PACKET_BYTES != 0 {
        violations.push(violation(
            Kind::Packet,
            format!(
                "segment is {} bytes, not a whole number of 188-byte packets",
                bytes.len()
            ),
        ));
    }
    for (index, packet) in bytes.chunks(TS_PACKET_BYTES).enumerate() {
        if packet.len() != TS_PACKET_BYTES {
            violations.push(violation(
                Kind::Packet,
                format!("packet {index} is {} bytes", packet.len()),
            ));
            continue;
        }
        if packet[0] != SYNC_BYTE {
            violations.push(violation(
                Kind::Packet,
                format!("packet {index} starts with {:#04x}, not 0x47", packet[0]),
            ));
            continue;
        }
        let payload_unit_start = packet[1] & 0x40 != 0;
        let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
        let has_adaptation = packet[3] & 0x20 != 0;
        let has_payload = packet[3] & 0x10 != 0;
        let continuity = packet[3] & 0x0f;
        let mut at = 4usize;
        let mut discontinuity_indicator = false;
        let mut random_access = false;
        let mut pcr = None;
        if has_adaptation {
            let length = usize::from(packet[4]);
            at = 5 + length;
            if at > TS_PACKET_BYTES {
                violations.push(violation(
                    Kind::Packet,
                    format!("packet {index} adaptation field runs past the packet"),
                ));
                continue;
            }
            if length > 0 {
                let flags = packet[5];
                discontinuity_indicator = flags & 0x80 != 0;
                random_access = flags & 0x40 != 0;
                if flags & 0x10 != 0 {
                    if length < 7 {
                        violations.push(violation(
                            Kind::Packet,
                            format!("packet {index} flags a PCR it has no room for"),
                        ));
                        continue;
                    }
                    let field = &packet[6..12];
                    let base = (u64::from(field[0]) << 25)
                        | (u64::from(field[1]) << 17)
                        | (u64::from(field[2]) << 9)
                        | (u64::from(field[3]) << 1)
                        | (u64::from(field[4]) >> 7);
                    let extension = (u64::from(field[4] & 0x01) << 8) | u64::from(field[5]);
                    pcr = Some(base * 300 + extension);
                }
            }
        }
        let payload = if has_payload {
            packet[at..].to_vec()
        } else {
            Vec::new()
        };
        out.push(Packet {
            index,
            pid,
            continuity,
            payload_unit_start,
            has_payload,
            discontinuity_indicator,
            random_access,
            pcr,
            payload,
        });
    }
    (out, violations)
}

// ---------------------------------------------------------------------------
// Program tables
// ---------------------------------------------------------------------------

/// What the PAT and PMT say: the PMT's PID, the PCR PID, and each
/// elementary PID's stream type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Program {
    pub(crate) pmt_pid: Option<u16>,
    pub(crate) pcr_pid: Option<u16>,
    pub(crate) streams: BTreeMap<u16, u8>,
}

/// The section bytes of a PSI packet: past the pointer field, bounded by
/// the section length.
fn section(packet: &Packet) -> Option<&[u8]> {
    if !packet.payload_unit_start {
        return None;
    }
    let pointer = usize::from(*packet.payload.first()?);
    let start = 1 + pointer;
    let header = packet.payload.get(start..start + 3)?;
    let length = (usize::from(header[1] & 0x0f) << 8) | usize::from(header[2]);
    packet.payload.get(start..start + 3 + length)
}

pub(crate) fn program(packets: &[Packet]) -> (Program, Vec<Violation>) {
    let mut program = Program::default();
    let mut violations = Vec::new();
    for packet in packets.iter().filter(|packet| packet.pid == PAT_PID) {
        let Some(section) = section(packet) else {
            continue;
        };
        if section[0] != 0 {
            continue;
        }
        // table_id, section_length(2), tsid(2), version, section_number,
        // last_section_number, then 4-byte program entries, then CRC.
        let entries = section
            .get(8..section.len().saturating_sub(4))
            .unwrap_or(&[]);
        for entry in entries.chunks_exact(4) {
            let number = (u16::from(entry[0]) << 8) | u16::from(entry[1]);
            let pid = (u16::from(entry[2] & 0x1f) << 8) | u16::from(entry[3]);
            if number != 0 {
                program.pmt_pid = Some(pid);
            }
        }
    }
    let Some(pmt_pid) = program.pmt_pid else {
        violations.push(violation(Kind::ProgramTables, "no PAT names a program"));
        return (program, violations);
    };
    let mut saw_pmt = false;
    for packet in packets.iter().filter(|packet| packet.pid == pmt_pid) {
        let Some(section) = section(packet) else {
            continue;
        };
        if section[0] != 2 || section.len() < 16 {
            continue;
        }
        saw_pmt = true;
        program.pcr_pid = Some((u16::from(section[8] & 0x1f) << 8) | u16::from(section[9]));
        let info_length = (usize::from(section[10] & 0x0f) << 8) | usize::from(section[11]);
        let mut at = 12 + info_length;
        let end = section.len().saturating_sub(4);
        while at + 5 <= end {
            let stream_type = section[at];
            let pid = (u16::from(section[at + 1] & 0x1f) << 8) | u16::from(section[at + 2]);
            let es_length =
                (usize::from(section[at + 3] & 0x0f) << 8) | usize::from(section[at + 4]);
            program.streams.insert(pid, stream_type);
            at += 5 + es_length;
        }
    }
    if !saw_pmt {
        violations.push(violation(
            Kind::ProgramTables,
            format!("no PMT on PID {pmt_pid:#x}"),
        ));
    }
    if program.pcr_pid.is_none_or(|pid| pid == 0x1fff) {
        violations.push(violation(Kind::ProgramTables, "the PMT names no PCR PID"));
    }
    (program, violations)
}

// ---------------------------------------------------------------------------
// PES
// ---------------------------------------------------------------------------

/// One PES packet, reassembled from the packets of its PID.
#[derive(Debug, Clone)]
pub(crate) struct Pes {
    pub(crate) pid: u16,
    /// Index of the transport packet the PES began in.
    pub(crate) packet_index: usize,
    pub(crate) pts: Option<u64>,
    pub(crate) dts: Option<u64>,
    /// The last PCR seen on the PCR PID before this PES began, when any.
    pub(crate) pcr_before: Option<u64>,
    /// NAL unit types in the elementary payload, in order (video only).
    pub(crate) nal_types: Vec<u8>,
}

impl Pes {
    /// The decode clock: DTS, or PTS when the two coincide.
    pub(crate) fn decode_time(&self) -> Option<u64> {
        self.dts.or(self.pts)
    }
}

fn timestamp(bytes: &[u8]) -> Result<u64, &'static str> {
    if bytes.len() < 5 {
        return Err("timestamp cut short");
    }
    if bytes[0] & 0x01 == 0 || bytes[2] & 0x01 == 0 || bytes[4] & 0x01 == 0 {
        return Err("timestamp marker bits are not set");
    }
    Ok((u64::from(bytes[0] & 0x0e) << 29)
        | (u64::from(bytes[1]) << 22)
        | (u64::from(bytes[2] & 0xfe) << 14)
        | (u64::from(bytes[3]) << 7)
        | (u64::from(bytes[4]) >> 1))
}

/// Annex-B NAL unit types, in order. Three- and four-byte start codes.
pub(crate) fn nal_types(es: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 3 <= es.len() {
        if es[at] == 0 && es[at + 1] == 0 && es[at + 2] == 1 {
            if let Some(&header) = es.get(at + 3) {
                out.push(header & 0x1f);
            }
            at += 3;
        } else {
            at += 1;
        }
    }
    out
}

fn parse_pes(
    pid: u16,
    packet_index: usize,
    pcr_before: Option<u64>,
    bytes: &[u8],
    video: bool,
) -> Result<Pes, Violation> {
    let fail = |detail: String| violation(Kind::Pes, format!("PID {pid:#x}: {detail}"));
    if bytes.len() < 9 || bytes[0] != 0 || bytes[1] != 0 || bytes[2] != 1 {
        return Err(fail("PES does not begin with a start code".into()));
    }
    if bytes[6] & 0xc0 != 0x80 {
        return Err(fail("PES header lacks its '10' marker".into()));
    }
    let flags = bytes[7] >> 6;
    let header_length = usize::from(bytes[8]);
    let header = bytes
        .get(9..9 + header_length)
        .ok_or_else(|| fail("PES header runs past the packet".into()))?;
    let (pts, dts) = match flags {
        0 => (None, None),
        2 => (Some(timestamp(header).map_err(|e| fail(e.into()))?), None),
        3 => (
            Some(timestamp(header).map_err(|e| fail(e.into()))?),
            Some(timestamp(header.get(5..).unwrap_or(&[])).map_err(|e| fail(e.into()))?),
        ),
        _ => return Err(fail("PES has the forbidden PTS_DTS_flags value 01".into())),
    };
    let declared = (usize::from(bytes[4]) << 8) | usize::from(bytes[5]);
    let es_start = 9 + header_length;
    let es_end = if declared == 0 {
        bytes.len()
    } else {
        (6 + declared).min(bytes.len())
    };
    let es = bytes.get(es_start..es_end).unwrap_or(&[]);
    Ok(Pes {
        pid,
        packet_index,
        pts,
        dts,
        pcr_before,
        nal_types: if video { nal_types(es) } else { Vec::new() },
    })
}

/// Reassemble every elementary PID's PES packets, in transport order.
pub(crate) fn pes_packets(packets: &[Packet], program: &Program) -> (Vec<Pes>, Vec<Violation>) {
    let mut open: BTreeMap<u16, (usize, Option<u64>, Vec<u8>)> = BTreeMap::new();
    let mut out = Vec::new();
    let mut violations = Vec::new();
    let mut last_pcr = None;
    let mut finish = |pid: u16, (index, pcr, bytes): (usize, Option<u64>, Vec<u8>)| {
        let video = program.streams.get(&pid).copied() == Some(0x1b);
        match parse_pes(pid, index, pcr, &bytes, video) {
            Ok(pes) => out.push(pes),
            Err(v) => violations.push(v),
        }
    };
    for packet in packets {
        if Some(packet.pid) == program.pcr_pid {
            if let Some(pcr) = packet.pcr {
                last_pcr = Some(pcr);
            }
        }
        if !program.streams.contains_key(&packet.pid) {
            continue;
        }
        if packet.payload_unit_start {
            if let Some(previous) = open.remove(&packet.pid) {
                finish(packet.pid, previous);
            }
            open.insert(packet.pid, (packet.index, last_pcr, packet.payload.clone()));
        } else if let Some((_, _, bytes)) = open.get_mut(&packet.pid) {
            bytes.extend_from_slice(&packet.payload);
        }
    }
    let leftover: Vec<(u16, (usize, Option<u64>, Vec<u8>))> = open.into_iter().collect();
    for (pid, pending) in leftover {
        finish(pid, pending);
    }
    out.sort_by_key(|pes| pes.packet_index);
    (out, violations)
}

// ---------------------------------------------------------------------------
// Segment checks
// ---------------------------------------------------------------------------

/// Everything a segment walk yields, for a check that spans segments.
#[derive(Debug, Clone)]
pub(crate) struct Walk {
    pub(crate) program: Program,
    pub(crate) packets: Vec<Packet>,
    pub(crate) pes: Vec<Pes>,
}

impl Walk {
    fn video_pid(&self) -> Option<u16> {
        self.program
            .streams
            .iter()
            .find(|(_, kind)| **kind == 0x1b)
            .map(|(pid, _)| *pid)
    }

    fn video(&self) -> Vec<&Pes> {
        let pid = self.video_pid();
        self.pes.iter().filter(|pes| Some(pes.pid) == pid).collect()
    }

    /// The last continuity counter written per PID on a payload packet.
    fn last_counters(&self) -> BTreeMap<u16, u8> {
        let mut out = BTreeMap::new();
        for packet in self.packets.iter().filter(|p| p.has_payload) {
            out.insert(packet.pid, packet.continuity);
        }
        out
    }

    fn pcrs(&self) -> Vec<u64> {
        self.packets
            .iter()
            .filter(|p| Some(p.pid) == self.program.pcr_pid)
            .filter_map(|p| p.pcr)
            .collect()
    }

    /// Last decode time per PID.
    fn last_decode_times(&self) -> BTreeMap<u16, u64> {
        let mut out = BTreeMap::new();
        for pes in &self.pes {
            if let Some(time) = pes.decode_time() {
                out.insert(pes.pid, time);
            }
        }
        out
    }
}

pub(crate) fn walk(bytes: &[u8]) -> (Walk, Vec<Violation>) {
    let (packets, mut violations) = packets(bytes);
    let (program, table_violations) = program(&packets);
    violations.extend(table_violations);
    let (pes, pes_violations) = pes_packets(&packets, &program);
    violations.extend(pes_violations);
    (
        Walk {
            program,
            packets,
            pes,
        },
        violations,
    )
}

/// What one segment leaves for the next to be checked against.
#[derive(Debug, Clone, Default)]
struct Carry {
    counters: BTreeMap<u16, u8>,
    pcr: Option<u64>,
    decode_times: BTreeMap<u16, u64>,
    first_video_pts: Option<u64>,
    duration_ms: u32,
}

/// Check one segment on its own: packets, tables, counters within it, the
/// PCR, timestamps, the leading key frame and the EXTINF duration.
pub(crate) fn check_segment(segment: &HlsSegment) -> Vec<Violation> {
    let (walk, mut violations) = walk(&segment.bytes);
    violations.extend(check_walk(&walk, segment, &Carry::default(), true));
    violations
}

fn check_walk(
    walk: &Walk,
    segment: &HlsSegment,
    carry: &Carry,
    standalone: bool,
) -> Vec<Violation> {
    let mut violations = Vec::new();
    let seq = segment.group_sequence;
    let reset_allowed = standalone || segment.discontinuity;

    // Continuity counters, carried in from the segment before unless the
    // seam is a declared discontinuity.
    let mut counters: BTreeMap<u16, u8> = if reset_allowed {
        BTreeMap::new()
    } else {
        carry.counters.clone()
    };
    for packet in &walk.packets {
        if !packet.has_payload {
            continue;
        }
        if let Some(last) = counters.get(&packet.pid) {
            let expected = (last + 1) & 0x0f;
            if packet.continuity != expected && !packet.discontinuity_indicator {
                violations.push(violation(
                    Kind::Continuity,
                    format!(
                        "segment {seq} packet {} PID {:#x}: counter {} after {} (expected {expected})",
                        packet.index, packet.pid, packet.continuity, last
                    ),
                ));
            }
        }
        counters.insert(packet.pid, packet.continuity);
    }

    // PCR: present, positive, clear of the wrap, monotonic.
    let pcrs = walk.pcrs();
    if walk.program.pcr_pid.is_some() && pcrs.is_empty() {
        violations.push(violation(
            Kind::PcrMissing,
            format!("segment {seq} carries no PCR on its PCR PID"),
        ));
    }
    let mut last_pcr = if reset_allowed { None } else { carry.pcr };
    for (n, &pcr) in pcrs.iter().enumerate() {
        let base = pcr / 300;
        if pcr == 0 {
            violations.push(violation(
                Kind::PcrRange,
                format!("segment {seq}: PCR #{n} is zero"),
            ));
        } else if base >= CLOCK_BITS - WRAP_GUARD_90K {
            violations.push(violation(
                Kind::PcrRange,
                format!("segment {seq}: PCR #{n} base {base} is in the wrap region"),
            ));
        }
        if let Some(last) = last_pcr {
            if pcr < last {
                violations.push(violation(
                    Kind::PcrOrder,
                    format!(
                        "segment {seq}: PCR #{n} {pcr} went backwards from {last} ({} ms)",
                        (last - pcr) / 27_000
                    ),
                ));
            }
        }
        last_pcr = Some(pcr);
    }

    // Timestamps: markers read, DTS advancing per PID, PTS >= DTS, PCR behind.
    let mut decode_times: BTreeMap<u16, u64> = if reset_allowed {
        BTreeMap::new()
    } else {
        carry.decode_times.clone()
    };
    for pes in &walk.pes {
        let Some(decode) = pes.decode_time() else {
            violations.push(violation(
                Kind::Pes,
                format!(
                    "segment {seq} PID {:#x}: PES at packet {} has no timestamp",
                    pes.pid, pes.packet_index
                ),
            ));
            continue;
        };
        if let (Some(pts), Some(dts)) = (pes.pts, pes.dts) {
            if pts < dts {
                violations.push(violation(
                    Kind::PtsBeforeDts,
                    format!("segment {seq} PID {:#x}: PTS {pts} < DTS {dts}", pes.pid),
                ));
            }
        }
        if decode >= CLOCK_BITS {
            violations.push(violation(
                Kind::Pes,
                format!(
                    "segment {seq} PID {:#x}: timestamp {decode} exceeds 33 bits",
                    pes.pid
                ),
            ));
        }
        if let Some(last) = decode_times.get(&pes.pid) {
            if decode <= *last {
                violations.push(violation(
                    Kind::TimestampOrder,
                    format!(
                        "segment {seq} PID {:#x}: decode time {decode} after {last}",
                        pes.pid
                    ),
                ));
            }
        }
        decode_times.insert(pes.pid, decode);
        if Some(pes.pid) == walk.program.pcr_pid {
            if let Some(pcr) = pes.pcr_before {
                if pcr / 300 > decode {
                    violations.push(violation(
                        Kind::PcrAheadOfDts,
                        format!(
                            "segment {seq} PID {:#x}: PCR {} is ahead of DTS {decode}",
                            pes.pid,
                            pcr / 300
                        ),
                    ));
                }
            }
        }
    }

    // The leading video access unit.
    let video = walk.video();
    match video.first() {
        None if walk.video_pid().is_some() => violations.push(violation(
            Kind::KeyFrame,
            format!("segment {seq} has a video PID and no video access unit"),
        )),
        None => {}
        Some(first) => {
            let idr = first.nal_types.iter().position(|&t| t == NAL_IDR);
            let sps = first.nal_types.iter().position(|&t| t == NAL_SPS);
            let pps = first.nal_types.iter().position(|&t| t == NAL_PPS);
            match (idr, sps, pps) {
                (Some(idr), Some(sps), Some(pps)) if sps < idr && pps < idr => {}
                _ => violations.push(violation(
                    Kind::KeyFrame,
                    format!(
                        "segment {seq}: first video access unit is {:?}, not SPS, PPS then IDR",
                        first.nal_types
                    ),
                )),
            }
            if first.nal_types.contains(&NAL_NON_IDR) && idr.is_none() {
                violations.push(violation(
                    Kind::KeyFrame,
                    format!("segment {seq} opens on a non-IDR slice"),
                ));
            }
        }
    }

    // EXTINF against the content span, when the segment carries enough
    // frames to know its own frame period.
    let ptss: Vec<u64> = video.iter().filter_map(|pes| pes.pts).collect();
    if ptss.len() >= 2 {
        let mut deltas: Vec<u64> = ptss.windows(2).map(|w| w[1].saturating_sub(w[0])).collect();
        deltas.sort_unstable();
        let period = deltas[deltas.len() / 2];
        let first = ptss.iter().copied().min().unwrap_or(0);
        let last = ptss.iter().copied().max().unwrap_or(0);
        let span_ms = (last + period).saturating_sub(first) / 90;
        if span_ms.abs_diff(u64::from(segment.duration_ms)) > DURATION_SLACK_MS {
            violations.push(violation(
                Kind::Duration,
                format!(
                    "segment {seq}: EXTINF {} ms but the frames span {span_ms} ms",
                    segment.duration_ms
                ),
            ));
        }
    }

    // Across a seam that is not a discontinuity, the EXTINF of the segment
    // before must be the step in presentation time.
    if !reset_allowed {
        if let (Some(before), Some(now)) = (carry.first_video_pts, ptss.first()) {
            let step_ms = now.saturating_sub(before) / 90;
            if step_ms.abs_diff(u64::from(carry.duration_ms)) > DURATION_SLACK_MS {
                violations.push(violation(
                    Kind::Duration,
                    format!(
                        "segment {seq}: the segment before said {} ms, its timestamps step {step_ms} ms",
                        carry.duration_ms
                    ),
                ));
            }
        }
    }

    violations
}

/// Check consecutive segments of one rendition as the stream a player sees:
/// counters, PCR and timestamps run on across every seam that is not a
/// declared discontinuity, and EXTINF is the step between segments.
pub(crate) fn check_run(segments: &[HlsSegment]) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut carry = Carry::default();
    let mut last_sequence: Option<u64> = None;
    for segment in segments {
        if let Some(last) = last_sequence {
            if segment.group_sequence != last + 1 {
                violations.push(violation(
                    Kind::Reload,
                    format!(
                        "segment {} follows {last}: a run is consecutive",
                        segment.group_sequence
                    ),
                ));
            }
        }
        last_sequence = Some(segment.group_sequence);
        let (walk, walk_violations) = walk(&segment.bytes);
        violations.extend(walk_violations);
        violations.extend(check_walk(&walk, segment, &carry, false));
        carry = Carry {
            counters: walk.last_counters(),
            pcr: walk.pcrs().last().copied(),
            decode_times: walk.last_decode_times(),
            first_video_pts: walk.video().first().and_then(|pes| pes.pts),
            duration_ms: segment.duration_ms,
        };
    }
    violations
}

// ---------------------------------------------------------------------------
// Playlists
// ---------------------------------------------------------------------------

/// A media playlist as the checks read it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Playlist {
    pub(crate) version: Option<u32>,
    pub(crate) target_duration: Option<u32>,
    pub(crate) media_sequence: Option<u64>,
    pub(crate) discontinuity_sequence: Option<u64>,
    pub(crate) vod: bool,
    pub(crate) endlist: bool,
    /// `(sequence, extinf in ms, discontinuity before it, uri)`.
    pub(crate) segments: Vec<(u64, u64, bool, String)>,
}

pub(crate) fn parse_playlist(text: &str) -> (Playlist, Vec<Violation>) {
    let mut violations = Vec::new();
    let mut playlist = Playlist {
        version: None,
        target_duration: None,
        media_sequence: None,
        discontinuity_sequence: None,
        vod: false,
        endlist: false,
        segments: Vec::new(),
    };
    let mut lines = text.lines();
    if lines.next() != Some("#EXTM3U") {
        violations.push(violation(Kind::Playlist, "the first line is not #EXTM3U"));
    }
    let mut pending: Option<(u64, bool)> = None;
    let mut discontinuity = false;
    let mut next_sequence: Option<u64> = None;
    for line in lines {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(value) = line.strip_prefix("#EXT-X-VERSION:") {
            playlist.version = value.parse().ok();
        } else if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            playlist.target_duration = value.parse().ok();
        } else if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            playlist.media_sequence = value.parse().ok();
            next_sequence = playlist.media_sequence;
        } else if let Some(value) = line.strip_prefix("#EXT-X-DISCONTINUITY-SEQUENCE:") {
            playlist.discontinuity_sequence = value.parse().ok();
        } else if line == "#EXT-X-PLAYLIST-TYPE:VOD" {
            playlist.vod = true;
        } else if line == "#EXT-X-ENDLIST" {
            playlist.endlist = true;
        } else if line == "#EXT-X-DISCONTINUITY" {
            discontinuity = true;
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            let seconds: f64 = value
                .split(',')
                .next()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(-1.0);
            if seconds < 0.0 {
                violations.push(violation(Kind::Playlist, format!("unreadable {line}")));
                continue;
            }
            #[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
            let ms = (seconds * 1_000.0).round() as u64;
            pending = Some((ms, discontinuity));
            discontinuity = false;
        } else if line.starts_with('#') {
            // A tag the checks do not read.
        } else {
            let Some((ms, discontinuity)) = pending.take() else {
                violations.push(violation(
                    Kind::Playlist,
                    format!("segment {line} has no EXTINF"),
                ));
                continue;
            };
            let sequence = next_sequence.unwrap_or(0);
            next_sequence = Some(sequence + 1);
            playlist
                .segments
                .push((sequence, ms, discontinuity, line.to_string()));
        }
    }
    if pending.is_some() {
        violations.push(violation(Kind::Playlist, "an EXTINF names no segment"));
    }
    (playlist, violations)
}

/// A media playlist's fixed header, and its target duration against its
/// segments. A playlist without `PLAYLIST-TYPE:VOD` is a live window.
pub(crate) fn check_playlist(text: &str) -> Vec<Violation> {
    let (playlist, mut violations) = parse_playlist(text);
    if playlist.version != Some(3) {
        violations.push(violation(
            Kind::Playlist,
            format!("EXT-X-VERSION is {:?}, not 3", playlist.version),
        ));
    }
    let longest_ms = playlist
        .segments
        .iter()
        .map(|(_, ms, _, _)| *ms)
        .max()
        .unwrap_or(0);
    let needed = longest_ms.div_ceil(1_000);
    match playlist.target_duration {
        None => violations.push(violation(Kind::Playlist, "no EXT-X-TARGETDURATION")),
        Some(target) if u64::from(target) < needed => violations.push(violation(
            Kind::Playlist,
            format!("EXT-X-TARGETDURATION {target} is under the longest EXTINF ({longest_ms} ms)"),
        )),
        Some(_) => {}
    }
    if playlist.media_sequence.is_none() {
        violations.push(violation(Kind::Playlist, "no EXT-X-MEDIA-SEQUENCE"));
    }
    if playlist.segments.is_empty() {
        violations.push(violation(Kind::Playlist, "the playlist lists no segment"));
    }
    if !playlist.vod {
        if playlist.discontinuity_sequence.is_none() {
            violations.push(violation(
                Kind::Playlist,
                "a live window has no EXT-X-DISCONTINUITY-SEQUENCE",
            ));
        }
        if playlist.endlist {
            violations.push(violation(
                Kind::Playlist,
                "a live window declares EXT-X-ENDLIST",
            ));
        }
    }
    violations
}

/// Successive reloads of one live playlist: the media sequence never goes
/// back, a listed segment never changes its EXTINF or its discontinuity, and
/// the discontinuity sequence counts exactly the discontinuous segments that
/// left the window.
pub(crate) fn check_reloads(texts: &[&str]) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut previous: Option<Playlist> = None;
    for (n, text) in texts.iter().enumerate() {
        let (playlist, parse_violations) = parse_playlist(text);
        violations.extend(parse_violations);
        if let Some(before) = &previous {
            let (Some(was), Some(now)) = (before.media_sequence, playlist.media_sequence) else {
                continue;
            };
            if now < was {
                violations.push(violation(
                    Kind::Reload,
                    format!("reload {n}: media sequence went from {was} to {now}"),
                ));
            }
            for (sequence, ms, discontinuity, _) in &before.segments {
                if let Some((_, now_ms, now_discontinuity, _)) = playlist
                    .segments
                    .iter()
                    .find(|(candidate, ..)| candidate == sequence)
                {
                    if now_ms != ms {
                        violations.push(violation(
                            Kind::Reload,
                            format!(
                                "reload {n}: segment {sequence} was {ms} ms, is now {now_ms} ms"
                            ),
                        ));
                    }
                    if now_discontinuity != discontinuity {
                        violations.push(violation(
                            Kind::Reload,
                            format!(
                                "reload {n}: segment {sequence} changed its discontinuity mark"
                            ),
                        ));
                    }
                }
            }
            let left: u64 = before
                .segments
                .iter()
                .filter(|(sequence, _, discontinuity, _)| *sequence < now && *discontinuity)
                .count()
                .try_into()
                .unwrap_or(u64::MAX);
            let (Some(was_dropped), Some(now_dropped)) = (
                before.discontinuity_sequence,
                playlist.discontinuity_sequence,
            ) else {
                continue;
            };
            if now_dropped != was_dropped + left {
                violations.push(violation(
                    Kind::Reload,
                    format!(
                        "reload {n}: discontinuity sequence went {was_dropped} -> {now_dropped} while {left} discontinuous segment(s) left the window"
                    ),
                ));
            }
        }
        previous = Some(playlist);
    }
    violations
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::live::LiveMediaHub;
    use crate::display::producer::tests::{lap, make};
    use crate::display::producer::{Splice, Timeline, WINDOW};

    const ORBIT: &str = "space/orbit";
    const RESOURCE: &str = "prog-1";

    fn assert_clean(violations: &[Violation]) {
        assert!(
            violations.is_empty(),
            "{} violation(s):\n{}",
            violations.len(),
            violations
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    fn kinds(violations: &[Violation]) -> Vec<Kind> {
        violations.iter().map(|v| v.kind).collect()
    }

    /// Two laps of a two-part program, a splice to a different program, and
    /// a picture swapped in place — the seams the producer makes.
    async fn produce_a_run() -> Vec<HlsSegment> {
        let mut timeline =
            Timeline::new(RESOURCE, lap(&[("a", 40, 2_000), ("b", 200, 3_000)]), 0, 0);
        let mut run = make(&mut timeline, 10).await;
        assert_eq!(
            timeline.offer(lap(&[("c", 120, 2_000), ("d", 60, 1_000)])),
            Splice::Era { at: 10 }
        );
        run.extend(make(&mut timeline, 4).await);
        assert_eq!(
            timeline.offer(lap(&[("c", 90, 2_000), ("d", 60, 1_000)])),
            Splice::InPlace
        );
        run.extend(make(&mut timeline, 5).await);
        run
    }

    #[tokio::test]
    async fn a_produced_run_is_one_continuous_transport_stream() {
        let run = produce_a_run().await;
        assert!(run.iter().any(|s| s.discontinuity), "the run has seams");
        assert!(
            run.iter().filter(|s| !s.discontinuity).count() > 1,
            "and segments that run on"
        );
        for segment in &run {
            assert_clean(&check_segment(segment));
        }
        assert_clean(&check_run(&run));
        // The walker saw what it claims to: a video PES with parameter sets
        // and an IDR, and a PCR, in every segment.
        for segment in &run {
            let (walk, _) = walk(&segment.bytes);
            assert_eq!(walk.program.pcr_pid, Some(0x101));
            assert!(!walk.pcrs().is_empty());
            let first = walk.video()[0].clone();
            assert!(first.nal_types.contains(&NAL_SPS));
            assert!(first.nal_types.contains(&NAL_PPS));
            assert!(first.nal_types.contains(&NAL_IDR));
            assert!(first.pts.is_some());
        }
    }

    /// A stream resumed by a restarted daemon carries the discontinuities its
    /// window had already dropped, so the header a player keeps its place by
    /// continues rather than restarting at zero against a media sequence
    /// that went on — the mismatch that had a player reloading a playlist it
    /// no longer believed and never asking for a segment.
    #[tokio::test]
    async fn a_resumed_window_carries_its_dropped_discontinuities_forward() {
        let mut timeline =
            Timeline::new(RESOURCE, lap(&[("a", 40, 1_000), ("b", 200, 1_000)]), 0, 0);
        let hub = LiveMediaHub::default();
        hub.install_rolling(ORBIT, RESOURCE, timeline.description(64, 48), WINDOW, 7)
            .unwrap();
        let segment = make(&mut timeline, 1).await.remove(0);
        hub.push_hls_segment(ORBIT, RESOURCE, segment).unwrap();
        let text = hub
            .hls_media_playlist(ORBIT, RESOURCE, RESOURCE, "..", 0)
            .unwrap();
        assert!(text.contains("#EXT-X-DISCONTINUITY-SEQUENCE:7\n"), "{text}");
        assert_eq!(hub.discontinuities_dropped(ORBIT, RESOURCE), Some(7));
        // Slide the window until seams fall off: the count only grows.
        for _ in 0..(WINDOW * 2) {
            let segment = make(&mut timeline, 1).await.remove(0);
            hub.push_hls_segment(ORBIT, RESOURCE, segment).unwrap();
        }
        assert!(hub.discontinuities_dropped(ORBIT, RESOURCE).unwrap() > 7);
    }

    #[tokio::test]
    async fn the_hubs_rolling_playlists_reload_without_contradiction() {
        let mut timeline =
            Timeline::new(RESOURCE, lap(&[("a", 40, 2_000), ("b", 200, 3_000)]), 0, 0);
        let hub = LiveMediaHub::default();
        hub.install_rolling(ORBIT, RESOURCE, timeline.description(64, 48), WINDOW, 0)
            .unwrap();
        let mut reloads = Vec::new();
        let count = WINDOW * 3;
        for n in 0..count {
            if n == WINDOW + 7 {
                assert_eq!(
                    timeline.offer(lap(&[("c", 120, 1_000)])),
                    Splice::Era { at: n as u64 }
                );
            }
            let segment = make(&mut timeline, 1).await.remove(0);
            hub.push_hls_segment(ORBIT, RESOURCE, segment).unwrap();
            let now = u64::try_from(n).unwrap() * 1_000;
            let text = hub
                .hls_media_playlist(ORBIT, RESOURCE, RESOURCE, "..", now)
                .unwrap();
            assert_clean(&check_playlist(&text));
            reloads.push(text);
        }
        let last = parse_playlist(reloads.last().unwrap()).0;
        assert_eq!(last.segments.len(), WINDOW, "the window slid");
        assert!(
            last.discontinuity_sequence.unwrap() > 0,
            "discontinuities fell off the front"
        );
        let texts: Vec<&str> = reloads.iter().map(String::as_str).collect();
        assert_clean(&check_reloads(&texts));
    }

    /// The checker catches what it claims to: a counter restarted at a seam
    /// and one flipped mid-segment — the first defect found by hand.
    #[tokio::test]
    async fn a_restarted_continuity_counter_is_reported() {
        let run = produce_a_run().await;
        let seam = run
            .iter()
            .position(|s| s.group_sequence > 0 && !s.discontinuity)
            .expect("a seam that runs on");
        let mut corrupt = run.clone();
        // Restart the video PID's counters at zero, as a fresh mux would.
        let mut next = 0u8;
        for packet in corrupt[seam].bytes.chunks_exact_mut(188) {
            let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
            if pid == 0x101 && packet[3] & 0x10 != 0 {
                packet[3] = (packet[3] & 0xf0) | next;
                next = (next + 1) & 0x0f;
            }
        }
        assert_clean(&check_segment(&corrupt[seam]));
        let found = check_run(&corrupt);
        assert!(
            kinds(&found).contains(&Kind::Continuity),
            "a restart at a seam is a continuity violation: {found:?}"
        );
        assert!(
            found.iter().all(|v| v.kind == Kind::Continuity),
            "and nothing else: {found:?}"
        );

        // Flip one counter in the middle of a segment.
        let mut flipped = run[seam].clone();
        let packet = flipped
            .bytes
            .chunks_exact_mut(188)
            .filter(|p| {
                let pid = (u16::from(p[1] & 0x1f) << 8) | u16::from(p[2]);
                pid == 0x101 && p[3] & 0x10 != 0
            })
            .nth(2)
            .expect("a third video payload packet");
        packet[3] ^= 0x08;
        let found = check_segment(&flipped);
        assert_eq!(kinds(&found), vec![Kind::Continuity, Kind::Continuity]);
    }

    /// A PCR of zero — what a part opened at the clock's origin wrote, one
    /// wrap away from 2^33 — is reported as out of range and, where it
    /// follows a real one, as backwards.
    #[tokio::test]
    async fn a_pcr_at_the_clocks_origin_is_reported() {
        let run = produce_a_run().await;
        let mut segment = run[0].clone();
        let mut zeroed = 0usize;
        for packet in segment.bytes.chunks_exact_mut(188) {
            let has_adaptation = packet[3] & 0x20 != 0;
            if has_adaptation && packet[4] >= 7 && packet[5] & 0x10 != 0 {
                for byte in &mut packet[6..12] {
                    *byte = 0;
                }
                zeroed += 1;
                break;
            }
        }
        assert_eq!(zeroed, 1, "the segment carries a PCR to zero");
        let found = check_segment(&segment);
        assert!(
            kinds(&found).contains(&Kind::PcrRange),
            "a zero PCR is out of range: {found:?}"
        );
        assert!(
            found
                .iter()
                .all(|v| matches!(v.kind, Kind::PcrRange | Kind::PcrOrder)),
            "and nothing else: {found:?}"
        );

        // Wrapped negative: the top of the 33-bit range.
        let mut wrapped = run[0].clone();
        for packet in wrapped.bytes.chunks_exact_mut(188) {
            let has_adaptation = packet[3] & 0x20 != 0;
            if has_adaptation && packet[4] >= 7 && packet[5] & 0x10 != 0 {
                packet[6..11].copy_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x80]);
                packet[11] = 0;
                break;
            }
        }
        let found = check_segment(&wrapped);
        assert!(
            kinds(&found).contains(&Kind::PcrRange),
            "a PCR at the wrap is out of range: {found:?}"
        );
    }

    #[tokio::test]
    async fn a_segment_that_does_not_open_on_a_key_frame_is_reported() {
        let run = produce_a_run().await;
        let mut segment = run[0].clone();
        // Turn the IDR NAL header into a non-IDR slice header, wherever the
        // start code lands in the packets.
        let bytes = &mut segment.bytes;
        let mut rewritten = false;
        for packet in bytes.chunks_exact_mut(188) {
            let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
            if pid != 0x101 {
                continue;
            }
            for i in 4..185 {
                if packet[i] == 0
                    && packet[i + 1] == 0
                    && packet[i + 2] == 1
                    && packet[i + 3] & 0x1f == NAL_IDR
                {
                    packet[i + 3] = (packet[i + 3] & 0xe0) | NAL_NON_IDR;
                    rewritten = true;
                    break;
                }
            }
            if rewritten {
                break;
            }
        }
        assert!(rewritten, "the IDR start code lies within one packet");
        let found = check_segment(&segment);
        assert_eq!(kinds(&found), vec![Kind::KeyFrame, Kind::KeyFrame]);
    }

    #[test]
    fn a_reload_that_contradicts_the_one_before_is_reported() {
        let first = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:4\n#EXT-X-DISCONTINUITY-SEQUENCE:1\n#EXTINF:1.000,\n../segments/4.ts\n#EXT-X-DISCONTINUITY\n#EXTINF:1.000,\n../segments/5.ts\n#EXTINF:1.000,\n../segments/6.ts\n";
        assert_clean(&check_playlist(first));

        // The window slid past 4 and 5; 5 was discontinuous, so the
        // discontinuity sequence must be 2, and 6 must still be one second.
        let honest = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:6\n#EXT-X-DISCONTINUITY-SEQUENCE:2\n#EXTINF:1.000,\n../segments/6.ts\n#EXTINF:1.000,\n../segments/7.ts\n";
        assert_clean(&check_reloads(&[first, honest]));

        let stale_count = honest.replace("DISCONTINUITY-SEQUENCE:2", "DISCONTINUITY-SEQUENCE:1");
        assert_eq!(
            kinds(&check_reloads(&[first, &stale_count])),
            vec![Kind::Reload]
        );

        let rewritten = honest.replace(
            "#EXTINF:1.000,\n../segments/6.ts",
            "#EXTINF:2.000,\n../segments/6.ts",
        );
        assert_eq!(
            kinds(&check_reloads(&[first, &rewritten])),
            vec![Kind::Reload]
        );

        let rewound = honest.replace("MEDIA-SEQUENCE:6", "MEDIA-SEQUENCE:3");
        assert!(kinds(&check_reloads(&[first, &rewound])).contains(&Kind::Reload));

        let ended = format!("{honest}#EXT-X-ENDLIST\n");
        assert_eq!(kinds(&check_playlist(&ended)), vec![Kind::Playlist]);
        let short = honest.replace("TARGETDURATION:2", "TARGETDURATION:0");
        assert_eq!(kinds(&check_playlist(&short)), vec![Kind::Playlist]);
    }

    /// The produced stream through a real demuxer and decoder. A part seam
    /// is a declared discontinuity, and ffmpeg says so once per seam when
    /// the segments are played as one file; nothing else may be said.
    /// Ignored (shells ffmpeg); CI runs it on Linux.
    #[tokio::test]
    #[ignore]
    async fn ffmpeg_decodes_a_produced_lap_without_a_warning() {
        use std::io::Write;
        use std::process::Command;
        let run = produce_a_run().await;
        let seams = run.iter().filter(|s| s.discontinuity).count();
        let dir = std::env::var("STILL_DUMP_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let path = dir.join("produced-lap.ts");
        let mut file = std::fs::File::create(&path).unwrap();
        for segment in &run {
            file.write_all(&segment.bytes).unwrap();
        }
        drop(file);
        let out = Command::new("ffmpeg")
            .args(["-v", "warning", "-i"])
            .arg(&path)
            .args(["-f", "null", "-"])
            .output()
            .expect("ffmpeg runs");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "ffmpeg failed:\n{stderr}");
        let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
        let (expected, unexpected): (Vec<&str>, Vec<&str>) = lines
            .iter()
            .partition(|line| line.contains("timestamp discontinuity"));
        assert!(
            unexpected.is_empty(),
            "ffmpeg warned about more than the seams:\n{stderr}"
        );
        assert!(
            expected.len() <= seams,
            "ffmpeg saw {} discontinuities in {seams} seams:\n{stderr}",
            expected.len()
        );
    }
}
