//! The exec plane's opening loop over real (in-memory) connections.
//!
//! What the E3 foundation promises, checked end to end: a dial on
//! `lait/exec/1` reaches a handler that reads the opening, judges it through
//! the shared admission path, and answers — and an admitted connection serves
//! no flow yet, refusing each one loudly rather than letting it hang. A
//! version claim the ALPN did not negotiate is refused as malformed, because
//! the ALPN is the version gate and inside it there is no skew to discuss.

/// How long a step here waits for the other end of a real connection.
///
/// These are synchronisation waits, not latency assertions: nothing in this
/// file measures how fast a refusal arrives, only that one arrives instead of
/// the flow hanging. So the budget is generous on purpose. Five seconds was
/// not — a loaded Windows runner spent it on scheduling and failed
/// `an_exec_dial_is_judged_answered_and_serves_no_flow_yet` for a reason that
/// is not the property, while the same test takes 0.16s on an idle machine.
///
/// A genuine hang still fails, just later, which is the right trade for a
/// deadline whose only job is to stop a wedge from becoming a timeout.
const ANSWERED: std::time::Duration = std::time::Duration::from_secs(30);

use std::sync::Arc;

use mechanics::{ids::SpaceId, station::Key};
use runtime::admission::PlanePolicy;
use runtime::lifecycle::CancelToken;
use runtime::plane::{bounds, exec, Accept, Open, Plane, Refusal, SPACE_ID_LEN};
use runtime::plane_driver::{run_driver, PlaneContext};
use runtime::world::{AuthorityView, PrincipalResolution};

struct Everyone;
impl AuthorityView for Everyone {
    fn resolve(&self, _device: &mechanics::ids::DeviceId) -> Option<PrincipalResolution> {
        Some(PrincipalResolution {
            actor: mechanics::ids::ActorId::parse(&format!("act_{}", "ef".repeat(32)))
                .expect("actor"),
            authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
        })
    }
}

fn space() -> SpaceId {
    SpaceId::from_digest([31u8; 16])
}

const SERVER_SEED: [u8; 32] = [21u8; 32];
const CLIENT_SEED: [u8; 32] = [22u8; 32];

/// Start the exec driver on one mem peer, exactly the way the mount does:
/// a pump splitting inbound connections into the plane's queue, and the
/// shared driver judging openings before the service ever sees a connection.
fn serve_exec(net: &comms::mem::MemNet) -> (Key, CancelToken) {
    let device = mechanics::actor::device_from_seed(&SERVER_SEED);
    let station = Key::from_device(&device).expect("station");
    let transport: Arc<dyn comms::Transport> = Arc::new(net.peer(device));
    let cancel = CancelToken::new();
    let context = PlaneContext {
        plane: Plane::Exec,
        space: space(),
        local_station: station.clone(),
        authority: Arc::new(Everyone),
        policy: PlanePolicy::default(),
        cancel: cancel.clone(),
        drain_deadline: runtime::lifecycle::DEFAULT_DRAIN_DEADLINE,
        authority_tick: None,
    };
    let (queue_tx, queue_rx) = tokio::sync::mpsc::channel(16);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("pump runtime");
        rt.block_on(async move {
            while let Some(incoming) = transport.accept_connection().await {
                if queue_tx.send(incoming).await.is_err() {
                    break;
                }
            }
        });
    });
    std::thread::spawn(move || run_driver(context, queue_rx, exec::Service::new()));
    (station, cancel)
}

fn opening(local: &Key, responder: &Key, version: u16) -> Open {
    let mut space_bytes = [0u8; SPACE_ID_LEN];
    space_bytes.copy_from_slice(space().as_str().as_bytes());
    Open {
        plane: Plane::Exec,
        protocol_version: version,
        features: 0,
        space: space_bytes,
        initiator_station: local.key_bytes(),
        responder_station: responder.key_bytes(),
        connection_id: [5u8; 16],
        connection_epoch: [6u8; 16],
        authority_frontier: Vec::new(),
        requested_lanes: Vec::new(),
    }
}

#[tokio::test]
async fn an_exec_dial_is_judged_answered_and_serves_no_flow_yet() {
    let net = comms::mem::MemNet::new();
    let (server, cancel) = serve_exec(&net);

    let client_device = mechanics::actor::device_from_seed(&CLIENT_SEED);
    let client_station = Key::from_device(&client_device).expect("station");
    let transport: Arc<dyn comms::Transport> = Arc::new(net.peer(client_device));

    let connection = transport
        .connect_session(server.as_device(), runtime::plane::EXEC_ALPN)
        .await
        .expect("the registered ALPN negotiates");
    let open = opening(&client_station, &server, Plane::Exec.protocol_version());
    let mut flow = connection.open_uni().await.expect("opening flow");
    flow.write_all(&open.encode()).await.expect("send opening");
    flow.finish().expect("finish opening");

    let answer = tokio::time::timeout(ANSWERED, async {
        let mut recv = connection.accept_uni().await.ok()??;
        recv.read_to_end(bounds::MAX_OPENING_BYTES).await.ok()
    })
    .await
    .expect("an answer within the deadline")
    .expect("the responder answers rather than hanging up");
    let accept = Accept::decode_canonical(&answer).expect("an admitted member gets an Accept");
    assert_eq!(accept.capability.plane, Plane::Exec);
    assert_eq!(
        accept.capability.protocol_version,
        Plane::Exec.protocol_version()
    );
    assert!(accept.granted_lanes.is_empty());

    // The admitted connection serves no flow vocabulary yet: a probe is
    // stopped loudly, not left to a deadline.
    let (mut probe_send, mut probe_recv) = connection.open_bi().await.expect("probe flow");
    probe_send
        .write_all(b"anything")
        .await
        .expect("probe write");
    let refused = tokio::time::timeout(ANSWERED, probe_recv.read_to_end(bounds::MAX_OPENING_BYTES))
        .await
        .expect("the refusal arrives within the deadline");
    assert!(
        refused.is_err(),
        "a foundation-plane flow must be reset, not answered or left open"
    );

    cancel.cancel();
}

/// Within one ALPN there is no legitimate version skew: the ALPN *is* the
/// version gate, so a peer on another generation never negotiates
/// `lait/exec/1` at all and a mismatched version claim inside it is a
/// malformed opening, not a compatibility conversation. (The judge's
/// `UnsupportedVersion` answer stays reachable for hub-routed openings —
/// `an_exec_opening_on_another_generation_names_the_supported_one` covers
/// it at the admission fixture.)
#[tokio::test]
async fn a_version_claim_inside_the_exec_alpn_is_malformed_not_negotiable() {
    let net = comms::mem::MemNet::new();
    let (server, cancel) = serve_exec(&net);

    let client_device = mechanics::actor::device_from_seed(&CLIENT_SEED);
    let client_station = Key::from_device(&client_device).expect("station");
    let transport: Arc<dyn comms::Transport> = Arc::new(net.peer(client_device));

    let connection = transport
        .connect_session(server.as_device(), runtime::plane::EXEC_ALPN)
        .await
        .expect("the registered ALPN negotiates");
    let open = opening(&client_station, &server, 2);
    let mut flow = connection.open_uni().await.expect("opening flow");
    flow.write_all(&open.encode()).await.expect("send opening");
    flow.finish().expect("finish opening");

    let answer = tokio::time::timeout(ANSWERED, async {
        let mut recv = connection.accept_uni().await.ok()??;
        recv.read_to_end(bounds::MAX_OPENING_BYTES).await.ok()
    })
    .await
    .expect("an answer within the deadline")
    .expect("a refusal is an answer, not a hangup");
    assert_eq!(
        Refusal::decode_canonical(&answer).expect("a canonical refusal"),
        Refusal::Malformed,
    );

    cancel.cancel();
}
