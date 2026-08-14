//! Identity-scoped address book on the daemon.
//!
//! The crate is a leaf; this module is the only thing that opens the store
//! under the selected identity directory and answers control-plane `Book*`
//! requests. It never calls [`crate::orbits::Router::place`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use addressbook::{Action, Author, Book, BookEngine, CardId, Coverage, Handle, Store};
use mechanics::ids::SystemUlidSource;
use mechanics::ids::{ActorId, DeviceId};
use serde::{Deserialize, Serialize};

use crate::control::{
    BookCardView, BookHitView, BookMigrationView, BookResolutionView, BookView, Request, Response,
};
use crate::orbits::{Catalog, Router, StationIdentity};

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
                // moment the identity asked for its book. It does not
                // retire aliases.json.
                let _ = self.migrate(router);
                self.list()
            }
            Request::BookGet { card } => self.get(&card),
            Request::BookPut { card, name, note } => self.put(card, name, note),
            Request::BookDelete { card } => self.delete(&card),
            Request::BookLink { card, handle } => self.link(&card, &handle),
            Request::BookUnlink { card, handle } => self.unlink(&card, &handle),
            Request::BookMerge { from, into } => self.merge(&from, &into),
            Request::BookClaimSelf { card } => self.claim_self(&card),
            Request::BookLookup { handle } => self.lookup(&handle),
            Request::BookResolve { orbit, handles } => self.resolve(router, &orbit, handles).await,
            Request::BookMigrateStatus => self.migrate_status(router.catalog()),
            Request::BookMigrate => self.migrate(router),
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
                let name = book
                    .cards
                    .get(&card)
                    .map(|card| card.name.value.clone())
                    .unwrap_or_default();
                hits.push(BookHitView {
                    card: card.to_string(),
                    handle: handle.to_wire(),
                    name,
                });
            }
        }
        Response::BookResolution(Box::new(BookResolutionView {
            hits,
            coverage: None,
        }))
    }

    fn migrate_status(&self, catalog: &Catalog) -> Response {
        Response::Book(Box::new(BookView {
            cards: Vec::new(),
            migration: self.migration_view(Some(catalog)),
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
            if progress.finished {
                continue;
            }
            if !path.exists() {
                progress.finished = true;
                continue;
            }
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
        Response::Book(Box::new(BookView {
            cards,
            migration: self.migration_view(None),
        }))
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
            | Request::BookDelete { .. }
            | Request::BookLink { .. }
            | Request::BookUnlink { .. }
            | Request::BookMerge { .. }
            | Request::BookClaimSelf { .. }
            | Request::BookLookup { .. }
            | Request::BookResolve { .. }
            | Request::BookMigrateStatus
            | Request::BookMigrate
    )
}

fn card_view(card: &addressbook::Card) -> BookCardView {
    BookCardView {
        card: card.id.to_string(),
        name: card.name.value.clone(),
        note: card.note.value.clone(),
        handles: card
            .handles
            .iter()
            .map(|link| link.handle.to_wire())
            .collect(),
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
            projects: vec![],
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

        let _ = std::fs::remove_dir_all(&base);
    }
}
