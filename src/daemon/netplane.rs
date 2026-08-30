//! The net plane: a person's own devices reach each other at L3.
//!
//! An IPv6 tunnel between the devices of one profile, addressed by device
//! key, carried over the identity's own endpoint. It is mounted **here**,
//! beside the display overlay, rather than in `runtime`, because a runtime
//! plane is Space-scoped by construction and this relation has no Space: the
//! peers are the profile's own devices, and the daemon is the only layer
//! where the identity, its endpoint and its kinship log all live.
//!
//! Three facts it keeps apart, because folding any two together is the
//! false-disconnection defect one layer down:
//!
//! - **The interface.** `Unsupported` is macOS and Windows, where the TUN
//!   seam is not built yet; `NotPermitted` is a Linux daemon without
//!   `CAP_NET_ADMIN` — the desktop case, since only the service unit `lait
//!   install` writes carries it; `Off` is the operator's own switch. None of
//!   the three is an error, none is "no peers", and the plane runs under all
//!   of them: the peer table still says who is reachable, and only the local
//!   interface is missing.
//! - **The set.** `None` on the own watch is "not restored", which admits and
//!   dials nobody — an unrestored daemon that carried for everyone would be
//!   open exactly while it could not tell who was its own.
//! - **A peer's reach.** `NoRoute`, `Unreachable` and `Retired` are three
//!   different absences, and the carry keeps them apart; this module only
//!   spells them for the view.
//!
//! Admission happens twice on purpose: the hub refuses a stranger on the
//! `NET_ALPN` lane before a byte is read ([`transport_hub::admit_own`]), and
//! the carry re-asks against the same set before a route exists. Admission
//! that lives in one place only is admission the next composition site
//! forgets.

use std::net::Ipv6Addr;
use std::sync::Arc;

use comms::Transport;
use mechanics::ids::DeviceId;
use netstack::carry::{Carry, Interface, Packets, Reach};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::control::{InterfaceView, ReachKind};
use crate::daemon::correspondence::OwnDevices;
use crate::daemon::transport_hub::NET_ALPN;

/// The interface this daemon asks the kernel for. One per machine: a second
/// identity on the same box gets `NotPermitted`-shaped honesty from the
/// kernel rather than a second tunnel, which is the right answer until a
/// device carries more than one profile.
const DEVICE: &str = "lait0";

/// What the reach view reads while the plane runs: the interface as it was
/// decided at mount, and the carry's live peer table.
///
/// Hooked into the correspondence service the way the fan-out's facts are.
/// Absent — no plane mounted — every device renders with no reach at all,
/// which is not "unreachable": nothing carried, so nothing measured.
pub(crate) struct Facts {
    interface: Interface,
    carry: Carry,
}

impl Facts {
    /// The interface as this mount found it. Never `None`: the plane always
    /// has an answer about the local interface, even when the answer is that
    /// this machine has none.
    pub(crate) fn interface(&self) -> InterfaceView {
        match &self.interface {
            Interface::Up { name, address } => InterfaceView::Up {
                name: name.clone(),
                address: address.to_string(),
            },
            Interface::NotPermitted => InterfaceView::NotPermitted,
            Interface::Unsupported => InterfaceView::Unsupported,
            Interface::Off => InterfaceView::Off,
        }
    }

    /// How one own device is reached, or `None` when the carry has no row for
    /// it — a device the set named after this run began, or one with no
    /// endpoint key. An absent row is not an unreachable one.
    pub(crate) fn reach_of(&self, device: &DeviceId) -> Option<ReachKind> {
        self.carry.reach_of(device).map(spell)
    }
}

/// A `Reach` as the view says it. `since` is unix seconds, computed from the
/// elapsed time rather than stored, because the carry holds a monotonic
/// instant and a view wants a clock a person shares.
fn spell(reach: Reach) -> ReachKind {
    match reach {
        Reach::Connected { via } => ReachKind::Connected {
            via: match via {
                comms::PathKind::Direct => "direct".into(),
                comms::PathKind::Relay => "relay".into(),
                comms::PathKind::Unknown => "unknown".into(),
            },
        },
        Reach::Dialing => ReachKind::Dialing,
        Reach::NoRoute => ReachKind::NoRoute,
        Reach::Unreachable { since } => ReachKind::Unreachable {
            since: crate::daemon::correspondence::now_secs()
                .saturating_sub(since.elapsed().as_secs()),
        },
        Reach::Retired => ReachKind::Retired,
    }
}

/// Mount the tunnel on this identity's endpoint.
///
/// `None` when the endpoint has no `NET_ALPN` lane left to take, or when this
/// device's id carries no key to derive an address from — both degradations,
/// never a reason for the daemon not to exist. The returned handle is joined
/// on stop like every other service the daemon owns.
pub(crate) async fn mount(
    transport: Arc<dyn Transport>,
    own: watch::Receiver<Option<OwnDevices>>,
    stop: watch::Receiver<bool>,
) -> Option<(Arc<Facts>, JoinHandle<()>)> {
    let queue = transport.take_session_queue(NET_ALPN)?;
    let Some(address) = netstack::ula_for(&transport.my_id()) else {
        tracing::warn!("this device's id carries no endpoint key; no tunnel address to serve");
        return None;
    };
    let (interface, packets) = raise(address, &stop).await;
    match &interface {
        Interface::Up { name, .. } => tracing::info!(dev = %name, %address, "interface up"),
        Interface::NotPermitted => tracing::info!(
            "interface: not permitted — this daemon holds no CAP_NET_ADMIN, so its own \
             devices are known but not routed here"
        ),
        Interface::Unsupported => {
            tracing::info!("interface: unsupported on this platform (Linux only for now)");
        }
        Interface::Off => tracing::info!("interface: off (LAIT_NET)"),
    }

    let carry = Carry::new(interface.clone());
    let facts = Arc::new(Facts {
        interface,
        carry: carry.clone(),
    });
    let task = tokio::spawn(async move {
        // The carry follows a plain list of ids; the daemon publishes an
        // `Option<OwnDevices>`. `None` becomes the empty list, which dials
        // and admits nobody — the same fail-closed answer the hub gives.
        let (ids, relay) = ids_of(own);
        let result = carry.run(transport, queue, ids, packets, stop).await;
        relay.abort();
        if let Err(error) = result {
            tracing::warn!(%error, "the net plane stopped");
        }
    });
    Some((facts, task))
}

/// Follow the device set as a list of ids, republishing on every change.
///
/// The forwarder is aborted with the plane rather than left to the sender's
/// lifetime: the correspondence service outlives every mount, so a task that
/// ended only when it dropped would be one leaked task per mount.
fn ids_of(
    mut own: watch::Receiver<Option<OwnDevices>>,
) -> (watch::Receiver<Vec<DeviceId>>, JoinHandle<()>) {
    let (tx, rx) = watch::channel(devices_in(&own.borrow_and_update()));
    let relay = tokio::spawn(async move {
        while own.changed().await.is_ok() {
            let devices = devices_in(&own.borrow_and_update());
            if tx.send(devices).is_err() {
                break;
            }
        }
    });
    (rx, relay)
}

fn devices_in(own: &Option<OwnDevices>) -> Vec<DeviceId> {
    own.as_ref()
        .map_or_else(Vec::new, |own| own.devices.clone())
}

/// Ask the kernel for the interface, and name what came back.
///
/// Every arm returns a working plane: without an interface the carry gets a
/// packet source that is immediately done and a sink that discards, so it
/// still dials, admits and keeps its peer table. That is the difference
/// between "this machine cannot route" and "you have no devices".
async fn raise(address: Ipv6Addr, stop: &watch::Receiver<bool>) -> (Interface, Packets) {
    if !crate::daemon::host::net_hosting() {
        return (Interface::Off, nowhere());
    }
    // `configure` shells out to `ip`, and this runs while the daemon is
    // coming up: forking a process on the runtime's own thread would hold
    // every other service's start behind it.
    let stop = stop.clone();
    match tokio::task::spawn_blocking(move || open(address, &stop)).await {
        Ok(Ok(up)) => up,
        Ok(Err(error)) => {
            let absent = absence(&error);
            tracing::info!(%error, "the tunnel interface was not opened");
            (absent, nowhere())
        }
        Err(error) => {
            tracing::warn!(%error, "the tunnel interface could not be asked for");
            (Interface::Unsupported, nowhere())
        }
    }
}

fn open(address: Ipv6Addr, stop: &watch::Receiver<bool>) -> std::io::Result<(Interface, Packets)> {
    let (file, name) = netstack::tun::open(DEVICE)?;
    netstack::tun::configure(&name, address)?;
    let (read, write) = netstack::tun::packets(file, stop.clone())?;
    Ok((Interface::Up { name, address }, Packets { read, write }))
}

/// Which kind of absence the kernel reported.
///
/// `PermissionDenied` is the one a person can act on — install the service,
/// which carries `CAP_NET_ADMIN` — so it keeps its own name. Everything else
/// (a platform with no TUN seam, a box with no `/dev/net/tun`, no iproute2)
/// is `Unsupported`: this machine does not offer the interface. The error
/// itself is logged beside it, because the category is for the view and the
/// reason is for whoever reads the journal.
fn absence(error: &std::io::Error) -> Interface {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => Interface::NotPermitted,
        _ => Interface::Unsupported,
    }
}

/// Packets from nowhere, to nowhere. `empty()` reports end-of-file at once,
/// so the carry's blocking reader ends immediately instead of parking a
/// thread the runtime would later wait on.
fn nowhere() -> Packets {
    Packets {
        read: Box::new(std::io::empty()),
        write: Box::new(std::io::sink()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anyhow::Result;
    use async_trait::async_trait;
    use comms::mem::MemNet;
    use comms::policy::Network;
    use comms::TransportFactory;
    use mechanics::actor::device_from_seed;

    use super::*;
    use crate::daemon::transport_hub::TransportHubFactory;

    struct MemFactory(MemNet);

    #[async_trait]
    impl TransportFactory for MemFactory {
        async fn build(
            &self,
            identity_seed: &[u8; 32],
            _network: &Network,
            _protocols: comms::Protocols<'_>,
        ) -> Result<Arc<dyn Transport>> {
            Ok(Arc::new(self.0.peer(device_from_seed(identity_seed))))
        }
    }

    fn set(seeds: &[[u8; 32]], me: &[u8; 32]) -> OwnDevices {
        let mut devices: Vec<DeviceId> = seeds.iter().map(device_from_seed).collect();
        devices.sort();
        OwnDevices {
            profile: mechanics::kinship::ProfileId::from_digest([5; 16]),
            me: device_from_seed(me),
            devices,
        }
    }

    /// One mounted plane: the hub's endpoint for one identity, the net lane
    /// taken from it, and the carry running over both.
    struct Side {
        seed: [u8; 32],
        factory: Arc<TransportHubFactory>,
        own: watch::Sender<Option<OwnDevices>>,
        facts: Arc<Facts>,
        stop: watch::Sender<bool>,
        task: JoinHandle<()>,
    }

    impl Side {
        async fn stand(net: &MemNet, seed: [u8; 32], own: OwnDevices) -> Self {
            let own = watch::Sender::new(Some(own));
            let factory = Arc::new(TransportHubFactory::new(
                Arc::new(MemFactory(net.clone())),
                own.subscribe(),
            ));
            let transport = factory
                .identity_transport(&seed, &Network::Isolated)
                .await
                .expect("the identity endpoint is raised");
            let stop = watch::Sender::new(false);
            let (facts, task) = mount(transport, own.subscribe(), stop.subscribe())
                .await
                .expect("the net lane is there to take");
            Self {
                seed,
                factory,
                own,
                facts,
                stop,
                task,
            }
        }

        fn me(&self) -> DeviceId {
            device_from_seed(&self.seed)
        }

        /// The reach of `peer` once it satisfies `wanted`, or whatever it was
        /// when the wait ran out — so a failure names what it saw.
        async fn reach_until(
            &self,
            peer: &DeviceId,
            wanted: impl Fn(&ReachKind) -> bool,
        ) -> Option<ReachKind> {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                let seen = self.facts.reach_of(peer);
                if seen.as_ref().is_some_and(&wanted) || std::time::Instant::now() > deadline {
                    return seen;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        async fn fold(self) {
            let _ = self.stop.send(true);
            let _ = tokio::time::timeout(Duration::from_secs(10), self.task).await;
            self.factory.shutdown().await;
        }
    }

    /// The whole composition, in memory: two devices of one profile carry for
    /// each other over the hub's identity lane; a third key the set does not
    /// name is closed before it can become a route; and a device the set
    /// drops loses the route it had, while the plane runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_stranger_never_becomes_a_route_and_a_retired_device_loses_the_one_it_had() {
        // No interface is asked for: this is about who is carried, not about
        // what a kernel does with the packets, and a test that raised `lait0`
        // would depend on the machine it ran on. Set process-wide, which is
        // safe because nextest gives every test its own process — the same
        // ground `LAIT_CONFIG_ROOT` is set on across this suite.
        std::env::set_var("LAIT_NET", "off");
        let net = MemNet::new().with_planes();
        let (a_seed, b_seed, stranger_seed) = ([21; 32], [22; 32], [23; 32]);
        let a = Side::stand(&net, a_seed, set(&[a_seed, b_seed], &a_seed)).await;
        let b = Side::stand(&net, b_seed, set(&[a_seed, b_seed], &b_seed)).await;

        assert_eq!(
            a.facts.interface(),
            InterfaceView::Off,
            "the operator's switch is its own answer, never a failure"
        );
        let carried = a
            .reach_until(&b.me(), |reach| {
                matches!(reach, ReachKind::Connected { .. })
            })
            .await;
        assert!(
            matches!(carried, Some(ReachKind::Connected { .. })),
            "two devices of one profile did not carry for each other: {carried:?}"
        );

        // A key the set does not name, dialing the lane directly. The hub
        // refuses it before a frame; nothing about it reaches the table.
        let stranger: Arc<dyn Transport> = Arc::new(net.peer(device_from_seed(&stranger_seed)));
        let refused = stranger
            .connect_session(a.me(), NET_ALPN)
            .await
            .expect("the network itself accepts the dial");
        tokio::time::timeout(Duration::from_secs(10), refused.closed())
            .await
            .expect("a stranger's session is closed, not parked");
        assert_eq!(
            a.facts.reach_of(&device_from_seed(&stranger_seed)),
            None,
            "a caller outside the own set got a row in the peer table"
        );

        // Retirement: B leaves A's set while the link is live.
        a.own.send_replace(Some(set(&[a_seed], &a_seed)));
        assert_eq!(
            a.reach_until(&b.me(), |reach| *reach == ReachKind::Retired)
                .await,
            Some(ReachKind::Retired),
            "a retired device kept its route"
        );

        a.fold().await;
        b.fold().await;
        std::env::remove_var("LAIT_NET");
    }

    /// A machine with no interface still knows its devices. What the mount
    /// can report is about the local interface and says nothing about who is
    /// own — folding the two would be the false-disconnection defect one
    /// layer down, where "this machine cannot route" reads as "you have no
    /// devices".
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_interface_the_machine_will_not_give_is_never_read_as_no_peers() {
        assert_eq!(
            absence(&std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            Interface::NotPermitted,
            "a daemon holding no CAP_NET_ADMIN must say so; it is the actionable one"
        );
        assert_eq!(
            absence(&std::io::Error::from(std::io::ErrorKind::Unsupported)),
            Interface::Unsupported,
            "a platform with no TUN seam is unsupported, not unpermitted"
        );
        assert_eq!(
            absence(&std::io::Error::from(std::io::ErrorKind::NotFound)),
            Interface::Unsupported,
            "a box with no /dev/net/tun does not offer the interface either"
        );

        // Process-wide, and safe for the reason above: one process per test.
        std::env::set_var("LAIT_NET", "off");
        let net = MemNet::new().with_planes();
        let (a_seed, b_seed) = ([31; 32], [32; 32]);
        let a = Side::stand(&net, a_seed, set(&[a_seed, b_seed], &a_seed)).await;
        assert!(
            !matches!(a.facts.interface(), InterfaceView::Up { .. }),
            "no interface was raised, and the plane claimed one"
        );
        let reach = a.reach_until(&device_from_seed(&b_seed), |_| true).await;
        assert!(
            reach.is_some(),
            "an absent interface was read as an absent device set"
        );
        a.fold().await;
        std::env::remove_var("LAIT_NET");
    }

    /// An unrestored daemon carries for nobody. `None` on the watch is "the
    /// set is not known", and a plane that dialed everyone it had ever heard
    /// of would be open exactly while it could not tell.
    #[test]
    fn a_set_that_is_not_held_names_no_devices() {
        assert!(devices_in(&None).is_empty());
        assert_eq!(devices_in(&Some(set(&[[41; 32]], &[41; 32]))).len(), 1);
    }
}
