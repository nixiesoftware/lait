//! Domain transactions over one Fabric Body, and the projection that reads them.

use std::collections::{BTreeMap, BTreeSet};

use fabric::{BodyExport, CollaborativeView, Engine, Key, Op, Transaction, Version};

use crate::codec::{
    decode_group, decode_link, decode_redirect, decode_stamp, decode_u64, encode, encode_group,
    encode_link, encode_redirect, encode_stamp, encode_u64, evidence_is_authored,
};
use crate::ids::CardId;
use crate::mapping::{
    card_id_entry, entry_device, entry_tag_counter, path_created, path_groups, path_handles,
    path_name, path_note, path_picture, path_self, BODY_KEY, ENTRY_CLOCK, ENTRY_SCHEMA,
    PATH_GRAVES, PATH_META, PATH_REDIRECTS,
};
use crate::types::{Author, Book, Card, Evidence, Field, GroupLink, Handle, Link, Stamp, Tag};
use crate::{bounds, Error};

/// A local mutation. One action is one Fabric transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Create {
        id: CardId,
        name: String,
    },
    SetName {
        id: CardId,
        name: String,
    },
    SetNote {
        id: CardId,
        note: String,
    },
    /// Set (or clear, with an empty string) the card's picture, in the stored
    /// `<mime>;base64,<data>` form.
    SetPicture {
        id: CardId,
        picture: String,
    },
    AddGroup {
        id: CardId,
        name: String,
    },
    RemoveGroup {
        id: CardId,
        name: String,
    },
    AddHandle {
        id: CardId,
        handle: Handle,
        evidence: Evidence,
    },
    RemoveHandle {
        id: CardId,
        handle: Handle,
    },
    Merge {
        from: CardId,
        into: CardId,
    },
    ClaimSelf {
        id: CardId,
    },
    Delete {
        id: CardId,
    },
}

/// The book as a Fabric Body plus the Lamport clock reconstructed from it.
pub struct BookEngine {
    engine: Engine,
    key: Key,
}

impl Default for BookEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl BookEngine {
    /// An empty book in a fresh Engine. The Body is created on the first apply.
    pub fn new() -> Self {
        Self {
            engine: Engine::new(),
            key: Key::from_bytes(BODY_KEY.to_vec()),
        }
    }

    /// The Body key this engine occupies.
    pub fn key(&self) -> &Key {
        &self.key
    }

    /// Causal version of the book Body, or empty if nothing has been written.
    pub fn version(&self) -> Result<Version, Error> {
        match self.engine.version(&self.key) {
            Ok(version) => Ok(version),
            Err(fabric::Invalid::NotCollaborative) => Ok(Version::empty()),
            Err(err) => Err(Error::Engine(err.to_string())),
        }
    }

    /// Export the collaborative Body. The envelope wraps this.
    pub fn export_body(&self) -> Result<BodyExport, Error> {
        self.engine
            .export_body(&self.key)
            .ok_or(Error::Corrupt("no body to export"))
    }

    /// Install a Body into a blank engine.
    pub fn import_body(export: &BodyExport) -> Result<Self, Error> {
        let mut engine = Engine::new();
        engine
            .import_body(&Key::from_bytes(BODY_KEY.to_vec()), export)
            .map_err(|err| Error::Engine(err.to_string()))?;
        Ok(Self {
            engine,
            key: Key::from_bytes(BODY_KEY.to_vec()),
        })
    }

    /// Project the live Book. Derived facts are not present.
    pub fn book(&self) -> Result<Book, Error> {
        match self.engine.read_collaborative(&self.key) {
            Ok(view) => project(&view),
            Err(fabric::projection::Failure::NotCollaborative) => Ok(Book {
                version: crate::codec::SCHEMA_VERSION,
                cards: BTreeMap::new(),
                graves: BTreeMap::new(),
                redirects: BTreeMap::new(),
                clock: 0,
                tag_counters: BTreeMap::new(),
            }),
            Err(err) => Err(Error::Engine(err.to_string())),
        }
    }

    /// Apply one local action. Bounds are checked against the resulting live
    /// projection before Fabric sees the transaction.
    pub fn apply(&mut self, author: &Author, action: Action) -> Result<Book, Error> {
        let before = self.book()?;
        let planned = plan(&before, author, action)?;
        check_bounds(&planned.book)?;
        let mut ops = Vec::new();
        if self.engine.version(&self.key).is_err() {
            ops.push(Op::CreateBody {
                key: self.key.clone(),
            });
        }
        ops.extend(planned.ops);
        self.engine.commit(Transaction::new("addressbook", ops))?;
        let after = self.book()?;
        if after != planned.book {
            return Err(Error::Engine(
                "projection after commit disagreed with the planned Book".into(),
            ));
        }
        Ok(after)
    }

    /// Export a checkpoint at the current frontier, import it into a blank
    /// Engine, and demand the same projection and causal version.
    pub fn compacted(&self) -> Result<Self, Error> {
        let before = self.book()?;
        let version = self.version()?;
        let checkpoint = self
            .engine
            .export_checkpoint(&self.key, &version)
            .map_err(|err| Error::Engine(err.to_string()))?;
        let mut engine = Engine::new();
        engine
            .import_artifact(&self.key, &checkpoint)
            .map_err(|err| Error::Engine(err.to_string()))?;
        let next = Self {
            engine,
            key: self.key.clone(),
        };
        let after = next.book()?;
        let after_version = next.version()?;
        if after != before {
            return Err(Error::Engine(
                "checkpoint projection did not match the live Book".into(),
            ));
        }
        if after_version != version {
            return Err(Error::Engine(
                "checkpoint causal version did not match the live Body".into(),
            ));
        }
        Ok(next)
    }
}

struct Planned {
    book: Book,
    ops: Vec<Op>,
}

fn plan(book: &Book, author: &Author, action: Action) -> Result<Planned, Error> {
    let mut book = book.clone();
    let clock = book.clock.saturating_add(1);
    book.clock = clock;
    let stamp = Stamp {
        lamport: clock,
        by: author.device.clone(),
        at: author.at,
    };
    let tag = next_tag(&mut book, &author.device);
    let key = Key::from_bytes(BODY_KEY.to_vec());
    let mut ops = vec![
        Op::MapSet {
            key: key.clone(),
            path: PATH_META.into(),
            entry: ENTRY_SCHEMA.into(),
            value: vec![crate::codec::SCHEMA_VERSION],
        },
        Op::MapSet {
            key: key.clone(),
            path: PATH_META.into(),
            entry: ENTRY_CLOCK.into(),
            value: encode_u64(book.clock),
        },
        Op::MapSet {
            key: key.clone(),
            path: PATH_META.into(),
            entry: entry_tag_counter(&author.device),
            value: encode_u64(tag.counter),
        },
    ];

    match action {
        Action::Create { id, name } => {
            validate_name(&name)?;
            if book.cards.contains_key(&id) || book.graves.contains_key(&id) {
                return Err(Error::Invalid("card already exists"));
            }
            let card = Card {
                id: id.clone(),
                name: Field {
                    value: name.clone(),
                    stamp: stamp.clone(),
                },
                note: Field {
                    value: String::new(),
                    stamp: stamp.clone(),
                },
                picture: Field {
                    value: String::new(),
                    stamp: stamp.clone(),
                },
                groups: Vec::new(),
                handles: Vec::new(),
                self_claim: None,
                created: stamp.clone(),
            };
            book.cards.insert(id.clone(), card);
            ops.extend(scalar_ops(
                &key,
                &id,
                &author.device,
                "name",
                &name,
                &stamp,
            )?);
            ops.extend(scalar_ops(&key, &id, &author.device, "note", "", &stamp)?);
            ops.push(Op::MapSet {
                key: key.clone(),
                path: path_created(&id),
                entry: entry_device(&author.device),
                value: encode_stamp(&stamp)?,
            });
        }
        Action::SetName { id, name } => {
            validate_name(&name)?;
            let card = live_mut(&mut book, &id)?;
            card.name = Field {
                value: name.clone(),
                stamp: stamp.clone(),
            };
            ops.extend(scalar_ops(
                &key,
                &id,
                &author.device,
                "name",
                &name,
                &stamp,
            )?);
        }
        Action::SetNote { id, note } => {
            validate_note(&note)?;
            let card = live_mut(&mut book, &id)?;
            card.note = Field {
                value: note.clone(),
                stamp: stamp.clone(),
            };
            ops.extend(scalar_ops(
                &key,
                &id,
                &author.device,
                "note",
                &note,
                &stamp,
            )?);
        }
        Action::SetPicture { id, picture } => {
            validate_picture(&picture)?;
            let card = live_mut(&mut book, &id)?;
            card.picture = Field {
                value: picture.clone(),
                stamp: stamp.clone(),
            };
            ops.extend(scalar_ops(
                &key,
                &id,
                &author.device,
                "picture",
                &picture,
                &stamp,
            )?);
        }
        Action::AddGroup { id, name } => {
            if name.is_empty() {
                return Err(Error::Invalid("empty group"));
            }
            let link = GroupLink {
                name,
                tag,
                added: stamp,
            };
            let card = live_mut(&mut book, &id)?;
            card.groups.push(link.clone());
            card.groups.sort();
            ops.push(Op::SetAdd {
                key: key.clone(),
                path: path_groups(&id),
                value: encode_group(&link)?,
            });
        }
        Action::RemoveGroup { id, name } => {
            let card = live_mut(&mut book, &id)?;
            let removed: Vec<GroupLink> = card
                .groups
                .iter()
                .filter(|link| link.name == name)
                .cloned()
                .collect();
            if removed.is_empty() {
                return Err(Error::Invalid("group is not on this card"));
            }
            card.groups.retain(|link| link.name != name);
            for link in removed {
                ops.push(Op::SetRemove {
                    key: key.clone(),
                    path: path_groups(&id),
                    value: encode_group(&link)?,
                });
            }
        }
        Action::AddHandle {
            id,
            handle,
            evidence,
        } => {
            if !evidence_is_authored(&evidence) {
                return Err(Error::Invalid("derived evidence is not authored"));
            }
            let link = Link {
                handle,
                tag,
                evidence,
                added: stamp,
                last_seen: None,
            };
            let card = live_mut(&mut book, &id)?;
            if card.handles.len() >= bounds::MAX_HANDLES_PER_CARD {
                return Err(Error::Bound("MAX_HANDLES_PER_CARD"));
            }
            card.handles.push(link.clone());
            card.handles.sort();
            ops.push(Op::SetAdd {
                key: key.clone(),
                path: path_handles(&id),
                value: encode_link(&link)?,
            });
        }
        Action::RemoveHandle { id, handle } => {
            let card = live_mut(&mut book, &id)?;
            let removed: Vec<Link> = card
                .handles
                .iter()
                .filter(|link| link.handle == handle)
                .cloned()
                .collect();
            if removed.is_empty() {
                return Err(Error::Invalid("handle is not on this card"));
            }
            card.handles.retain(|link| link.handle != handle);
            for link in removed {
                ops.push(Op::SetRemove {
                    key: key.clone(),
                    path: path_handles(&id),
                    value: encode_link(&link)?,
                });
            }
        }
        Action::Merge { from, into } => {
            if from == into {
                return Err(Error::Invalid("a card cannot merge into itself"));
            }
            if !book.cards.contains_key(&from) || !book.cards.contains_key(&into) {
                return Err(Error::NoSuchCard);
            }
            if would_cycle(&book.redirects, &from, &into) {
                return Err(Error::Invalid("redirect would cycle"));
            }
            let lost = book.cards.remove(&from).ok_or(Error::NoSuchCard)?;
            if let Some(survivor) = book.cards.get_mut(&into) {
                fold_card(survivor, lost);
            }
            book.redirects
                .insert(from.clone(), (into.clone(), stamp.clone()));
            ops.push(Op::MapSet {
                key: key.clone(),
                path: PATH_REDIRECTS.into(),
                entry: card_id_entry(&from),
                value: encode_redirect(&into, &stamp)?,
            });
        }
        Action::ClaimSelf { id } => {
            live_mut(&mut book, &id)?.self_claim = Some(stamp.clone());
            resolve_self_claims(&mut book, &stamp, &mut ops, &key)?;
            ops.push(Op::MapSet {
                key: key.clone(),
                path: path_self(&id),
                entry: entry_device(&author.device),
                value: encode_stamp(&stamp)?,
            });
        }
        Action::Delete { id } => {
            if book.cards.remove(&id).is_none() {
                return Err(Error::NoSuchCard);
            }
            if book.graves.len() >= bounds::MAX_TOMBSTONES {
                return Err(Error::Bound("MAX_TOMBSTONES"));
            }
            book.graves.insert(id.clone(), stamp.clone());
            ops.push(Op::MapSet {
                key: key.clone(),
                path: PATH_GRAVES.into(),
                entry: card_id_entry(&id),
                value: encode_stamp(&stamp)?,
            });
        }
    }

    Ok(Planned { book, ops })
}

fn scalar_ops(
    key: &Key,
    id: &CardId,
    device: &mechanics::ids::DeviceId,
    field: &str,
    value: &str,
    stamp: &Stamp,
) -> Result<Vec<Op>, Error> {
    let path = match field {
        "name" => path_name(id),
        "note" => path_note(id),
        "picture" => path_picture(id),
        _ => return Err(Error::Invalid("unknown scalar")),
    };
    Ok(vec![Op::MapSet {
        key: key.clone(),
        path,
        entry: entry_device(device),
        value: encode(&(value.to_owned(), stamp))?,
    }])
}

fn live_mut<'a>(book: &'a mut Book, id: &CardId) -> Result<&'a mut Card, Error> {
    book.cards.get_mut(id).ok_or(Error::NoSuchCard)
}

fn next_tag(book: &mut Book, device: &mechanics::ids::DeviceId) -> Tag {
    let next = book
        .tag_counters
        .get(device)
        .copied()
        .unwrap_or(0)
        .saturating_add(1);
    book.tag_counters.insert(device.clone(), next);
    Tag {
        device: device.clone(),
        counter: next,
    }
}

fn validate_name(name: &str) -> Result<(), Error> {
    // An empty name is a structural refusal, not a bound: reporting it as
    // MAX_NAME_BYTES sends the operator to fix a length that is zero.
    if name.trim().is_empty() {
        return Err(Error::Invalid("a card needs a nonempty name"));
    }
    if name.len() > bounds::MAX_NAME_BYTES {
        return Err(Error::Bound("MAX_NAME_BYTES"));
    }
    Ok(())
}

fn validate_note(note: &str) -> Result<(), Error> {
    if note.len() > bounds::MAX_NOTE_BYTES {
        return Err(Error::Bound("MAX_NOTE_BYTES"));
    }
    Ok(())
}

/// A picture is `<mime>;base64,<data>` from a short allowlist, or empty (the
/// clear). Validated at write so a stored picture is always drawable: a reader
/// that had to sniff or repair would be a second opinion about what the book
/// holds.
fn validate_picture(picture: &str) -> Result<(), Error> {
    if picture.is_empty() {
        return Ok(());
    }
    if picture.len() > bounds::MAX_PICTURE_BYTES {
        return Err(Error::Bound("MAX_PICTURE_BYTES"));
    }
    const MIMES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];
    let Some((mime, data)) = picture.split_once(";base64,") else {
        return Err(Error::Invalid("a picture is `<mime>;base64,<data>`"));
    };
    if !MIMES.contains(&mime) {
        return Err(Error::Invalid("picture mime must be png, jpeg, or webp"));
    }
    if data.is_empty()
        || data_encoding::BASE64
            .decode(data.as_bytes())
            .map(|bytes| bytes.is_empty())
            .unwrap_or(true)
    {
        return Err(Error::Invalid("picture payload is not base64"));
    }
    Ok(())
}

fn would_cycle(
    redirects: &BTreeMap<CardId, (CardId, Stamp)>,
    from: &CardId,
    into: &CardId,
) -> bool {
    let mut seen = BTreeSet::new();
    let mut cursor = into;
    seen.insert(from.clone());
    while let Some((next, _)) = redirects.get(cursor) {
        if !seen.insert(next.clone()) {
            return true;
        }
        if next == from {
            return true;
        }
        cursor = next;
    }
    false
}

fn fold_card(survivor: &mut Card, lost: Card) {
    if lost.name.stamp > survivor.name.stamp {
        survivor.name = lost.name;
    }
    if lost.note.stamp > survivor.note.stamp {
        survivor.note = lost.note;
    }
    survivor.groups.extend(lost.groups);
    survivor.groups.sort();
    survivor.groups.dedup();
    survivor.handles.extend(lost.handles);
    survivor.handles.sort();
    survivor.handles.dedup();
    match (survivor.self_claim.clone(), lost.self_claim) {
        (Some(keep), Some(other)) if other < keep => survivor.self_claim = Some(other),
        (None, Some(other)) => survivor.self_claim = Some(other),
        _ => {}
    }
}

fn resolve_self_claims(
    book: &mut Book,
    stamp: &Stamp,
    ops: &mut Vec<Op>,
    key: &Key,
) -> Result<(), Error> {
    let mut claims: Vec<(Stamp, CardId)> = book
        .cards
        .iter()
        .filter_map(|(id, card)| card.self_claim.clone().map(|claim| (claim, id.clone())))
        .collect();
    if claims.len() < 2 {
        return Ok(());
    }
    claims.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let Some((_, winner)) = claims.first() else {
        return Ok(());
    };
    let winner = winner.clone();
    let losers: Vec<CardId> = claims.into_iter().skip(1).map(|(_, id)| id).collect();
    for loser in losers {
        if would_cycle(&book.redirects, &loser, &winner) {
            continue;
        }
        if let Some(lost) = book.cards.remove(&loser) {
            if let Some(survivor) = book.cards.get_mut(&winner) {
                fold_card(survivor, lost);
            }
        }
        book.redirects
            .insert(loser.clone(), (winner.clone(), stamp.clone()));
        ops.push(Op::MapSet {
            key: key.clone(),
            path: PATH_REDIRECTS.into(),
            entry: card_id_entry(&loser),
            value: encode_redirect(&winner, stamp)?,
        });
    }
    Ok(())
}

fn check_bounds(book: &Book) -> Result<(), Error> {
    if book.cards.len() > bounds::MAX_CARDS {
        return Err(Error::Bound("MAX_CARDS"));
    }
    if book.graves.len() > bounds::MAX_TOMBSTONES {
        return Err(Error::Bound("MAX_TOMBSTONES"));
    }
    for card in book.cards.values() {
        if card.handles.len() > bounds::MAX_HANDLES_PER_CARD {
            return Err(Error::Bound("MAX_HANDLES_PER_CARD"));
        }
        if card.note.value.len() > bounds::MAX_NOTE_BYTES {
            return Err(Error::Bound("MAX_NOTE_BYTES"));
        }
        let devices = card
            .handles
            .iter()
            .filter(|link| matches!(link.handle, Handle::Device(_)))
            .count();
        if devices > bounds::MAX_SHARED_DEVICES {
            return Err(Error::Bound("MAX_SHARED_DEVICES"));
        }
    }
    let encoded = encode(book)?;
    if encoded.len() > bounds::MAX_BOOK_BYTES {
        return Err(Error::Bound("MAX_BOOK_BYTES"));
    }
    Ok(())
}

fn project(view: &CollaborativeView) -> Result<Book, Error> {
    let meta = view.maps.get(PATH_META);
    let schema = match meta
        .and_then(|m| m.get(ENTRY_SCHEMA))
        .and_then(|b| b.first().copied())
    {
        Some(schema) => schema,
        // A blank Engine has no Body and therefore no schema entry; only a
        // Body already carrying material without one is corrupt. Assuming
        // "current" for a populated Body would read a future book wrongly
        // instead of refusing it.
        None if view.maps.is_empty() => crate::codec::SCHEMA_VERSION,
        None => return Err(Error::Corrupt("schema missing")),
    };
    if schema != crate::codec::SCHEMA_VERSION {
        return Err(Error::UnsupportedVersion(schema));
    }
    let clock = meta
        .and_then(|m| m.get(ENTRY_CLOCK))
        .map(|b| decode_u64(b))
        .transpose()?
        .unwrap_or(0);

    let mut graves = BTreeMap::new();
    if let Some(entries) = view.maps.get(PATH_GRAVES) {
        for (id, raw) in entries {
            let id = CardId::parse(id).ok_or(Error::Corrupt("grave id"))?;
            graves.insert(id, decode_stamp(raw)?);
        }
    }
    let mut redirects = BTreeMap::new();
    if let Some(entries) = view.maps.get(PATH_REDIRECTS) {
        for (id, raw) in entries {
            let id = CardId::parse(id).ok_or(Error::Corrupt("redirect id"))?;
            redirects.insert(id, decode_redirect(raw)?);
        }
    }

    let mut raw_cards: BTreeMap<CardId, PartialCard> = BTreeMap::new();
    collect_scalars(view, "name", &mut raw_cards, |c, field| {
        c.name = Some(field)
    })?;
    collect_scalars(view, "note", &mut raw_cards, |c, field| {
        c.note = Some(field)
    })?;
    collect_scalars(view, "picture", &mut raw_cards, |c, field| {
        c.picture = Some(field)
    })?;
    for (path, entries) in &view.maps {
        if let Some(id) = path
            .strip_prefix("card/")
            .and_then(|p| p.strip_suffix("/created"))
        {
            let id = CardId::parse(id).ok_or(Error::Corrupt("created id"))?;
            let stamp = entries
                .values()
                .map(|raw| decode_stamp(raw))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .min()
                .ok_or(Error::Corrupt("created stamp"))?;
            raw_cards.entry(id).or_default().created = Some(stamp);
        }
        if let Some(id) = path
            .strip_prefix("card/")
            .and_then(|p| p.strip_suffix("/self"))
        {
            let id = CardId::parse(id).ok_or(Error::Corrupt("self id"))?;
            let stamp = entries
                .values()
                .map(|raw| decode_stamp(raw))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .min();
            raw_cards.entry(id).or_default().self_claim = stamp;
        }
    }
    for (path, members) in &view.sets {
        if let Some(id) = path
            .strip_prefix("card/")
            .and_then(|p| p.strip_suffix("/groups"))
        {
            let id = CardId::parse(id).ok_or(Error::Corrupt("group id"))?;
            let mut groups = Vec::new();
            for raw in members {
                groups.push(decode_group(raw)?);
            }
            groups.sort();
            raw_cards.entry(id).or_default().groups = groups;
        }
        if let Some(id) = path
            .strip_prefix("card/")
            .and_then(|p| p.strip_suffix("/handles"))
        {
            let id = CardId::parse(id).ok_or(Error::Corrupt("handle id"))?;
            let mut handles = Vec::new();
            for raw in members {
                handles.push(decode_link(raw)?);
            }
            handles.sort();
            raw_cards.entry(id).or_default().handles = handles;
        }
    }

    let mut cards = BTreeMap::new();
    for (id, partial) in raw_cards {
        if graves.contains_key(&id) {
            continue;
        }
        let Some(card) = partial.into_card(id.clone()) else {
            continue;
        };
        cards.insert(id, card);
    }

    apply_redirects(&mut cards, &redirects);

    let mut clock = clock;
    advance_clock(&mut clock, &cards, &graves, &redirects);

    let mut tag_counters = BTreeMap::new();
    if let Some(meta) = view.maps.get(PATH_META) {
        for (entry, raw) in meta {
            if let Some(device) = entry.strip_prefix("tag:") {
                if let Some(id) = mechanics::ids::DeviceId::parse(device) {
                    tag_counters.insert(id, decode_u64(raw)?);
                }
            }
        }
    }

    Ok(Book {
        version: schema,
        cards,
        graves,
        redirects,
        clock,
        tag_counters,
    })
}

#[derive(Default)]
struct PartialCard {
    name: Option<Field<String>>,
    note: Option<Field<String>>,
    picture: Option<Field<String>>,
    groups: Vec<GroupLink>,
    handles: Vec<Link>,
    self_claim: Option<Stamp>,
    created: Option<Stamp>,
}

impl PartialCard {
    fn into_card(self, id: CardId) -> Option<Card> {
        let name = self.name?;
        let created = self.created?;
        Some(Card {
            id,
            name,
            note: self.note.unwrap_or_else(|| Field {
                value: String::new(),
                stamp: created.clone(),
            }),
            // A book written before pictures existed projects an empty one —
            // the default face, not an error.
            picture: self.picture.unwrap_or_else(|| Field {
                value: String::new(),
                stamp: created.clone(),
            }),
            groups: self.groups,
            handles: self.handles,
            self_claim: self.self_claim,
            created,
        })
    }
}

fn collect_scalars(
    view: &CollaborativeView,
    field: &str,
    into: &mut BTreeMap<CardId, PartialCard>,
    write: impl Fn(&mut PartialCard, Field<String>),
) -> Result<(), Error> {
    let suffix = format!("/{field}");
    for (path, entries) in &view.maps {
        let Some(id) = path
            .strip_prefix("card/")
            .and_then(|p| p.strip_suffix(&suffix))
        else {
            continue;
        };
        let id = CardId::parse(id).ok_or(Error::Corrupt("scalar id"))?;
        let mut best: Option<Field<String>> = None;
        for raw in entries.values() {
            let (value, stamp): (String, Stamp) = crate::codec::decode(raw)?;
            let field = Field { value, stamp };
            best = Some(match best {
                None => field,
                Some(cur) if field.stamp > cur.stamp => field,
                Some(cur) => cur,
            });
        }
        if let Some(field) = best {
            write(into.entry(id).or_default(), field);
        }
    }
    Ok(())
}

fn apply_redirects(
    cards: &mut BTreeMap<CardId, Card>,
    redirects: &BTreeMap<CardId, (CardId, Stamp)>,
) {
    let sources: Vec<CardId> = redirects.keys().cloned().collect();
    for from in sources {
        let Some(to) = resolve_redirect(redirects, &from) else {
            continue;
        };
        if from == to {
            continue;
        }
        if let Some(lost) = cards.remove(&from) {
            if let Some(survivor) = cards.get_mut(&to) {
                fold_card(survivor, lost);
            } else {
                cards.insert(to, lost);
            }
        }
    }
}

fn resolve_redirect(
    redirects: &BTreeMap<CardId, (CardId, Stamp)>,
    from: &CardId,
) -> Option<CardId> {
    let mut seen = BTreeSet::new();
    let mut cursor = from.clone();
    while let Some((next, _)) = redirects.get(&cursor) {
        if !seen.insert(cursor.clone()) {
            return None;
        }
        cursor = next.clone();
    }
    if cursor == *from {
        None
    } else {
        Some(cursor)
    }
}

fn advance_clock(
    clock: &mut u64,
    cards: &BTreeMap<CardId, Card>,
    graves: &BTreeMap<CardId, Stamp>,
    redirects: &BTreeMap<CardId, (CardId, Stamp)>,
) {
    for card in cards.values() {
        bump(clock, card.name.stamp.lamport);
        bump(clock, card.note.stamp.lamport);
        bump(clock, card.created.lamport);
        if let Some(claim) = &card.self_claim {
            bump(clock, claim.lamport);
        }
        for link in &card.handles {
            bump(clock, link.added.lamport);
        }
        for link in &card.groups {
            bump(clock, link.added.lamport);
        }
    }
    for stamp in graves.values() {
        bump(clock, stamp.lamport);
    }
    for (_, stamp) in redirects.values() {
        bump(clock, stamp.lamport);
    }
}

fn bump(clock: &mut u64, seen: u64) {
    if seen > *clock {
        *clock = seen;
    }
}
