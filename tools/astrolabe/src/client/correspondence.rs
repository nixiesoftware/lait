//! The opt-in **front-end validation fixture** for correspondence.
//!
//! This is not a backend and does not pretend to be one. It stands up only
//! under `LAIT_CORRESPONDENCE_DEMO`; without the flag the client connects to no
//! carrier and every correspondence action refuses honestly. Its purpose is to
//! **drive and validate the chat UI with no daemon** — a reusable harness for
//! front-end work, seeded with the whole matrix of correspondents and message
//! kinds so every surface state has data behind it.
//!
//! When enabled it is a real **loopback carrier**: a real [`MemCarrier`], real
//! actor planes, and this identity's real seed, all in-process. Every byte it
//! moves goes through the real [`correspondence`] crate — a [`Letter`] is
//! composed, signed, sealed to its recipient, deposited under an unforgeable
//! [`egress`](mechanics::egress) witness, collected, opened, verified, and
//! filed. What is *stubbed* is only the reach: the carrier is this process
//! rather than the Space's hosted `lait-post`, and the correspondents are people
//! this fixture also holds the seeds for, so a conversation has a real other
//! side. The real backend — a daemon-held carrier and directory — replaces this
//! at the same `send`/`collect`/`block`/… seam.
//!
//! # Conversations, not an inbox
//!
//! Correspondence is drawn as a chat reached from the address book, never a
//! mailbox. So this world is organised by **person**: a person folds all their
//! devices into one conversation, and a message from any of their devices lands
//! in the one transcript.
//!
//! # The seeded matrix
//!
//! The demo covers the whole cross of who wrote and what they sent, so every
//! surface state has data behind it. Two axes:
//!
//! * **added** — a contact in the book (a friend) vs an **unadded** stranger who
//!   wrote first; and whether the correspondent is a person or an **agent**, and
//!   if an agent, whose.
//! * **kind** — a message vs an invitation.
//!
//! Five correspondents, each sending one message and one invitation: an added
//! person (Ada, two devices), an unadded person (Grace), a standalone added
//! agent (Turing), an added person's agent (Ada's assistant), and an unadded
//! person's agent (Grace's scheduler).
//!
//! When the daemon grows a real carrier binding and a real person→device
//! directory, this module is the seam that gets pointed at them — the handlers
//! above it call `send`/`collect`/`block`/`open`/`focus`/`close` without caring
//! that the carrier and the directory underneath are in-memory.

use std::collections::{BTreeMap, BTreeSet};

use correspondence::{Carrier, Content, Letter, MemCarrier, Missed};
use mechanics::actor::{
    self, consent_sign, device_from_seed, sign_event, ActorOp, ConsentCtx, SignedEvent,
};
use mechanics::egress;
use mechanics::ids::{ActorId, DeviceId, SpaceId, SystemUlidSource};

use crate::client::ClientError;
use crate::model::{ChatMessage, Contact, Conversation, Correspondence};

/// The seed for "me" — this client's device, in the loopback world. Fixed so the
/// demo is the same every launch; a real binding takes the identity's own seed.
const ME_SEED: [u8; 32] = [7u8; 32];

/// How long a letter is worth holding, from when it is sent.
const RETENTION: u64 = 60 * 60 * 24 * 7;

/// A dummy invitation payload. Opaque here — the chat draws an invitation
/// component rather than reading it, and verifying it is a World's job.
const INVITE_COORDINATES: &[u8] = b"lait/demo/invitation/coordinates";

/// One device with everything needed to spend its key: its seed, its identity on
/// the plane, and the events that prove the binding for `egress`.
struct DeviceHandle {
    seed: [u8; 32],
    device: DeviceId,
    actor: ActorId,
    events: Vec<SignedEvent>,
}

impl DeviceHandle {
    fn incept(seed: [u8; 32], nonce: u8, space: &SpaceId) -> Self {
        let (events, actor) = incept(&seed, nonce, space);
        Self {
            seed,
            device: device_from_seed(&seed),
            actor,
            events,
        }
    }

    /// Prove this device's key against a freshly replayed directory, and hand
    /// back the witness. The directory is built here and lent to the witness,
    /// which is why the closure that uses it must not outlive this call.
    fn egress<T>(
        &self,
        space: &SpaceId,
        with: impl FnOnce(&egress::Egress<'_>) -> Result<T, ClientError>,
    ) -> Result<T, ClientError> {
        let plane = actor::replay(space, &self.events);
        let egress = egress::authorize(&plane, &self.actor, &self.device)
            .map_err(|refused| ClientError::internal(format!("prove a key: {refused}")))?;
        with(&egress)
    }
}

/// A correspondent: a person or an agent, added or not, with each of their
/// devices.
struct DemoPerson {
    id: String,
    name: String,
    added: bool,
    is_agent: bool,
    parent_id: Option<String>,
    parent_name: Option<String>,
    devices: Vec<DeviceHandle>,
}

/// The loopback carrier, the people it reaches, and the conversations so far.
pub struct DemoCarrier {
    space: SpaceId,
    me: DeviceHandle,
    people: Vec<DemoPerson>,
    /// Which person owns a device, so a received letter's signer resolves to the
    /// conversation it belongs in. The demo's stand-in for the person→device
    /// directory.
    owner_of: BTreeMap<String, String>,
    carrier: MemCarrier,
    /// One transcript per person id, in send order.
    transcripts: BTreeMap<String, Vec<ChatMessage>>,
    /// Deposit ids already filed, so a re-collection does not double a message.
    filed: BTreeSet<String>,
    /// Conversations that have been opened, so their received messages count as
    /// read. Everything received before the first open is unread.
    opened: BTreeSet<String>,
    open_tabs: Vec<String>,
    active_tab: Option<String>,
}

impl DemoCarrier {
    /// Build the world and seed the whole matrix, so every surface state — an
    /// unread friend, a stranger's request, an agent, an invitation — has data.
    pub fn new(now: u64) -> Self {
        let space = SpaceId::mint(&SystemUlidSource);
        let me = DeviceHandle::incept(ME_SEED, 1, &space);

        // Each correspondent: (id, name, added, is_agent, parent, device seeds).
        // Distinct seeds and inception nonces keep every device a real key.
        let people = vec![
            person(
                "ada",
                "Ada",
                true,
                false,
                None,
                None,
                &[([11u8; 32], 2), ([12u8; 32], 3)],
                &space,
            ),
            person(
                "grace",
                "Grace",
                false,
                false,
                None,
                None,
                &[([20u8; 32], 4)],
                &space,
            ),
            person(
                "turing",
                "Turing",
                true,
                true,
                None,
                None,
                &[([30u8; 32], 5)],
                &space,
            ),
            person(
                "ada-assistant",
                "Ada's assistant",
                true,
                true,
                Some("ada".into()),
                Some("Ada".into()),
                &[([31u8; 32], 6)],
                &space,
            ),
            person(
                "grace-scheduler",
                "Grace's scheduler",
                false,
                true,
                Some("grace".into()),
                Some("Grace".into()),
                &[([40u8; 32], 7)],
                &space,
            ),
        ];

        let mut owner_of = BTreeMap::new();
        for person in &people {
            for device in &person.devices {
                owner_of.insert(device.device.as_str().to_owned(), person.id.clone());
            }
        }

        let mut world = Self {
            space,
            me,
            people,
            owner_of,
            carrier: MemCarrier::new(),
            transcripts: BTreeMap::new(),
            filed: BTreeSet::new(),
            opened: BTreeSet::new(),
            open_tabs: Vec::new(),
            active_tab: None,
        };

        // Every correspondent sends one message and one invitation — the ten
        // combinations of the matrix. Timestamps are spread over days so the
        // chat has date separators, grouped runs, and a long-gap divider to
        // draw; Ada's two arrive from her two devices, to prove they fold into
        // one conversation.
        let day = 86_400;
        let minute = 60;

        // Ada — a real back-and-forth across three days: a grouped run of two
        // (from her two devices), my reply the next day, a follow-up, then an
        // invitation just now.
        world.seed_message("ada", 0, "Saw your issue land — nice.", now - 2 * day);
        world.seed_message(
            "ada",
            1,
            "Ping me when you push the fix.",
            now - 2 * day + minute,
        );
        world.seed_mine("ada", "On it — pushing tonight.", now - day);
        world.seed_mine("ada", "Done. Mind taking a look?", now - day + minute);
        world.seed_message("ada", 0, "Looks great. Shipping it.", now - 3 * minute);
        world.seed_invitation("ada", 1, now - minute);

        world.seed_message("grace", 0, "Hi — saw your project. Can we talk?", now - day);
        world.seed_invitation("grace", 0, now - 2 * minute);
        world.seed_message(
            "turing",
            0,
            "I triaged three issues for you.",
            now - 5 * minute,
        );
        world.seed_invitation("turing", 0, now - 4 * minute);
        world.seed_message(
            "ada-assistant",
            0,
            "Ada asked me to share the plan.",
            now - 6 * minute,
        );
        world.seed_invitation("ada-assistant", 0, now - 5 * minute);
        world.seed_message(
            "grace-scheduler",
            0,
            "Grace's scheduler here — a proposal.",
            now - 7 * minute,
        );
        world.seed_invitation("grace-scheduler", 0, now - 6 * minute);
        world.collect(now);
        world
    }

    /// Send a message to a person: compose as me, seal to their first device,
    /// deposit under my witness, and record it in their transcript.
    pub fn send(&mut self, person_id: &str, body: &str, now: u64) -> Result<(), ClientError> {
        let device = {
            let person = self.person(person_id)?;
            person
                .devices
                .first()
                .map(|handle| handle.device.clone())
                .ok_or_else(|| ClientError::internal("that person has no device to reach"))?
        };
        let letter = Letter::compose(
            &ME_SEED,
            Content::Message {
                body: body.to_owned(),
            },
            now,
        );
        let sealed = letter
            .seal_to(&device, now + RETENTION)
            .map_err(|refused| ClientError::internal(format!("seal a letter: {refused}")))?;
        let space = self.space.clone();
        self.me.egress(&space, |egress| {
            self.carrier
                .deposit(egress, &sealed, now)
                .map(|_id| ())
                .map_err(|refused| {
                    ClientError::refused(format!("the carrier refused it: {refused}"))
                })
        })?;

        self.transcripts
            .entry(person_id.to_owned())
            .or_default()
            .push(ChatMessage {
                id: None,
                mine: true,
                kind: "message".into(),
                body: Some(body.to_owned()),
                sent_at: now,
                from_device: self.me.device.as_str().to_owned(),
                provenance_agrees: true,
            });
        Ok(())
    }

    /// Collect anything waiting for me, open and verify it, and file each into
    /// the conversation of whoever sent it.
    pub fn collect(&mut self, now: u64) {
        let answer = self.carrier.collect(&self.me.device, now);
        let Missed::Held(waiting) = answer else {
            return;
        };
        let mut acknowledged = Vec::new();
        for item in &waiting {
            if self.filed.contains(&item.id) {
                continue;
            }
            let Some(letter) = Letter::open(&ME_SEED, &self.me.device, &item.sealed) else {
                continue;
            };
            let from = letter.from.as_str().to_owned();
            let Some(person_id) = self.owner_of.get(&from).cloned() else {
                continue;
            };
            let (kind, body) = match &letter.content {
                Content::Message { body } => ("message", Some(body.clone())),
                Content::Invitation { .. } => ("invitation", None),
            };
            self.transcripts
                .entry(person_id)
                .or_default()
                .push(ChatMessage {
                    id: None,
                    mine: false,
                    kind: kind.to_owned(),
                    body,
                    sent_at: letter.sent_at,
                    from_device: from,
                    provenance_agrees: item.deposited_by == letter.from,
                });
            self.filed.insert(item.id.clone());
            acknowledged.push(item.id.clone());
        }
        for messages in self.transcripts.values_mut() {
            messages.sort_by_key(|message| message.sent_at);
        }
        let _ = self
            .carrier
            .acknowledge(&self.me.device, &acknowledged, now);
    }

    /// Block a person: refuse every one of their devices at the carrier, on my
    /// own authority.
    pub fn block(&mut self, person_id: &str, now: u64) -> Result<(), ClientError> {
        let devices: Vec<DeviceId> = {
            let person = self.person(person_id)?;
            person.devices.iter().map(|d| d.device.clone()).collect()
        };
        let space = self.space.clone();
        for sender in &devices {
            self.me.egress(&space, |egress| {
                self.carrier
                    .block(egress, sender, true, now)
                    .map_err(|refused| {
                        ClientError::refused(format!("the carrier refused it: {refused}"))
                    })
            })?;
        }
        Ok(())
    }

    /// Accept an unknown correspondent into the address book — they become a
    /// known contact, and their conversation moves out of Incoming. An agent's
    /// added-ness follows its parent, so accepting the parent lifts them too.
    pub fn accept(&mut self, person_id: &str) {
        let parent = self
            .people
            .iter()
            .find(|person| person.id == person_id)
            .and_then(|person| person.parent_id.clone());
        for person in &mut self.people {
            if person.id == person_id
                || person.parent_id.as_deref() == Some(person_id)
                || (parent.is_some() && person.parent_id == parent)
                || Some(person.id.as_str()) == parent.as_deref()
            {
                person.added = true;
            }
        }
    }

    /// Open a conversation as a tab, focus it, and mark it read.
    pub fn open(&mut self, person_id: &str) {
        if self.person(person_id).is_err() {
            return;
        }
        if !self.open_tabs.iter().any(|id| id == person_id) {
            self.open_tabs.push(person_id.to_owned());
        }
        self.active_tab = Some(person_id.to_owned());
        self.opened.insert(person_id.to_owned());
    }

    /// Focus an already-open tab, and mark it read. A no-op if it is not open.
    pub fn focus(&mut self, person_id: &str) {
        if self.open_tabs.iter().any(|id| id == person_id) {
            self.active_tab = Some(person_id.to_owned());
            self.opened.insert(person_id.to_owned());
        }
    }

    /// Close a tab, and move focus to a neighbour if it was the active one.
    pub fn close(&mut self, person_id: &str) {
        let Some(index) = self.open_tabs.iter().position(|id| id == person_id) else {
            return;
        };
        self.open_tabs.remove(index);
        if self.active_tab.as_deref() == Some(person_id) {
            self.active_tab = self
                .open_tabs
                .get(index.min(self.open_tabs.len().saturating_sub(1)))
                .cloned();
        }
    }

    /// The read model the App holds: contacts, conversations, and open tabs.
    pub fn snapshot(&self) -> Correspondence {
        let contacts = self
            .people
            .iter()
            .map(|person| Contact {
                id: person.id.clone(),
                name: person.name.clone(),
                devices: person
                    .devices
                    .iter()
                    .map(|d| d.device.as_str().to_owned())
                    .collect(),
                added: person.added,
                is_agent: person.is_agent,
                parent_id: person.parent_id.clone(),
                parent_name: person.parent_name.clone(),
                unread: self.unread(&person.id),
            })
            .collect();
        let conversations = self
            .people
            .iter()
            .map(|person| Conversation {
                peer_id: person.id.clone(),
                peer_name: person.name.clone(),
                messages: self
                    .transcripts
                    .get(&person.id)
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect();
        Correspondence {
            my_device: Some(self.me.device.as_str().to_owned()),
            my_reach: None,
            me: None,
            contacts,
            conversations,
            open_tabs: self.open_tabs.clone(),
            active_tab: self.active_tab.clone(),
        }
    }

    /// How many received messages in a conversation have not been seen. Zero once
    /// the conversation has been opened.
    fn unread(&self, person_id: &str) -> u32 {
        if self.opened.contains(person_id) {
            return 0;
        }
        let count = self
            .transcripts
            .get(person_id)
            .map(|messages| messages.iter().filter(|message| !message.mine).count())
            .unwrap_or(0);
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    fn person(&self, person_id: &str) -> Result<&DemoPerson, ClientError> {
        self.people
            .iter()
            .find(|person| person.id == person_id)
            .ok_or_else(|| ClientError::invalid(format!("no such person: {person_id}")))
    }

    /// Record an outgoing message straight into a transcript — no carrier round
    /// trip, because a message I sent is mine to remember whether or not it has
    /// been fetched back. For seeding a lifelike back-and-forth.
    fn seed_mine(&mut self, person_id: &str, body: &str, now: u64) {
        self.transcripts
            .entry(person_id.to_owned())
            .or_default()
            .push(ChatMessage {
                id: None,
                mine: true,
                kind: "message".into(),
                body: Some(body.to_owned()),
                sent_at: now,
                from_device: self.me.device.as_str().to_owned(),
                provenance_agrees: true,
            });
    }

    /// Deposit a message from one of a person's devices, sealed to me, under that
    /// device's own witness.
    fn seed_message(&mut self, person_id: &str, device_index: usize, body: &str, now: u64) {
        self.seed(
            person_id,
            device_index,
            Content::Message {
                body: body.to_owned(),
            },
            now,
        );
    }

    /// Deposit an invitation from one of a person's devices.
    fn seed_invitation(&mut self, person_id: &str, device_index: usize, now: u64) {
        self.seed(
            person_id,
            device_index,
            Content::Invitation {
                coordinates: INVITE_COORDINATES.to_vec(),
            },
            now,
        );
    }

    fn seed(&mut self, person_id: &str, device_index: usize, content: Content, now: u64) {
        let space = self.space.clone();
        let Some(person) = self.people.iter().find(|person| person.id == person_id) else {
            return;
        };
        let Some(sender) = person.devices.get(device_index) else {
            return;
        };
        let letter = Letter::compose(&sender.seed, content, now);
        let Ok(sealed) = letter.seal_to(&self.me.device, now + RETENTION) else {
            return;
        };
        let _ = sender.egress(&space, |egress| {
            self.carrier
                .deposit(egress, &sealed, now)
                .map_err(|refused| ClientError::internal(format!("seed deposit: {refused}")))
        });
    }
}

/// Build a correspondent and incept each of its devices.
#[allow(clippy::too_many_arguments)]
fn person(
    id: &str,
    name: &str,
    added: bool,
    is_agent: bool,
    parent_id: Option<String>,
    parent_name: Option<String>,
    devices: &[([u8; 32], u8)],
    space: &SpaceId,
) -> DemoPerson {
    DemoPerson {
        id: id.to_owned(),
        name: name.to_owned(),
        added,
        is_agent,
        parent_id,
        parent_name,
        devices: devices
            .iter()
            .map(|(seed, nonce)| DeviceHandle::incept(*seed, *nonce, space))
            .collect(),
    }
}

/// Incept a single-device actor, so there is a real device→actor binding for
/// `egress` to resolve. The same shape the correspondence crate's hop tests use.
fn incept(seed: &[u8; 32], nonce: u8, space: &SpaceId) -> (Vec<SignedEvent>, ActorId) {
    let devices = vec![device_from_seed(seed)];
    let binding = consent_sign(
        seed,
        space.as_str(),
        [nonce; 16],
        &ConsentCtx::Incept {
            incept_nonce: &[nonce; 16],
            devices: &devices,
            recovery_commit: &None,
        },
    );
    let event = sign_event(
        seed,
        &ActorOp::Incept {
            space: space.as_str().to_owned(),
            nonce: [nonce; 16],
            devices: vec![binding],
            recovery_commit: None,
        },
        vec![],
        space,
    );
    let id = ActorId::from_incept_hash(&event.hash());
    (vec![event], id)
}

/// A correspondence backend that carries **real mail over a hosted Post** — not
/// a fixture. It stands the [`ReachPlane`](crate::client::reach::ReachPlane) up
/// against a live `lait-post` and runs the whole pipe: seal, deposit over HTTP,
/// fetch back, open, file.
///
/// It reaches anyone whose card this identity has learned, and itself. What a
/// directory (AUTH-12) later adds is not reach — it is a short spoken address
/// in place of a card carried by hand.
pub struct PostBackend {
    reach: crate::client::reach::PostReach,
    /// What I have composed and sent to myself — mine to remember whether or not
    /// it has been fetched back, the same reason a sent message is kept locally.
    sent: Vec<(u64, String)>,
    /// What I have sent to each correspondent, keyed by their address. The Post
    /// drops a letter once its recipient acknowledges, so the sender's copy is
    /// the only durable one there will ever be.
    outbound: BTreeMap<String, Vec<(u64, String)>>,
    /// Where this plane's durable state lives. Held by the backend rather than
    /// beside it: an owner that can be `None` while the plane exists is two
    /// facts that agree only by construction, and they stop agreeing the first
    /// time one of them is set somewhere else.
    home: Option<std::path::PathBuf>,
}

impl PostBackend {
    /// Stand the plane up from this identity's device seeds, pointed at `base`.
    pub fn found(
        seeds: Vec<[u8; 32]>,
        base: String,
        now: u64,
    ) -> Result<Self, crate::client::reach::ReachError> {
        Self::restore(seeds, None, base, now)
    }

    /// Stand the plane up, reusing durable state when there is any.
    pub fn restore(
        seeds: Vec<[u8; 32]>,
        state: Option<addressbook::ReachState>,
        base: String,
        now: u64,
    ) -> Result<Self, crate::client::reach::ReachError> {
        Self::restore_at(seeds, state, base, now, None)
    }

    /// As [`Self::restore`], remembering where the state came from so the plane
    /// can keep itself.
    pub fn restore_at(
        seeds: Vec<[u8; 32]>,
        state: Option<addressbook::ReachState>,
        base: String,
        now: u64,
        home: Option<std::path::PathBuf>,
    ) -> Result<Self, crate::client::reach::ReachError> {
        Ok(Self {
            reach: crate::client::reach::PostReach::restore(seeds, state, base, now)?,
            sent: Vec::new(),
            outbound: BTreeMap::new(),
            home,
        })
    }

    /// This identity's own address, the id its self-conversation is keyed on.
    fn self_id(&self) -> String {
        self.reach.profile().as_str().to_owned()
    }

    /// Send a message to yourself, or to a correspondent this identity has
    /// learned. A profile that has not been learned is *not reachable* — which
    /// is a different fact from the message failing, and is said as such.
    pub fn send(&mut self, to: &str, body: &str, now: u64) -> Result<(), ClientError> {
        if to == self.self_id() {
            self.reach
                .send_self(body, now)
                .map_err(|error| ClientError::internal(format!("send over the Post: {error:?}")))?;
            self.sent.push((now, body.to_owned()));
            return Ok(());
        }
        let profile = mechanics::kinship::ProfileId::parse(to)
            .ok_or_else(|| ClientError::invalid("that is not an address"))?;
        match self.reach.send_to(&profile, body, now) {
            Ok(_) => {
                self.outbound
                    .entry(to.to_owned())
                    .or_default()
                    .push((now, body.to_owned()));
                Ok(())
            }
            Err(crate::client::reach::ReachError::NotReachable) => Err(ClientError::refused(
                "we do not know how to reach them yet — add their address first",
            )),
            // Only a carrier fault is worth trying again. Sealing, kinship and
            // egress are decided locally and deterministically: offering a
            // retry on those is offering an act that cannot succeed.
            Err(error @ crate::client::reach::ReachError::Carrier(_)) => Err(
                ClientError::unreachable(format!("send over the Post: {error}")),
            ),
            Err(error) => Err(ClientError::refused(format!("send over the Post: {error}"))),
        }
    }

    /// Publish this identity's reach as an artifact a person can hand over.
    pub fn announce(&mut self) -> Result<String, ClientError> {
        let reader = self.reach.standing();
        let announcement = self
            .reach
            .announce(mechanics::kinship::Audience::Public, &reader)
            .map_err(|error| ClientError::internal(format!("announce: {error:?}")))?;
        let card = announcement
            .render()
            .map_err(|error| ClientError::internal(format!("render an announcement: {error}")))?;
        self.keep()?;
        Ok(card)
    }

    /// Take in a correspondent's announcement. Returns the profile learned.
    pub fn learn(&mut self, pasted: &str) -> Result<String, ClientError> {
        let announcement = addressbook::Announcement::parse(pasted)
            .map_err(|_| ClientError::invalid("that is not an announcement"))?;
        let reader = self.reach.standing();
        let profile = self
            .reach
            .learn(announcement, &reader)
            .map_err(|error| ClientError::refused(format!("that card was refused: {error}")))?;
        self.keep()?;
        Ok(profile.as_str().to_owned())
    }

    /// The coordinates one arriving invitation carries, by deposit id.
    #[must_use]
    pub fn invitation(&self, message: &str) -> Option<Vec<u8>> {
        self.reach.invitation(message)
    }

    /// Carry an invitation to a learned correspondent.
    pub fn send_invitation(
        &mut self,
        to: &str,
        coordinates: Vec<u8>,
        now: u64,
    ) -> Result<(), ClientError> {
        let profile = mechanics::kinship::ProfileId::parse(to)
            .ok_or_else(|| ClientError::invalid("that is not an address"))?;
        match self.reach.send_invitation(&profile, coordinates, now) {
            Ok(_) => Ok(()),
            Err(crate::client::reach::ReachError::NotReachable) => Err(ClientError::refused(
                "we do not know how to reach them yet — add their address first",
            )),
            Err(error @ crate::client::reach::ReachError::Carrier(_)) => Err(
                ClientError::unreachable(format!("send an invitation: {error}")),
            ),
            Err(error) => Err(ClientError::refused(format!("send an invitation: {error}"))),
        }
    }

    /// What has to survive a restart.
    #[must_use]
    pub fn state(&self) -> addressbook::ReachState {
        self.reach.state()
    }

    /// Write this plane's state where it was read from.
    ///
    /// Called by the acts that change it rather than by the caller afterwards:
    /// a save the caller forgets is a correspondent that vanishes on restart,
    /// and a save the caller does *after* reporting success is a person told a
    /// thing landed that did not.
    fn keep(&self) -> Result<(), ClientError> {
        let Some(home) = self.home.as_ref() else {
            return Ok(());
        };
        addressbook::ReachStore::at(home)
            .save(&self.state())
            .map_err(|error| ClientError::internal(format!("keep the reach plane: {error}")))
    }

    /// Open or focus a conversation this backend has.
    ///
    /// Satisfied rather than refused for anyone [`Self::snapshot`] already
    /// reports: a control that refuses what the view says is already true is its
    /// own kind of dishonesty.
    pub fn arrange(&self, person: &str) -> Result<(), ClientError> {
        if person == self.self_id()
            || self.outbound.contains_key(person)
            || self
                .reach
                .correspondents()
                .iter()
                .any(|held| held == person)
        {
            Ok(())
        } else {
            Err(ClientError::refused(
                "only your own conversation exists over the Post yet",
            ))
        }
    }

    /// Fetch and file anything waiting over the Post.
    ///
    /// A Post that could not be reached is reported, never absorbed: the caller
    /// has to be able to tell "you have no mail" from "we could not look".
    pub fn collect(&mut self, now: u64) -> Result<(), ClientError> {
        match self.reach.collect(now).unasked {
            None => Ok(()),
            Some(why) => Err(ClientError::unreachable(format!(
                "the Post could not be asked: {why}"
            ))),
        }
    }

    /// Project the self-conversation as the model draws it.
    pub fn snapshot(&self) -> Correspondence {
        let me = self.self_id();
        let my_device = self.reach.my_device().as_str().to_owned();
        let name = "You (over the Post)".to_owned();

        let mut messages: Vec<ChatMessage> = self
            .sent
            .iter()
            .map(|(sent_at, body)| ChatMessage {
                id: None,
                mine: true,
                kind: "message".into(),
                body: Some(body.clone()),
                sent_at: *sent_at,
                from_device: my_device.clone(),
                provenance_agrees: true,
            })
            .collect();
        // A letter proves its sending *device*; a person is a device set. Route
        // each into the conversation of whoever avows the signer, so mail from a
        // correspondent does not pile into this identity's own transcript.
        let mut received: BTreeMap<String, Vec<ChatMessage>> = BTreeMap::new();
        for opened in self.reach.opened() {
            let (kind, body) = match &opened.content {
                correspondence::Content::Message { body } => ("message", Some(body.clone())),
                // Drawn as its own widget rather than a text bubble, and it
                // carries no body: an invitation is acted on, not read.
                correspondence::Content::Invitation { .. } => ("invitation", None),
            };
            let from = opened.from.clone();
            let message = ChatMessage {
                id: Some(opened.id.clone()),
                mine: false,
                kind: kind.into(),
                body,
                sent_at: opened.sent_at,
                from_device: from.as_str().to_owned(),
                provenance_agrees: opened.provenance_agrees,
            };
            match self.reach.profile_of_device(&from) {
                Some(profile) if profile.as_str() != me => {
                    received
                        .entry(profile.as_str().to_owned())
                        .or_default()
                        .push(message);
                }
                // Mine, or from a device no held profile avows — a stranger
                // writing first. Both belong in the transcript that always
                // exists rather than being dropped.
                _ => messages.push(message),
            }
        }
        messages.sort_by_key(|message| message.sent_at);

        let mut contacts = vec![Contact {
            id: me.clone(),
            name: name.clone(),
            devices: vec![my_device.clone()],
            added: true,
            is_agent: false,
            parent_id: None,
            parent_name: None,
            unread: 0,
        }];
        let mut conversations = vec![Conversation {
            peer_id: me.clone(),
            peer_name: name,
            messages,
        }];
        let mut open_tabs = vec![me.clone()];

        // Everyone this identity has learned. Their name is their address until
        // the book can hold a profile handle — a truthful placeholder rather
        // than an invented one.
        let mut everyone: BTreeSet<String> = self.reach.correspondents().into_iter().collect();
        everyone.extend(self.outbound.keys().cloned());
        everyone.extend(received.keys().cloned());
        for address in everyone {
            let sent = self.outbound.get(&address).cloned().unwrap_or_default();
            let devices = self.reach.resolve_str(&address).unwrap_or_default();
            contacts.push(Contact {
                id: address.clone(),
                name: address.clone(),
                devices,
                added: true,
                is_agent: false,
                parent_id: None,
                parent_name: None,
                unread: 0,
            });
            let mut transcript: Vec<ChatMessage> = sent
                .iter()
                .map(|(sent_at, body)| ChatMessage {
                    id: None,
                    mine: true,
                    kind: "message".into(),
                    body: Some(body.clone()),
                    sent_at: *sent_at,
                    from_device: my_device.clone(),
                    provenance_agrees: true,
                })
                .collect();
            transcript.extend(received.remove(&address).unwrap_or_default());
            transcript.sort_by_key(|message| message.sent_at);
            conversations.push(Conversation {
                peer_id: address.clone(),
                peer_name: address.clone(),
                messages: transcript,
            });
            open_tabs.push(address.clone());
        }

        Correspondence {
            my_device: Some(my_device),
            my_reach: self.reach.card(),
            me: Some(me.clone()),
            contacts,
            conversations,
            open_tabs,
            active_tab: Some(me),
        }
    }
}

/// The correspondence backend the runtime holds: either the in-process fixture
/// or a real hosted-Post plane. The surface calls the same
/// `send`/`collect`/`block`/… on whichever is underneath and never learns which.
pub enum Correspondent {
    /// The opt-in front-end validation fixture, `LAIT_CORRESPONDENCE_DEMO`.
    Demo(DemoCarrier),
    /// A real plane carrying mail over a hosted Post, `LAIT_POST_URL`.
    Post(PostBackend),
}

impl Correspondent {
    pub fn snapshot(&self) -> Correspondence {
        match self {
            Self::Demo(demo) => demo.snapshot(),
            Self::Post(post) => post.snapshot(),
        }
    }

    pub fn send(&mut self, to: &str, body: &str, now: u64) -> Result<(), ClientError> {
        match self {
            Self::Demo(demo) => demo.send(to, body, now),
            Self::Post(post) => post.send(to, body, now),
        }
    }

    /// The coordinates one arriving invitation carries. The fixture's are a
    /// stand-in payload and verify against nothing, so it offers none.
    #[must_use]
    pub fn invitation(&self, message: &str) -> Option<Vec<u8>> {
        match self {
            Self::Demo(_) => None,
            Self::Post(post) => post.invitation(message),
        }
    }

    /// Carry an invitation to a correspondent.
    pub fn send_invitation(
        &mut self,
        to: &str,
        coordinates: Vec<u8>,
        now: u64,
    ) -> Result<(), ClientError> {
        match self {
            Self::Demo(_) => Err(ClientError::refused(
                "the demo fixture cannot carry an invitation",
            )),
            Self::Post(post) => post.send_invitation(to, coordinates, now),
        }
    }

    /// Publish this identity's reach. The fixture has no real address to give.
    pub fn announce(&mut self) -> Result<String, ClientError> {
        match self {
            Self::Demo(_) => Err(ClientError::refused(
                "the demo fixture has no address to hand out",
            )),
            Self::Post(post) => post.announce(),
        }
    }

    /// Take in a correspondent's announcement.
    pub fn learn(&mut self, announcement: &str) -> Result<String, ClientError> {
        match self {
            Self::Demo(_) => Err(ClientError::refused(
                "the demo fixture cannot learn a correspondent",
            )),
            Self::Post(post) => post.learn(announcement),
        }
    }

    pub fn collect(&mut self, now: u64) -> Result<(), ClientError> {
        match self {
            // The loopback carrier is this process; it cannot be out of reach.
            Self::Demo(demo) => {
                demo.collect(now);
                Ok(())
            }
            Self::Post(post) => post.collect(now),
        }
    }

    pub fn block(&mut self, person: &str, now: u64) -> Result<(), ClientError> {
        match self {
            Self::Demo(demo) => demo.block(person, now),
            // No stranger can reach you over the Post yet, so there is nobody to
            // block — say so rather than pretend it landed.
            Self::Post(_) => Err(ClientError::refused(
                "blocking is not available over the Post yet",
            )),
        }
    }

    /// Take a stranger into the contact list.
    ///
    /// The Post arm refuses: a correspondent arrives by learning their card,
    /// which is what puts them in the list. There is no separate "added" flag
    /// for this backend to flip, and an arm that returned `Ok` would report a
    /// change the next snapshot contradicts.
    pub fn accept(&mut self, person: &str) -> Result<(), ClientError> {
        match self {
            Self::Demo(demo) => {
                demo.accept(person);
                Ok(())
            }
            Self::Post(_) => Err(ClientError::refused(
                "adding a contact is not available over the Post yet",
            )),
        }
    }

    pub fn open(&mut self, person: &str) -> Result<(), ClientError> {
        match self {
            Self::Demo(demo) => {
                demo.open(person);
                Ok(())
            }
            Self::Post(post) => post.arrange(person),
        }
    }

    pub fn focus(&mut self, person: &str) -> Result<(), ClientError> {
        match self {
            Self::Demo(demo) => {
                demo.focus(person);
                Ok(())
            }
            Self::Post(post) => post.arrange(person),
        }
    }

    /// Closing is the one of the three the Post arm cannot honour: its tab set is
    /// synthesised on every pump, so a close would be undone by the next frame.
    pub fn close(&mut self, person: &str) -> Result<(), ClientError> {
        match self {
            Self::Demo(demo) => {
                demo.close(person);
                Ok(())
            }
            Self::Post(_) => Err(ClientError::refused(
                "a conversation cannot be closed over the Post yet",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn contact<'a>(world: &'a Correspondence, id: &str) -> &'a Contact {
        world
            .contacts
            .iter()
            .find(|contact| contact.id == id)
            .expect("contact")
    }

    fn conversation<'a>(world: &'a Correspondence, id: &str) -> &'a Conversation {
        world
            .conversations
            .iter()
            .find(|conversation| conversation.peer_id == id)
            .expect("conversation")
    }

    /// The mock seeds the whole matrix: every correspondent kind, each with a
    /// message and an invitation.
    #[test]
    fn the_mock_covers_the_whole_matrix() {
        let world = DemoCarrier::new(NOW).snapshot();

        // Added person, unadded person, standalone agent, an added person's
        // agent, and an unadded person's agent.
        let expected = [
            ("ada", true, false, None),
            ("grace", false, false, None),
            ("turing", true, true, None),
            ("ada-assistant", true, true, Some("ada")),
            ("grace-scheduler", false, true, Some("grace")),
        ];
        for (id, added, is_agent, parent) in expected {
            let c = contact(&world, id);
            assert_eq!(c.added, added, "{id} added");
            assert_eq!(c.is_agent, is_agent, "{id} agent");
            assert_eq!(c.parent_id.as_deref(), parent, "{id} parent");

            // Each conversation has both a message and an invitation received.
            let messages = &conversation(&world, id).messages;
            assert!(
                messages.iter().any(|m| !m.mine && m.kind == "message"),
                "{id} has a received message"
            );
            assert!(
                messages.iter().any(|m| !m.mine && m.kind == "invitation"),
                "{id} has a received invitation"
            );
        }
    }

    /// Ada's two devices fold into one conversation.
    #[test]
    fn ada_folds_two_devices_into_one_conversation() {
        let world = DemoCarrier::new(NOW).snapshot();
        assert_eq!(contact(&world, "ada").devices.len(), 2);
        let devices: BTreeSet<&str> = conversation(&world, "ada")
            .messages
            .iter()
            .filter(|m| !m.mine)
            .map(|m| m.from_device.as_str())
            .collect();
        assert_eq!(devices.len(), 2, "one item from each of Ada's devices");
    }

    /// A conversation is unread until it is opened, then it clears.
    #[test]
    fn unread_clears_on_open() {
        let mut world = DemoCarrier::new(NOW);
        assert!(
            contact(&world.snapshot(), "ada").unread > 0,
            "unread at first"
        );
        world.open("ada");
        assert_eq!(
            contact(&world.snapshot(), "ada").unread,
            0,
            "read once opened"
        );
    }

    /// Sending records the message on my side of the conversation, in order.
    #[test]
    fn sending_appends_to_the_conversation_as_mine() {
        let mut world = DemoCarrier::new(NOW);
        let before = conversation(&world.snapshot(), "ada")
            .messages
            .iter()
            .filter(|m| m.mine)
            .count();
        world.send("ada", "on it", NOW + 100).expect("send");
        let world = world.snapshot();
        let mine: Vec<&ChatMessage> = conversation(&world, "ada")
            .messages
            .iter()
            .filter(|m| m.mine)
            .collect();
        assert_eq!(mine.len(), before + 1, "one more of mine");
        assert_eq!(
            mine.last().and_then(|m| m.body.as_deref()),
            Some("on it"),
            "the newest is what I just sent"
        );
    }

    /// A blocked person's later message never lands — accept-shaped, so the
    /// send still looks accepted and a collect files nothing new from them.
    #[test]
    fn blocking_a_person_stops_their_messages_landing() {
        let mut world = DemoCarrier::new(NOW);
        let before = conversation(&world.snapshot(), "grace").messages.len();
        world.block("grace", NOW + 20).expect("block");
        world.seed_message("grace", 0, "are you there?", NOW + 21);
        world.collect(NOW + 22);
        let after = conversation(&world.snapshot(), "grace").messages.len();
        assert_eq!(after, before, "a blocked person's message never lands");
    }

    /// The real backend carries a self-message over the **deployed** Post and the
    /// snapshot the model draws shows both sides — the sent copy and the one
    /// fetched back. Gated on `POST_SMOKE_URL`; a no-op without it.
    #[test]
    fn post_backend_round_trips_a_self_message_over_the_deployed_post() {
        let Ok(base) = std::env::var("POST_SMOKE_URL") else {
            return;
        };
        let base = base.trim_end_matches('/').to_owned();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        // Per-run seeds, so the persistent Post never crosses runs.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
            .to_le_bytes();
        let mk = |tag: u8| {
            let mut seed = [0u8; 32];
            seed[..16].copy_from_slice(&stamp);
            seed[16] = tag;
            seed
        };

        let mut backend = PostBackend::found(vec![mk(1), mk(2)], base, now).expect("found");
        let me = backend.self_id();
        backend
            .send(&me, "reached myself over the Post", now)
            .expect("send self");
        backend.collect(now);

        let snapshot = backend.snapshot();
        let convo = conversation(&snapshot, &me);
        assert!(
            convo
                .messages
                .iter()
                .any(|m| m.mine && m.body.as_deref() == Some("reached myself over the Post")),
            "the sent copy is in the transcript"
        );
        assert!(
            convo
                .messages
                .iter()
                .any(|m| !m.mine && m.body.as_deref() == Some("reached myself over the Post")),
            "the copy fetched back over the Post is in the transcript"
        );
    }

    /// A backend that cannot carry an operation says so, rather than returning
    /// success and leaving the next snapshot to contradict it.
    ///
    /// This is the property `block` always had and the other four did not: they
    /// matched only the fixture arm, so over a real Post they accepted the
    /// click, changed nothing, and reported it done. Needs no network — the
    /// plane is founded from seeds and the refusals are decided before anything
    /// would be asked of the carrier.
    #[test]
    fn the_post_arm_refuses_what_it_cannot_carry_rather_than_going_quiet() {
        let mut post = Correspondent::Post(
            PostBackend::found(
                vec![[7u8; 32], [9u8; 32]],
                "https://post.invalid".into(),
                NOW,
            )
            .expect("the plane is founded from seeds alone"),
        );

        // Its own conversation is the one thing it has, and the snapshot already
        // reports that tab open and active — so asking for it is satisfied
        // rather than refused. A control that refuses what the view says is
        // already true is its own kind of dishonesty.
        let me = post.snapshot().contacts[0].id.clone();
        post.open(&me)
            .expect("its own conversation is already open");
        post.focus(&me).expect("and already active");

        for (what, refused) in [
            ("accept", post.accept("prf_stranger")),
            ("open", post.open("prf_stranger")),
            ("focus", post.focus("prf_stranger")),
            ("close", post.close(&me)),
        ] {
            let error = refused.expect_err(&format!("{what} is refused over the Post"));
            assert!(
                !error.retryable,
                "{what} is refused, which trying again does not change"
            );
            assert!(!error.message.is_empty(), "{what} says why it was refused");
        }

        // The fixture still carries all four, so the refusal is the backend's
        // own answer and not a rule the caller imposed on both.
        let mut demo = Correspondent::Demo(DemoCarrier::new(NOW));
        let someone = demo.snapshot().contacts[0].id.clone();
        demo.accept(&someone).expect("the fixture accepts");
        demo.open(&someone).expect("the fixture opens");
        demo.focus(&someone).expect("the fixture focuses");
        demo.close(&someone).expect("the fixture closes");
    }

    /// The address a person hands out survives the client restarting.
    ///
    /// The chain, not the parts: identity home → seeds → genesis → `ProfileId`.
    /// Two independent foundings against one home must agree, or mail deposited
    /// to yesterday's address sits at the Post until it expires.
    #[test]
    fn one_identity_home_founds_one_address_however_often_the_client_starts() {
        let home = std::env::temp_dir().join(format!("astro-addr-{}", std::process::id()));
        std::fs::create_dir_all(&home).expect("home");
        lait::config::load_or_create_identity(&home).expect("identity");

        let found = || {
            let seeds = lait::config::load_or_create_kinship_seeds(&home).expect("seeds");
            PostBackend::found(seeds, "https://post.invalid".into(), NOW)
                .expect("found")
                .self_id()
        };
        assert_eq!(found(), found(), "the same identity is the same address");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// A Post that could not be reached is a failure, not an empty mailbox.
    ///
    /// These are the two facts a person acts on differently — "nobody wrote to
    /// you" and "we could not find out" — and collapsing them is the
    /// false-disconnection defect one layer down. Driven against a closed port,
    /// the same way `correspondence`'s own carrier test proves it.
    #[test]
    fn a_post_that_cannot_be_asked_is_never_reported_as_no_mail() {
        let mut post = Correspondent::Post(
            // Port 1 on loopback: refused immediately, so this is a transport
            // fault rather than a slow one, and the test owes nothing to a clock.
            PostBackend::found(vec![[3u8; 32], [5u8; 32]], "http://127.0.0.1:1".into(), NOW)
                .expect("the plane is founded from seeds alone"),
        );

        let error = post
            .collect(NOW)
            .expect_err("an unreachable Post is reported, not absorbed");
        assert!(
            error.retryable,
            "the Post being out of reach is worth trying again, unlike a refusal"
        );

        // And the mailbox stayed empty rather than being reported as collected.
        let snapshot = post.snapshot();
        assert!(
            snapshot.conversations.iter().all(|c| c.messages.is_empty()),
            "nothing was filed by a pass that never reached the carrier"
        );
    }
}
