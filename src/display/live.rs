//! Coordinator-owned native-media ingest and bounded receiver fanout.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use runtime::plane::live::media::{
    Catalog, Control, Event, EventBody, ReceivedGroup, RequestKeyframe, Session, Subscribe,
    TrackKind, CATALOG_TRACK, DEFAULT_MAX_LATENCY_MS,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::daemon::OrbitAddress;
use crate::orbits::Router;

use super::{
    CmafCatalogPackager, CmafFragment, CmafTrackDescription, HlsCatalogPackager,
    HlsRenditionDescription, HlsSegment,
};

const CATALOG_SUBSCRIPTION_ID: u64 = 1;
const FIRST_MEDIA_SUBSCRIPTION_ID: u64 = 2;
const RETAINED_SEGMENTS: usize = 6;
const RECEIVER_QUEUE: usize = 16;
/// The peer slot for a presentation no peer produced.
const STORED_SOURCE: &str = "stored";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveTransport {
    Mse,
    Hls,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveMediaTrack {
    pub rendition: String,
    pub kind: String,
    pub mime_type: String,
    pub timescale: u32,
    pub target_latency_ms: u32,
    pub render_group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveMediaPacket {
    Init {
        rendition: String,
        bytes: Vec<u8>,
    },
    Fragment {
        rendition: String,
        group_sequence: u64,
        published_at_micros: i64,
        start_timestamp: u64,
        duration: u64,
        discontinuity: bool,
        bytes: Vec<u8>,
    },
}

pub struct LiveMediaSnapshot {
    pub tracks: Vec<LiveMediaTrack>,
    pub packets: Vec<LiveMediaPacket>,
    pub updates: broadcast::Receiver<LiveMediaPacket>,
}

#[derive(Clone, Default)]
pub struct LiveMediaHub {
    inner: Arc<Mutex<HubState>>,
}

#[derive(Default)]
struct HubState {
    active_orbits: BTreeSet<String>,
    presentations: BTreeMap<PresentationKey, Presentation>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PresentationKey {
    orbit: String,
    peer: String,
    connection: String,
}

/// How much of a presentation the hub keeps, and whether more is coming.
///
/// These were one constant applied at two call sites, which is the same as
/// saying every source is a live edge. A live edge keeps the last few because
/// nothing will ask for what fell off it, and never completes — the peer
/// disconnects and the presentation goes with it. A finite source keeps
/// everything, because a player joining late still has to reach its first
/// segment, and completes, which is what lets the playlist say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retention {
    /// Keep the last `n`. There is always a next one.
    Rolling(usize),
    /// Keep every segment. `complete` once the last group is in.
    Whole { complete: bool },
}

impl Retention {
    fn trim<T>(self, retained: &mut VecDeque<T>) {
        if let Self::Rolling(depth) = self {
            while retained.len() > depth {
                retained.pop_front();
            }
        }
    }

    /// Whether a playlist built from this may declare its end.
    const fn is_complete(self) -> bool {
        matches!(self, Self::Whole { complete: true })
    }
}

struct Presentation {
    cmaf_tracks: Vec<CmafTrackDescription>,
    cmaf_fragments: BTreeMap<String, VecDeque<CmafFragment>>,
    hls_renditions: Vec<HlsRenditionDescription>,
    hls_segments: BTreeMap<String, VecDeque<HlsSegment>>,
    retention: Retention,
    /// For a planned presentation: the table a segment is built from on demand.
    ///
    /// `install_whole` materialises every segment, which is right for a clip
    /// and wrong for a film — two hours at two-second groups is ~3,600 segments
    /// held for the life of the presentation. A plan holds the *table* instead;
    /// the playlist is rendered from it, and a segment's bytes are read and
    /// packaged when that segment is asked for.
    plan: Option<Arc<super::StoredPlan>>,
    updates: broadcast::Sender<LiveMediaPacket>,
}

struct SourceSession {
    session: Session,
    catalog_requested: bool,
    next_subscription_id: u64,
    subscribed_tracks: BTreeSet<String>,
    video_tracks: BTreeSet<String>,
    cmaf: Option<CmafCatalogPackager>,
    hls: Option<HlsCatalogPackager>,
}

impl SourceSession {
    fn new(session: Session) -> Self {
        Self {
            session,
            catalog_requested: false,
            next_subscription_id: FIRST_MEDIA_SUBSCRIPTION_ID,
            subscribed_tracks: BTreeSet::new(),
            video_tracks: BTreeSet::new(),
            cmaf: None,
            hls: None,
        }
    }
}

impl LiveMediaHub {
    /// Ensure this coordinator consumes media for the assigned Orbit. The
    /// task is one-per-Orbit no matter how many receivers are assigned.
    pub async fn ensure_orbit(&self, router: Arc<Router>, address: OrbitAddress) -> Result<()> {
        let orbit = orbit_key(&address);
        {
            let mut state = lock(&self.inner)?;
            if !state.active_orbits.insert(orbit.clone()) {
                return Ok(());
            }
        }
        let subscription = router.live_media(&address).await;
        let (sessions, events) = match subscription {
            Ok(subscription) => subscription,
            Err(error) => {
                lock(&self.inner)?.active_orbits.remove(&orbit);
                return Err(error);
            }
        };
        let hub = self.clone();
        tokio::spawn(async move {
            if let Err(error) = consume_orbit(hub.clone(), orbit.clone(), sessions, events).await {
                tracing::debug!(%error, %orbit, "display live-media ingest ended");
            }
            hub.remove_orbit(&orbit);
        });
        Ok(())
    }

    /// Install a presentation from a finite, already-written sequence of groups.
    ///
    /// The packagers are the reusable half of this plane: they take a `Catalog`
    /// and `ReceivedGroup`s and know nothing about where either came from —
    /// they are already unit-tested from struct literals. What was missing was
    /// any way to put one behind a presentation without a live session to hang
    /// it off, so a stored content had no route to a receiver even though every
    /// piece between the two existed.
    ///
    /// Keyed under `STORED_SOURCE` rather than a peer and connection, because a
    /// finite source has neither. `unique_presentation` still requires the
    /// resource to be unique per Orbit, so a stored id and a live rendition of
    /// the same name refuse each other rather than one shadowing the other.
    pub fn install_whole(
        &self,
        orbit: &str,
        resource: &str,
        catalog: &Catalog,
        groups: impl IntoIterator<Item = ReceivedGroup>,
    ) -> Result<()> {
        let mut cmaf = CmafCatalogPackager::new(catalog).ok();
        let mut hls = HlsCatalogPackager::new(catalog).ok();
        if cmaf.is_none() && hls.is_none() {
            return Err(anyhow!("no rendition in this catalog can be packaged"));
        }
        let mut cmaf_fragments: BTreeMap<String, VecDeque<CmafFragment>> = BTreeMap::new();
        let mut hls_segments: BTreeMap<String, VecDeque<HlsSegment>> = BTreeMap::new();
        for group in groups {
            if let Some(packager) = cmaf.as_mut() {
                match packager.push_group(&group) {
                    Ok(fragment) => cmaf_fragments
                        .entry(fragment.rendition)
                        .or_default()
                        .push_back(fragment.fragment),
                    // A group the CMAF side cannot take is not fatal to the HLS
                    // side, which packages a different subset of the catalog.
                    Err(error) => {
                        tracing::debug!(%error, "stored group refused by the CMAF packager")
                    }
                }
            }
            if let Some(packager) = hls.as_mut() {
                match packager.push_group(&group) {
                    Ok(Some(segment)) => hls_segments
                        .entry(segment.rendition.clone())
                        .or_default()
                        .push_back(segment),
                    Ok(None) => {}
                    Err(error) => {
                        tracing::debug!(%error, "stored group refused by the HLS packager")
                    }
                }
            }
        }
        if cmaf_fragments.is_empty() && hls_segments.is_empty() {
            return Err(anyhow!("no group in this content produced a segment"));
        }
        let (updates, _) = broadcast::channel(RECEIVER_QUEUE);
        let mut state = lock(&self.inner)?;
        state.presentations.insert(
            PresentationKey {
                orbit: orbit.to_string(),
                peer: STORED_SOURCE.into(),
                connection: resource.to_string(),
            },
            Presentation {
                cmaf_tracks: cmaf
                    .as_ref()
                    .map(|packager| packager.descriptions().to_vec())
                    .unwrap_or_default(),
                cmaf_fragments,
                hls_renditions: hls
                    .as_ref()
                    .map(|packager| packager.descriptions().to_vec())
                    .unwrap_or_default(),
                hls_segments,
                // Every group is already in, so this is complete on arrival.
                retention: Retention::Whole { complete: true },
                plan: None,
                updates,
            },
        );
        Ok(())
    }

    /// Install a planned presentation: the catalog and the table, no segments.
    ///
    /// The playlist is rendered from the plan and every segment is built when
    /// it is asked for, so what this holds is proportional to the sample table
    /// rather than to the film.
    pub fn install_planned(
        &self,
        orbit: &str,
        resource: &str,
        plan: super::StoredPlan,
    ) -> Result<()> {
        let catalog = plan.catalog.clone();
        let hls = HlsCatalogPackager::new(&catalog)
            .map_err(|error| anyhow!("this catalog cannot be packaged: {error}"))?;
        let (updates, _) = broadcast::channel(RECEIVER_QUEUE);
        let mut state = lock(&self.inner)?;
        state.presentations.insert(
            PresentationKey {
                orbit: orbit.to_string(),
                peer: STORED_SOURCE.into(),
                connection: resource.to_string(),
            },
            Presentation {
                cmaf_tracks: Vec::new(),
                cmaf_fragments: BTreeMap::new(),
                hls_renditions: hls.descriptions().to_vec(),
                hls_segments: BTreeMap::new(),
                retention: Retention::Whole { complete: true },
                plan: Some(Arc::new(plan)),
                updates,
            },
        );
        Ok(())
    }

    /// Which bytes a planned segment needs, answered off the lock.
    ///
    /// Reading a film's bytes under the hub mutex would stall every other
    /// presentation behind one range request, so the lock is held only long
    /// enough to clone an `Arc` of the plan.
    pub fn planned_segment(
        &self,
        orbit: &str,
        resource: &str,
        sequence: u64,
    ) -> Result<(Arc<super::StoredPlan>, super::SegmentPlan)> {
        let plan = {
            let state = lock(&self.inner)?;
            let (_, presentation) =
                unique_presentation(&state, orbit, resource, LiveTransport::Hls)?;
            presentation
                .plan
                .clone()
                .ok_or_else(|| anyhow!("this presentation is not planned"))?
        };
        let index = usize::try_from(sequence).map_err(|_| anyhow!("segment sequence overflow"))?;
        let segment = plan
            .plan(index)
            .ok_or_else(|| anyhow!("HLS segment is not in this presentation"))?;
        Ok((plan, segment))
    }

    /// Build and mux one planned segment from bytes read against its plan.
    ///
    /// A fresh packager per segment is what makes random access possible: the
    /// live packagers refuse a sequence that does not advance, and a fresh one
    /// has no last sequence to advance past — group N is its first group.
    pub fn package_planned(
        plan: &super::StoredPlan,
        sequence: u64,
        bytes: &[Vec<u8>],
    ) -> Result<Vec<u8>> {
        let index = usize::try_from(sequence).map_err(|_| anyhow!("segment sequence overflow"))?;
        let groups = plan
            .build(index, bytes)
            .map_err(|error| anyhow!("segment {sequence} would not build: {error}"))?;
        let mut packager = HlsCatalogPackager::new(&plan.catalog)
            .map_err(|error| anyhow!("this catalog cannot be packaged: {error}"))?;
        let mut out = None;
        for group in &groups {
            if let Some(segment) = packager
                .push_group(group)
                .map_err(|error| anyhow!("segment {sequence} refused: {error}"))?
            {
                out = Some(segment.bytes);
            }
        }
        out.ok_or_else(|| anyhow!("segment {sequence} produced no transport stream"))
    }

    pub fn mse_snapshot(&self, orbit: &str, resource: &str) -> Result<LiveMediaSnapshot> {
        let state = lock(&self.inner)?;
        let (_, presentation) = unique_presentation(&state, orbit, resource, LiveTransport::Mse)?;
        let tracks = presentation
            .cmaf_tracks
            .iter()
            .filter(|track| cmaf_matches(track, resource))
            .map(|track| LiveMediaTrack {
                rendition: track.rendition.clone(),
                kind: track_kind_name(track.kind).into(),
                mime_type: track.mime_type.clone(),
                timescale: track.timescale,
                target_latency_ms: track.target_latency_ms,
                render_group: track.render_group.clone(),
            })
            .collect::<Vec<_>>();
        let mut packets = Vec::new();
        for track in presentation
            .cmaf_tracks
            .iter()
            .filter(|track| cmaf_matches(track, resource))
        {
            packets.push(LiveMediaPacket::Init {
                rendition: track.rendition.clone(),
                bytes: track.init_segment.clone(),
            });
            if let Some(fragments) = presentation.cmaf_fragments.get(&track.rendition) {
                packets.extend(fragments.iter().cloned().map(|fragment| {
                    LiveMediaPacket::Fragment {
                        rendition: track.rendition.clone(),
                        group_sequence: fragment.group_sequence,
                        published_at_micros: fragment.published_at_micros,
                        start_timestamp: fragment.start_timestamp,
                        duration: fragment.duration,
                        discontinuity: fragment.discontinuity,
                        bytes: fragment.bytes,
                    }
                }));
            }
        }
        Ok(LiveMediaSnapshot {
            tracks,
            packets,
            updates: presentation.updates.subscribe(),
        })
    }

    pub fn hls_master(&self, orbit: &str, resource: &str, base: &str) -> Result<String> {
        let state = lock(&self.inner)?;
        let (_, presentation) = unique_presentation(&state, orbit, resource, LiveTransport::Hls)?;
        let renditions = presentation
            .hls_renditions
            .iter()
            .filter(|rendition| rendition.rendition == resource)
            .collect::<Vec<_>>();
        if renditions.is_empty() {
            return Err(anyhow!("live HLS resource is unavailable"));
        }
        let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
        for rendition in renditions {
            playlist.push_str("#EXT-X-STREAM-INF:BANDWIDTH=");
            playlist.push_str(&rendition.bitrate_bps.to_string());
            if !rendition.codecs.is_empty() {
                playlist.push_str(",CODECS=\"");
                playlist.push_str(&rendition.codecs.join(","));
                playlist.push('"');
            }
            if let (Some(width), Some(height)) = (rendition.width, rendition.height) {
                playlist.push_str(",RESOLUTION=");
                playlist.push_str(&format!("{width}x{height}"));
            }
            playlist.push('\n');
            playlist.push_str(base);
            playlist.push_str("/renditions/");
            playlist.push_str(&rendition.rendition);
            playlist.push_str(".m3u8\n");
        }
        Ok(playlist)
    }

    pub fn hls_media_playlist(
        &self,
        orbit: &str,
        resource: &str,
        rendition: &str,
        base: &str,
    ) -> Result<String> {
        if resource != rendition {
            return Err(anyhow!("HLS rendition is not assignment-bound"));
        }
        let state = lock(&self.inner)?;
        let (_, presentation) = unique_presentation(&state, orbit, resource, LiveTransport::Hls)?;
        let description = presentation
            .hls_renditions
            .iter()
            .find(|candidate| candidate.rendition == rendition)
            .ok_or_else(|| anyhow!("HLS rendition is unavailable"))?;
        if let Some(plan) = &presentation.plan {
            // A planned presentation lists every group from its table; no
            // segment has to exist for the playlist to be complete.
            let count = plan.group_count();
            let target_seconds = description
                .target_duration_ms
                .checked_add(999)
                .and_then(|value| value.checked_div(1_000))
                .unwrap_or(1)
                .max(1);
            let mut playlist = format!(
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target_seconds}\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n",
            );
            for sequence in 0..count {
                let duration = plan.group_duration_ms(sequence).unwrap_or(1);
                playlist.push_str(&format!(
                    "#EXTINF:{:.3},\n{base}/segments/{sequence}.ts\n",
                    f64::from(duration) / 1_000.0,
                ));
            }
            playlist.push_str("#EXT-X-ENDLIST\n");
            return Ok(playlist);
        }
        let segments = presentation
            .hls_segments
            .get(rendition)
            .ok_or_else(|| anyhow!("HLS live edge is not ready"))?;
        let first = segments
            .front()
            .ok_or_else(|| anyhow!("HLS live edge is not ready"))?;
        let target_seconds = description
            .target_duration_ms
            .checked_add(999)
            .and_then(|value| value.checked_div(1_000))
            .unwrap_or(1)
            .max(1);
        let mut playlist = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target_seconds}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
            first.group_sequence
        );
        if presentation.retention.is_complete() {
            playlist.push_str("#EXT-X-PLAYLIST-TYPE:VOD\n");
        }
        for segment in segments {
            if segment.discontinuity {
                playlist.push_str("#EXT-X-DISCONTINUITY\n");
            }
            playlist.push_str(&format!(
                "#EXTINF:{:.3},\n{base}/segments/{}.ts\n",
                f64::from(segment.duration_ms) / 1_000.0,
                segment.group_sequence
            ));
        }
        if presentation.retention.is_complete() {
            playlist.push_str("#EXT-X-ENDLIST\n");
        }
        Ok(playlist)
    }

    pub fn hls_segment(
        &self,
        orbit: &str,
        resource: &str,
        rendition: &str,
        sequence: u64,
    ) -> Result<Vec<u8>> {
        if resource != rendition {
            return Err(anyhow!("HLS rendition is not assignment-bound"));
        }
        let state = lock(&self.inner)?;
        let (_, presentation) = unique_presentation(&state, orbit, resource, LiveTransport::Hls)?;
        presentation
            .hls_segments
            .get(rendition)
            .and_then(|segments| {
                // Both packagers refuse a sequence that does not advance, so a
                // deque is sorted by construction. A linear scan cost nothing
                // over six retained segments; over a whole film a player walks
                // every one of them and pays for the walk each time.
                segments
                    .binary_search_by_key(&sequence, |segment| segment.group_sequence)
                    .ok()
                    .and_then(|index| segments.get(index))
            })
            .map(|segment| segment.bytes.clone())
            .ok_or_else(|| match presentation.retention {
                Retention::Rolling(_) => anyhow!("HLS segment is outside the live window"),
                Retention::Whole { .. } => anyhow!("HLS segment is not in this presentation"),
            })
    }

    pub fn has_resource(&self, orbit: &str, resource: &str, transport: LiveTransport) -> bool {
        lock(&self.inner)
            .and_then(|state| {
                let (_, presentation) = unique_presentation(&state, orbit, resource, transport)?;
                if transport == LiveTransport::Hls
                    && !presentation
                        .hls_segments
                        .get(resource)
                        .is_some_and(|segments| !segments.is_empty())
                {
                    return Err(anyhow!("HLS live edge is not ready"));
                }
                Ok(())
            })
            .is_ok()
    }

    fn remove_orbit(&self, orbit: &str) {
        if let Ok(mut state) = lock(&self.inner) {
            state.active_orbits.remove(orbit);
            state.presentations.retain(|key, _| key.orbit != orbit);
        }
    }
}

async fn consume_orbit(
    hub: LiveMediaHub,
    orbit: String,
    sessions: Vec<Session>,
    mut events: broadcast::Receiver<Event>,
) -> Result<()> {
    let mut sources = BTreeMap::<(String, String), SourceSession>::new();
    for session in sessions {
        let key = session_key(&session);
        sources.insert(key, SourceSession::new(session));
    }
    for source in sources.values_mut() {
        request_catalog(source).await;
    }
    loop {
        match events.recv().await {
            Ok(event) => handle_event(&hub, &orbit, &mut sources, event).await,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                // Re-establish subscriptions at the newest Group. Existing
                // packagers deliberately retain no unbounded catch-up state.
                for source in sources.values() {
                    request_keyframes(source).await;
                }
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

async fn handle_event(
    hub: &LiveMediaHub,
    orbit: &str,
    sources: &mut BTreeMap<(String, String), SourceSession>,
    event: Event,
) {
    let session_id = (
        event.peer.to_string(),
        data_encoding::HEXLOWER.encode(&event.connection_id),
    );
    match event.body {
        EventBody::Connected => {
            let source = sources
                .entry(session_id)
                .or_insert_with(|| SourceSession::new(event.session));
            request_catalog(source).await;
        }
        EventBody::Disconnected => {
            sources.remove(&session_id);
            remove_presentation(hub, orbit, &session_id);
        }
        EventBody::Control(Control::Setup(_)) => {
            if let Some(source) = sources.get_mut(&session_id) {
                request_catalog(source).await;
            }
        }
        EventBody::Group(group) if group.header.track == CATALOG_TRACK => {
            let Some(source) = sources.get_mut(&session_id) else {
                return;
            };
            let Ok(catalog) = Catalog::from_group(&group) else {
                return;
            };
            install_catalog(hub, orbit, &session_id, source, catalog).await;
        }
        EventBody::Group(group) => {
            let Some(source) = sources.get_mut(&session_id) else {
                return;
            };
            let mut request_keyframe = false;
            if let Some(packager) = source.cmaf.as_mut() {
                match packager.push_group(&group) {
                    Ok(fragment) => {
                        request_keyframe |= fragment.fragment.discontinuity;
                        publish_cmaf(hub, orbit, &session_id, fragment);
                    }
                    Err(_) => request_keyframe = group.header.track_kind == TrackKind::Video,
                }
            }
            if let Some(packager) = source.hls.as_mut() {
                match packager.push_group(&group) {
                    Ok(Some(segment)) => publish_hls(hub, orbit, &session_id, segment),
                    Ok(None) => {}
                    Err(_) => request_keyframe |= group.header.track_kind == TrackKind::Video,
                }
            }
            if request_keyframe {
                let _ = source
                    .session
                    .send(Control::RequestKeyframe(RequestKeyframe {
                        track: group.header.track.clone(),
                    }))
                    .await;
            }
        }
        EventBody::Control(_) | EventBody::Fetch(_) => {}
    }
}

async fn request_catalog(source: &mut SourceSession) {
    if source.catalog_requested || !source.session.is_ready() {
        return;
    }
    if source
        .session
        .send(Control::Subscribe(Subscribe {
            subscription_id: CATALOG_SUBSCRIPTION_ID,
            track: CATALOG_TRACK.into(),
            priority: 0,
            ordered: false,
            max_latency_ms: DEFAULT_MAX_LATENCY_MS,
            start_group: None,
            end_group: None,
        }))
        .await
        .is_ok()
    {
        source.catalog_requested = true;
    }
}

async fn install_catalog(
    hub: &LiveMediaHub,
    orbit: &str,
    session_id: &(String, String),
    source: &mut SourceSession,
    catalog: Catalog,
) {
    let cmaf = CmafCatalogPackager::new(&catalog).ok();
    let hls = HlsCatalogPackager::new(&catalog).ok();
    if cmaf.is_none() && hls.is_none() {
        return;
    }
    let cmaf_tracks = cmaf
        .as_ref()
        .map(|packager| packager.descriptions().to_vec())
        .unwrap_or_default();
    let hls_renditions = hls
        .as_ref()
        .map(|packager| packager.descriptions().to_vec())
        .unwrap_or_default();
    let wanted = catalog
        .tracks
        .iter()
        .filter(|track| track.cmaf_rendition.is_some() || track.hls_v3_rendition.is_some())
        .map(|track| track.track.clone())
        .collect::<Vec<_>>();
    source.video_tracks = catalog
        .tracks
        .iter()
        .filter(|track| {
            track.kind == TrackKind::Video
                && (track.cmaf_rendition.is_some() || track.hls_v3_rendition.is_some())
        })
        .map(|track| track.track.clone())
        .collect();
    for track in wanted {
        if source.subscribed_tracks.contains(&track) {
            continue;
        }
        let subscription_id = source.next_subscription_id;
        source.next_subscription_id = match source.next_subscription_id.checked_add(1) {
            Some(next) => next,
            None => return,
        };
        if source
            .session
            .send(Control::Subscribe(Subscribe {
                subscription_id,
                track: track.clone(),
                priority: 1,
                ordered: false,
                max_latency_ms: DEFAULT_MAX_LATENCY_MS,
                start_group: None,
                end_group: None,
            }))
            .await
            .is_ok()
        {
            source.subscribed_tracks.insert(track);
        }
    }
    source.cmaf = cmaf;
    source.hls = hls;
    let (updates, _) = broadcast::channel(RECEIVER_QUEUE);
    if let Ok(mut state) = lock(&hub.inner) {
        state.presentations.insert(
            presentation_key(orbit, session_id),
            Presentation {
                cmaf_tracks,
                cmaf_fragments: BTreeMap::new(),
                hls_renditions,
                hls_segments: BTreeMap::new(),
                retention: Retention::Rolling(RETAINED_SEGMENTS),
                plan: None,
                updates,
            },
        );
    }
    request_keyframes(source).await;
}

async fn request_keyframes(source: &SourceSession) {
    let tracks = source
        .video_tracks
        .intersection(&source.subscribed_tracks)
        .cloned()
        .collect::<Vec<_>>();
    for track in tracks {
        let _ = source
            .session
            .send(Control::RequestKeyframe(RequestKeyframe { track }))
            .await;
    }
}

fn publish_cmaf(
    hub: &LiveMediaHub,
    orbit: &str,
    session_id: &(String, String),
    fragment: super::CmafRenditionFragment,
) {
    let Ok(mut state) = lock(&hub.inner) else {
        return;
    };
    let Some(presentation) = state
        .presentations
        .get_mut(&presentation_key(orbit, session_id))
    else {
        return;
    };
    let retention = presentation.retention;
    let retained = presentation
        .cmaf_fragments
        .entry(fragment.rendition.clone())
        .or_default();
    retained.push_back(fragment.fragment.clone());
    retention.trim(retained);
    let packet = LiveMediaPacket::Fragment {
        rendition: fragment.rendition,
        group_sequence: fragment.fragment.group_sequence,
        published_at_micros: fragment.fragment.published_at_micros,
        start_timestamp: fragment.fragment.start_timestamp,
        duration: fragment.fragment.duration,
        discontinuity: fragment.fragment.discontinuity,
        bytes: fragment.fragment.bytes,
    };
    let _ = presentation.updates.send(packet);
}

fn publish_hls(
    hub: &LiveMediaHub,
    orbit: &str,
    session_id: &(String, String),
    segment: HlsSegment,
) {
    let Ok(mut state) = lock(&hub.inner) else {
        return;
    };
    let Some(presentation) = state
        .presentations
        .get_mut(&presentation_key(orbit, session_id))
    else {
        return;
    };
    let retention = presentation.retention;
    let retained = presentation
        .hls_segments
        .entry(segment.rendition.clone())
        .or_default();
    retained.push_back(segment);
    retention.trim(retained);
}

fn unique_presentation<'a>(
    state: &'a HubState,
    orbit: &str,
    resource: &str,
    transport: LiveTransport,
) -> Result<(&'a PresentationKey, &'a Presentation)> {
    let mut matches = state.presentations.iter().filter(|(key, presentation)| {
        key.orbit == orbit
            && match transport {
                LiveTransport::Mse => presentation
                    .cmaf_tracks
                    .iter()
                    .any(|track| cmaf_matches(track, resource)),
                LiveTransport::Hls => presentation
                    .hls_renditions
                    .iter()
                    .any(|rendition| rendition.rendition == resource),
            }
    });
    let first = matches
        .next()
        .ok_or_else(|| anyhow!("live media resource is unavailable"))?;
    if matches.next().is_some() {
        return Err(anyhow!(
            "live media resource is ambiguous across source sessions"
        ));
    }
    Ok(first)
}

fn cmaf_matches(track: &CmafTrackDescription, resource: &str) -> bool {
    track.rendition == resource || track.render_group.as_deref() == Some(resource)
}

fn session_key(session: &Session) -> (String, String) {
    (
        session.peer().to_string(),
        data_encoding::HEXLOWER.encode(&session.connection_id()),
    )
}

fn presentation_key(orbit: &str, session: &(String, String)) -> PresentationKey {
    PresentationKey {
        orbit: orbit.into(),
        peer: session.0.clone(),
        connection: session.1.clone(),
    }
}

fn remove_presentation(hub: &LiveMediaHub, orbit: &str, session: &(String, String)) {
    if let Ok(mut state) = lock(&hub.inner) {
        state
            .presentations
            .remove(&presentation_key(orbit, session));
    }
}

fn orbit_key(address: &OrbitAddress) -> String {
    format!("{}/{}", address.space.as_str(), address.orbit.as_str())
}

pub(crate) fn assignment_orbit_key(space: &str, orbit: &str) -> String {
    format!("{space}/{orbit}")
}

fn track_kind_name(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "video",
        TrackKind::Audio => "audio",
        TrackKind::Catalog => "catalog",
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| anyhow!("live media state lock was poisoned"))
}

#[cfg(test)]
mod tests {
    use super::*;

    use runtime::plane::live::media::{
        CatalogTrack, Frame, FrameHeader, FrameKind, GroupHeader, CATALOG_VERSION,
        DEFAULT_MAX_GROUP_DURATION_MS, DEFAULT_MAX_LATENCY_MS,
    };

    /// One H.264 rendition, the baseline `Catalog::validate` insists on.
    fn stored_catalog() -> Catalog {
        Catalog {
            version: CATALOG_VERSION,
            jitter_hint_ms: 50,
            tracks: vec![CatalogTrack {
                track: "video-main".into(),
                kind: TrackKind::Video,
                codec: "avc1.42c01e".into(),
                timescale: 90_000,
                decoder_config_hex: "0142c01effe100046742c01e01000268ce".into(),
                max_group_duration_ms: DEFAULT_MAX_GROUP_DURATION_MS,
                target_latency_ms: DEFAULT_MAX_LATENCY_MS,
                bitrate_bps: 2_000_000,
                width: Some(1280),
                height: Some(720),
                frame_rate_milli: Some(30_000),
                sample_rate: None,
                channels: None,
                render_group: Some("film".into()),
                cmaf_rendition: Some("film".into()),
                hls_v3_rendition: Some("film".into()),
            }],
        }
    }

    fn stored_group(sequence: u64) -> ReceivedGroup {
        let payload = vec![0, 0, 0, 2, 0x65, 0x88];
        ReceivedGroup {
            header: GroupHeader {
                subscription_id: 2,
                track: "video-main".into(),
                track_kind: TrackKind::Video,
                group_sequence: sequence,
                published_at_micros: 1_000_000,
                timescale: 90_000,
                max_group_duration_ms: DEFAULT_MAX_GROUP_DURATION_MS,
            },
            frames: vec![Frame {
                header: FrameHeader {
                    timestamp: i64::try_from(sequence).unwrap() * 90_000,
                    duration: Some(90_000),
                    timescale: 90_000,
                    kind: FrameKind::Key,
                    payload_len: u32::try_from(payload.len()).unwrap(),
                },
                payload,
            }],
        }
    }

    /// A planned presentation lists every segment and holds none of them.
    ///
    /// The film-scale property: what the hub keeps is the table, the playlist
    /// is rendered from it, and one segment's bytes are read and packaged when
    /// that segment is asked for — in any order, because a fresh packager per
    /// segment has no last sequence to advance past.
    #[test]
    fn a_planned_presentation_serves_any_segment_without_holding_all_of_them() {
        use crate::display::{CatalogPolicy, StoredPlan};

        // Sixty one-second all-intra samples: a "film" with sixty groups at a
        // one-second budget, which a rolling window could never list. The
        // sample offset is derived from the built file rather than assumed —
        // a fixture that guesses where its own bytes are tests the refusal
        // path by accident, which is exactly how this test first failed.
        let probe = demux_file(&demux_trak(vec![6; 60], 0));
        let mdat_payload_at = u32::try_from(
            probe
                .windows(4)
                .position(|window| window == b"mdat")
                .expect("the file has an mdat")
                + 4,
        )
        .unwrap();
        let trak = demux_trak(vec![6; 60], mdat_payload_at);
        let policy = CatalogPolicy {
            max_group_duration_ms: 1_000,
            target_latency_ms: 3_000,
            jitter_hint_ms: 50,
            rendition: "film".into(),
        };
        let file = demux_file(&trak);
        let total = u64::try_from(file.len()).unwrap();
        let reader = |offset: u64, size: u32| {
            let start = usize::try_from(offset).unwrap();
            let end = start + usize::try_from(size).unwrap();
            Ok(file
                .get(start..end.min(file.len()))
                .unwrap_or_default()
                .to_vec())
        };
        let plan = StoredPlan::read(total, reader, &policy).expect("a plan reads");
        assert_eq!(plan.group_count(), 60);

        let hub = LiveMediaHub::default();
        hub.install_planned("space/orbit", "film", plan)
            .expect("a plan installs");

        // The playlist lists all sixty, ends, and no segment exists yet.
        let playlist = hub
            .hls_media_playlist("space/orbit", "film", "film", "..")
            .unwrap();
        assert_eq!(playlist.matches("#EXTINF:").count(), 60);
        assert!(playlist.contains("#EXT-X-PLAYLIST-TYPE:VOD"));
        assert!(playlist.trim_end().ends_with("#EXT-X-ENDLIST"));

        // Ask for segment 41 first — random access, no segment 0 built.
        let (plan, segment) = hub.planned_segment("space/orbit", "film", 41).unwrap();
        let bytes: Vec<Vec<u8>> = segment
            .ranges
            .iter()
            .map(|(offset, size)| reader(*offset, *size).unwrap())
            .collect();
        let ts = LiveMediaHub::package_planned(&plan, 41, &bytes).unwrap();
        assert_eq!(ts.len() % 188, 0, "a real transport stream");
        assert_eq!(ts.first(), Some(&0x47));

        // Then an earlier one: order does not matter to a fresh packager.
        let (plan, segment) = hub.planned_segment("space/orbit", "film", 3).unwrap();
        let bytes: Vec<Vec<u8>> = segment
            .ranges
            .iter()
            .map(|(offset, size)| reader(*offset, *size).unwrap())
            .collect();
        assert!(LiveMediaHub::package_planned(&plan, 3, &bytes).is_ok());

        // Past the end is absent, in the planned vocabulary.
        assert!(hub.planned_segment("space/orbit", "film", 60).is_err());
    }

    /// The container the planned test reads: samples first, table after.
    fn demux_file(trak: &mp4_atom::Trak) -> Vec<u8> {
        use mp4_atom::{Encode, FourCC, Ftyp, Moov, Mvhd};
        let sample_count = match &trak.mdia.minf.stbl.stsz.samples {
            mp4_atom::StszSamples::Different { sizes } => sizes.len(),
            mp4_atom::StszSamples::Identical { count, .. } => *count as usize,
        };
        let unit = [0u8, 0, 0, 2, 0x65, 0x88];
        let mut file = Vec::new();
        Ftyp {
            major_brand: FourCC::new(b"iso6"),
            minor_version: 1,
            compatible_brands: vec![FourCC::new(b"iso6")],
        }
        .encode(&mut file)
        .unwrap();
        let mdat: Vec<u8> = unit
            .iter()
            .copied()
            .cycle()
            .take(unit.len() * sample_count)
            .collect();
        let mut boxed = Vec::new();
        boxed.extend_from_slice(&u32::try_from(mdat.len() + 8).unwrap().to_be_bytes());
        boxed.extend_from_slice(b"mdat");
        boxed.extend_from_slice(&mdat);
        file.extend_from_slice(&boxed);
        Moov {
            mvhd: Mvhd {
                timescale: 90_000,
                ..Default::default()
            },
            trak: vec![trak.clone()],
            ..Default::default()
        }
        .encode(&mut file)
        .unwrap();
        file
    }

    /// An all-intra track whose chunk offsets point into `demux_file`'s mdat.
    fn demux_trak(sizes: Vec<u32>, first_sample_at: u32) -> mp4_atom::Trak {
        use mp4_atom::{
            Avc1, Avcc, Decode, Mdhd, Stco, Stsc, StscEntry, Stsd, Stsz, StszSamples, Stts,
            SttsEntry, Visual,
        };
        let count = sizes.len();
        let mut trak = mp4_atom::Trak::default();
        trak.mdia.mdhd = Mdhd {
            timescale: 90_000,
            ..Default::default()
        };
        trak.mdia.minf.stbl.stts = Stts {
            entries: vec![SttsEntry {
                sample_count: u32::try_from(count).unwrap(),
                sample_delta: 90_000,
            }],
        };
        trak.mdia.minf.stbl.stsz = Stsz {
            samples: StszSamples::Different { sizes },
        };
        trak.mdia.minf.stbl.stsc = Stsc {
            entries: vec![StscEntry {
                first_chunk: 1,
                samples_per_chunk: u32::try_from(count).unwrap(),
                sample_description_index: 1,
            }],
        };
        trak.mdia.minf.stbl.stco = Some(Stco {
            entries: vec![first_sample_at],
        });
        // No stss: all-intra, every sample a legal group start.
        let config = data_encoding::HEXLOWER
            .decode(b"0142c01effe100046742c01e01000268ce")
            .unwrap();
        let mut avcc_box = Vec::new();
        avcc_box.extend_from_slice(&u32::try_from(config.len() + 8).unwrap().to_be_bytes());
        avcc_box.extend_from_slice(b"avcC");
        avcc_box.extend_from_slice(&config);
        trak.mdia.minf.stbl.stsd = Stsd {
            codecs: vec![Avc1 {
                visual: Visual {
                    data_reference_index: 1,
                    width: 1280,
                    height: 720,
                    ..Default::default()
                },
                avcc: Avcc::decode(&mut avcc_box.as_slice()).unwrap(),
                ..Default::default()
            }
            .into()],
            ..Default::default()
        };
        trak
    }

    /// A finite source reaches a receiver through the same packagers a peer
    /// feeds, and the whole of it stays reachable.
    ///
    /// Ten groups is more than `RETAINED_SEGMENTS`, so a rolling window would
    /// have dropped the first four — the assertion on segment 0 is what makes
    /// this a test of retention rather than of the packagers, which already
    /// have their own.
    #[test]
    fn a_stored_presentation_serves_every_segment_and_declares_its_end() {
        let hub = LiveMediaHub::default();
        hub.install_whole(
            "space/orbit",
            "film",
            &stored_catalog(),
            (0..10).map(stored_group),
        )
        .expect("a finite catalog and its groups install");

        let playlist = hub
            .hls_media_playlist("space/orbit", "film", "film", "..")
            .expect("a finite presentation serves a playlist");
        assert!(playlist.contains("#EXT-X-PLAYLIST-TYPE:VOD"));
        assert!(playlist.trim_end().ends_with("#EXT-X-ENDLIST"));
        assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:0"));
        assert_eq!(
            playlist.matches("#EXTINF:").count(),
            10,
            "every group is listed, not the last {RETAINED_SEGMENTS}"
        );

        let first = hub
            .hls_segment("space/orbit", "film", "film", 0)
            .expect("the first segment outlives a window it does not have");
        assert_eq!(first.len() % 188, 0, "a real transport stream");
        assert_eq!(first.first(), Some(&0x47));
        assert!(hub.hls_segment("space/orbit", "film", "film", 10).is_err());
    }

    /// A dropped group leaves a gap, and a lookup has to survive it.
    ///
    /// `push_group` refuses a sequence that does not advance, so a deque is
    /// sorted — but it is not contiguous: a group the packager rejected, or one
    /// the peer never sent, leaves a hole. Binary search is only correct here
    /// because sorted is the property it needs and contiguous is not.
    #[test]
    fn a_gap_in_the_sequence_is_still_addressable_on_both_sides() {
        let hub = LiveMediaHub::default();
        hub.install_whole(
            "space/orbit",
            "film",
            &stored_catalog(),
            [0, 1, 2, 5, 6, 9].into_iter().map(stored_group),
        )
        .unwrap();

        for present in [0, 1, 2, 5, 6, 9] {
            assert!(
                hub.hls_segment("space/orbit", "film", "film", present)
                    .is_ok(),
                "segment {present} was installed"
            );
        }
        for absent in [3, 4, 7, 8, 10] {
            assert!(
                hub.hls_segment("space/orbit", "film", "film", absent)
                    .is_err(),
                "segment {absent} was never installed"
            );
        }
    }

    /// The MSE half of the same install, since both packagers run on one pass.
    #[test]
    fn a_stored_presentation_also_serves_its_mse_snapshot() {
        let hub = LiveMediaHub::default();
        hub.install_whole(
            "space/orbit",
            "film",
            &stored_catalog(),
            (0..10).map(stored_group),
        )
        .unwrap();

        let snapshot = hub.mse_snapshot("space/orbit", "film").unwrap();
        assert_eq!(snapshot.tracks.len(), 1);
        assert_eq!(snapshot.tracks[0].rendition, "film");
        let inits = snapshot
            .packets
            .iter()
            .filter(|packet| matches!(packet, LiveMediaPacket::Init { .. }))
            .count();
        let fragments = snapshot
            .packets
            .iter()
            .filter(|packet| matches!(packet, LiveMediaPacket::Fragment { .. }))
            .count();
        assert_eq!(inits, 1, "one init segment per rendition");
        assert_eq!(fragments, 10, "the whole file, not a catch-up window");
    }

    /// A catalog nothing can package is refused where the caller can see it.
    #[test]
    fn a_stored_install_refuses_rather_than_leaving_an_empty_presentation() {
        let hub = LiveMediaHub::default();
        let mut catalog = stored_catalog();
        catalog.tracks[0].cmaf_rendition = None;
        catalog.tracks[0].hls_v3_rendition = None;
        assert!(hub
            .install_whole("space/orbit", "film", &catalog, (0..2).map(stored_group))
            .is_err());
        assert!(
            hub.hls_media_playlist("space/orbit", "film", "film", "..")
                .is_err(),
            "a refused install leaves nothing behind"
        );
    }

    fn hls_presentation() -> Presentation {
        hls_presentation_with(Retention::Rolling(RETAINED_SEGMENTS))
    }

    fn hls_presentation_with(retention: Retention) -> Presentation {
        let (updates, _) = broadcast::channel(RECEIVER_QUEUE);
        Presentation {
            retention,
            plan: None,
            cmaf_tracks: Vec::new(),
            cmaf_fragments: BTreeMap::new(),
            hls_renditions: vec![HlsRenditionDescription {
                rendition: "main".into(),
                target_duration_ms: 2_000,
                codecs: vec!["avc1.640028".into(), "mp4a.40.2".into()],
                width: Some(1_920),
                height: Some(1_080),
                bitrate_bps: 4_128_000,
            }],
            hls_segments: BTreeMap::from([(
                "main".into(),
                VecDeque::from([
                    HlsSegment {
                        rendition: "main".into(),
                        group_sequence: 41,
                        published_at_micros: 1_000,
                        duration_ms: 2_000,
                        discontinuity: false,
                        bytes: vec![0x47; 188],
                    },
                    HlsSegment {
                        rendition: "main".into(),
                        group_sequence: 42,
                        published_at_micros: 3_000,
                        duration_ms: 1_500,
                        discontinuity: true,
                        bytes: vec![0x47; 376],
                    },
                ]),
            )]),
            updates,
        }
    }

    #[test]
    fn hls_urls_are_relative_to_the_opaque_ticket_and_window_is_bounded() {
        let hub = LiveMediaHub::default();
        lock(&hub.inner).unwrap().presentations.insert(
            PresentationKey {
                orbit: "space/orbit".into(),
                peer: "peer".into(),
                connection: "connection".into(),
            },
            hls_presentation(),
        );

        let master = hub.hls_master("space/orbit", "main", ".").unwrap();
        assert!(master.contains("#EXT-X-VERSION:3"));
        assert!(master.contains("./renditions/main.m3u8"));
        let media = hub
            .hls_media_playlist("space/orbit", "main", "main", "..")
            .unwrap();
        assert!(media.contains("#EXT-X-MEDIA-SEQUENCE:41"));
        assert!(media.contains("../segments/41.ts"));
        assert!(media.contains("#EXT-X-DISCONTINUITY\n#EXTINF:1.500"));
        assert_eq!(
            hub.hls_segment("space/orbit", "main", "main", 42)
                .unwrap()
                .len(),
            376
        );
        assert!(hub.hls_segment("space/orbit", "main", "main", 40).is_err());
    }

    /// A finite presentation says it is finite, and says where it ends.
    ///
    /// Without `#EXT-X-PLAYLIST-TYPE:VOD` and `#EXT-X-ENDLIST` a player treats
    /// the last segment as the live edge and keeps re-fetching the playlist
    /// waiting for one more — the file plays through and then hangs instead of
    /// ending. Neither line appears anywhere in a live playlist, and neither
    /// appeared anywhere in this repo before there was a finite source to emit
    /// them for.
    #[test]
    fn a_complete_presentation_declares_its_end_and_a_live_one_never_does() {
        let hub = LiveMediaHub::default();
        {
            let mut state = lock(&hub.inner).unwrap();
            state.presentations.insert(
                PresentationKey {
                    orbit: "space/orbit".into(),
                    peer: "peer".into(),
                    connection: "connection".into(),
                },
                hls_presentation_with(Retention::Whole { complete: true }),
            );
        }
        let finite = hub
            .hls_media_playlist("space/orbit", "main", "main", "..")
            .unwrap();
        assert!(finite.contains("#EXT-X-PLAYLIST-TYPE:VOD"));
        assert!(finite.trim_end().ends_with("#EXT-X-ENDLIST"));

        let live = LiveMediaHub::default();
        {
            let mut state = lock(&live.inner).unwrap();
            state.presentations.insert(
                PresentationKey {
                    orbit: "space/orbit".into(),
                    peer: "peer".into(),
                    connection: "connection".into(),
                },
                hls_presentation(),
            );
        }
        let rolling = live
            .hls_media_playlist("space/orbit", "main", "main", "..")
            .unwrap();
        assert!(!rolling.contains("#EXT-X-ENDLIST"));
        assert!(!rolling.contains("#EXT-X-PLAYLIST-TYPE"));
    }

    /// A segment nobody kept and a segment that never existed are different
    /// facts, and only the first is about a window that moved.
    #[test]
    fn a_missing_segment_says_which_kind_of_missing_it_is() {
        let rolling = LiveMediaHub::default();
        {
            let mut state = lock(&rolling.inner).unwrap();
            state.presentations.insert(
                PresentationKey {
                    orbit: "space/orbit".into(),
                    peer: "peer".into(),
                    connection: "connection".into(),
                },
                hls_presentation(),
            );
        }
        let aged_out = rolling
            .hls_segment("space/orbit", "main", "main", 40)
            .unwrap_err()
            .to_string();
        assert!(aged_out.contains("live window"), "{aged_out}");

        let whole = LiveMediaHub::default();
        {
            let mut state = lock(&whole.inner).unwrap();
            state.presentations.insert(
                PresentationKey {
                    orbit: "space/orbit".into(),
                    peer: "peer".into(),
                    connection: "connection".into(),
                },
                hls_presentation_with(Retention::Whole { complete: true }),
            );
        }
        let never_was = whole
            .hls_segment("space/orbit", "main", "main", 40)
            .unwrap_err()
            .to_string();
        assert!(
            !never_was.contains("live window"),
            "a finite presentation has no live window: {never_was}"
        );
    }

    /// Retention is the whole difference between the two sources.
    #[test]
    fn a_whole_presentation_keeps_what_a_rolling_one_drops() {
        let mut rolling = VecDeque::from([1, 2, 3, 4, 5, 6, 7]);
        Retention::Rolling(RETAINED_SEGMENTS).trim(&mut rolling);
        assert_eq!(rolling.len(), RETAINED_SEGMENTS);
        assert_eq!(rolling.front().copied(), Some(2), "the oldest fell off");

        let mut whole = VecDeque::from([1, 2, 3, 4, 5, 6, 7]);
        Retention::Whole { complete: false }.trim(&mut whole);
        assert_eq!(whole.len(), 7, "a player joining late reaches the first");
    }

    #[test]
    fn a_resource_is_refused_when_two_source_sessions_claim_it() {
        let hub = LiveMediaHub::default();
        let mut state = lock(&hub.inner).unwrap();
        for connection in ["first", "second"] {
            state.presentations.insert(
                PresentationKey {
                    orbit: "space/orbit".into(),
                    peer: "peer".into(),
                    connection: connection.into(),
                },
                hls_presentation(),
            );
        }
        drop(state);
        assert!(hub.hls_master("space/orbit", "main", ".").is_err());
        assert!(!hub.has_resource("space/orbit", "main", LiveTransport::Hls));
    }
}
