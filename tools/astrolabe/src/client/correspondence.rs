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
/// v1 reaches the one correspondent that needs no directory: **yourself**. A
/// person seals to their own profile's devices and reads it back — proof the
/// carriage works end to end against real infrastructure. Reaching another
/// person is the identical `send`/`collect` once a directory (AUTH-12) lets a
/// stranger's profile be learned; nothing here changes when it does.
pub struct PostBackend {
    reach: crate::client::reach::PostReach,
    /// What I have composed and sent — mine to remember whether or not it has
    /// been fetched back, the same reason a sent message is kept locally.
    sent: Vec<(u64, String)>,
}

impl PostBackend {
    /// Stand the plane up from this identity's device seeds, pointed at `base`.
    pub fn found(
        seeds: Vec<[u8; 32]>,
        base: String,
        now: u64,
    ) -> Result<Self, crate::client::reach::ReachError> {
        Ok(Self {
            reach: crate::client::reach::PostReach::found(seeds, base, now)?,
            sent: Vec::new(),
        })
    }

    /// This identity's own address, the id its self-conversation is keyed on.
    fn self_id(&self) -> String {
        self.reach.profile().as_str().to_owned()
    }

    /// Send a message. Only this identity's own address is reachable until a
    /// directory carries a stranger's profile — anything else refuses honestly.
    pub fn send(&mut self, to: &str, body: &str, now: u64) -> Result<(), ClientError> {
        if to != self.self_id() {
            return Err(ClientError::refused(
                "only your own address is reachable over the Post yet",
            ));
        }
        self.reach
            .send_self(body, now)
            .map_err(|error| ClientError::internal(format!("send over the Post: {error:?}")))?;
        self.sent.push((now, body.to_owned()));
        Ok(())
    }

    /// Fetch and file anything waiting over the Post.
    pub fn collect(&mut self, now: u64) {
        self.reach.collect(now);
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
                mine: true,
                kind: "message".into(),
                body: Some(body.clone()),
                sent_at: *sent_at,
                from_device: my_device.clone(),
                provenance_agrees: true,
            })
            .collect();
        for (from, body, sent_at, provenance_agrees) in self.reach.inbox() {
            messages.push(ChatMessage {
                mine: false,
                kind: "message".into(),
                body: Some(body),
                sent_at,
                from_device: from.as_str().to_owned(),
                provenance_agrees,
            });
        }
        messages.sort_by_key(|message| message.sent_at);

        let contact = Contact {
            id: me.clone(),
            name: name.clone(),
            devices: vec![my_device.clone()],
            added: true,
            is_agent: false,
            parent_id: None,
            parent_name: None,
            unread: 0,
        };
        Correspondence {
            my_device: Some(my_device),
            contacts: vec![contact],
            conversations: vec![Conversation {
                peer_id: me.clone(),
                peer_name: name,
                messages,
            }],
            open_tabs: vec![me.clone()],
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

    pub fn collect(&mut self, now: u64) {
        match self {
            Self::Demo(demo) => demo.collect(now),
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

    pub fn accept(&mut self, person: &str) {
        if let Self::Demo(demo) = self {
            demo.accept(person);
        }
    }

    pub fn open(&mut self, person: &str) {
        if let Self::Demo(demo) = self {
            demo.open(person);
        }
    }

    pub fn focus(&mut self, person: &str) {
        if let Self::Demo(demo) = self {
            demo.focus(person);
        }
    }

    pub fn close(&mut self, person: &str) {
        if let Self::Demo(demo) = self {
            demo.close(person);
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
}
