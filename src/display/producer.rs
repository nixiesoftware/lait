//! The per-assignment producer: one endless stream, materialised at the live
//! edge from a schedule that is edited in place, never rebuilt.
//!
//! A receiver holds one HLS URL for the life of its assignment. What it plays
//! is a **timeline**: a monotonic sequence of segments anchored to a wall-clock
//! epoch, each filled by one part of the program — a still (a rendered card,
//! an uploaded image, a black slot) or a clip. The timeline is laid out as a
//! **lap** repeated forever; segment `n` is the lap's slot `n mod K`, and the
//! wall clock says which `n` is the live edge. Everything that used to be a
//! new presentation — a data tick, a slide edit, a clip cut — is now an edit
//! to the lap at or after the first segment nobody has been shown yet:
//!
//! - a frame whose pixels changed but whose place in the program did not is
//!   **swapped in place**, and the next segment of that part carries it under
//!   the same decoder parameters, with no seam;
//! - a program whose shape changed opens a **new era** at `next`: the same
//!   sequence counter, a new lap, a discontinuity on its first segment, and
//!   nothing before it touched.
//!
//! The media sequence therefore never goes backwards and the epoch never
//! moves, which is the whole of why a receiver cannot freeze on a change: the
//! stream it is playing is never torn down, only extended. The static program
//! is the degenerate case — a lap nobody edits — and costs a cache lookup per
//! segment once its stills are encoded.
//!
//! Composition used to run on the receiver's poll and lay a whole program out
//! as segments up front, ~3 MB per program-second at 1080p. Here a still is
//! held as one encoded access unit and muxed into each segment on demand, so
//! what a program costs to hold is its number of distinct pictures, not its
//! length; a clip is held as its plan and read one segment at a time.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use mediabox::h264still::StillH264;
use runtime::plane::live::media::TrackKind;
use sha2::{Digest, Sha256};
use world_interface::display::{DisplayProjection, FrameMediaType, MediaOrigin, RenderedScene};

use super::{HlsCatalogPackager, HlsRenditionDescription, HlsSegment, StoredPlan};

/// Segments the playlist lists behind the live edge, so a late reload still
/// finds what it wants.
pub(crate) const TRAIL: u64 = 3;
/// How far ahead of the wall clock segments are made, in media time. A
/// player sits a few target durations behind the end of what is listed, so
/// this is the runway it plays from; counted in segments it would be six
/// seconds of stills and fifteen of a clip, and at the seam between the two
/// the player found itself at the production frontier, waiting for each
/// segment as it was made.
pub(crate) const LEAD_MS: u64 = 12_000;
/// What the hub retains: the lead in one-second stills, the trail, and slack
/// for a slow fetch.
pub(crate) const WINDOW: usize = 20;
/// The target duration the stream declares, at least. A player sizes its
/// live buffer in target durations, so a one-second still segment must not
/// talk the buffer down to a second; the still packager always said two.
const MIN_TARGET_DURATION_MS: u32 = 2_000;
/// A rendered slot with no duration of its own holds this long.
const DEFAULT_ITEM_MS: u32 = 10_000;
/// The still segment length; a still part is laid out in whole seconds.
const STILL_SEGMENT_MS: u32 = 1_000;
/// A `moov` is a table of contents; bound it like one.
const MAX_MOOV: u64 = 8 * 1024 * 1024;

/// Bytes a clip part needs, by content id and range. The producer never
/// holds a clip; it asks for the ranges one segment's plan names, when that
/// segment is next.
pub(crate) trait ClipReader: Send + Sync {
    fn size<'a>(&'a self, resource: &'a str) -> ReadFuture<'a, u64>;
    fn read<'a>(&'a self, resource: &'a str, offset: u64, len: u64) -> ReadFuture<'a, Vec<u8>>;
}

pub(crate) type ReadFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Walk a stored container's boxes to its `moov`, reading headers only.
pub(crate) async fn fetch_moov(
    reader: &dyn ClipReader,
    resource: &str,
    total: u64,
) -> Result<Vec<u8>> {
    const HEADER: u32 = 16;
    let mut at = 0u64;
    while at < total {
        let header = reader.read(resource, at, u64::from(HEADER)).await?;
        let (size, kind, _) = mediabox::box_header(&header, total, at)
            .map_err(|error| anyhow!("stored container refused: {error}"))?;
        if &kind == b"moov" {
            if size > MAX_MOOV {
                return Err(anyhow!("stored moov exceeds its bound"));
            }
            return reader.read(resource, at, size).await;
        }
        at = at
            .checked_add(size)
            .ok_or_else(|| anyhow!("stored container box overflows"))?;
    }
    Err(anyhow!("stored content has no moov"))
}

/// Read the sample ranges one segment needs. A group's samples are dozens
/// to hundreds of ranges a few kilobytes apart; asked for one at a time over
/// the content channel they cost a round trip each, which is where a clip
/// segment took seconds to make and the stream starved. The span they cover
/// is read whole when it is not much bigger than the samples themselves —
/// the gaps are the other track's samples, interleaved — and sliced.
async fn read_ranges(
    reader: &dyn ClipReader,
    resource: &str,
    ranges: &[(u64, u32)],
) -> Result<Vec<Vec<u8>>> {
    /// Read the span whole when the gaps in it are at most this many times
    /// the samples; beyond that the ranges are asked for one by one.
    const SPAN_WASTE_LIMIT: u64 = 3;
    let Some(low) = ranges.iter().map(|&(offset, _)| offset).min() else {
        return Ok(Vec::new());
    };
    let high = ranges
        .iter()
        .map(|&(offset, size)| offset.saturating_add(u64::from(size)))
        .max()
        .unwrap_or(low);
    let wanted: u64 = ranges
        .iter()
        .map(|&(_, size)| u64::from(size))
        .fold(0, u64::saturating_add);
    let span = high.saturating_sub(low);
    if wanted > 0 && span <= wanted.saturating_mul(SPAN_WASTE_LIMIT) {
        let bytes = reader.read(resource, low, span).await?;
        let mut out = Vec::with_capacity(ranges.len());
        for &(offset, size) in ranges {
            let start = usize::try_from(offset.saturating_sub(low)).unwrap_or(usize::MAX);
            let end = start.saturating_add(usize::try_from(size).unwrap_or(usize::MAX));
            let slice = bytes
                .get(start..end)
                .ok_or_else(|| anyhow!("clip span read fell short of a sample"))?;
            out.push(slice.to_vec());
        }
        return Ok(out);
    }
    let mut out = Vec::with_capacity(ranges.len());
    for &(offset, size) in ranges {
        out.push(reader.read(resource, offset, u64::from(size)).await?);
    }
    Ok(out)
}

/// Why a rendered program cannot be one stream. The receiver gets the
/// per-item program instead, which still works.
pub(crate) fn unstreamable(projection: &DisplayProjection) -> Option<&'static str> {
    projection
        .program
        .items
        .iter()
        .find_map(|item| match &item.scene {
            RenderedScene::Media(media) if media.origin.is_live() => {
                Some("a live source has no finite bytes to schedule")
            }
            RenderedScene::StoredFrame(_) => Some("a stored frame reached the producer unresolved"),
            _ => None,
        })
}

/// A part's identity in the program: what makes two laps the same shape.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PartKey {
    Frame {
        item: String,
        width: u32,
        height: u32,
    },
    Clip {
        content: String,
    },
    Blank {
        item: String,
    },
}

/// What fills a part's segments.
#[derive(Clone)]
enum PartSource {
    Still(Arc<StillH264>),
    Clip {
        resource: String,
        plan: Arc<StoredPlan>,
    },
}

#[derive(Clone)]
struct Part {
    key: PartKey,
    duration_ms: u32,
    source: PartSource,
}

/// One segment's place in a lap.
#[derive(Debug, Clone, Copy)]
struct Slot {
    part: usize,
    local: usize,
    duration_ms: u32,
}

/// One pass through the program, as segments.
#[derive(Clone)]
pub(crate) struct Lap {
    parts: Vec<Part>,
    slots: Vec<Slot>,
    /// The one frame size every part of this lap is coded at.
    frame: (u32, u32),
    /// `starts_ms[i]` is where slot `i` begins within the lap; one past the
    /// end is the lap's length.
    starts_ms: Vec<u64>,
    lap_ms: u64,
}

impl Lap {
    fn from_parts(parts: Vec<Part>, frame: (u32, u32)) -> Result<Self> {
        let mut slots = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            match &part.source {
                PartSource::Still(_) => {
                    let count = part.duration_ms.div_ceil(STILL_SEGMENT_MS).max(1);
                    for local in 0..count {
                        slots.push(Slot {
                            part: index,
                            local: usize::try_from(local).unwrap_or(usize::MAX),
                            duration_ms: STILL_SEGMENT_MS,
                        });
                    }
                }
                PartSource::Clip { plan, .. } => {
                    for local in 0..plan.group_count() {
                        slots.push(Slot {
                            part: index,
                            local,
                            duration_ms: plan.group_duration_ms(local).unwrap_or(1).max(1),
                        });
                    }
                }
            }
        }
        if slots.is_empty() {
            return Err(anyhow!("a program lap needs at least one segment"));
        }
        let mut starts_ms = Vec::with_capacity(slots.len().saturating_add(1));
        let mut at = 0u64;
        for slot in &slots {
            starts_ms.push(at);
            at = at.saturating_add(u64::from(slot.duration_ms));
        }
        starts_ms.push(at);
        Ok(Self {
            parts,
            slots,
            frame,
            starts_ms,
            lap_ms: at.max(1),
        })
    }

    /// The frame size every part is coded at.
    pub(crate) const fn frame(&self) -> (u32, u32) {
        self.frame
    }

    fn same_shape(&self, other: &Self) -> bool {
        self.parts.len() == other.parts.len()
            && self
                .parts
                .iter()
                .zip(&other.parts)
                .all(|(a, b)| a.key == b.key && a.duration_ms == b.duration_ms)
    }

    fn slot_count(&self) -> u64 {
        u64::try_from(self.slots.len()).unwrap_or(u64::MAX).max(1)
    }

    /// The still digests this lap draws on, so an encode cache can keep
    /// exactly these.
    fn frame_digests(&self) -> Vec<[u8; 32]> {
        self.parts
            .iter()
            .filter_map(|part| match (&part.key, &part.source) {
                (PartKey::Frame { .. } | PartKey::Blank { .. }, PartSource::Still(still)) => {
                    Some(still_digest(still))
                }
                _ => None,
            })
            .collect()
    }
}

fn still_digest(still: &StillH264) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(still.width.to_be_bytes());
    digest.update(still.height.to_be_bytes());
    digest.update(&still.access_unit);
    digest.finalize().into()
}

/// A lap laid on the clock from a sequence and an instant onward.
struct Era {
    from_logical: u64,
    from_unix_ms: u64,
    lap: Lap,
}

/// What an offered lap did to the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Splice {
    /// Same shape: pictures swapped under the running parts, no seam.
    InPlace,
    /// A new era opens at this sequence with a discontinuity.
    Era { at: u64 },
}

/// The schedule as it stands, and how far it has been made.
pub(crate) struct Timeline {
    resource: String,
    era: Era,
    /// The next logical sequence to materialise.
    next: u64,
    /// The part being materialised and its packager, so consecutive
    /// segments of one part share a timeline. A part that opens gets a fresh
    /// packager and a discontinuity.
    open: Option<(usize, HlsCatalogPackager)>,
    made_any: bool,
}

impl Timeline {
    /// Start a timeline: the lap from `epoch_unix_ms` as sequence 0, with
    /// the first segment to make being a trail behind the edge at `now`.
    pub(crate) fn new(resource: &str, lap: Lap, epoch_unix_ms: u64, now_unix_ms: u64) -> Self {
        let mut timeline = Self {
            resource: resource.to_string(),
            era: Era {
                from_logical: 0,
                from_unix_ms: epoch_unix_ms,
                lap,
            },
            next: 0,
            open: None,
            made_any: false,
        };
        timeline.next = timeline.edge(now_unix_ms).saturating_sub(TRAIL);
        timeline
    }

    /// The segment whose interval contains `now`.
    pub(crate) fn edge(&self, now_unix_ms: u64) -> u64 {
        let era = &self.era;
        let Some(elapsed) = now_unix_ms.checked_sub(era.from_unix_ms) else {
            return era.from_logical;
        };
        let laps = elapsed.checked_div(era.lap.lap_ms).unwrap_or(0);
        let within = elapsed.checked_rem(era.lap.lap_ms).unwrap_or(0);
        let index = era
            .lap
            .starts_ms
            .partition_point(|&start| start <= within)
            .saturating_sub(1)
            .min(era.lap.slots.len().saturating_sub(1));
        era.from_logical
            .saturating_add(laps.saturating_mul(era.lap.slot_count()))
            .saturating_add(u64::try_from(index).unwrap_or(u64::MAX))
    }

    /// Which lap and slot logical `n` falls in. `n` before the era is
    /// clamped to its first slot.
    fn locate(&self, n: u64) -> (u64, usize) {
        let offset = n.saturating_sub(self.era.from_logical);
        let count = self.era.lap.slot_count();
        let laps = offset.checked_div(count).unwrap_or(0);
        let index = usize::try_from(offset.checked_rem(count).unwrap_or(0)).unwrap_or(0);
        (laps, index)
    }

    /// When logical `n` begins on the wall clock.
    pub(crate) fn start_ms(&self, n: u64) -> u64 {
        let (laps, index) = self.locate(n);
        self.era
            .from_unix_ms
            .saturating_add(laps.saturating_mul(self.era.lap.lap_ms))
            .saturating_add(self.era.lap.starts_ms.get(index).copied().unwrap_or(0))
    }

    fn end_ms(&self, n: u64) -> u64 {
        let (_, index) = self.locate(n);
        self.start_ms(n).saturating_add(
            self.era
                .lap
                .slots
                .get(index)
                .map(|slot| u64::from(slot.duration_ms))
                .unwrap_or(0),
        )
    }

    /// The frame size the current lap is coded at.
    pub(crate) const fn frame(&self) -> (u32, u32) {
        self.era.lap.frame
    }

    /// The next sequence this timeline will make.
    pub(crate) const fn next(&self) -> u64 {
        self.next
    }

    /// The last sequence that begins within the lead of `now`: what the
    /// keeper makes through, so the runway is the same seconds whatever the
    /// slots are.
    pub(crate) fn target_through(&self, now_unix_ms: u64) -> u64 {
        let horizon = now_unix_ms.saturating_add(LEAD_MS);
        let mut n = self.edge(now_unix_ms);
        // Bounded by the lead over the shortest slot; a still is a second.
        for _ in 0..(LEAD_MS.checked_div(250).unwrap_or(1)) {
            let following = n.saturating_add(1);
            if self.start_ms(following) > horizon {
                break;
            }
            n = following;
        }
        n
    }

    /// Skip what the clock has already passed. A producer that was idle —
    /// the screen was off — resumes a trail behind the edge rather than
    /// making every segment nobody was there to fetch. The part it lands in
    /// is opened afresh, which is a discontinuity, as any part opening is.
    pub(crate) fn catch_up(&mut self, now_unix_ms: u64) {
        let behind = self.edge(now_unix_ms).saturating_sub(TRAIL);
        if behind > self.next {
            self.next = behind;
            self.open = None;
        }
    }

    /// Take a freshly built lap. Same shape: the parts' pictures are swapped
    /// under the running schedule and the next segment of each part carries
    /// the new one. A different shape opens a new era at the first segment
    /// nobody has been shown, on the same counter and the same clock.
    pub(crate) fn offer(&mut self, lap: Lap) -> Splice {
        if self.era.lap.same_shape(&lap) {
            for (current, fresh) in self.era.lap.parts.iter_mut().zip(lap.parts) {
                current.source = fresh.source;
            }
            return Splice::InPlace;
        }
        if !self.made_any {
            // Nothing has been shown: the era simply is the new lap.
            self.era.lap = lap;
            self.open = None;
            return Splice::Era {
                at: self.era.from_logical,
            };
        }
        let at = self.next;
        let from_unix_ms = self.end_ms(at.saturating_sub(1));
        self.era = Era {
            from_logical: at,
            from_unix_ms,
            lap,
        };
        self.open = None;
        Splice::Era { at }
    }

    /// Make the next segment. A part that opens — the first segment of the
    /// timeline aside — is a discontinuity, because the decoder parameters
    /// and the timeline origin change there; within a part the timestamps
    /// run on.
    pub(crate) async fn materialise_next(&mut self, reader: &dyn ClipReader) -> Result<HlsSegment> {
        let n = self.next;
        let (_, index) = self.locate(n);
        let slot = *self
            .era
            .lap
            .slots
            .get(index)
            .ok_or_else(|| anyhow!("timeline slot is missing"))?;
        let part = self
            .era
            .lap
            .parts
            .get(slot.part)
            .cloned()
            .ok_or_else(|| anyhow!("timeline part is missing"))?;
        let opens = slot.local == 0
            || !self
                .open
                .as_ref()
                .is_some_and(|(open, _)| *open == slot.part);
        if opens {
            let catalog = match &part.source {
                PartSource::Still(still) => super::live::still_catalog(&self.resource, still),
                PartSource::Clip { plan, .. } => plan.catalog.clone(),
            };
            let packager = HlsCatalogPackager::new(&catalog)
                .map_err(|error| anyhow!("program part cannot be HLS: {error}"))?;
            self.open = Some((slot.part, packager));
        }
        let groups = match &part.source {
            PartSource::Still(still) => {
                vec![super::live::still_group(&self.resource, still, slot.local)]
            }
            PartSource::Clip { resource, plan } => {
                let planned = plan
                    .plan(slot.local)
                    .ok_or_else(|| anyhow!("clip segment plan is missing"))?;
                let byteses = read_ranges(reader, resource, &planned.ranges).await?;
                plan.build(slot.local, &byteses)
                    .map_err(|error| anyhow!("clip segment would not build: {error}"))?
            }
        };
        let (_, packager) = self
            .open
            .as_mut()
            .ok_or_else(|| anyhow!("timeline has no open part"))?;
        let mut segment = None;
        for group in &groups {
            // The HLS rendition carries the video track; a clip's sound is
            // not something this stream carries, and a group the packager
            // does not know is refused rather than skipped.
            if group.header.track_kind != TrackKind::Video {
                continue;
            }
            if let Some(built) = packager
                .push_group(group)
                .map_err(|error| anyhow!("program group refused: {error}"))?
            {
                segment = Some(built);
            }
        }
        let mut segment = segment.ok_or_else(|| anyhow!("program slot produced no segment"))?;
        segment.rendition = self.resource.clone();
        segment.group_sequence = n;
        segment.discontinuity = (opens && self.made_any) || segment.discontinuity;
        self.made_any = true;
        self.next = n
            .checked_add(1)
            .ok_or_else(|| anyhow!("program stream sequence overflowed"))?;
        Ok(segment)
    }

    /// The rendition a master playlist describes. Parts differ in size and
    /// bitrate; the stream is described by the frame it is drawn for and the
    /// codec every part shares.
    pub(crate) fn description(&self) -> HlsRenditionDescription {
        let (width, height) = self.era.lap.frame;
        let bitrate_bps = self
            .era
            .lap
            .parts
            .iter()
            .map(|part| match &part.source {
                PartSource::Still(still) => {
                    u32::try_from(still.access_unit.len().saturating_mul(8)).unwrap_or(u32::MAX)
                }
                PartSource::Clip { plan, .. } => plan
                    .catalog
                    .tracks
                    .iter()
                    .map(|track| track.bitrate_bps)
                    .max()
                    .unwrap_or(0),
            })
            .max()
            .unwrap_or(0);
        let target_duration_ms = self
            .era
            .lap
            .slots
            .iter()
            .map(|slot| slot.duration_ms)
            .max()
            .unwrap_or(STILL_SEGMENT_MS)
            .max(MIN_TARGET_DURATION_MS);
        // Every codec a part carries, so a player that checks the master
        // playlist before decoding is told about the clip and not only the
        // stills.
        let mut codecs: Vec<String> = Vec::new();
        for part in &self.era.lap.parts {
            let found: Vec<String> = match &part.source {
                PartSource::Still(still) => vec![still.codec.clone()],
                PartSource::Clip { plan, .. } => plan
                    .catalog
                    .tracks
                    .iter()
                    .filter(|track| track.hls_v3_rendition.is_some())
                    .map(|track| track.codec.clone())
                    .collect(),
            };
            for codec in found {
                if !codecs.contains(&codec) {
                    codecs.push(codec);
                }
            }
        }
        HlsRenditionDescription {
            rendition: self.resource.clone(),
            target_duration_ms,
            codecs,
            width: Some(width),
            height: Some(height),
            bitrate_bps,
        }
    }
}

/// Encoded stills by digest, so a picture the program keeps showing is
/// encoded once, and one it stopped showing is let go.
#[derive(Default, Clone)]
pub(crate) struct StillCache {
    stills: BTreeMap<[u8; 32], Arc<StillH264>>,
}

impl StillCache {
    fn keep(&mut self, lap: &Lap) {
        let wanted = lap.frame_digests();
        self.stills.retain(|digest, _| wanted.contains(digest));
    }
}

/// Build the lap a rendered program describes. Stills are encoded off the
/// async runtime, once per distinct picture; a clip is planned from its table
/// of contents and never read whole.
pub(crate) async fn build_lap(
    projection: &DisplayProjection,
    cache: &mut StillCache,
    reader: &dyn ClipReader,
    viewport: (u32, u32),
) -> Result<Lap> {
    if let Some(reason) = unstreamable(projection) {
        return Err(anyhow!("program cannot be one stream: {reason}"));
    }
    // Clips first: a clip cannot be rescaled here, so the first one decides
    // the frame every still is fitted into; without one, the screen does.
    let mut plans: BTreeMap<String, Arc<StoredPlan>> = BTreeMap::new();
    let mut frame: Option<(u32, u32)> = None;
    for item in &projection.program.items {
        if let RenderedScene::Media(media) = &item.scene {
            if let MediaOrigin::Stored(content) = &media.origin {
                let resource = data_encoding::HEXLOWER.encode(content.as_bytes());
                if plans.contains_key(&resource) {
                    continue;
                }
                let total = reader.size(&resource).await?;
                let moov = fetch_moov(reader, &resource, total).await?;
                let policy = mediabox::CatalogPolicy {
                    max_group_duration_ms:
                        runtime::plane::live::media::DEFAULT_MAX_GROUP_DURATION_MS,
                    target_latency_ms: runtime::plane::live::media::DEFAULT_MAX_LATENCY_MS,
                    jitter_hint_ms: 50,
                    rendition: resource.clone(),
                };
                let plan = StoredPlan::from_moov(&moov, &policy)
                    .map_err(|error| anyhow!("clip would not plan: {error}"))?;
                let size = plan
                    .catalog
                    .tracks
                    .iter()
                    .find(|track| track.kind == TrackKind::Video)
                    .and_then(|track| Some((track.width?, track.height?)));
                match (frame, size) {
                    (None, Some(size)) => frame = Some(size),
                    (Some(chosen), Some(size)) if chosen != size => {
                        tracing::warn!(
                            ?chosen,
                            ?size,
                            "a second clip size in one program; the stream keeps the first, and this clip will change the decoder's resolution"
                        );
                    }
                    _ => {}
                }
                plans.insert(resource, Arc::new(plan));
            }
        }
    }
    let frame = frame.unwrap_or(viewport);
    let mut parts = Vec::with_capacity(projection.program.items.len());
    for item in &projection.program.items {
        let duration_ms = item.duration_ms.unwrap_or(DEFAULT_ITEM_MS).max(1);
        let part = match &item.scene {
            RenderedScene::Frame(picture) => {
                let digest = frame_digest(picture.media_type, &picture.bytes, frame);
                let still = match cache.stills.get(&digest) {
                    Some(still) => still.clone(),
                    None => {
                        let bytes = picture.bytes.clone();
                        let (width, height) = frame;
                        let still = tokio::task::spawn_blocking(move || {
                            mediabox::h264still::encode_still_fitted(&bytes, width, height)
                        })
                        .await
                        .context("still encode task")?
                        .map_err(|error| anyhow!("still could not be encoded: {error:?}"))?;
                        let still = Arc::new(still);
                        cache.stills.insert(digest, still.clone());
                        still
                    }
                };
                Part {
                    key: PartKey::Frame {
                        item: item.id.clone(),
                        width: still.width,
                        height: still.height,
                    },
                    duration_ms,
                    source: PartSource::Still(still),
                }
            }
            RenderedScene::Media(media) => match &media.origin {
                MediaOrigin::Stored(content) => {
                    let resource = data_encoding::HEXLOWER.encode(content.as_bytes());
                    let plan = plans
                        .get(&resource)
                        .cloned()
                        .ok_or_else(|| anyhow!("clip was not planned"))?;
                    Part {
                        key: PartKey::Clip {
                            content: resource.clone(),
                        },
                        duration_ms,
                        source: PartSource::Clip { resource, plan },
                    }
                }
                MediaOrigin::Live(_) => {
                    return Err(anyhow!("a live source has no finite bytes to schedule"))
                }
            },
            RenderedScene::StoredFrame(_) => {
                return Err(anyhow!("a stored frame reached the producer unresolved"))
            }
            RenderedScene::Blank(_) => {
                let (width, height) = frame;
                let digest = blank_digest(width, height);
                let still = match cache.stills.get(&digest) {
                    Some(still) => still.clone(),
                    None => {
                        let still = tokio::task::spawn_blocking(move || black_still(width, height))
                            .await
                            .context("black still task")??;
                        let still = Arc::new(still);
                        cache.stills.insert(digest, still.clone());
                        still
                    }
                };
                Part {
                    key: PartKey::Blank {
                        item: item.id.clone(),
                    },
                    duration_ms,
                    source: PartSource::Still(still),
                }
            }
        };
        parts.push(part);
    }
    let lap = Lap::from_parts(parts, frame)?;
    cache.keep(&lap);
    Ok(lap)
}

/// A picture's identity in the encode cache: its bytes and the frame it
/// was fitted into, since the same picture fitted elsewhere is another still.
fn frame_digest(media_type: FrameMediaType, bytes: &[u8], frame: (u32, u32)) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"frame");
    digest.update([match media_type {
        FrameMediaType::Png => 0u8,
        FrameMediaType::Jpeg => 1,
        FrameMediaType::WebP => 2,
    }]);
    digest.update(frame.0.to_be_bytes());
    digest.update(frame.1.to_be_bytes());
    digest.update(bytes);
    digest.finalize().into()
}

fn blank_digest(width: u32, height: u32) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"blank");
    digest.update(width.to_be_bytes());
    digest.update(height.to_be_bytes());
    digest.finalize().into()
}

/// A black card at the screen's own size, so a blank slot shares the
/// decoder parameters of the cards around it.
fn black_still(width: u32, height: u32) -> Result<StillH264> {
    let pixels = usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
        .and_then(|count| count.checked_mul(4))
        .ok_or_else(|| anyhow!("blank card size overflows"))?;
    let mut black = vec![0u8; pixels];
    for pixel in black.chunks_exact_mut(4) {
        if let Some(alpha) = pixel.get_mut(3) {
            *alpha = 255;
        }
    }
    mediabox::h264still::encode_still(&black, width, height)
        .map_err(|error| anyhow!("black card could not be encoded: {error:?}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A reader for programs with no clips in them.
    pub(crate) struct NoClips;

    impl ClipReader for NoClips {
        fn size<'a>(&'a self, _resource: &'a str) -> ReadFuture<'a, u64> {
            Box::pin(async { Err(anyhow!("this program has no clips")) })
        }
        fn read<'a>(
            &'a self,
            _resource: &'a str,
            _offset: u64,
            _len: u64,
        ) -> ReadFuture<'a, Vec<u8>> {
            Box::pin(async { Err(anyhow!("this program has no clips")) })
        }
    }

    /// One flat-shaded still, encoded for real.
    pub(crate) fn still(shade: u8, width: u32, height: u32) -> Arc<StillH264> {
        let mut rgba = vec![shade; (width * height * 4) as usize];
        for pixel in rgba.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        Arc::new(mediabox::h264still::encode_still(&rgba, width, height).unwrap())
    }

    /// A lap of flat 64x48 stills: `(item, shade, duration_ms)` each.
    pub(crate) fn lap(parts: &[(&str, u8, u32)]) -> Lap {
        Lap::from_parts(
            parts
                .iter()
                .map(|&(item, shade, duration_ms)| Part {
                    key: PartKey::Frame {
                        item: item.into(),
                        width: 64,
                        height: 48,
                    },
                    duration_ms,
                    source: PartSource::Still(still(shade, 64, 48)),
                })
                .collect(),
            (64, 48),
        )
        .unwrap()
    }

    /// Materialise the next `count` segments of a clip-free timeline.
    pub(crate) async fn make(timeline: &mut Timeline, count: usize) -> Vec<HlsSegment> {
        let mut out = Vec::new();
        for _ in 0..count {
            out.push(timeline.materialise_next(&NoClips).await.unwrap());
        }
        out
    }

    /// A reader that records what it was asked for and answers from one
    /// byte string.
    struct Recording {
        bytes: Vec<u8>,
        calls: std::sync::Mutex<Vec<(u64, u64)>>,
    }

    impl ClipReader for Recording {
        fn size<'a>(&'a self, _resource: &'a str) -> ReadFuture<'a, u64> {
            Box::pin(async move { Ok(self.bytes.len() as u64) })
        }
        fn read<'a>(
            &'a self,
            _resource: &'a str,
            offset: u64,
            len: u64,
        ) -> ReadFuture<'a, Vec<u8>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push((offset, len));
                Ok(self.bytes[offset as usize..(offset + len) as usize].to_vec())
            })
        }
    }

    /// Interleaved samples are read as one span and sliced; samples spread
    /// far apart are read one by one. Either way every sample comes back
    /// exactly as it lies.
    #[tokio::test]
    async fn a_clip_segments_samples_are_read_in_one_span_when_they_are_close() {
        let reader = Recording {
            bytes: (0..=255u8).cycle().take(4096).collect(),
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let close = [(100u64, 50u32), (200, 50), (300, 50)];
        let got = read_ranges(&reader, "clip", &close).await.unwrap();
        assert_eq!(reader.calls.lock().unwrap().as_slice(), &[(100, 250)]);
        assert_eq!(got[1], reader.bytes[200..250].to_vec());
        reader.calls.lock().unwrap().clear();
        let far = [(0u64, 10u32), (4000, 10)];
        let got = read_ranges(&reader, "clip", &far).await.unwrap();
        assert_eq!(reader.calls.lock().unwrap().len(), 2);
        assert_eq!(got[1], reader.bytes[4000..4010].to_vec());
    }

    /// The whole reason the producer exists, as the loop test used to state
    /// it: the stream is endless, a lap past the first is the same bytes
    /// behind a discontinuity, and the media sequence only ever grows.
    #[tokio::test]
    async fn a_program_is_served_as_an_endless_looping_live_stream() {
        // Two stills, 2 s and 1 s: a lap of three segments.
        let mut timeline = Timeline::new(
            "prog-1",
            lap(&[("a", 40, 2_000), ("b", 200, 1_000)]),
            1_000,
            1_000,
        );
        assert_eq!(timeline.edge(1_000), 0);
        assert_eq!(timeline.edge(2_999), 1);
        assert_eq!(timeline.edge(3_000), 2);
        assert_eq!(
            timeline.edge(4_000),
            3,
            "the second lap starts at sequence 3"
        );
        assert_eq!(timeline.start_ms(3), 4_000);
        assert_eq!(timeline.start_ms(4), 5_000);

        let first_lap = make(&mut timeline, 3).await;
        let second_lap = make(&mut timeline, 3).await;
        let sequences: Vec<u64> = first_lap
            .iter()
            .chain(&second_lap)
            .map(|s| s.group_sequence)
            .collect();
        assert_eq!(sequences, vec![0, 1, 2, 3, 4, 5], "one monotonic sequence");
        assert!(
            !first_lap[0].discontinuity,
            "the first segment ever has nothing to reset from"
        );
        assert!(
            !first_lap[1].discontinuity,
            "a still's second second runs on"
        );
        assert!(first_lap[2].discontinuity, "a part seam is a decoder reset");
        assert!(
            second_lap[0].discontinuity,
            "the wrap is a part seam like any other"
        );
        assert_eq!(
            first_lap[0].bytes, second_lap[0].bytes,
            "a lap past the first is the same bytes"
        );
        assert_eq!(first_lap[0].bytes.len() % 188, 0, "a real transport stream");
        assert_eq!(first_lap[0].duration_ms, 1_000);
    }

    /// A card whose pixels change but whose place does not is swapped under
    /// the running part: the next segment carries the new picture with no
    /// seam, and the sequence and clock are untouched.
    #[tokio::test]
    async fn a_changed_picture_is_swapped_in_place_without_a_seam() {
        let mut timeline = Timeline::new("prog-1", lap(&[("clock", 40, 3_000)]), 0, 0);
        let before = make(&mut timeline, 1).await;
        assert_eq!(
            timeline.offer(lap(&[("clock", 90, 3_000)])),
            Splice::InPlace
        );
        let after = make(&mut timeline, 2).await;
        assert_eq!(after[0].group_sequence, 1);
        assert!(
            !after[0].discontinuity,
            "same part, same parameters: no seam"
        );
        assert_ne!(
            before[0].bytes, after[0].bytes,
            "the new picture is what plays"
        );
        assert!(!after[1].discontinuity);
        assert_eq!(timeline.start_ms(3), 3_000, "the clock did not move");
    }

    /// A program whose shape changed opens a new era at the first segment
    /// nobody has been shown, on the same counter, and the segments already
    /// made are not touched.
    #[tokio::test]
    async fn a_reshaped_program_splices_at_the_edge_and_never_rewinds() {
        let mut timeline =
            Timeline::new("prog-1", lap(&[("a", 40, 2_000), ("b", 200, 2_000)]), 0, 0);
        let shown = make(&mut timeline, 3).await;
        assert_eq!(timeline.next(), 3);
        // Cut to a program of one 1 s slide.
        let splice = timeline.offer(lap(&[("c", 120, 1_000)]));
        assert_eq!(splice, Splice::Era { at: 3 });
        assert_eq!(
            timeline.start_ms(3),
            3_000,
            "the era begins where the last shown segment ended"
        );
        assert_eq!(timeline.edge(3_500), 3);
        assert_eq!(timeline.edge(5_000), 5, "the new lap is one segment long");
        let spliced = make(&mut timeline, 2).await;
        assert_eq!(spliced[0].group_sequence, 3);
        assert!(spliced[0].discontinuity, "a new era opens with a reset");
        assert!(
            spliced[1].discontinuity,
            "and a one-segment lap wraps every segment"
        );
        assert_eq!(shown.last().unwrap().group_sequence, 2);
    }

    /// The lead is seconds of media, whatever the slots: over one-second
    /// stills it is many segments, over long clip slots it is few.
    #[test]
    fn the_lead_is_measured_in_media_time_not_segments() {
        let timeline = Timeline::new("prog-1", lap(&[("a", 40, 30_000)]), 0, 0);
        assert_eq!(timeline.target_through(0), LEAD_MS / 1_000);
        assert_eq!(timeline.target_through(5_000), 5 + LEAD_MS / 1_000);
        // A lap of one 30 s still: the same in the second lap.
        assert_eq!(timeline.target_through(30_000), 30 + LEAD_MS / 1_000);
    }

    /// The trail behind the edge is where a cold start begins, so a receiver
    /// joining late has a window to buffer from; a start before the epoch
    /// begins at the epoch; and a producer that slept skips to the trail
    /// rather than making what nobody fetched.
    #[tokio::test]
    async fn a_cold_start_begins_a_trail_behind_the_edge() {
        let timeline = Timeline::new("prog-1", lap(&[("a", 40, 1_000)]), 0, 10_000);
        assert_eq!(timeline.next(), 10 - TRAIL);
        let early = Timeline::new("prog-1", lap(&[("a", 40, 1_000)]), 50_000, 10_000);
        assert_eq!(early.next(), 0);
        assert_eq!(early.edge(10_000), 0);

        let mut slept = Timeline::new("prog-1", lap(&[("a", 40, 2_000), ("b", 90, 1_000)]), 0, 0);
        make(&mut slept, 2).await;
        slept.catch_up(1_000);
        assert_eq!(slept.next(), 2, "not behind: nothing to skip");
        slept.catch_up(3_600_000);
        assert_eq!(slept.next(), 3_600 - TRAIL);
        let resumed = make(&mut slept, 1).await;
        assert_eq!(resumed[0].group_sequence, 3_600 - TRAIL);
        assert!(resumed[0].discontinuity, "a part opened mid-way is a reset");
    }
}
