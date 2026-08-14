//! The identity-scoped address book.
//!
//! Daemon route only. Listing never places an Orbit. Writes return the book
//! the daemon just persisted; a refusal is the daemon's words.

use lait::control::{BookView, ControlRoute, Request, Response};

use super::{Client, ClientError, ClientResult};

/// The identity's book, as the daemon last answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookSnapshot {
    pub cards: Vec<CardFacts>,
    pub migration_complete: bool,
    pub migration_pending: usize,
    pub migration_imported: usize,
    pub suggestions: Vec<SuggestionFacts>,
}

/// One staged card-exchange suggestion, awaiting review. Not in the book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestionFacts {
    pub suggestion: String,
    pub name: String,
    pub note: String,
    pub handles: Vec<String>,
}

/// One authored Card. No derived reachability, no online bit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardFacts {
    pub card: String,
    pub name: String,
    pub note: String,
    pub handles: Vec<String>,
    pub groups: Vec<String>,
    pub self_claim: bool,
}

impl Client {
    pub async fn book_list(&self) -> ClientResult<BookSnapshot> {
        self.book_request(Request::BookList).await
    }

    pub async fn book_put(
        &self,
        card: Option<String>,
        name: String,
        note: Option<String>,
    ) -> ClientResult<BookSnapshot> {
        self.book_request(Request::BookPut { card, name, note })
            .await
    }

    pub async fn book_delete(&self, card: String) -> ClientResult<BookSnapshot> {
        self.book_request(Request::BookDelete { card }).await
    }

    pub async fn book_merge(&self, from: String, into: String) -> ClientResult<BookSnapshot> {
        self.book_request(Request::BookMerge { from, into }).await
    }

    pub async fn book_claim_self(&self, card: String) -> ClientResult<BookSnapshot> {
        self.book_request(Request::BookClaimSelf { card }).await
    }

    pub async fn book_link(&self, card: String, handle: String) -> ClientResult<BookSnapshot> {
        self.book_request(Request::BookLink { card, handle }).await
    }

    pub async fn book_unlink(&self, card: String, handle: String) -> ClientResult<BookSnapshot> {
        self.book_request(Request::BookUnlink { card, handle })
            .await
    }

    /// Write a shareable bundle. Local-agent handles and My Card do not travel.
    pub async fn book_export(
        &self,
        path: String,
        cards: Option<Vec<String>>,
    ) -> ClientResult<BookSnapshot> {
        let book = self.book_list().await?;
        let mut shared = Vec::new();
        for card in &book.cards {
            if let Some(filter) = cards.as_ref() {
                if !filter.iter().any(|id| id == &card.card) {
                    continue;
                }
            }
            let handles = card
                .handles
                .iter()
                .filter(|raw| {
                    addressbook::Handle::parse_wire(raw)
                        .map(|handle| handle.may_leave_device())
                        .unwrap_or(false)
                })
                .cloned()
                .collect();
            shared.push(addressbook::SharedCard {
                name: card.name.clone(),
                note: card.note.clone(),
                handles,
            });
        }
        let bundle = addressbook::CardBundle::propose(shared)
            .map_err(|error| ClientError::refused(error.to_string()))?;
        let bytes = bundle
            .encode()
            .map_err(|error| ClientError::refused(error.to_string()))?;
        std::fs::write(&path, bytes).map_err(|error| {
            ClientError::internal(format!("write card bundle {}: {error}", path))
        })?;
        Ok(book)
    }

    /// Stage a shareable bundle as pending suggestions. Nothing enters the
    /// book until each suggestion is reviewed and accepted; the daemon
    /// preflights the bounds and refuses local-agent handles before staging.
    pub async fn book_propose(&self, path: String) -> ClientResult<BookSnapshot> {
        let bytes = std::fs::read(&path).map_err(|error| {
            ClientError::internal(format!("read card bundle {}: {error}", path))
        })?;
        let bundle = String::from_utf8(bytes)
            .map_err(|_| ClientError::refused("a card bundle is UTF-8 JSON".to_owned()))?;
        self.book_request(Request::BookPropose { bundle }).await
    }

    /// Accept one staged suggestion into the book.
    pub async fn book_accept(&self, suggestion: String) -> ClientResult<BookSnapshot> {
        self.book_request(Request::BookSuggestAccept { suggestion })
            .await
    }

    /// Discard one staged suggestion. The book is untouched.
    pub async fn book_dismiss(&self, suggestion: String) -> ClientResult<BookSnapshot> {
        self.book_request(Request::BookSuggestDismiss { suggestion })
            .await
    }

    async fn book_request(&self, request: Request) -> ClientResult<BookSnapshot> {
        let daemon = self.daemon()?;
        let reply = daemon
            .request(ControlRoute::Daemon, &request, None)
            .await
            .map_err(|error| ClientError::unreachable(format!("{error:#}")))?;
        match reply {
            Response::Book(view) => Ok(from_view(*view)),
            Response::Error { message, .. } => Err(ClientError::refused(message)),
            other => Err(ClientError::internal(format!(
                "unexpected address-book reply: {other:?}"
            ))),
        }
    }
}

fn from_view(view: BookView) -> BookSnapshot {
    BookSnapshot {
        cards: view
            .cards
            .into_iter()
            .map(|card| CardFacts {
                card: card.card,
                name: card.name,
                note: card.note,
                handles: card.handles,
                groups: card.groups,
                self_claim: card.self_claim,
            })
            .collect(),
        migration_complete: view.migration.complete,
        migration_pending: view.migration.pending,
        migration_imported: view.migration.imported,
        suggestions: view
            .suggestions
            .into_iter()
            .map(|s| SuggestionFacts {
                suggestion: s.suggestion,
                name: s.name,
                note: s.note,
                handles: s.handles,
            })
            .collect(),
    }
}
