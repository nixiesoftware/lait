//! A [`Store`] over Firestore, for the hosted deployment.
//!
//! `FsStore` is correct on a machine whose disk persists; Cloud Run's does
//! not, and an undelivered letter must survive an instance dying — that is
//! the whole durability promise a carrier makes. This store keeps every
//! deposit in the same Firestore project the co-deployed directory already
//! uses, over the same thin REST client and the same metadata credentials,
//! so the hosted Post gains no new secret and no new infrastructure.
//!
//! # Layout
//!
//! - `post-mailboxes/{device}/deposits/{id}` — one document per waiting
//!   envelope: the serialized [`Deposited`] and its expiry. Subcollections,
//!   so listing one mailbox never reads another's.
//! - `post-expiry/{device}~{id}` — a flat index of every deposit's expiry,
//!   because the sweep must find expired material *across* mailboxes and the
//!   thin client has no collection-group query. O(total deposits), which is
//!   the same cost the filesystem sweep pays walking its directories.
//! - `post-blocks/{recipient}~{sender}` — presence is the block. Idempotent
//!   by construction: writing a block twice writes the same document.
//!
//! A deposit is two writes (mailbox, index) that are not atomic. The order
//! makes the failure benign: the mailbox document is written first, so a
//! crash between the two leaves a deliverable letter whose index entry is
//! missing — it is delivered and acknowledged normally, and acknowledging
//! removes both halves. The reverse order would leave an index entry sweeping
//! a letter that never existed.
//!
//! The envelope stays opaque here exactly as it does on disk: one JSON string
//! the store never reads into. A maximal envelope is 256 KiB, hex-encoded in
//! that JSON to ~512 KiB, comfortably under Firestore's document ceiling.

use std::sync::Mutex;

use anyhow::{anyhow, Result};
use lait_directory::firestore::{integer, string, FirestoreStore};
use mechanics::ids::DeviceId;
use serde_json::json;

use crate::store::{deposit_id, Deposited, Store};
use crate::Envelope;

const MAILBOXES: &str = "post-mailboxes";
const EXPIRY: &str = "post-expiry";
const BLOCKS: &str = "post-blocks";

/// The Post's deposits, in Firestore.
///
/// Interior mutability because [`Store`] reads through `&self` while the REST
/// client refreshes its token through `&mut self`; the lock is uncontended in
/// practice — the service already serializes on the `Post` mutex above this.
pub struct FirestoreDeposits {
    store: Mutex<FirestoreStore>,
}

impl FirestoreDeposits {
    /// Open against one project's `(default)` database, with credentials from
    /// the instance metadata server — the deployed shape.
    #[must_use]
    pub fn open(project: &str, credentials: lait_directory::Credentials) -> Self {
        Self {
            store: Mutex::new(FirestoreStore::open(project, credentials)),
        }
    }

    /// Open against an explicit documents base — the emulator, or a named
    /// database.
    #[must_use]
    pub fn at(base: &str, credentials: lait_directory::Credentials) -> Self {
        Self {
            store: Mutex::new(FirestoreStore::at(base, credentials)),
        }
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, FirestoreStore> {
        match self.store.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn mailbox(device: &DeviceId) -> String {
        format!("{MAILBOXES}/{}/deposits", device.as_str())
    }

    fn index_id(device: &DeviceId, id: &str) -> String {
        format!("{}~{id}", device.as_str())
    }
}

impl Store for FirestoreDeposits {
    fn put(&mut self, sender: &DeviceId, envelope: &Envelope, now: u64) -> Result<String> {
        let id = deposit_id(sender, envelope);
        let held = Deposited {
            id: id.clone(),
            sender: sender.clone(),
            envelope: envelope.clone(),
            deposited_at: now,
        };
        let record = serde_json::to_string(&held)?;
        let mut store = self.locked();
        // Mailbox first, index second — see the module doc for why this
        // order makes a crash between the two benign.
        store.put(
            &Self::mailbox(&envelope.recipient),
            &id,
            json!({
                "record": { "stringValue": record },
                "expires_at": { "integerValue": envelope.expires_at.to_string() },
            }),
        )?;
        store.put(
            EXPIRY,
            &Self::index_id(&envelope.recipient, &id),
            json!({
                "device": { "stringValue": envelope.recipient.as_str() },
                "id": { "stringValue": id },
                "expires_at": { "integerValue": envelope.expires_at.to_string() },
            }),
        )?;
        Ok(id)
    }

    fn list(&self, device: &DeviceId, now: u64) -> Result<Vec<Deposited>> {
        let documents = self.locked().list(&Self::mailbox(device))?;
        let mut waiting = Vec::new();
        for (id, document) in documents {
            let Some(record) = string(&document, "record") else {
                return Err(anyhow!("deposit {id} has no record"));
            };
            let held: Deposited = serde_json::from_str(&record)
                .map_err(|error| anyhow!("deposit {id} does not decode: {error}"))?;
            if held.envelope.expires_at > now {
                waiting.push(held);
            }
        }
        waiting.sort_by(|a, b| (a.deposited_at, &a.id).cmp(&(b.deposited_at, &b.id)));
        Ok(waiting)
    }

    fn count(&self, device: &DeviceId) -> Result<usize> {
        Ok(self.locked().list(&Self::mailbox(device))?.len())
    }

    fn drop_all(&mut self, device: &DeviceId, ids: &[String]) -> Result<usize> {
        let mut store = self.locked();
        let mailbox = Self::mailbox(device);
        let mut gone = 0usize;
        for id in ids {
            if store.delete_if_present(&mailbox, id)? {
                gone = gone.saturating_add(1);
            }
            store.delete_if_present(EXPIRY, &Self::index_id(device, id))?;
        }
        Ok(gone)
    }

    fn sweep(&mut self, now: u64) -> Result<usize> {
        let mut store = self.locked();
        let index = store.list(EXPIRY)?;
        let mut collected = 0usize;
        for (entry, document) in index {
            let expires_at = integer(&document, "expires_at").unwrap_or(0);
            if expires_at > now {
                continue;
            }
            let (Some(device), Some(id)) = (string(&document, "device"), string(&document, "id"))
            else {
                // An index entry naming nothing sweeps itself and nothing else.
                store.delete_if_present(EXPIRY, &entry)?;
                continue;
            };
            let device = DeviceId::from_key_string(device);
            if store.delete_if_present(&Self::mailbox(&device), &id)? {
                collected = collected.saturating_add(1);
            }
            store.delete_if_present(EXPIRY, &entry)?;
        }
        Ok(collected)
    }

    fn set_block(&mut self, recipient: &DeviceId, sender: &DeviceId, blocked: bool) -> Result<()> {
        let id = format!("{}~{}", recipient.as_str(), sender.as_str());
        let mut store = self.locked();
        if blocked {
            store.put(BLOCKS, &id, json!({ "blocked": { "booleanValue": true } }))?;
        } else {
            store.delete_if_present(BLOCKS, &id)?;
        }
        Ok(())
    }

    fn is_blocked(&self, recipient: &DeviceId, sender: &DeviceId) -> Result<bool> {
        let id = format!("{}~{}", recipient.as_str(), sender.as_str());
        Ok(self.locked().get(BLOCKS, &id)?.is_some())
    }
}
