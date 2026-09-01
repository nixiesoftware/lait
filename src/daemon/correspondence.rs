//! Identity-scoped correspondence on the daemon.
//!
//! Correspondence was the desktop client's alone: `crates/correspondence` was
//! reached by exactly one crate, and the plane — the device seeds, the kinship
//! registry, the mailbox — was built inside the Astrolabe process. Two things
//! follow from that, and both are wrong.
//!
//! A **World cannot send.** A World's head talks to this daemon; Astrolabe sits
//! above Worlds and launches them, so a World has no upward call. Offering
//! "invite this person" from a tracker was structurally impossible, not merely
//! unbuilt.
//!
//! And **mail arrives only while a window is open.** The one process able to
//! collect was the one a person closes.
//!
//! So the plane lives here, beside the address book, for the same reason the
//! address book does: it is the identity's, not any surface's. Astrolabe asks
//! for it over the control plane like every other caller, and stops being the
//! place it happens to live.
//!
//! What does **not** move down here is judgement. This service carries letters
//! and holds who a person has learned. It admits nobody to anything: an
//! invitation it delivers is verified at the Space that issued it, and delivery
//! was never admission.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use correspondence::Contractor;
use lait_directory::Directory;
use mechanics::ids::DeviceId;
use mechanics::kinship::{Audience, ProfileId};
use tokio::sync::watch;

use crate::control::{Request, Response};

/// The directory this deployment is pointed at, if any.
///
/// `LAIT_DIRECTORY_URL` names one. Nothing is assumed absent it, for the reason
/// the carrier gives one function down and one this service is even more
/// exposed to: a directory that answers "not available" because it was
/// misconfigured is indistinguishable, at the surface, from a person who does
/// not exist. An absent directory refuses in words instead — and absence is
/// now a *choice*: the endpoint resolves through `Settings::directory_url`
/// (env override, then the config key, then the cloud built-in release
/// builds carry), so the packaged product connects and an operator opts out
/// by emptying it.
#[must_use]
pub fn configured_directory(identity: &std::path::Path) -> Option<Box<dyn Directory + Send>> {
    let base = crate::config::Settings::load(Some(identity)).directory_url()?;
    Some(Box::new(lait_directory::Remote::at(&base)))
}

/// The carrier this deployment is pointed at, if any.
///
/// The Post this identity carries over, resolved through
/// `Settings::post_url` — env override, config key, then the cloud built-in
/// release builds carry. An unreachable host read as an empty mailbox is
/// still the defect this plane is most careful about; the default moving to
/// the cloud changes who chooses the host, not what an unreachable one is
/// allowed to look like.
#[must_use]
pub fn configured_carrier(identity: &std::path::Path) -> Option<Box<dyn Contractor>> {
    let base = crate::config::Settings::load(Some(identity)).post_url()?;
    Some(Box::new(correspondence::post::PostContractor::new(&base)))
}

/// The carrier's base URL, when one is configured — for the wake listener,
/// which speaks to the same service the carrier does but is not a carrier.
pub fn configured_post_url(identity: &std::path::Path) -> Option<String> {
    crate::config::Settings::load(Some(identity)).post_url()
}

/// Hold one standing SSE subscription to the carrier's wake doorbell, and
/// touch `woken` whenever it rings — the same doorbell shape the update feed
/// uses (`update::notify`): a frame only *wakes* the collector, which then
/// collects over the signed path exactly as it does on its period. Losing the
/// stream costs the period, never a letter.
///
/// A thread rather than a task because the read is a blocking body stream
/// (`ureq`), reconnecting forever with a capped backoff. It holds nothing but
/// a URL and a device id, and it writes nothing.
pub fn serve_wake(base: String, device: String, woken: std::sync::Arc<tokio::sync::Notify>) {
    std::thread::Builder::new()
        .name("post-wake".into())
        .spawn(move || {
            let url = format!("{}/wake?device={}", base.trim_end_matches('/'), device);
            let agent = ureq::AgentBuilder::new().build();
            let mut backoff = std::time::Duration::from_secs(1);
            loop {
                match agent.get(&url).set("Accept", "text/event-stream").call() {
                    Ok(response) => {
                        let reader = std::io::BufReader::new(response.into_reader());
                        use std::io::BufRead;
                        for line in reader.lines() {
                            let Ok(line) = line else { break };
                            if line.trim() == "event: mail" {
                                woken.notify_one();
                            }
                            // A connected stream is a healthy one, whatever
                            // it says: the next drop starts polite again.
                            backoff = std::time::Duration::from_secs(1);
                        }
                    }
                    Err(_) => {}
                }
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(std::time::Duration::from_secs(300));
            }
        })
        .ok();
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The device set a plane holds, as it is published. Sorted so two daemons
/// holding the same log publish the same value.
fn own_of(reach: &correspondence::plane::ReachPlane) -> OwnDevices {
    let mut devices = reach.my_devices();
    devices.sort();
    OwnDevices {
        profile: reach.profile().clone(),
        me: reach.canonical_device(),
        devices,
    }
}

/// The correspondents a plane holds, as address spellings.
fn correspondents(reach: &correspondence::plane::ReachPlane) -> Vec<String> {
    let mine = reach.profile().clone();
    reach
        .registry_profiles()
        .into_iter()
        .filter(|profile| profile != &mine)
        .map(|profile| profile.as_str().to_owned())
        .collect()
}

/// Whether a request belongs to this service.
///
/// Named rather than matched at the call site so the dispatch in
/// [`crate::daemon::host`] reads as one line, the way the book's does.
pub fn is_correspondence_request(request: &Request) -> bool {
    matches!(
        request,
        Request::ReachShare
            | Request::ReachLearn { .. }
            | Request::ReachResolve { .. }
            | Request::ReachView
            | Request::CorrespondSend { .. }
            | Request::CorrespondCollect
            | Request::CorrespondBlock { .. }
            | Request::CorrespondInvite { .. }
    )
}

/// The profile's live device set as this daemon holds it.
///
/// `None` on the watch is "not restored, not held" — unmeasured is absent, and
/// an empty `Vec` would read as a profile with no devices, which no profile
/// is. The hub's own-admission fails closed on `None` for exactly that reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnDevices {
    pub profile: ProfileId,
    pub me: DeviceId,
    /// Sorted, and includes `me`.
    pub devices: Vec<DeviceId>,
}

/// What a sponsor hands a joiner at `Start`: its own card, which device is
/// speaking, and the epoch the plane is at.
pub struct OwnCard {
    pub card: addressbook::Announcement,
    pub epoch: u64,
}

/// The identity's correspondence plane, and the durable state under it.
pub struct CorrespondenceService {
    identity: PathBuf,
    /// The device set, published to whoever admits on it. `None` until the
    /// plane is restored, and republished by every act that changes the set;
    /// the hub fails closed on `None`.
    own: watch::Sender<Option<OwnDevices>>,
    /// `None` until the plane is restored. Restoring needs no carrier: the
    /// device set is this identity's whether or not anything carries, and a
    /// plane that only stood beside a Post would leave the hub admitting
    /// nobody on every daemon without one.
    plane: Mutex<Option<Plane>>,
    /// The identity's book, hooked once at wiring. The one seam between the
    /// two services, and it carries gestures, not state: learning a
    /// correspondent installs their card, sharing reach presents My Card.
    /// Absent (tests, a book that failed to open), both halves simply do not
    /// happen — reach itself never depends on the book.
    book: std::sync::OnceLock<std::sync::Arc<crate::daemon::address_book::AddressBookService>>,
    /// What the fan-out knows about the own devices, hooked once at wiring
    /// like the book. Absent, every device renders unprobed and holding
    /// nothing known — a daemon that runs no fan-out has not asked.
    fanout: std::sync::OnceLock<std::sync::Arc<crate::daemon::fanout::Facts>>,
    /// What the net plane knows about reaching the own devices, hooked once
    /// like the rest. Absent — no plane mounted — every device renders with
    /// no reach and the interface is `None`, which is "nothing carried" and
    /// never "unreachable".
    netplane: std::sync::OnceLock<std::sync::Arc<crate::daemon::netplane::Facts>>,
}

/// What a restored plane holds: the identity's reach, and something to carry
/// by. The carrier is boxed because which contractor is carrying is a
/// deployment choice rather than an architecture commitment — memory in tests,
/// a hosted Post today, a direct peer later — and the plane never learns which.
struct Plane {
    reach: correspondence::plane::ReachPlane,
    /// `None` until a carrier is configured. Correspondence with no carrier is
    /// not an empty mailbox — every send and collect refuses in words, because
    /// the two are different facts and only one is worth acting on.
    contractor: Option<Box<dyn Contractor>>,
    /// Where a short address is issued and resolved. Separate from the carrier
    /// on purpose: the two answer different questions — *who is this person and
    /// which devices do they hold* against *may you read this mailbox* — and
    /// one being down must not take the other with it.
    directory: Option<Box<dyn Directory + Send>>,
}

impl CorrespondenceService {
    /// Open the service for one identity. Nothing stands yet: the plane is
    /// restored by [`Self::restore`], and until then this refuses honestly.
    pub fn open(identity: &Path) -> Self {
        Self {
            identity: identity.to_path_buf(),
            own: watch::Sender::new(None),
            plane: Mutex::new(None),
            book: std::sync::OnceLock::new(),
            fanout: std::sync::OnceLock::new(),
            netplane: std::sync::OnceLock::new(),
        }
    }

    /// The device set as it is republished: by restore, and by every act that
    /// changes it. A reader sees `None` until the first publication.
    #[must_use]
    pub fn own_devices(&self) -> watch::Receiver<Option<OwnDevices>> {
        self.own.subscribe()
    }

    /// Hook the identity's book, once, at wiring time.
    pub fn hook_book(&self, book: std::sync::Arc<crate::daemon::address_book::AddressBookService>) {
        let _ = self.book.set(book);
    }

    /// Hook the fan-out's facts, once, when the daemon mounts the loop.
    pub(crate) fn hook_fanout(&self, facts: std::sync::Arc<crate::daemon::fanout::Facts>) {
        let _ = self.fanout.set(facts);
    }

    /// The fan-out's facts, for the acts that change what it remembers.
    /// `None` on a daemon that runs no fan-out, which is a daemon that has
    /// never asked — not one whose devices hold nothing.
    pub(crate) fn fanout(&self) -> Option<&std::sync::Arc<crate::daemon::fanout::Facts>> {
        self.fanout.get()
    }

    /// Hook the net plane's facts, once, when the daemon mounts the tunnel.
    pub(crate) fn hook_netplane(&self, facts: std::sync::Arc<crate::daemon::netplane::Facts>) {
        let _ = self.netplane.set(facts);
    }

    /// Publish a device set as if the plane held it. For tests that stand
    /// two daemons as one profile without running the pairing ceremony.
    #[cfg(test)]
    pub(crate) fn set_own_for_test(&self, own: Option<OwnDevices>) {
        self.own.send_replace(own);
    }

    /// Where this identity's durable correspondence state lives.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.identity
    }

    /// Stand the plane up from what the identity home holds, and publish the
    /// device set.
    ///
    /// Reads the carried genesis back and never derives: a store that is
    /// absent or carries no genesis is refused, because boot founds or
    /// migrates the home before this runs (`config::identity_profile`) and a
    /// plane that founded here in its place would answer a new profile under
    /// an old address. Needs no carrier — the set is this identity's either
    /// way, and the hub admits on it. A carrier already configured survives a
    /// second restore.
    pub fn restore(&self, now: u64) -> Result<(), String> {
        let seed =
            crate::config::load_identity(&self.identity).map_err(|error| error.to_string())?;
        let held = addressbook::ReachStore::at(&self.identity)
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "this identity carries no kinship store".to_string())?;
        let reach = correspondence::plane::ReachPlane::restore(seed, held, now)
            .map_err(|error| format!("{error}"))?;
        let own = own_of(&reach);
        {
            let mut plane = self
                .plane
                .lock()
                .map_err(|_| "the correspondence plane is poisoned".to_string())?;
            let (contractor, directory) = plane
                .take()
                .map_or((None, None), |held| (held.contractor, held.directory));
            *plane = Some(Plane {
                reach,
                contractor,
                directory,
            });
        }
        self.own.send_replace(Some(own));
        Ok(())
    }

    /// Carry over a Post. Sets the carrier only: the plane must already be
    /// restored, because carrying was never what made it stand.
    pub fn carry_over(&self, contractor: Box<dyn Contractor>, now: u64) -> Result<(), String> {
        self.carry_over_with(contractor, configured_directory(&self.identity), now)
    }

    /// The same, with the directory named rather than read from the
    /// environment. What the tests use, and what keeps `carry_over` honest
    /// about having exactly one source of deployment truth.
    pub fn carry_over_with(
        &self,
        contractor: Box<dyn Contractor>,
        directory: Option<Box<dyn Directory + Send>>,
        _now: u64,
    ) -> Result<(), String> {
        let mut held = self
            .plane
            .lock()
            .map_err(|_| "the correspondence plane is poisoned".to_string())?;
        let plane = held
            .as_mut()
            .ok_or_else(|| "the reach plane is not restored".to_string())?;
        plane.contractor = Some(contractor);
        plane.directory = directory;
        drop(held);
        Ok(())
    }

    /// Write the plane's durable half back where it was read from.
    fn keep(&self, plane: &Plane) -> Result<(), String> {
        addressbook::ReachStore::at(&self.identity)
            .save(&plane.reach.state())
            .map_err(|error| error.to_string())
    }

    /// Announce this identity's reach publicly, presenting My Card when one
    /// is claimed, and publish it to the directory when one is configured.
    ///
    /// Sharing reach is the gesture that presents: a person who claimed My
    /// Card and now publishes where they answer means to be recognized there.
    /// No card claimed, nothing presented — authoring is never publishing on
    /// its own. Publishing to a directory is best-effort and deliberately so:
    /// announcing is what makes this identity reachable *at all* — the pasted
    /// artifact works with no service anywhere — and a directory being down
    /// must not take that with it. What is lost when it fails is the short
    /// spelling, and the next share publishes again. The caller keeps.
    fn present(&self, plane: &mut Plane, now: u64) -> Result<(), String> {
        let reader = plane.reach.standing();
        let portrait = self.book.get().and_then(|book| book.my_portrait());
        let announced = match &portrait {
            Some(portrait) => plane
                .reach
                .announce_presenting(Audience::Public, &reader, portrait),
            None => plane.reach.announce(Audience::Public, &reader),
        };
        let announcement = announced.map_err(|error| format!("{error}"))?;
        if let Some(directory) = plane.directory.as_deref_mut() {
            let seed = plane.reach.seed();
            match lait_directory::publish_as(directory, &seed, &announcement, now) {
                Ok(published) => {
                    plane
                        .reach
                        .issued(published.issued.address.as_str().to_owned());
                    // The receipt rides back with the address, and this is
                    // where it stops being the directory's word: the leaf is
                    // recomputed from the bytes just signed, the head is
                    // ratcheted against the pin this identity holds for that
                    // marker, and the marks are checked under it. Best effort
                    // by construction — what a marker recorded is a tier a
                    // reader weighs, and it must never gate the share that
                    // made this identity reachable at all.
                    self.follow_marker(&published);
                }
                Err(refusal) => {
                    tracing::warn!(%refusal, "the directory did not take this publication");
                }
            }
        }
        Ok(())
    }

    /// Ratchet the directory's chronicle on the receipt it just answered.
    ///
    /// The directory was chronicle-blind: it received receipts and never judged
    /// one, so nothing this identity published through it was ever placed in a
    /// log it follows. It goes through the same store the route publication
    /// does — one pin per marker, whoever brought the receipt — and a refusal is
    /// a line in the journal, never a failed share.
    ///
    /// The receipt is bound to the bytes first, exactly as the route path's
    /// `check_receipt` does. A receipt that proves the inclusion of some *other*
    /// publication is not a receipt for this one, and an older one is the
    /// dangerous case rather than the harmless one: its marks verify against the
    /// pinned head perfectly, and letting them through would replace this
    /// identity's mark set with a stale one — re-certifying a device the
    /// publication just made had dropped.
    fn follow_marker(&self, published: &lait_directory::Publication) {
        let Some(base) = crate::config::Settings::load(Some(&self.identity)).directory_url() else {
            return;
        };
        let receipt = &published.issued.receipt;
        let bound = match (receipt.head.as_ref(), receipt.entry) {
            // No head: a directory that keeps no chronicle. There is nothing to
            // bind and nothing will be stored from it.
            (None, _) => true,
            (Some(head), Some(entry)) => mechanics::chronicle::verify_inclusion(
                &published.leaf,
                entry,
                head.size,
                &head.root,
                &receipt.inclusion,
            )
            .is_ok(),
            (Some(_), None) => false,
        };
        if !bound {
            tracing::warn!(
                "the directory's receipt does not prove it recorded THIS publication; its marks                  are ignored and the mark set this identity holds is left alone"
            );
        }
        let entry = crate::daemon::markers::entry_for(&self.identity, &base);
        let receipt = bound.then_some(receipt);
        if let Some(why) =
            crate::daemon::markers::ratchet(&self.identity, &entry, receipt).refused()
        {
            tracing::warn!(%why, "the directory's chronicle did not check out");
        }
    }

    /// This identity's card for its own device, with the epoch a sponsor's
    /// pairing `Start` seals the link one past.
    pub fn own_card(&self) -> Result<OwnCard, String> {
        let held = self
            .plane
            .lock()
            .map_err(|_| "the correspondence plane is poisoned".to_string())?;
        let plane = held
            .as_ref()
            .ok_or_else(|| "the reach plane is not restored".to_string())?;
        let me = plane.reach.canonical_device();
        let card = plane
            .reach
            .own_card(&me)
            .map_err(|error| format!("{error}"))?;
        Ok(OwnCard {
            card,
            epoch: plane.reach.state().epoch,
        })
    }

    /// How this device came to hold its profile; `None` until restored.
    #[must_use]
    pub fn origin(&self) -> Option<addressbook::reach_store::Origin> {
        self.plane
            .lock()
            .ok()?
            .as_ref()
            .map(|plane| plane.reach.origin().clone())
    }

    /// The sponsor's adoption: append the assembled link, keep, republish the
    /// device set, then — when a directory is configured — announce so
    /// correspondents learn the grown set. Kept before the watch moves: a hub
    /// admitting a device the store does not yet name would be admitting on
    /// a fact a crash could take back.
    pub fn adopt_device(
        &self,
        link: mechanics::kinship::DeviceLink,
        now: u64,
    ) -> Result<(), String> {
        let mut held = self
            .plane
            .lock()
            .map_err(|_| "the correspondence plane is poisoned".to_string())?;
        let plane = held
            .as_mut()
            .ok_or_else(|| "the reach plane is not restored".to_string())?;
        plane
            .reach
            .adopt_device(link)
            .map_err(|error| format!("{error}"))?;
        self.keep(plane)?;
        self.own.send_replace(Some(own_of(&plane.reach)));
        // Announcing comes after the link is durable: a publication that
        // avowed a device the store did not yet name would be evidence of a
        // fact a crash could take back. Best-effort, and kept again on
        // success because it moves the epoch.
        if plane.directory.is_some() {
            match self.present(plane, now) {
                Ok(()) => {
                    if let Err(error) = self.keep(plane) {
                        tracing::warn!(%error, "the announcement's epoch could not be kept");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "the grown device set could not be announced");
                }
            }
        }
        Ok(())
    }

    /// Retire one of this profile's devices: append the signed retirement,
    /// keep, republish the set, then announce so correspondents stop sealing
    /// to it.
    ///
    /// Kept before the watch moves, for the reason adoption is: the hub, the
    /// fan-out and the tunnel all admit on what this publishes, and a set
    /// narrowed on a fact a crash could take back would leave a device
    /// refused here and named on disk. The de-listing that follows in every
    /// Space is a separate signed act per Space and is never derived from
    /// this one — kinship says who is a device of this person, and only a
    /// Space's own ledger says who may write in it.
    pub fn retire_device(&self, device: &DeviceId, now: u64) -> Result<(), String> {
        let mut held = self
            .plane
            .lock()
            .map_err(|_| "the correspondence plane is poisoned".to_string())?;
        let plane = held
            .as_mut()
            .ok_or_else(|| "the reach plane is not restored".to_string())?;
        plane
            .reach
            .retire_device(device)
            .map_err(|error| format!("{error}"))?;
        self.keep(plane)?;
        self.own.send_replace(Some(own_of(&plane.reach)));
        if plane.directory.is_some() {
            match self.present(plane, now) {
                Ok(()) => {
                    if let Err(error) = self.keep(plane) {
                        tracing::warn!(%error, "the announcement's epoch could not be kept");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "the narrowed device set could not be announced");
                }
            }
        }
        Ok(())
    }

    /// The joiner's adoption: become a device of the carried profile, keep,
    /// republish. Refuses — through the plane — a device that has already
    /// corresponded as its own profile.
    pub fn become_device_of(
        &self,
        card: addressbook::Announcement,
        from: DeviceId,
        link: mechanics::kinship::DeviceLink,
        now: u64,
    ) -> Result<(), String> {
        let mut held = self
            .plane
            .lock()
            .map_err(|_| "the correspondence plane is poisoned".to_string())?;
        let plane = held
            .as_mut()
            .ok_or_else(|| "the reach plane is not restored".to_string())?;
        plane
            .reach
            .become_device_of(card, from, link, now)
            .map_err(|error| format!("{error}"))?;
        self.keep(plane)?;
        self.own.send_replace(Some(own_of(&plane.reach)));
        Ok(())
    }

    /// The book half of learning somebody. The consent was the learn itself —
    /// pasting an announcement or resolving an address *is* accepting the
    /// introduction — so their declared name and devices land as a card with
    /// `Declared` evidence, no second question. No name legible to this
    /// reader means nothing installs: a bare device set is not a person worth
    /// a row, and reach never depends on the book either way. Failures are
    /// noted, not raised — the learn already succeeded and stays succeeded.
    fn adopt_into_book(&self, plane: &Plane, profile: &mechanics::kinship::ProfileId) {
        let Some(book) = self.book.get() else {
            return;
        };
        // Never your own profile. Learning your own announcement (testing it,
        // or a correspondent echoing it back) resolves your own name and
        // devices, and would mint a phantom self-card — a duplicate that also
        // becomes a send target.
        if profile == plane.reach.profile() {
            return;
        }
        let reader = plane.reach.standing();
        let Some(name) = plane.reach.declared_name(profile, &reader) else {
            return;
        };
        // The presented self-description, when the profile avowed one: the
        // bio half of the portrait, carried onto the card beside the name.
        let note = plane
            .reach
            .portrait(profile, &reader)
            .map(|portrait| portrait.detail)
            .unwrap_or_default();
        let Some(devices) = plane.reach.resolve(profile) else {
            return;
        };
        let handles: Vec<addressbook::Handle> = devices
            .into_iter()
            .map(addressbook::Handle::Device)
            .collect();
        match book.install_introduced(profile, &name, &note, &handles) {
            Ok(true) => tracing::info!(name, "an introduced correspondent joined the book"),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, "the introduced correspondent could not join the book");
            }
        }
    }

    /// This identity's reach and everything said on it, as every answer
    /// reports it. Whole rather than incremental: a caller that had to
    /// reconstruct a transcript from deltas would be keeping a second copy of
    /// the mailbox, which is the thing this service exists to stop.
    fn reach_view(&self, plane: &Plane) -> Response {
        let mine = plane.reach.profile().as_str().to_owned();
        let held = correspondents(&plane.reach);

        // Received letters file under whoever signed them. A letter from a
        // device no held profile avows is a stranger writing first; it has
        // nobody to file under, so it goes to this identity's own transcript
        // rather than being dropped.
        let mut arrived: BTreeMap<String, Vec<crate::control::LetterView>> = BTreeMap::new();
        for opened in plane.reach.opened() {
            let (kind, body, invitation) = match &opened.content {
                correspondence::Content::Message { body } => ("message", Some(body.clone()), None),
                correspondence::Content::Invitation { coordinates } => (
                    "invitation",
                    None,
                    Some(
                        data_encoding::BASE32_NOPAD
                            .encode(coordinates)
                            .to_lowercase(),
                    ),
                ),
            };
            let peer = plane
                .reach
                .profile_of_device(&opened.from)
                .map_or_else(|| mine.clone(), |profile| profile.as_str().to_owned());
            arrived
                .entry(peer)
                .or_default()
                .push(crate::control::LetterView {
                    id: Some(opened.id.clone()),
                    mine: false,
                    kind: kind.into(),
                    body,
                    sent_at: opened.sent_at,
                    from_device: opened.from.as_str().to_owned(),
                    provenance_agrees: opened.provenance_agrees,
                    invitation,
                });
        }

        let my_device = plane.reach.canonical_device().as_str().to_owned();
        let mut peers: std::collections::BTreeSet<String> = held.iter().cloned().collect();
        peers.extend(arrived.keys().cloned());
        peers.insert(mine.clone());

        let conversations = peers
            .into_iter()
            .map(|peer| {
                let mut letters: Vec<crate::control::LetterView> = plane
                    .reach
                    .sent_to(&peer)
                    .iter()
                    .map(|sent| crate::control::LetterView {
                        id: None,
                        mine: true,
                        kind: if sent.invitation {
                            "invitation".into()
                        } else {
                            "message".into()
                        },
                        body: (!sent.invitation).then(|| sent.body.clone()),
                        sent_at: sent.at,
                        from_device: my_device.clone(),
                        provenance_agrees: true,
                        // A sent invitation is not acted on by the sender.
                        invitation: None,
                    })
                    .collect();
                letters.extend(arrived.remove(&peer).unwrap_or_default());
                letters.sort_by_key(|letter| letter.sent_at);
                crate::control::ConversationView { peer, letters }
            })
            .collect();

        // The device set as published, not re-read from the registry: the
        // view and the hub's admission must never disagree about who is own.
        let own = self.own.borrow().clone();
        let facts = self.fanout.get();
        // Read from the marker files, not asked over the network: drawing a
        // window must not be a network act, and a marker answering fine ten
        // seconds ago must not read as unreachable because a view was slow.
        let weighed = crate::daemon::markers::weighed(&self.identity);
        let net = self.netplane.get();
        let devices = own.as_ref().map_or_else(Vec::new, |own| {
            own.devices
                .iter()
                .map(|device| crate::control::OwnDeviceView {
                    device: device.as_str().to_owned(),
                    me: device == &own.me,
                    liveness: facts
                        .map_or_else(Default::default, |facts| facts.liveness_of(device)),
                    held: facts.map_or_else(Vec::new, |facts| facts.held_by(device)),
                    certified_by: weighed.certifying(device),
                    reach: net.and_then(|net| net.reach_of(device)),
                })
                .collect()
        });
        // The exclusions are read from the file for the view, not from what
        // the fan-out happens to remember: a decision a person made outlives
        // this process, and a restart that drew it as "nothing has offered
        // this yet" would be the fold the file exists to prevent.
        let spaces = match (facts, own.as_ref()) {
            (Some(facts), Some(own)) => facts.view(
                own,
                &crate::daemon::replica::ReplicaPolicy::load(&self.identity),
            ),
            _ => Vec::new(),
        };
        let origin = match plane.reach.origin() {
            addressbook::reach_store::Origin::Founded => crate::control::OriginView::Founded,
            addressbook::reach_store::Origin::Adopted { from, at } => {
                crate::control::OriginView::Adopted {
                    from: from.as_str().to_owned(),
                    at: *at,
                }
            }
        };

        Response::Reach(Box::new(crate::control::ReachView {
            announcement: plane
                .reach
                .card(&plane.reach.standing())
                .and_then(|card| card.render().ok()),
            profile: mine,
            address: plane.reach.address().map(ToOwned::to_owned),
            correspondents: held,
            resolved: None,
            conversations,
            me: Some(my_device),
            origin: Some(origin),
            devices,
            device_set_unknown: own.is_none(),
            spaces,
            markers: weighed.markers,
            interface: net.map(|net| net.interface()),
        }))
    }

    /// Whether a carrier is configured.
    #[must_use]
    pub fn carrying(&self) -> bool {
        self.plane.lock().is_ok_and(|plane| {
            plane
                .as_ref()
                .is_some_and(|plane| plane.contractor.is_some())
        })
    }

    /// The whole view, naming the profile a learn or resolve just took in —
    /// so a caller that resolved an address knows *who* without diffing the
    /// roster.
    fn reach_view_resolved(
        &self,
        plane: &Plane,
        profile: &mechanics::kinship::ProfileId,
    ) -> Response {
        let mut response = self.reach_view(plane);
        if let Response::Reach(view) = &mut response {
            view.resolved = Some(profile.as_str().to_owned());
        }
        response
    }

    /// This machine's device id on the wire, for the wake subscription.
    /// `None` when the plane never stood.
    #[must_use]
    pub fn my_wire_device(&self) -> Option<String> {
        let held = self.plane.lock().ok()?;
        held.as_ref()
            .map(|plane| plane.reach.canonical_device().as_str().to_owned())
    }

    /// One standing collect, for the daemon's own background collector: ask
    /// the carrier, file what waited, and persist only when something arrived.
    ///
    /// `Ok(filed)`. A daemon with no carrier or an unrestored plane answers
    /// `Err`, and the collector's cadence counts that as unreachable — the
    /// same distinction the request path keeps between an empty mailbox and a
    /// carrier that could not be asked.
    pub fn collect_standing(&self, now: u64) -> Result<usize, String> {
        let Ok(mut held) = self.plane.lock() else {
            return Err("the correspondence plane is poisoned".into());
        };
        let Some(plane) = held.as_mut() else {
            return Err("the reach plane is not restored".into());
        };
        let Some(contractor) = plane.contractor.as_deref() else {
            return Err("no carrier is configured for correspondence".into());
        };
        let collected = plane.reach.collect_via(contractor, now);
        if collected.filed > 0 {
            self.keep(plane)?;
        }
        match collected.unasked {
            Some(why) => Err(format!("the carrier could not be asked: {why}")),
            None => Ok(collected.filed),
        }
    }

    /// Answer one control-plane request.
    pub async fn handle(&self, request: Request) -> Response {
        let Ok(mut held) = self.plane.lock() else {
            return Response::err("the correspondence plane is poisoned");
        };
        let Some(plane) = held.as_mut() else {
            // Not "no devices" and not an empty mailbox: nothing stands. The
            // boot path says why in its own log; here a caller only needs to
            // know the plane was never restored.
            return Response::err("the reach plane is not restored");
        };
        let now = now_secs();
        // Not an empty mailbox. A caller has to be able to tell "nobody wrote
        // to you" from "we are carrying nothing at all", and only one of those
        // is worth acting on.
        const NO_CARRIER: &str = "no carrier is configured for correspondence";

        match request {
            Request::ReachView => self.reach_view(plane),

            Request::ReachShare => {
                if let Err(error) = self.present(plane, now) {
                    return Response::err(error);
                }
                match self.keep(plane) {
                    Ok(()) => self.reach_view(plane),
                    Err(error) => Response::err(error),
                }
            }

            Request::ReachResolve {
                address,
                accept_change,
            } => {
                let Ok(address) = lait_directory::Address::parse(&address) else {
                    return Response::err("that is not an address");
                };
                let Some(directory) = plane.directory.as_deref_mut() else {
                    // Not "nobody by that name". The two are different facts and
                    // only one is worth acting on.
                    return Response::err("no directory is configured to resolve an address");
                };
                let seed = plane.reach.seed();
                let announcement = match lait_directory::resolve_as(directory, &seed, &address, now)
                {
                    Ok(announcement) => announcement,
                    Err(refusal) => return Response::err(format!("{refusal}")),
                };

                // AUTH-18, and the reason this is a refusal rather than a
                // badge: a directory that answered with a substituted key must
                // not be able to cause a seal to it, and a warning is not a
                // defence when 13 to 14 percent of people read one.
                let reader = plane.reach.standing();
                if !accept_change {
                    if let Some(change) = plane.reach.change_on_learning(&announcement, &reader) {
                        return Response::err(format!(
                            "the devices {} avows have changed since you learned them                              ({} held, {} offered) — accept the change to go on",
                            change.profile.as_str(),
                            change.held.len(),
                            change.incoming.len()
                        ));
                    }
                }
                match plane.reach.learn(announcement, &reader) {
                    Ok(profile) => {
                        self.adopt_into_book(plane, &profile);
                        match self.keep(plane) {
                            Ok(()) => self.reach_view_resolved(plane, &profile),
                            Err(error) => Response::err(error),
                        }
                    }
                    Err(error) => Response::err(format!("{error}")),
                }
            }

            Request::ReachLearn { announcement } => {
                let Ok(parsed) = addressbook::Announcement::parse(&announcement) else {
                    return Response::err("that is not an announcement");
                };
                let reader = plane.reach.standing();
                match plane.reach.learn(parsed, &reader) {
                    Ok(profile) => {
                        self.adopt_into_book(plane, &profile);
                        match self.keep(plane) {
                            Ok(()) => self.reach_view_resolved(plane, &profile),
                            Err(error) => Response::err(error),
                        }
                    }
                    Err(error) => Response::err(format!("{error}")),
                }
            }

            Request::CorrespondSend { to, body } => {
                let Some(profile) = ProfileId::parse(&to) else {
                    return Response::err("that is not an address");
                };
                let Some(contractor) = plane.contractor.as_deref() else {
                    return Response::err(NO_CARRIER);
                };
                let content = correspondence::Content::Message { body };
                match plane.reach.send_via(contractor, &profile, content, now) {
                    // Durable before it is reported. The carrier forgets a
                    // letter once its recipient acknowledges, so this copy is
                    // the only one there will ever be — and a send that answered
                    // before keeping it would lose the half a person wrote on
                    // the next restart, with nothing to say it had.
                    Ok(_) => match self.keep(plane) {
                        Ok(()) => self.reach_view(plane),
                        Err(error) => Response::err(error),
                    },
                    Err(error) => Response::err(format!("{error}")),
                }
            }

            Request::CorrespondInvite { to, link } => {
                let Some(profile) = ProfileId::parse(&to) else {
                    return Response::err("that is not an address");
                };
                let body = link
                    .trim()
                    .strip_prefix("lait://join/")
                    .unwrap_or_else(|| link.trim());
                let cleaned: String = body.chars().filter(|c| !c.is_whitespace()).collect();
                let Ok(coordinates) =
                    data_encoding::BASE32_NOPAD.decode(cleaned.to_uppercase().as_bytes())
                else {
                    return Response::err("that is not an invite link");
                };
                // Opaque the whole way. This service carries an invitation and
                // never judges one: it verifies at the Space that issued it, and
                // delivery was never admission.
                let Some(contractor) = plane.contractor.as_deref() else {
                    return Response::err(NO_CARRIER);
                };
                let content = correspondence::Content::Invitation { coordinates };
                match plane.reach.send_via(contractor, &profile, content, now) {
                    // Durable before it is reported. The carrier forgets a
                    // letter once its recipient acknowledges, so this copy is
                    // the only one there will ever be — and a send that answered
                    // before keeping it would lose the half a person wrote on
                    // the next restart, with nothing to say it had.
                    Ok(_) => match self.keep(plane) {
                        Ok(()) => self.reach_view(plane),
                        Err(error) => Response::err(error),
                    },
                    Err(error) => Response::err(format!("{error}")),
                }
            }

            Request::CorrespondCollect => {
                let Some(contractor) = plane.contractor.as_deref() else {
                    return Response::err(NO_CARRIER);
                };
                let collected = plane.reach.collect_via(contractor, now);
                // Persist only what changed: a collect that filed nothing has
                // nothing to keep, and a client polling for freshness must
                // not turn into a disk write per poll.
                if collected.filed > 0 {
                    if let Err(error) = self.keep(plane) {
                        return Response::err(error);
                    }
                }
                match collected.unasked {
                    // A carrier that could not be asked is reported, never
                    // folded into "nothing was waiting".
                    Some(why) => Response::err(format!("the carrier could not be asked: {why}")),
                    None => self.reach_view(plane),
                }
            }

            Request::CorrespondBlock { device, blocked } => {
                let Some(device) = DeviceId::parse(&device) else {
                    return Response::err("that is not a device");
                };
                let Some(contractor) = plane.contractor.as_deref() else {
                    return Response::err(NO_CARRIER);
                };
                match plane.reach.block_via(contractor, &device, blocked, now) {
                    Ok(()) => self.reach_view(plane),
                    Err(error) => Response::err(format!("{error}")),
                }
            }

            _ => Response::err("not a correspondence request"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(tag: &str) -> CorrespondenceService {
        let dir = std::env::temp_dir().join(format!("corr-svc-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        CorrespondenceService::open(&dir)
    }

    /// What boot does before the router stands: mint the identity and found
    /// (or carry) its profile. The service restores from that and never
    /// founds, so a test that skipped this would be testing a refusal.
    fn found(home: &Path) {
        std::fs::create_dir_all(home).unwrap();
        crate::config::load_or_create_identity(home).expect("identity");
        crate::config::identity_profile(home).expect("profile");
    }

    /// A service stood up over a founded home, carrying nothing yet.
    fn restored(home: &Path) -> CorrespondenceService {
        found(home);
        let service = CorrespondenceService::open(home);
        service.restore(now_secs()).expect("restore");
        service
    }

    #[test]
    fn every_correspondence_request_reaches_this_service_and_nothing_else_does() {
        assert!(is_correspondence_request(&Request::ReachShare));
        assert!(is_correspondence_request(&Request::ReachView));
        assert!(is_correspondence_request(&Request::CorrespondCollect));
        assert!(is_correspondence_request(&Request::ReachLearn {
            announcement: "x".into()
        }));
        // Omitting a routed request here strands it on "no daemon-scoped
        // handler" — ReachResolve shipped unreachable exactly this way.
        assert!(is_correspondence_request(&Request::ReachResolve {
            address: "tin-harbor-quiet-4417".into(),
            accept_change: false,
        }));
        assert!(is_correspondence_request(&Request::CorrespondSend {
            to: "prf_x".into(),
            body: "hi".into()
        }));
        assert!(is_correspondence_request(&Request::CorrespondInvite {
            to: "prf_x".into(),
            link: "lait://join/aa".into()
        }));
        assert!(is_correspondence_request(&Request::CorrespondBlock {
            device: "dev_x".into(),
            blocked: true,
        }));
        assert!(!is_correspondence_request(&Request::BookList));
    }

    fn reach(response: &Response) -> &crate::control::ReachView {
        match response {
            Response::Reach(view) => view,
            other => panic!("expected a reach view, got {other:?}"),
        }
    }

    /// The daemon carries correspondence with no service anywhere.
    ///
    /// This is what the contractor seam is for. `MemCarrier` stands in for the
    /// Post the way `comms::mem::MemTransport` stands in for a network, so the
    /// plane can be exercised where it now lives without standing anything up —
    /// and a test that needed a hosted Post to prove the daemon holds a mailbox
    /// would be proving the Post instead.
    /// The whole block journey over the daemon's own verbs: a letter lands,
    /// the proven writer is refused at the carrier, the next letter never
    /// lands, and lifting the block lets one land again. Also holds the
    /// learn reply naming *who* was learned — the fact a caller that
    /// resolved a friend code sends an invitation to.
    #[tokio::test]
    async fn a_blocked_sender_stops_landing_at_the_carrier() {
        let root = std::env::temp_dir().join(format!("corr-block-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (ada_home, grace_home) = (root.join("ada"), root.join("grace"));
        let shared = correspondence::SharedMem::new();
        let ada = restored(&ada_home);
        let grace = restored(&grace_home);
        let now = now_secs();
        ada.carry_over_with(Box::new(shared.clone()), None, now)
            .expect("ada");
        grace
            .carry_over_with(Box::new(shared.clone()), None, now)
            .expect("grace");

        let ada_card = reach(&ada.handle(Request::ReachShare).await)
            .announcement
            .clone()
            .expect("ada publishes");
        let grace_view = reach(&grace.handle(Request::ReachShare).await).clone();
        let grace_card = grace_view.announcement.clone().expect("grace publishes");
        let learned = reach(
            &grace
                .handle(Request::ReachLearn {
                    announcement: ada_card,
                })
                .await,
        )
        .clone();
        assert_eq!(
            learned.resolved.as_deref(),
            Some(learned.correspondents[0].as_str()),
            "the learn reply names who was learned"
        );
        ada.handle(Request::ReachLearn {
            announcement: grace_card,
        })
        .await;

        ada.handle(Request::CorrespondSend {
            to: grace_view.profile.clone(),
            body: "first".into(),
        })
        .await;
        let view = reach(&grace.handle(Request::CorrespondCollect).await).clone();
        let writer = view
            .conversations
            .iter()
            .flat_map(|conversation| conversation.letters.iter())
            .find(|letter| !letter.mine)
            .expect("the first letter landed")
            .from_device
            .clone();

        let blocked = grace
            .handle(Request::CorrespondBlock {
                device: writer.clone(),
                blocked: true,
            })
            .await;
        assert!(
            matches!(blocked, Response::Reach(_)),
            "blocking answers with the view, got {blocked:?}"
        );
        ada.handle(Request::CorrespondSend {
            to: grace_view.profile.clone(),
            body: "second".into(),
        })
        .await;
        let after = reach(&grace.handle(Request::CorrespondCollect).await).clone();
        let landed = |view: &crate::control::ReachView| -> usize {
            view.conversations
                .iter()
                .flat_map(|conversation| conversation.letters.iter())
                .filter(|letter| !letter.mine)
                .count()
        };
        assert_eq!(landed(&after), 1, "the blocked sender's letter never lands");

        grace
            .handle(Request::CorrespondBlock {
                device: writer,
                blocked: false,
            })
            .await;
        ada.handle(Request::CorrespondSend {
            to: grace_view.profile.clone(),
            body: "third".into(),
        })
        .await;
        let lifted = reach(&grace.handle(Request::CorrespondCollect).await).clone();
        assert_eq!(landed(&lifted), 2, "lifting the block lets mail land again");
    }

    #[tokio::test]
    async fn the_daemon_carries_a_letter_between_two_identities_with_no_service() {
        let root = std::env::temp_dir().join(format!("corr-mem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (ada_home, grace_home) = (root.join("ada"), root.join("grace"));

        // One carrier, two services: the shared store two people deposit into,
        // which is exactly the Post's role without the Post.
        let shared = correspondence::SharedMem::new();
        let ada = restored(&ada_home);
        let grace = restored(&grace_home);
        let now = now_secs();
        ada.carry_over(Box::new(shared.clone()), now).expect("ada");
        grace
            .carry_over(Box::new(shared.clone()), now)
            .expect("grace");

        // Each publishes; each takes the other in. Nothing else is shared.
        let ada_card = reach(&ada.handle(Request::ReachShare).await)
            .announcement
            .clone()
            .expect("ada publishes");
        let grace_view = reach(&grace.handle(Request::ReachShare).await).clone();
        let grace_card = grace_view.announcement.clone().expect("grace publishes");

        let learned = reach(
            &grace
                .handle(Request::ReachLearn {
                    announcement: ada_card,
                })
                .await,
        )
        .clone();
        assert_eq!(learned.correspondents.len(), 1, "Grace holds Ada");
        let ada_address = learned.correspondents[0].clone();

        ada.handle(Request::ReachLearn {
            announcement: grace_card,
        })
        .await;

        let sent = ada
            .handle(Request::CorrespondSend {
                to: grace_view.profile.clone(),
                body: "carried by the daemon".into(),
            })
            .await;
        assert!(
            matches!(sent, Response::Reach(_)),
            "the send is answered with what changed, got {sent:?}"
        );

        let collected = grace.handle(Request::CorrespondCollect).await;
        let view = reach(&collected).clone();
        assert_eq!(
            view.correspondents,
            vec![ada_address.clone()],
            "Grace still holds exactly the one correspondent she learned"
        );

        // The letter is filed under whoever signed it, in the transcript the
        // daemon holds — not under Grace's own conversation, and not nowhere.
        let from_ada = view
            .conversations
            .iter()
            .find(|c| c.peer == ada_address)
            .expect("a conversation with Ada");
        assert_eq!(from_ada.letters.len(), 1);
        let letter = &from_ada.letters[0];
        assert!(!letter.mine);
        assert_eq!(letter.kind, "message");
        assert_eq!(letter.body.as_deref(), Some("carried by the daemon"));
        assert!(letter.id.is_some(), "an arrived letter names itself");
        assert!(letter.provenance_agrees);

        // Ada's own copy of what she wrote, which the carrier will forget the
        // moment it is acknowledged.
        let ada_side = reach(&ada.handle(Request::ReachView).await).clone();
        let to_grace = ada_side
            .conversations
            .iter()
            .find(|c| c.peer == grace_view.profile)
            .expect("Ada's side of it");
        assert_eq!(to_grace.letters.len(), 1);
        assert!(to_grace.letters[0].mine, "the half she wrote");
        assert_eq!(
            to_grace.letters[0].body.as_deref(),
            Some("carried by the daemon")
        );

        // The daemon goes away entirely and comes back from disk.
        drop(ada);
        let ada = restored(&ada_home);
        ada.carry_over(Box::new(shared.clone()), now)
            .expect("ada again");
        let after = reach(&ada.handle(Request::ReachView).await).clone();
        assert_eq!(
            after.profile, ada_side.profile,
            "the address Ada handed out still names her"
        );
        assert_eq!(
            after
                .conversations
                .iter()
                .find(|c| c.peer == grace_view.profile)
                .expect("still there")
                .letters
                .len(),
            1,
            "and what she wrote survived the restart"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two identities on one carrier, and a third that learns nothing.
    ///
    /// The first assertion a reader looks for in something called sealed
    /// correspondence, and one no test at this level made while the mailbox
    /// lived in the client.
    #[tokio::test]
    async fn a_third_identity_on_the_same_carrier_gets_nothing() {
        let world = World::new("bystander", &["ada", "grace", "eve"]).await;
        world.introduce("ada", "grace").await;

        world
            .service("ada")
            .handle(Request::CorrespondSend {
                to: world.address("grace").await,
                body: "for Grace only".into(),
            })
            .await;

        world
            .service("eve")
            .handle(Request::CorrespondCollect)
            .await;
        let seen: Vec<String> = reach(&world.service("eve").handle(Request::ReachView).await)
            .conversations
            .iter()
            .flat_map(|conversation| conversation.letters.iter())
            .filter_map(|letter| letter.body.clone())
            .collect();
        assert!(seen.is_empty(), "a bystander collects nothing: {seen:?}");
    }

    /// An invitation crosses as an invitation, and arrives carrying the
    /// coordinates a Space will judge.
    #[tokio::test]
    async fn an_invitation_arrives_as_one_with_its_coordinates_intact() {
        let world = World::new("invite", &["ada", "grace"]).await;
        world.introduce("ada", "grace").await;

        let link = "lait://join/aebagbafaydqqcikbmga";
        world
            .service("ada")
            .handle(Request::CorrespondInvite {
                to: world.address("grace").await,
                link: link.into(),
            })
            .await;
        world
            .service("grace")
            .handle(Request::CorrespondCollect)
            .await;

        let ada_address = world.address("ada").await;
        let view = reach(&world.service("grace").handle(Request::ReachView).await).clone();
        let from_ada = view
            .conversations
            .iter()
            .find(|c| c.peer == ada_address)
            .expect("a conversation with Ada");
        let letter = from_ada.letters.first().expect("one letter");
        assert_eq!(letter.kind, "invitation");
        assert!(letter.body.is_none(), "an invitation is acted on, not read");
        assert_eq!(
            letter.invitation.as_deref(),
            Some("aebagbafaydqqcikbmga"),
            "the coordinates crossed intact, which is what lets the Space judge them"
        );
    }

    /// An address nobody has handed over is not reachable — a different answer
    /// from the message failing, and the one a surface can act on.
    #[tokio::test]
    async fn a_stranger_is_not_reachable_rather_than_a_failed_send() {
        let world = World::new("stranger", &["ada", "nobody"]).await;
        let unknown = world.address("nobody").await;
        let answer = world
            .service("ada")
            .handle(Request::CorrespondSend {
                to: unknown,
                body: "hello?".into(),
            })
            .await;
        assert!(
            matches!(&answer, Response::Error { message, .. } if message.contains("reach")),
            "{answer:?}"
        );
    }

    /// A carrier that cannot be asked is reported, never folded into "nothing
    /// was waiting". The two look identical to a surface and only one is worth
    /// acting on.
    #[tokio::test]
    async fn a_carrier_that_cannot_be_asked_is_not_an_empty_mailbox() {
        let world = World::new("dark", &["ada"]).await;
        world.carrier.seal_off("the carrier is down");
        let answer = world
            .service("ada")
            .handle(Request::CorrespondCollect)
            .await;
        assert!(
            matches!(&answer, Response::Error { message, .. } if message.contains("could not be asked")),
            "{answer:?}"
        );
    }

    /// Sharing publishes to the directory too, and the address comes back in
    /// the view — the short, speakable thing a person says out loud instead of
    /// pasting two thousand base32 characters.
    #[tokio::test]
    async fn sharing_a_reach_issues_a_short_address() {
        let world = World::new("issued", &["ada"]).await;
        let answer = world.service("ada").handle(Request::ReachShare).await;
        let view = reach(&answer);
        let address = view.address.as_deref().expect("an address was issued");
        assert!(
            lait_directory::Address::parse(address).is_ok(),
            "{address} is not something this build would spell"
        );

        // Stable afterwards. A person hands this out; it cannot move under them.
        let again = world.service("ada").handle(Request::ReachShare).await;
        assert_eq!(reach(&again).address.as_deref(), Some(address));
    }

    /// The acceptance line, through the daemon: one identity says an address,
    /// the other reaches them with no other channel and no pasted artifact.
    #[tokio::test]
    async fn a_short_address_is_all_one_identity_needs_to_reach_another() {
        let world = World::new("byaddr", &["ada", "bob"]).await;

        let shared = world.service("ada").handle(Request::ReachShare).await;
        let address = reach(&shared).address.clone().expect("ada has an address");
        let ada = reach(&shared).profile.clone();

        let learned = world
            .service("bob")
            .handle(Request::ReachResolve {
                address,
                accept_change: false,
            })
            .await;
        assert!(
            reach(&learned).correspondents.contains(&ada),
            "bob resolved an address and did not learn who it named"
        );

        // And the relationship is usable, which is the only thing that makes
        // the address worth having.
        world
            .service("bob")
            .handle(Request::CorrespondSend {
                to: ada,
                body: "found you by address".into(),
            })
            .await;
        let collected = world
            .service("ada")
            .handle(Request::CorrespondCollect)
            .await;
        let arrived: Vec<&str> = reach(&collected)
            .conversations
            .iter()
            .flat_map(|c| c.letters.iter())
            .filter_map(|l| l.body.as_deref())
            .collect();
        assert!(
            arrived.contains(&"found you by address"),
            "nothing arrived: {arrived:?}"
        );
    }

    /// No directory is not "nobody by that name".
    ///
    /// The discipline this whole plane is most careful about, at the one place
    /// it is easiest to get wrong: a misconfigured directory and a person who
    /// does not exist look identical at a surface, and only one is worth acting
    /// on.
    #[tokio::test]
    async fn with_no_directory_an_address_refuses_in_words() {
        let root = std::env::temp_dir().join(format!("corr-nodir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let service = restored(&root);
        service
            .carry_over_with(Box::new(correspondence::SharedMem::new()), None, now_secs())
            .expect("carry");

        let answer = service
            .handle(Request::ReachResolve {
                address: "act-able-zoo-1234".into(),
                accept_change: false,
            })
            .await;
        assert!(
            matches!(&answer, Response::Error { message, .. } if message.contains("no directory")),
            "{answer:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Several identities, one carrier between them — the Post's shape without
    /// the Post.
    struct World {
        root: std::path::PathBuf,
        carrier: correspondence::SharedMem,
        /// One directory all of them reach, for the reason `SharedMem` exists:
        /// a directory is a place several people publish into and resolve from,
        /// and one owned by a single identity is not that.
        directory: lait_directory::Shared,
        services: BTreeMap<String, CorrespondenceService>,
    }

    impl World {
        async fn new(tag: &str, who: &[&str]) -> Self {
            let root = std::env::temp_dir().join(format!("corr-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let carrier = correspondence::SharedMem::new();
            let directory = lait_directory::Shared::new();
            let mut services = BTreeMap::new();
            for name in who {
                let home = root.join(name);
                let service = restored(&home);
                service
                    .carry_over_with(
                        Box::new(carrier.clone()),
                        Some(Box::new(directory.clone())),
                        now_secs(),
                    )
                    .expect("carry");
                services.insert((*name).to_owned(), service);
            }
            Self {
                root,
                carrier,
                directory,
                services,
            }
        }

        fn service(&self, who: &str) -> &CorrespondenceService {
            self.services.get(who).expect("who")
        }

        async fn address(&self, who: &str) -> String {
            reach(&self.service(who).handle(Request::ReachView).await)
                .profile
                .clone()
        }

        /// Each publishes, each takes the other in. Nothing else is shared.
        async fn introduce(&self, one: &str, other: &str) {
            for (a, b) in [(one, other), (other, one)] {
                let card = reach(&self.service(a).handle(Request::ReachShare).await)
                    .announcement
                    .clone()
                    .expect("published");
                self.service(b)
                    .handle(Request::ReachLearn { announcement: card })
                    .await;
            }
        }
    }

    impl Drop for World {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// No carrier is not an empty mailbox. A service that answered "nothing
    /// waiting" here would be the false-disconnection defect one layer down.
    #[tokio::test]
    async fn with_no_carrier_every_operation_refuses_in_words() {
        let root = std::env::temp_dir().join(format!("corr-nocarrier-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let service = restored(&root);
        assert!(!service.carrying());
        let answer = service.handle(Request::CorrespondCollect).await;
        assert!(
            matches!(&answer, Response::Error { message, .. } if message.contains("no carrier")),
            "{answer:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The device set is this identity's whether or not anything carries. A
    /// daemon with no Post configured — every debug build, and any deployment
    /// that opted out — still answers who its devices are, and the hub admits
    /// on that answer; only sending needs a carrier, and it says so in words a
    /// caller can tell from an empty mailbox.
    #[tokio::test]
    async fn a_daemon_with_no_post_still_answers_its_device_set() {
        let root = std::env::temp_dir().join(format!("corr-nopost-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let service = restored(&root);
        assert!(!service.carrying(), "nothing carries");

        let me = mechanics::actor::device_from_seed(
            &crate::config::load_identity(&root).expect("identity"),
        );
        let published = service
            .own_devices()
            .borrow()
            .clone()
            .expect("restore published the set");
        assert_eq!(published.me, me);
        assert_eq!(published.devices, vec![me.clone()]);

        let view = reach(&service.handle(Request::ReachView).await).clone();
        assert!(!view.device_set_unknown);
        assert_eq!(view.me.as_deref(), Some(me.as_str()));
        assert_eq!(view.devices.len(), 1, "{:?}", view.devices);
        assert_eq!(view.devices[0].device, me.as_str());
        assert!(view.devices[0].me);
        assert_eq!(
            view.devices[0].liveness,
            crate::control::Liveness::NotProbed,
            "nothing has asked, and that is not \"down\""
        );
        assert_eq!(view.origin, Some(crate::control::OriginView::Founded));
        assert_eq!(view.profile, published.profile.as_str());

        let answer = service
            .handle(Request::CorrespondSend {
                to: view.profile.clone(),
                body: "to nowhere".into(),
            })
            .await;
        assert!(
            matches!(&answer, Response::Error { message, .. } if message.contains("no carrier")),
            "{answer:?}"
        );

        // A carrier configured afterwards does not restore again, and the
        // published set is untouched by it.
        service
            .carry_over_with(Box::new(correspondence::SharedMem::new()), None, now_secs())
            .expect("carry");
        assert!(service.carrying());
        assert_eq!(
            service.own_devices().borrow().as_ref(),
            Some(&published),
            "carrying changes nothing about who the devices are"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A retirement republishes the set without the device — the one watch
    /// the hub admits on, the fan-out offers on and the tunnel routes on, so
    /// this narrowing is what drops a retired device's live route. Kept
    /// before it is published, and retiring the device this daemon *is* is
    /// refused: that seed is the one that would have to sign its own absence.
    #[tokio::test]
    async fn a_retirement_narrows_the_set_this_daemon_publishes() {
        let root = std::env::temp_dir().join(format!("corr-retire-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let service = restored(&root);
        let my_seed = crate::config::load_identity(&root).expect("identity");
        let me = mechanics::actor::device_from_seed(&my_seed);
        let other_seed = [93u8; 32];
        let other = mechanics::actor::device_from_seed(&other_seed);
        let (nonce, epoch) = ([44u8; 16], 3);
        let link = mechanics::kinship::DeviceLink::assemble(
            (
                me.clone(),
                mechanics::kinship::DeviceLink::half(&my_seed, &other, nonce, epoch),
            ),
            (
                other.clone(),
                mechanics::kinship::DeviceLink::half(&other_seed, &me, nonce, epoch),
            ),
            nonce,
            epoch,
        )
        .expect("assemble");
        service.adopt_device(link, now_secs()).expect("adopt");
        assert_eq!(
            service
                .own_devices()
                .borrow()
                .as_ref()
                .map(|own| own.devices.len()),
            Some(2)
        );

        assert!(
            service.retire_device(&me, now_secs()).is_err(),
            "a daemon retired the device it signs as"
        );
        service.retire_device(&other, now_secs()).expect("retire");
        let published = service
            .own_devices()
            .borrow()
            .clone()
            .expect("the set is published");
        assert_eq!(published.devices, vec![me.clone()]);

        // Durable before it is published: a fresh service over the same home
        // reads the retirement back.
        let again = CorrespondenceService::open(&root);
        again.restore(now_secs()).expect("restore");
        assert_eq!(
            again
                .own_devices()
                .borrow()
                .as_ref()
                .map(|own| own.devices.clone()),
            Some(vec![me])
        );
        assert!(
            service.retire_device(&other, now_secs()).is_err(),
            "retiring it twice reported as though something happened"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Unmeasured is absent. Before restore the watch holds `None`, not an
    /// empty set — the hub fails closed on it, and an empty `Vec` would read
    /// as a profile with no devices, which no profile is. Carrying over a
    /// plane that never stood is refused rather than standing it up.
    #[tokio::test]
    async fn the_watch_is_none_until_restored() {
        let service = service("unrestored");
        assert!(service.own_devices().borrow().is_none());
        assert!(!service.carrying());
        assert!(
            service
                .carry_over_with(Box::new(correspondence::SharedMem::new()), None, now_secs())
                .is_err(),
            "a carrier does not stand the plane up"
        );
        // A restore that fails — nothing founded here — publishes nothing:
        // the watch stays `None`, never `Some(empty)`.
        assert!(
            service.restore(now_secs()).is_err(),
            "nothing is founded in this home, so restore must refuse"
        );
        assert!(service.own_devices().borrow().is_none());
        let answer = service.handle(Request::ReachView).await;
        assert!(
            matches!(&answer, Response::Error { message, .. } if message.contains("not restored")),
            "{answer:?}"
        );
    }

    /// The seamless chain, asserted whole: Ada claims My Card, shares her
    /// reach — the gesture that presents — and Grace learns the announcement
    /// — the gesture that accepts the introduction. Ada lands in Grace's book
    /// under Ada's own declared name, Declared evidence, no second question.
    /// And the boundary holds twice over: learning again rewrites nothing,
    /// and an identity with no claimed card presents nothing.
    #[tokio::test]
    async fn learning_a_presented_announcement_installs_the_card() {
        let root = std::env::temp_dir().join(format!("corr-portrait-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (ada_home, grace_home) = (root.join("ada"), root.join("grace"));
        for home in [&ada_home, &grace_home] {
            std::fs::create_dir_all(home).unwrap();
            crate::config::load_or_create_identity(home).expect("identity");
        }

        // Ada authors My Card. Authoring publishes nothing on its own.
        {
            let store = addressbook::Store::at(&ada_home);
            let mut engine = addressbook::BookEngine::new();
            let author = addressbook::Author {
                device: mechanics::actor::device_from_seed(&[5u8; 32]),
                at: 1,
            };
            let id = addressbook::CardId::mint(&mechanics::ids::SystemUlidSource);
            engine
                .apply(
                    &author,
                    addressbook::Action::Create {
                        id: id.clone(),
                        name: "Ada Lovelace".into(),
                    },
                )
                .expect("create");
            engine
                .apply(&author, addressbook::Action::ClaimSelf { id })
                .expect("claim");
            store.replace(&engine).expect("save");
        }

        let shared = correspondence::SharedMem::new();
        let ada = restored(&ada_home);
        let grace = restored(&grace_home);
        ada.hook_book(std::sync::Arc::new(
            crate::daemon::address_book::AddressBookService::open(&ada_home).expect("ada book"),
        ));
        grace.hook_book(std::sync::Arc::new(
            crate::daemon::address_book::AddressBookService::open(&grace_home).expect("grace book"),
        ));
        let now = now_secs();
        ada.carry_over(Box::new(shared.clone()), now).expect("ada");
        grace
            .carry_over(Box::new(shared.clone()), now)
            .expect("grace");

        let ada_card = reach(&ada.handle(Request::ReachShare).await)
            .announcement
            .clone()
            .expect("ada publishes");
        grace
            .handle(Request::ReachLearn {
                announcement: ada_card.clone(),
            })
            .await;

        let book = addressbook::Store::at(&grace_home)
            .open()
            .expect("read")
            .expect("a book exists now")
            .book()
            .expect("project");
        let installed: Vec<_> = book
            .cards
            .values()
            .filter(|card| card.name.value == "Ada Lovelace")
            .collect();
        assert_eq!(
            installed.len(),
            1,
            "Ada joined Grace's book by her own name"
        );
        assert!(
            installed[0]
                .handles
                .iter()
                .all(|link| link.evidence == addressbook::Evidence::Declared),
            "worth what a self-claim is worth"
        );

        // Learning again rewrites nothing: the handle is known, so the book
        // is left alone.
        grace
            .handle(Request::ReachLearn {
                announcement: ada_card,
            })
            .await;
        let again = addressbook::Store::at(&grace_home)
            .open()
            .expect("read")
            .expect("book")
            .book()
            .expect("project");
        assert_eq!(again.cards.len(), book.cards.len(), "no duplicate row");

        // Grace claimed no card, so Ada learned no name: the presenting half
        // is the claimed card, not the identity's existence.
        let grace_card = reach(&grace.handle(Request::ReachShare).await)
            .announcement
            .clone()
            .expect("grace publishes");
        ada.handle(Request::ReachLearn {
            announcement: grace_card,
        })
        .await;
        let ada_book = addressbook::Store::at(&ada_home)
            .open()
            .expect("read")
            .expect("book")
            .book()
            .expect("project");
        assert_eq!(
            ada_book.cards.len(),
            1,
            "only My Card — an unpresented identity installs nothing"
        );
    }
}
