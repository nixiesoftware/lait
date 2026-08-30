//! Where this profile's own devices were last reachable.
//!
//! Learned routes live in the transport, which means they die with the
//! process. That is the whole of the failure this exists for: a daemon that
//! restarts — an adoption asks for a fresh generation, so a device that has
//! just been paired always does — comes back knowing no address for any of
//! its siblings, and under a policy with no relay or discovery a bare id
//! resolves to nothing. Both directions then fail: the sibling holds an
//! address that died with the old port, and the restarted device holds none
//! at all. Nobody can announce, and the silence is permanent.
//!
//! So the addresses an own device told us are kept, and taught back to the
//! transport at start. This is not discovery: nothing here is asked about a
//! device that is not already in the profile's set, nothing is published,
//! and nothing is dialled that the ceremony or an announcement did not
//! already hand over. It is the same fact the pairing carried, not forgotten
//! on reboot.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use comms::Transport;
use mechanics::ids::DeviceId;

/// One line per own device. JSON so a person looking at an identity
/// directory can read what their device believes about the others.
fn file(identity: &Path) -> PathBuf {
    identity.join("own-routes.json")
}

fn load(identity: &Path) -> BTreeMap<String, Vec<SocketAddr>> {
    addressbook::durable::open_or_recover(&file(identity), |path| {
        let bytes = std::fs::read(path)?;
        Ok(serde_json::from_slice(&bytes).unwrap_or_default())
    })
    .unwrap_or_default()
    .unwrap_or_default()
}

/// Remember where `device` just answered from.
///
/// Empty routes are not written: a transport with no addresses to advertise
/// (a relay policy, the in-memory one) says nothing about where anybody is,
/// and recording that as "no route" would overwrite an address that still
/// works with a fact nobody measured.
pub(crate) fn remember(identity: &Path, device: &DeviceId, addrs: &[SocketAddr]) {
    if addrs.is_empty() {
        return;
    }
    let mut held = load(identity);
    let mine: Vec<SocketAddr> = addrs.to_vec();
    if held.get(device.as_str()) == Some(&mine) {
        return;
    }
    held.insert(device.as_str().to_owned(), mine);
    let Ok(bytes) = serde_json::to_vec_pretty(&held) else {
        return;
    };
    if let Err(error) = addressbook::durable::atomic_replace(&file(identity), &bytes) {
        tracing::debug!(%error, "could not record where an own device is");
    }
}

/// Teach the transport every route this identity remembers, so the first
/// dial after a restart has somewhere to go.
pub(crate) fn teach(identity: &Path, transport: &dyn Transport) {
    for (device, addrs) in load(identity) {
        let Some(device) = DeviceId::parse(&device) else {
            continue;
        };
        transport.learn(device, &addrs);
    }
}
