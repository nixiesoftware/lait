//! Identity-scoped address book on the daemon.
//!
//! The crate is a leaf; this module is the only thing that opens the store
//! under the selected identity directory and answers control-plane `Book*`
//! requests. It never calls [`crate::orbits::Router::place`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use addressbook::{
    Action, Author, Book, BookEngine, CardBundle, CardId, Coverage, Handle, Store,
    MAX_PENDING_SUGGESTIONS,
};
use mechanics::ids::SystemUlidSource;
use mechanics::ids::{ActorId, DeviceId};
use serde::{Deserialize, Serialize};

use crate::control::{
    BookCardView, BookHitView, BookMigrationView, BookResolutionView, BookSuggestionView, BookView,
    Request, Response,
};
use crate::orbits::{Catalog, Router, StationIdentity};

// The agent group's canonical name lives with the rest of the wire
// vocabulary; this module is its writer, not its owner.
use crate::control::AGENT_GROUP;

/// Durable record of alias-file import. Lives beside the book.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MigrationState {
    files: BTreeMap<String, FileProgress>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FileProgress {
    imported: usize,
    pending: Vec<PendingSelector>,
    discarded: usize,
    finished: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingSelector {
    orbit: String,
    selector: String,
    name: String,
}

/// Staged card-exchange proposals, durable beside the book. Review is the
/// only way into the book: nothing here has touched the Engine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SuggestionState {
    suggestions: Vec<StagedSuggestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StagedSuggestion {
    /// Content-derived (`sug_` + 16 hex of a hash over name, note and
    /// handles), so proposing the same card twice stages it once.
    id: String,
    name: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    handles: Vec<String>,
}

impl StagedSuggestion {
    fn from_shared(card: &addressbook::SharedCard) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(card.name.as_bytes());
        hasher.update(&[0]);
        hasher.update(card.note.as_bytes());
        for handle in &card.handles {
            hasher.update(&[0]);
            hasher.update(handle.as_bytes());
        }
        let digest = hasher.finalize();
        let hex = data_encoding::HEXLOWER.encode(digest.as_bytes());
        Self {
            id: format!("sug_{}", hex.get(..16).unwrap_or(&hex)),
            name: card.name.clone(),
            note: card.note.clone(),
            handles: card.handles.clone(),
        }
    }
}

pub(crate) struct AddressBookService {
    store: Store,
    engine: Mutex<BookEngine>,
    identity_dir: PathBuf,
}

impl AddressBookService {
    pub(crate) fn open(identity_dir: &Path) -> Result<Self, addressbook::Error> {
        let store = Store::at(identity_dir);
        let engine = match store.open()? {
            Some(engine) => engine,
            None => BookEngine::new(),
        };
        Ok(Self {
            store,
            engine: Mutex::new(engine),
            identity_dir: identity_dir.to_path_buf(),
        })
    }

    pub(crate) async fn handle(&self, request: Request, router: &Router) -> Response {
        match request {
            Request::BookList => {
                // Import is demand-driven: listing is the first honest
                // moment the identity asked for its book. A pass that
                // imports a file completely also retires it — migration
                // is a move, not a copy.
                let _ = self.migrate(router);
                self.list()
            }
            Request::BookGet { card } => self.get(&card),
            Request::BookPut { card, name, note } => self.put(card, name, note),
            Request::BookDelete { card } => self.delete(&card),
            Request::BookSetPicture { card, picture } => self.set_picture(&card, picture),
            Request::BookLink { card, handle } => self.link(&card, &handle),
            Request::BookUnlink { card, handle } => self.unlink(&card, &handle),
            Request::BookMerge { from, into } => self.merge(&from, &into),
            Request::BookClaimSelf { card } => self.claim_self(&card),
            Request::BookLookup { handle } => self.lookup(&handle),
            Request::BookResolve { orbit, handles } => self.resolve(router, &orbit, handles).await,
            Request::BookMigrateStatus => self.migrate_status(router.catalog()),
            Request::BookMigrate => self.migrate(router),
            Request::BookPropose { bundle } => self.propose(&bundle),
            Request::BookSuggestAccept { suggestion } => self.suggest_accept(&suggestion),
            Request::BookSuggestDismiss { suggestion } => self.suggest_dismiss(&suggestion),
            other => Response::err(format!("not an address-book request: {other:?}")),
        }
    }

    fn list(&self) -> Response {
        match self.book() {
            Ok(book) => self.view(&book),
            Err(err) => Response::err(err.to_string()),
        }
    }

    fn get(&self, card: &str) -> Response {
        let book = match self.book() {
            Ok(book) => book,
            Err(err) => return Response::err(err.to_string()),
        };
        let Some(card) = book.cards.values().find(|c| c.id.as_str() == card) else {
            return Response::err("no such card");
        };
        Response::Book(Box::new(BookView {
            cards: vec![card_view(card)],
            migration: self.migration_view(None),
            suggestions: Vec::new(),
        }))
    }

    fn put(&self, card: Option<String>, name: String, note: Option<String>) -> Response {
        let author = match self.author() {
            Ok(author) => author,
            Err(err) => return Response::err(err.to_string()),
        };
        let mut engine = match self.engine.lock() {
            Ok(engine) => engine,
            Err(_) => return Response::err("address book is poisoned"),
        };
        let id = match card {
            Some(raw) => match CardId::parse(&raw) {
                Some(id) => id,
                None => return Response::err("invalid card id"),
            },
            None => CardId::mint(&SystemUlidSource),
        };
        let exists = engine
            .book()
            .map(|book| book.cards.contains_key(&id))
            .unwrap_or(false);
        let action = if exists {
            Action::SetName {
                id: id.clone(),
                name,
            }
        } else {
            Action::Create {
                id: id.clone(),
                name,
            }
        };
        if let Err(err) = engine.apply(&author, action) {
            return Response::err(err.to_string());
        }
        if let Some(note) = note {
            if let Err(err) = engine.apply(
                &author,
                Action::SetNote {
                    id: id.clone(),
                    note,
                },
            ) {
                return Response::err(err.to_string());
            }
        }
        if let Err(err) = self.store.replace(&engine) {
            return Response::err(err.to_string());
        }
        match engine.book() {
            Ok(book) => self.view(&book),
            Err(err) => Response::err(err.to_string()),
        }
    }

    fn delete(&self, card: &str) -> Response {
        self.apply_one(card, |id| Action::Delete { id })
    }

    fn set_picture(&self, card: &str, picture: String) -> Response {
        self.apply_one(card, |id| Action::SetPicture { id, picture })
    }

    fn claim_self(&self, card: &str) -> Response {
        self.apply_one(card, |id| Action::ClaimSelf { id })
    }

    fn link(&self, card: &str, handle: &str) -> Response {
        let handle = match Handle::parse_wire(handle) {
            Ok(handle) => handle,
            Err(err) => return Response::err(err.to_string()),
        };
        self.apply_one(card, |id| Action::AddHandle {
            id,
            handle,
            evidence: addressbook::Evidence::Declared,
        })
    }

    fn unlink(&self, card: &str, handle: &str) -> Response {
        let handle = match Handle::parse_wire(handle) {
            Ok(handle) => handle,
            Err(err) => return Response::err(err.to_string()),
        };
        self.apply_one(card, |id| Action::RemoveHandle { id, handle })
    }

    fn merge(&self, from: &str, into: &str) -> Response {
        let from = match CardId::parse(from) {
            Some(id) => id,
            None => return Response::err("invalid source card"),
        };
        let into = match CardId::parse(into) {
            Some(id) => id,
            None => return Response::err("invalid target card"),
        };
        self.apply_action(Action::Merge { from, into })
    }

    fn lookup(&self, handle: &str) -> Response {
        let handle = match Handle::parse_wire(handle) {
            Ok(handle) => handle,
            Err(err) => return Response::err(err.to_string()),
        };
        let book = match self.book() {
            Ok(book) => book,
            Err(err) => return Response::err(err.to_string()),
        };
        let cards: Vec<BookCardView> = book
            .authored_cards_for(&handle)
            .into_iter()
            .filter_map(|id| book.cards.get(&id).map(card_view))
            .collect();
        Response::Book(Box::new(BookView {
            cards,
            migration: self.migration_view(None),
            suggestions: Vec::new(),
        }))
    }

    async fn resolve(&self, router: &Router, orbit: &str, handles: Vec<String>) -> Response {
        let snapshot = router.active_handle_snapshot(orbit).await;
        let Some(snapshot) = snapshot else {
            return Response::BookResolution(Box::new(BookResolutionView {
                hits: Vec::new(),
                coverage: Some(coverage_label(Coverage::Unavailable).to_owned()),
            }));
        };
        let book = match self.book() {
            Ok(book) => book,
            Err(err) => return Response::err(err.to_string()),
        };
        let mut hits = Vec::new();
        for raw in handles {
            let Ok(handle) = Handle::parse_wire(&raw) else {
                continue;
            };
            if !snapshot.contains(&handle) {
                continue;
            }
            for card in book.authored_cards_for(&handle) {
                let (name, picture) = book
                    .cards
                    .get(&card)
                    .map(|card| {
                        (
                            card.name.value.clone(),
                            Some(card.picture.value.clone()).filter(|picture| !picture.is_empty()),
                        )
                    })
                    .unwrap_or_default();
                hits.push(BookHitView {
                    card: card.to_string(),
                    handle: handle.to_wire(),
                    name,
                    picture,
                });
            }
        }
        Response::BookResolution(Box::new(BookResolutionView {
            hits,
            coverage: None,
        }))
    }

    /// Decorate a Space reply's naming fields from the book — the one namer.
    ///
    /// Member rows carry actor handles scoped to the reply's space; presence
    /// rows carry device handles. A handle no Card names stays bare, and a
    /// book that cannot open decorates nothing: an absent name is an absence,
    /// never an error, and never a reason to fail the reply it rides on.
    pub(crate) fn decorate(&self, space: &mechanics::ids::SpaceId, response: &mut Response) {
        let Ok(book) = self.book() else { return };
        match response {
            Response::Members { members } => {
                let mut agents = Vec::new();
                for member in members.iter_mut() {
                    let Some(actor) = ActorId::parse(&member.key) else {
                        continue;
                    };
                    let handle = Handle::Actor {
                        space: space.clone(),
                        actor,
                    };
                    if let Some(name) = first_authored_name(&book, &handle) {
                        member.alias = name;
                    }
                    if member.sponsor.is_some() {
                        agents.push(handle);
                    }
                }
                // The inverse decoration: the roster is the live authority on
                // sponsorship, and sponsorship is agenthood, so a sponsored
                // member's card is (re)filed under the agent group here. This
                // heals books written before the group existed — provisioning
                // stamped the name and the face but not what the actor is.
                drop(book);
                for handle in agents {
                    self.file_as_agent(&handle);
                }
            }
            Response::Who { peers } => {
                for peer in peers.iter_mut() {
                    let Some(device) = DeviceId::parse(&peer.id) else {
                        continue;
                    };
                    if let Some(name) = first_authored_name(&book, &Handle::Device(device)) {
                        peer.nick = name;
                    }
                }
            }
            Response::Seeds { seeds } => {
                for seed in seeds.iter_mut() {
                    let Some(device) = DeviceId::parse(&seed.id) else {
                        continue;
                    };
                    if let Some(name) = first_authored_name(&book, &Handle::Device(device)) {
                        seed.nick = name;
                    }
                }
            }
            _ => {}
        }
    }

    /// Author a Card for a just-provisioned co-located agent.
    ///
    /// Called by the daemon funnel after a successful `AgentProvision`,
    /// because a Station has no reach into the identity-scoped book. Without
    /// this, the one verb that creates an agent leaves it unnamed on the one
    /// surface built to show agents. The card also receives the canonical
    /// face the product ships for its tool, when one exists and the card has
    /// none of its own — applications then resolve the picture through the
    /// book like any other identity fact, instead of matching names
    /// themselves.
    pub(crate) fn name_agent(&self, space: &mechanics::ids::SpaceId, actor: &str, name: &str) {
        let Some(actor) = ActorId::parse(actor) else {
            return;
        };
        let handle = Handle::Actor {
            space: space.clone(),
            actor,
        };
        let _ = self.upsert_named(name, handle.clone());
        self.stamp_face_if_absent(&handle, name);
        self.file_as_agent(&handle);
    }

    /// File the card carrying `handle` under [`AGENT_GROUP`].
    ///
    /// The space plane holds the fact — sponsorship is agenthood — and the
    /// book's own vocabulary carries it, so a client can part agents from
    /// people without matching names. Idempotent: a card already in the group
    /// is left untouched, and a handle no card carries files nothing — the
    /// book never invents a card here.
    fn file_as_agent(&self, handle: &Handle) {
        let Ok(author) = self.author() else {
            return;
        };
        let Ok(mut engine) = self.engine.lock() else {
            return;
        };
        let Ok(book) = engine.book() else {
            return;
        };
        let Some(id) = book.authored_cards_for(handle).into_iter().next() else {
            return;
        };
        let filed = book
            .cards
            .get(&id)
            .map(|card| card.groups.iter().any(|link| link.name == AGENT_GROUP))
            .unwrap_or(true);
        if filed {
            return;
        }
        if engine
            .apply(
                &author,
                Action::AddGroup {
                    id,
                    name: AGENT_GROUP.to_owned(),
                },
            )
            .is_err()
        {
            return;
        }
        let _ = self.store.replace(&engine);
    }

    /// Put the shipped canonical face onto the card carrying `handle`, iff a
    /// face exists for `name` and the card has no picture. An authored
    /// picture is never overwritten — the ship is a default, not an
    /// authority.
    fn stamp_face_if_absent(&self, handle: &Handle, name: &str) {
        let Some(bytes) = canonical_agent_face(name) else {
            return;
        };
        let Ok(stored) = addressbook::encode_picture(bytes) else {
            return;
        };
        let Ok(author) = self.author() else {
            return;
        };
        let Ok(mut engine) = self.engine.lock() else {
            return;
        };
        let Ok(book) = engine.book() else {
            return;
        };
        let Some(id) = book.authored_cards_for(handle).into_iter().next() else {
            return;
        };
        let has_picture = book
            .cards
            .get(&id)
            .map(|card| !card.picture.value.is_empty())
            .unwrap_or(true);
        if has_picture {
            return;
        }
        if engine
            .apply(
                &author,
                Action::SetPicture {
                    id,
                    picture: stored,
                },
            )
            .is_err()
        {
            return;
        }
        let _ = self.store.replace(&engine);
    }

    fn migrate_status(&self, catalog: &Catalog) -> Response {
        Response::Book(Box::new(BookView {
            cards: Vec::new(),
            migration: self.migration_view(Some(catalog)),
            suggestions: Vec::new(),
        }))
    }

    fn migrate(&self, router: &Router) -> Response {
        let catalog = router.catalog();
        let mut state = self.load_migration();
        let before = serde_json::to_vec(&state).unwrap_or_default();
        for binding in catalog.bindings() {
            if !matches!(binding.identity, StationIdentity::Own) {
                continue;
            }
            let home = PathBuf::from(&binding.entry.path);
            let path = home.join("aliases.json");
            let key = path.display().to_string();
            let progress = state.files.entry(key).or_default();
            if !path.exists() {
                progress.finished = true;
                continue;
            }
            // The file's existence outranks the durable record. A `finished`
            // record with the file still on disk is either the pre-retirement
            // design's leftover or a writer that touched the file after the
            // record closed (agent provisioning used to) — and both hid
            // selectors from the book for good. Re-reading is idempotent:
            // a selector whose handle is already on a Card imports as a no-op.
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            let map: BTreeMap<String, String> = match serde_json::from_slice(&bytes) {
                Ok(map) => map,
                Err(_) => {
                    // Fail closed: a corrupt aliases.json is not an empty book.
                    continue;
                }
            };
            if map.is_empty() {
                progress.finished = true;
                // An empty map has nothing to lose; retire the file so no
                // second reader can resurrect the old naming design.
                let _ = std::fs::remove_file(&path);
                continue;
            }
            let space = binding.entry.space.clone();
            for (selector, name) in map {
                if let Some(actor) = ActorId::parse(&selector) {
                    if self.import_actor(&space, &actor, &name) {
                        progress.imported = progress.imported.saturating_add(1);
                    }
                    continue;
                }
                if DeviceId::parse(&selector).is_some() {
                    if self.import_device(&selector, &name) {
                        progress.imported = progress.imported.saturating_add(1);
                    }
                    continue;
                }
                if progress.pending.iter().any(|row| row.selector == selector) {
                    continue;
                }
                let orbit = crate::daemon::LocalOrbitId::for_store(&home);
                progress.pending.push(PendingSelector {
                    orbit: orbit.to_string(),
                    selector,
                    name,
                });
            }
            if progress.pending.is_empty() {
                progress.finished = true;
                // Migration is a move, not a copy: every selector above is
                // now a Card (durably, before this line), so the file is
                // retired on the spot. A corrupt file never reaches here —
                // it was skipped unread and its bytes stay for a human.
                let _ = std::fs::remove_file(&path);
            }
        }
        // Rewrite the durable record only when this pass changed it: BookList
        // runs migrate on every read, and an unchanged pass owes the disk
        // nothing.
        let after = serde_json::to_vec(&state).unwrap_or_default();
        if before != after {
            if let Err(err) = self.save_migration(&state) {
                return Response::err(err);
            }
        }
        self.migrate_status(catalog)
    }

    fn import_actor(&self, space: &str, actor: &ActorId, name: &str) -> bool {
        let Some(space) = mechanics::ids::SpaceId::parse(space) else {
            return false;
        };
        let handle = Handle::Actor {
            space,
            actor: actor.clone(),
        };
        self.upsert_named(name, handle)
    }

    fn import_device(&self, device: &str, name: &str) -> bool {
        let Some(id) = DeviceId::parse(device) else {
            return false;
        };
        self.upsert_named(name, Handle::Device(id))
    }

    fn upsert_named(&self, name: &str, handle: Handle) -> bool {
        let author = match self.author() {
            Ok(author) => author,
            Err(_) => return false,
        };
        let Ok(mut engine) = self.engine.lock() else {
            return false;
        };
        let Ok(book) = engine.book() else {
            return false;
        };
        if !book.authored_cards_for(&handle).is_empty() {
            return false;
        }
        let id = CardId::mint(&SystemUlidSource);
        if engine
            .apply(
                &author,
                Action::Create {
                    id: id.clone(),
                    name: name.to_owned(),
                },
            )
            .is_err()
        {
            return false;
        }
        if engine
            .apply(
                &author,
                Action::AddHandle {
                    id,
                    handle,
                    evidence: addressbook::Evidence::Declared,
                },
            )
            .is_err()
        {
            return false;
        }
        self.store.replace(&engine).is_ok()
    }

    fn apply_one(&self, card: &str, build: impl FnOnce(CardId) -> Action) -> Response {
        let Some(id) = CardId::parse(card) else {
            return Response::err("invalid card id");
        };
        self.apply_action(build(id))
    }

    fn apply_action(&self, action: Action) -> Response {
        let author = match self.author() {
            Ok(author) => author,
            Err(err) => return Response::err(err.to_string()),
        };
        let mut engine = match self.engine.lock() {
            Ok(engine) => engine,
            Err(_) => return Response::err("address book is poisoned"),
        };
        if let Err(err) = engine.apply(&author, action) {
            return Response::err(err.to_string());
        }
        if let Err(err) = self.store.replace(&engine) {
            return Response::err(err.to_string());
        }
        match engine.book() {
            Ok(book) => self.view(&book),
            Err(err) => Response::err(err.to_string()),
        }
    }

    fn book(&self) -> Result<Book, addressbook::Error> {
        self.engine
            .lock()
            .map_err(|_| addressbook::Error::Invalid("poisoned"))?
            .book()
    }

    fn author(&self) -> Result<Author, String> {
        // Load-only: a book write must never mint identity material.
        let seed =
            crate::config::load_identity(&self.identity_dir).map_err(|err| err.to_string())?;
        Ok(Author {
            device: mechanics::actor::device_from_seed(&seed),
            at: mechanics::wallclock::now_millis(),
        })
    }

    fn view(&self, book: &Book) -> Response {
        let cards = book.cards.values().map(card_view).collect();
        // Display tolerates an unreadable suggestions store so the book stays
        // listable; the three suggestion verbs refuse loudly on the same
        // fault, which is where the person can act on it.
        let suggestions = self
            .load_suggestions()
            .unwrap_or_default()
            .suggestions
            .iter()
            .map(suggestion_view)
            .collect();
        Response::Book(Box::new(BookView {
            cards,
            migration: self.migration_view(None),
            suggestions,
        }))
    }

    /// Stage a card-exchange bundle for review. Decode preflights the bounds
    /// and refuses local-agent spellings before anything is staged; nothing
    /// reaches the Engine here.
    fn propose(&self, bundle: &str) -> Response {
        let bundle = match CardBundle::decode(bundle.as_bytes()) {
            Ok(bundle) => bundle,
            Err(err) => return Response::err(err.to_string()),
        };
        let mut state = match self.load_suggestions() {
            Ok(state) => state,
            Err(err) => return Response::err(err),
        };
        for card in &bundle.cards {
            let staged = StagedSuggestion::from_shared(card);
            if state.suggestions.iter().any(|s| s.id == staged.id) {
                continue;
            }
            if state.suggestions.len() >= MAX_PENDING_SUGGESTIONS {
                return Response::err(
                    "too many staged suggestions; review or dismiss some before proposing more",
                );
            }
            state.suggestions.push(staged);
        }
        if let Err(err) = self.save_suggestions(&state) {
            return Response::err(err);
        }
        self.list()
    }

    /// Accept one staged suggestion: mint the Card, link its handles, retire
    /// the suggestion. A refusal leaves the suggestion staged, and a partial
    /// application resynchronises the in-memory book from disk rather than
    /// letting memory and envelope diverge.
    fn suggest_accept(&self, suggestion: &str) -> Response {
        let mut state = match self.load_suggestions() {
            Ok(state) => state,
            Err(err) => return Response::err(err),
        };
        let Some(index) = state.suggestions.iter().position(|s| s.id == suggestion) else {
            return Response::err("no such suggestion");
        };
        let staged = match state.suggestions.get(index) {
            Some(staged) => staged.clone(),
            None => return Response::err("no such suggestion"),
        };
        let author = match self.author() {
            Ok(author) => author,
            Err(err) => return Response::err(err),
        };
        let mut engine = match self.engine.lock() {
            Ok(engine) => engine,
            Err(_) => return Response::err("address book is poisoned"),
        };
        let id = CardId::mint(&SystemUlidSource);
        let mut actions = vec![Action::Create {
            id: id.clone(),
            name: staged.name.clone(),
        }];
        if !staged.note.is_empty() {
            actions.push(Action::SetNote {
                id: id.clone(),
                note: staged.note.clone(),
            });
        }
        for wire in &staged.handles {
            let handle = match Handle::parse_wire(wire) {
                Ok(handle) => handle,
                Err(err) => {
                    return Response::err(format!("suggested handle {wire}: {err}"));
                }
            };
            if !handle.may_leave_device() {
                // Decode already refuses these; a store edited by hand does
                // not get a second chance here.
                return Response::err("a local-agent handle cannot be accepted from a bundle");
            }
            actions.push(Action::AddHandle {
                id: id.clone(),
                handle,
                evidence: addressbook::Evidence::Declared,
            });
        }
        for action in actions {
            if let Err(err) = engine.apply(&author, action) {
                self.resync(&mut engine);
                return Response::err(err.to_string());
            }
        }
        if let Err(err) = self.store.replace(&engine) {
            self.resync(&mut engine);
            return Response::err(err.to_string());
        }
        state.suggestions.remove(index);
        if let Err(err) = self.save_suggestions(&state) {
            return Response::err(err);
        }
        drop(engine);
        self.list()
    }

    /// Discard one staged suggestion. The book is untouched.
    fn suggest_dismiss(&self, suggestion: &str) -> Response {
        let mut state = match self.load_suggestions() {
            Ok(state) => state,
            Err(err) => return Response::err(err),
        };
        let before = state.suggestions.len();
        state.suggestions.retain(|s| s.id != suggestion);
        if state.suggestions.len() == before {
            return Response::err("no such suggestion");
        }
        if let Err(err) = self.save_suggestions(&state) {
            return Response::err(err);
        }
        self.list()
    }

    /// Re-read the envelope after a failed multi-action application, so the
    /// in-memory book never drifts from disk. When even the re-read fails the
    /// old state is kept: a broken disk is not a licence to invent one.
    fn resync(&self, engine: &mut BookEngine) {
        match self.store.open() {
            Ok(Some(fresh)) => *engine = fresh,
            Ok(None) => *engine = BookEngine::new(),
            Err(_) => {}
        }
    }

    fn suggestions_path(&self) -> PathBuf {
        self.identity_dir.join("addressbook.suggestions.json")
    }

    /// Absent is an empty store; unreadable is a refusal, never an empty
    /// default — silently answering nothing would discard staged review work
    /// on one bad byte, the aliases.json failure this initiative exists to
    /// retire.
    fn load_suggestions(&self) -> Result<SuggestionState, String> {
        let path = self.suggestions_path();
        if !path.exists() {
            return Ok(SuggestionState::default());
        }
        let bytes =
            std::fs::read(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
        serde_json::from_slice(&bytes).map_err(|_| {
            format!(
                "suggestions store unreadable at {}; fix or remove it",
                path.display()
            )
        })
    }

    fn save_suggestions(&self, state: &SuggestionState) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(state).map_err(|err| err.to_string())?;
        std::fs::write(self.suggestions_path(), bytes).map_err(|err| err.to_string())
    }

    fn migration_path(&self) -> PathBuf {
        self.identity_dir.join("addressbook.migration.json")
    }

    fn load_migration(&self) -> MigrationState {
        std::fs::read(self.migration_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save_migration(&self, state: &MigrationState) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(state).map_err(|err| err.to_string())?;
        std::fs::write(self.migration_path(), bytes).map_err(|err| err.to_string())
    }

    fn migration_view(&self, catalog: Option<&Catalog>) -> BookMigrationView {
        let state = self.load_migration();
        let pending: usize = state.files.values().map(|file| file.pending.len()).sum();
        let imported: usize = state.files.values().map(|file| file.imported).sum();
        let files = if let Some(catalog) = catalog {
            catalog
                .bindings()
                .into_iter()
                .filter(|binding| matches!(binding.identity, StationIdentity::Own))
                .count()
        } else {
            state.files.len()
        };
        let complete = !state.files.is_empty()
            && state
                .files
                .values()
                .all(|file| file.finished && file.pending.is_empty());
        BookMigrationView {
            complete,
            pending,
            imported,
            files,
        }
    }
}

pub(crate) fn is_book_request(request: &Request) -> bool {
    matches!(
        request,
        Request::BookList
            | Request::BookGet { .. }
            | Request::BookPut { .. }
            | Request::BookSetPicture { .. }
            | Request::BookDelete { .. }
            | Request::BookLink { .. }
            | Request::BookUnlink { .. }
            | Request::BookMerge { .. }
            | Request::BookClaimSelf { .. }
            | Request::BookLookup { .. }
            | Request::BookResolve { .. }
            | Request::BookMigrateStatus
            | Request::BookMigrate
            | Request::BookPropose { .. }
            | Request::BookSuggestAccept { .. }
            | Request::BookSuggestDismiss { .. }
    )
}

fn suggestion_view(staged: &StagedSuggestion) -> BookSuggestionView {
    BookSuggestionView {
        suggestion: staged.id.clone(),
        name: staged.name.clone(),
        note: staged.note.clone(),
        handles: staged.handles.clone(),
    }
}

/// The canonical face the product ships for a known coding agent, or none.
///
/// These are the same marks the issue tracker used to carry as its own
/// name-matched table; the book is their home now, stamped at provision, and
/// every application resolves them through the book API like any other
/// identity fact. Matching is exact on the provisioned agent name — a face
/// is a default for the tool's own name, never an inference.
fn canonical_agent_face(name: &str) -> Option<&'static [u8]> {
    match name.to_ascii_lowercase().as_str() {
        "claude" => Some(include_bytes!("agent_faces/claude.png")),
        "codex" => Some(include_bytes!("agent_faces/codex.png")),
        "grok" => Some(include_bytes!("agent_faces/grok.png")),
        _ => None,
    }
}

/// The first non-empty authored name for a handle, or nothing. Decoration
/// wants one name per row; a handle on several Cards answers with the first
/// authored one rather than failing the row.
fn first_authored_name(book: &Book, handle: &Handle) -> Option<String> {
    book.authored_cards_for(handle)
        .into_iter()
        .filter_map(|id| book.cards.get(&id))
        .map(|card| card.name.value.clone())
        .find(|name| !name.is_empty())
}

fn card_view(card: &addressbook::Card) -> BookCardView {
    // One set, two readings: `handles` keeps every wire spelling for clients
    // that predate the split, and the phone-book triplet files each handle
    // under what it is — an address (somewhere this person is someone), a
    // device (a machine that answers as them), or a co-located agent.
    let mut addresses = Vec::new();
    let mut devices = Vec::new();
    let mut agents = Vec::new();
    for link in &card.handles {
        let wire = link.handle.to_wire();
        match link.handle {
            Handle::Actor { .. } => addresses.push(wire),
            Handle::Device(_) => devices.push(wire),
            Handle::LocalAgent { .. } => agents.push(wire),
        }
    }
    BookCardView {
        card: card.id.to_string(),
        name: card.name.value.clone(),
        note: card.note.value.clone(),
        handles: card
            .handles
            .iter()
            .map(|link| link.handle.to_wire())
            .collect(),
        addresses,
        devices,
        agents,
        picture: Some(card.picture.value.clone()).filter(|p| !p.is_empty()),
        groups: card.groups.iter().map(|link| link.name.clone()).collect(),
        self_claim: card.self_claim.is_some(),
    }
}

fn coverage_label(coverage: Coverage) -> &'static str {
    match coverage {
        Coverage::Partial => "partial",
        Coverage::Stale => "stale",
        Coverage::Unavailable => "unavailable",
    }
}

/// Snapshot of authored handles an active Orbit can currently speak for.
#[derive(Debug, Clone)]
pub(crate) struct HandleSnapshot {
    /// The Space the snapshot was taken in. An Actor handle names a Space,
    /// and a handle for another Space must not resolve here — the same actor
    /// id in a different Space is a fact this Orbit cannot speak for.
    pub space: mechanics::ids::SpaceId,
    pub actors: Vec<ActorId>,
    pub devices: Vec<DeviceId>,
}

impl HandleSnapshot {
    fn contains(&self, handle: &Handle) -> bool {
        match handle {
            Handle::Actor { space, actor } => {
                space == &self.space && self.actors.iter().any(|id| id == actor)
            }
            Handle::Device(device) => self.devices.iter().any(|id| id == device),
            Handle::LocalAgent { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_survives_reopen() {
        let dir = std::env::temp_dir().join(format!(
            "lait-book-svc-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp");
        // A book write never mints identity material, so the identity must
        // exist before the first put — same as a real daemon's startup.
        crate::config::load_or_create_identity(&dir).expect("seed");
        let svc = AddressBookService::open(&dir).expect("open");
        let Response::Book(view) = svc.put(None, "Ada".into(), Some("n".into())) else {
            panic!("put should return the book");
        };
        assert_eq!(view.cards.len(), 1);
        assert_eq!(view.cards[0].name, "Ada");
        drop(svc);
        let svc = AddressBookService::open(&dir).expect("reopen");
        let Response::Book(view) = svc.list() else {
            panic!("list should return the book");
        };
        assert_eq!(view.cards.len(), 1);
        assert_eq!(view.cards[0].note, "n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_short_prefix_alias_stays_pending() {
        let dir = std::env::temp_dir().join(format!(
            "lait-book-mig-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp");
        let aliases = dir.join("aliases.json");
        std::fs::write(&aliases, r#"{"ab":"short","not-a-key":"x"}"#).expect("aliases");
        // The migrate path only reads Own catalog bindings. Without a catalog
        // row this file is ignored — pending is recorded only when migrate runs
        // against a binding. This test locks the classifier the importer uses.
        assert!(ActorId::parse("ab").is_none());
        assert!(DeviceId::parse("ab").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real migration path, against a real catalog binding: a full actor
    /// id becomes a Card with an Actor handle, a short prefix stays pending
    /// rather than being canonicalised, a corrupt file is left alone, and a
    /// second pass duplicates nothing.
    #[test]
    fn migration_imports_full_ids_and_keeps_prefixes_pending() {
        let base = std::env::temp_dir().join(format!(
            "lait-book-mig-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let identity = base.join("identity");
        let agents = base.join("agents");
        let home = base.join("home");
        let broken = base.join("broken");
        for dir in [&identity, &agents, &home, &broken] {
            std::fs::create_dir_all(dir).expect("temp");
        }
        // author() is load-only by design; the test mints the seed up front.
        crate::config::load_or_create_identity(&identity).expect("seed");

        let space = mechanics::ids::SpaceId::from_digest([7; 16]);
        let actor = ActorId::from_incept_hash(&data_encoding::HEXLOWER.encode(&[7u8; 32]));
        std::fs::write(
            home.join("aliases.json"),
            format!(r#"{{"{}":"Ada","ab":"short"}}"#, actor.as_str()),
        )
        .expect("aliases");
        std::fs::write(broken.join("aliases.json"), b"{ not json").expect("broken");

        let entry = |home: &std::path::Path| crate::orbits::Entry {
            space: space.as_str().to_owned(),
            name: "Mig".into(),
            path: home.display().to_string(),
            origin: crate::orbits::Origin::Founded,
            host_nick: String::new(),
            last_opened: 0,
        };
        let router = Router::new(
            crate::orbits::Catalog::with_entries(
                crate::config::canonical(&identity),
                crate::config::canonical(&agents),
                false,
                vec![entry(&home), entry(&broken)],
            ),
            crate::world::packages(),
        );
        let service = AddressBookService::open(&identity).expect("open");

        let first = service.migrate(&router);
        let Response::Book(view) = first else {
            panic!("migrate answers the migration view");
        };
        assert_eq!(view.migration.imported, 1, "the full actor id imported");
        assert_eq!(view.migration.pending, 1, "the prefix stayed pending");
        assert!(
            !view.migration.complete,
            "a pending selector means incomplete"
        );

        let book = service.engine.lock().expect("lock").book().expect("book");
        let handle = Handle::Actor {
            space: space.clone(),
            actor: actor.clone(),
        };
        let cards = book.authored_cards_for(&handle);
        assert_eq!(cards.len(), 1, "one Card carries the imported handle");

        // A second pass changes nothing: no duplicate Card, no duplicate
        // pending row, and the corrupt file is still there, untouched.
        let second = service.migrate(&router);
        let Response::Book(view) = second else {
            panic!("migrate answers the migration view");
        };
        assert_eq!(view.migration.imported, 1, "no re-import");
        assert_eq!(view.migration.pending, 1, "no duplicate pending row");
        let book = service.engine.lock().expect("lock").book().expect("book");
        assert_eq!(book.authored_cards_for(&handle).len(), 1);
        assert!(
            broken.join("aliases.json").exists(),
            "a corrupt aliases.json is left alone"
        );
        assert!(
            home.join("aliases.json").exists(),
            "a file with pending selectors keeps its source until they resolve"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Migration is a move, not a copy: a file whose every selector became a
    /// Card is retired on the spot, and a later pass neither misses it nor
    /// resurrects the old design from it.
    #[test]
    fn a_fully_imported_aliases_file_is_retired() {
        let base = std::env::temp_dir().join(format!(
            "lait-book-retire-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let identity = base.join("identity");
        let agents = base.join("agents");
        let home = base.join("home");
        for dir in [&identity, &agents, &home] {
            std::fs::create_dir_all(dir).expect("temp");
        }
        crate::config::load_or_create_identity(&identity).expect("seed");

        let space = mechanics::ids::SpaceId::from_digest([8; 16]);
        let actor = ActorId::from_incept_hash(&data_encoding::HEXLOWER.encode(&[8u8; 32]));
        let path = home.join("aliases.json");
        std::fs::write(&path, format!(r#"{{"{}":"Ada"}}"#, actor.as_str())).expect("aliases");

        let router = Router::new(
            crate::orbits::Catalog::with_entries(
                crate::config::canonical(&identity),
                crate::config::canonical(&agents),
                false,
                vec![crate::orbits::Entry {
                    space: space.as_str().to_owned(),
                    name: "Retire".into(),
                    path: home.display().to_string(),
                    origin: crate::orbits::Origin::Founded,
                    host_nick: String::new(),
                    last_opened: 0,
                }],
            ),
            crate::world::packages(),
        );
        let service = AddressBookService::open(&identity).expect("open");

        let Response::Book(view) = service.migrate(&router) else {
            panic!("migrate answers the migration view");
        };
        assert_eq!(view.migration.imported, 1);
        assert_eq!(view.migration.pending, 0);
        assert!(!path.exists(), "a fully-imported file is retired");

        // The Card is durable independently of the file that seeded it.
        let handle = Handle::Actor { space, actor };
        let book = service.engine.lock().expect("lock").book().expect("book");
        assert_eq!(book.authored_cards_for(&handle).len(), 1);

        let Response::Book(view) = service.migrate(&router) else {
            panic!("migrate answers the migration view");
        };
        assert_eq!(
            view.migration.imported, 1,
            "a second pass re-imports nothing"
        );

        // The file's existence outranks the durable record. The
        // pre-retirement design closed a `finished` record and then kept
        // writing to the file (agent provisioning did), hiding those
        // selectors from the book for good — so a reappeared file gets one
        // more idempotent pass and is retired again.
        let late = ActorId::from_incept_hash(&data_encoding::HEXLOWER.encode(&[13u8; 32]));
        std::fs::write(&path, format!(r#"{{"{}":"Claude"}}"#, late.as_str())).expect("aliases");
        let Response::Book(view) = service.migrate(&router) else {
            panic!("migrate answers the migration view");
        };
        assert_eq!(view.migration.imported, 2, "the late selector imports");
        assert!(!path.exists(), "the reappeared file is retired again");
        let late_handle = Handle::Actor {
            space: mechanics::ids::SpaceId::from_digest([8; 16]),
            actor: late,
        };
        let book = service.engine.lock().expect("lock").book().expect("book");
        assert_eq!(book.authored_cards_for(&late_handle).len(), 1);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The daemon decorates Space replies from the book: a member row whose
    /// actor a Card names gains that name, a presence row gains it from a
    /// device handle, and rows no Card names stay bare.
    #[test]
    fn members_and_presence_are_decorated_from_the_book() {
        let dir = std::env::temp_dir().join(format!(
            "lait-book-deco-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp");
        crate::config::load_or_create_identity(&dir).expect("seed");
        let service = AddressBookService::open(&dir).expect("open");

        let space = mechanics::ids::SpaceId::from_digest([9; 16]);
        let actor = ActorId::from_incept_hash(&data_encoding::HEXLOWER.encode(&[9u8; 32]));
        let device = mechanics::actor::device_from_seed(&[11u8; 32]);
        assert!(service.upsert_named(
            "Ada",
            Handle::Actor {
                space: space.clone(),
                actor: actor.clone(),
            },
        ));
        assert!(service.upsert_named("Basalt", Handle::Device(device.clone())));

        let bare = crate::dto::MemberDto {
            key: "act_0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            role: "member".into(),
            did: None,
            me: false,
            sponsor: None,
            alias: String::new(),
        };
        let named = crate::dto::MemberDto {
            key: actor.as_str().to_owned(),
            ..bare.clone()
        };
        let mut response = Response::Members {
            members: vec![named, bare],
        };
        service.decorate(&space, &mut response);
        let Response::Members { members } = &response else {
            panic!("still a members reply");
        };
        assert_eq!(members[0].alias, "Ada", "the book names the actor");
        assert_eq!(members[1].alias, "", "an unnamed actor stays bare");

        let mut presence = Response::Who {
            peers: vec![crate::control::PresenceEntry {
                id: device.to_string(),
                nick: String::new(),
                actor: None,
                state: "online".into(),
                online: true,
                last_seen_secs: 0,
                dialable: true,
                blocked_by: None,
                pending: false,
                due_in_secs: 0,
                route_lease_secs: 0,
                failures: 0,
            }],
        };
        service.decorate(&space, &mut presence);
        let Response::Who { peers } = &presence else {
            panic!("still a presence reply");
        };
        assert_eq!(peers[0].nick, "Basalt", "the book names the device");

        // A pin carries no name of its own; the book names the seed row too.
        let mut seeds = Response::Seeds {
            seeds: vec![crate::dto::SeedDto {
                id: device.to_string(),
                nick: String::new(),
                space: space.as_str().to_owned(),
                state: "offline".into(),
                online: false,
            }],
        };
        service.decorate(&space, &mut seeds);
        let Response::Seeds { seeds } = &seeds else {
            panic!("still a seeds reply");
        };
        assert_eq!(seeds[0].nick, "Basalt", "the book names the pinned seed");

        // The provision seam authors a Card the roster decoration then reads
        // — and a known coding agent receives the canonical face the product
        // ships, so applications resolve it through the book, never by
        // matching names themselves.
        let agent = ActorId::from_incept_hash(&data_encoding::HEXLOWER.encode(&[14u8; 32]));
        service.name_agent(&space, agent.as_str(), "claude");
        let book = service.engine.lock().expect("lock").book().expect("book");
        let claude = book
            .authored_cards_for(&Handle::Actor {
                space: space.clone(),
                actor: agent,
            })
            .into_iter()
            .next()
            .expect("the agent card exists");
        assert!(
            book.cards[&claude]
                .picture
                .value
                .starts_with("image/png;base64,"),
            "a known agent's card carries the shipped canonical face"
        );
        assert!(
            book.cards[&claude]
                .groups
                .iter()
                .any(|link| link.name == AGENT_GROUP),
            "provisioning files the agent's card under the agent group"
        );
        drop(book);

        let scout = ActorId::from_incept_hash(&data_encoding::HEXLOWER.encode(&[12u8; 32]));
        service.name_agent(&space, scout.as_str(), "scout");
        let mut roster = Response::Members {
            members: vec![crate::dto::MemberDto {
                key: scout.as_str().to_owned(),
                role: "member".into(),
                did: None,
                me: false,
                sponsor: Some(actor.as_str().to_owned()),
                alias: String::new(),
            }],
        };
        service.decorate(&space, &mut roster);
        let Response::Members { members } = &roster else {
            panic!("still a members reply");
        };
        assert_eq!(members[0].alias, "scout");

        // The inverse decoration heals a book written before the group
        // existed: Ada's card predates the stamp (a migration import, not a
        // provision), and a roster naming her a sponsored member files her
        // card under the agent group. A second pass adds nothing — the stamp
        // is idempotent.
        let mut sponsored = Response::Members {
            members: vec![crate::dto::MemberDto {
                key: actor.as_str().to_owned(),
                role: "member".into(),
                did: None,
                me: false,
                sponsor: Some(scout.as_str().to_owned()),
                alias: String::new(),
            }],
        };
        service.decorate(&space, &mut sponsored);
        service.decorate(&space, &mut sponsored);
        let book = service.engine.lock().expect("lock").book().expect("book");
        let ada = book
            .authored_cards_for(&Handle::Actor {
                space: space.clone(),
                actor: actor.clone(),
            })
            .into_iter()
            .next()
            .expect("Ada's card exists");
        let filings: Vec<_> = book.cards[&ada]
            .groups
            .iter()
            .filter(|link| link.name == AGENT_GROUP)
            .collect();
        assert_eq!(
            filings.len(),
            1,
            "a sponsored member's card is filed under the agent group exactly once"
        );
        drop(book);

        // An unsponsored member files nothing: Basalt's device card holds no
        // actor handle and the bare roster row above carried no sponsor, so
        // the only agent-group filings in the whole book are the three above.
        let book = service.engine.lock().expect("lock").book().expect("book");
        let filed = book
            .cards
            .values()
            .filter(|card| card.groups.iter().any(|link| link.name == AGENT_GROUP))
            .count();
        assert_eq!(filed, 3, "claude, scout and Ada; nobody else");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
