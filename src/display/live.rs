//! Coordinator-owned native-media ingest and bounded receiver fanout.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use runtime::plane::live::media::{
    Catalog, Control, Event, EventBody, RequestKeyframe, Session, Subscribe, TrackKind,
    CATALOG_TRACK, DEFAULT_MAX_LATENCY_MS,
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

struct Presentation {
    cmaf_tracks: Vec<CmafTrackDescription>,
    cmaf_fragments: BTreeMap<String, VecDeque<CmafFragment>>,
    hls_renditions: Vec<HlsRenditionDescription>,
    hls_segments: BTreeMap<String, VecDeque<HlsSegment>>,
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
                segments
                    .iter()
                    .find(|segment| segment.group_sequence == sequence)
            })
            .map(|segment| segment.bytes.clone())
            .ok_or_else(|| anyhow!("HLS segment is outside the live window"))
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
    let retained = presentation
        .cmaf_fragments
        .entry(fragment.rendition.clone())
        .or_default();
    retained.push_back(fragment.fragment.clone());
    while retained.len() > RETAINED_SEGMENTS {
        retained.pop_front();
    }
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
    let retained = presentation
        .hls_segments
        .entry(segment.rendition.clone())
        .or_default();
    retained.push_back(segment);
    while retained.len() > RETAINED_SEGMENTS {
        retained.pop_front();
    }
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

    fn hls_presentation() -> Presentation {
        let (updates, _) = broadcast::channel(RECEIVER_QUEUE);
        Presentation {
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
