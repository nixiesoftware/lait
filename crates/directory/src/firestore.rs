//! A [`Store`] over Firestore's REST API.
//!
//! # Why Firestore, and why REST rather than an SDK
//!
//! The operation that decides the backing store is [`Store::claim`]: two
//! replicas minting concurrently must not both win. Firestore gives that
//! without a transaction, because **creating a document with a chosen id fails
//! if the id is taken** — so the address *is* the document id, and
//! create-if-absent is the atomic mint.
//!
//! REST over `ureq` rather than a cloud SDK for two reasons that are both about
//! what a dependency costs here. This workspace already refuses `reqwest`,
//! `native-tls` and OpenSSL and states why at the one outbound client it has;
//! adding a cloud SDK would bring all three back transitively. And credentials
//! come from the instance metadata server, so there is no key file to hold, no
//! rotation to run, and nothing this service could leak if it were compromised
//! — which is the same posture as holding no signing key.
//!
//! # What is not atomic, said plainly
//!
//! Rate windows are read-modify-write and therefore racy under concurrency: two
//! resolutions arriving together can both read the same count. That is
//! tolerable and it is tolerable *for a stated reason* — the rate limit is
//! AUTH-16's second layer, and sparseness is the first. A rate limit that
//! occasionally allows one extra resolution has not changed what an attacker can
//! do, because an attacker mints fresh device keys rather than exhausting one.
//!
//! Everything that must be exact — the claim, and spending a challenge — is.

use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use mechanics::{ids::DeviceId, kinship::ProfileId};
use serde_json::{json, Value};

use crate::{address::Address, store::Published, wire::Challenge, Store};

/// Where a service running on GCE asks for its own credentials.
const METADATA_TOKEN: &str = "http://metadata.google.internal/computeMetadata/v1/\
     instance/service-accounts/default/token";

/// Refresh a token this long before it actually expires, so a request never
/// races the boundary.
const TOKEN_MARGIN: Duration = Duration::from_secs(120);

/// How long any single call may take. A directory that hangs is a directory
/// that is down, and `Refusal::Unavailable` is a better answer than a stuck
/// request holding the service mutex.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

const ADDRESSES: &str = "addresses";
const REGISTRY_BINDINGS: &str = "registry-bindings";
const REGISTRY_ROUTES: &str = "registry-routes";
const PROFILES: &str = "profiles";
const CHALLENGES: &str = "challenges";
const RESOLVES: &str = "resolves";

/// A bearer token and when it stops being usable.
struct Token {
    value: String,
    good_until: Instant,
}

/// How this store gets a token.
///
/// An enum rather than a trait object because there are exactly two answers and
/// one of them is "a test told me". A trait here would be a seam nobody needs.
pub enum Credentials {
    /// Ask the instance metadata server. What a deployed service uses.
    Metadata,
    /// A fixed bearer token. For pointing at the Firestore emulator, or at a
    /// project with an explicitly minted token, without a metadata server.
    Fixed(String),
}

/// The directory's store, in Firestore.
pub struct FirestoreStore {
    base: String,
    /// The `projects/…/databases/…/documents` half of `base`.
    ///
    /// Firestore's commit API names documents by *resource path*, not by URL, so
    /// this is derived once at construction rather than reconstructed by string
    /// surgery at each call — which is how it was written first, and which
    /// happened to work against the hosted base and would have produced a
    /// malformed name against the emulator.
    resource: String,
    credentials: Credentials,
    token: Option<Token>,
    agent: ureq::Agent,
}

impl FirestoreStore {
    /// Open a store against one project's `(default)` database.
    #[must_use]
    pub fn open(project: &str, credentials: Credentials) -> Self {
        Self::at(
            &format!(
                "https://firestore.googleapis.com/v1/projects/{project}/databases/(default)/documents"
            ),
            credentials,
        )
    }

    /// Open a store against an explicit documents base — the emulator, or a
    /// named database.
    #[must_use]
    pub fn at(base: &str, credentials: Credentials) -> Self {
        let base = base.trim_end_matches('/').to_owned();
        let resource = base
            .split_once("/v1/")
            .map_or_else(|| base.clone(), |(_, path)| path.to_owned());
        Self {
            base,
            resource,
            credentials,
            token: None,
            agent: ureq::AgentBuilder::new().timeout(CALL_TIMEOUT).build(),
        }
    }

    /// A usable bearer token, fetched or remembered.
    fn token(&mut self) -> Result<String> {
        if let Credentials::Fixed(value) = &self.credentials {
            return Ok(value.clone());
        }
        if let Some(held) = &self.token {
            if Instant::now() < held.good_until {
                return Ok(held.value.clone());
            }
        }
        let answered: Value = self
            .agent
            .get(METADATA_TOKEN)
            .set("Metadata-Flavor", "Google")
            .call()
            .context("ask the metadata server for a token")?
            .into_json()
            .context("the metadata server answered something that is not a token")?;
        let value = answered["access_token"]
            .as_str()
            .ok_or_else(|| anyhow!("no access_token in the metadata answer"))?
            .to_owned();
        let lifetime = answered["expires_in"].as_u64().unwrap_or(600);
        self.token = Some(Token {
            value: value.clone(),
            good_until: Instant::now() + Duration::from_secs(lifetime).saturating_sub(TOKEN_MARGIN),
        });
        Ok(value)
    }

    fn get(&mut self, collection: &str, id: &str) -> Result<Option<Value>> {
        let token = self.token()?;
        let url = format!("{}/{collection}/{}", self.base, encode(id));
        match self
            .agent
            .get(&url)
            .set("Authorization", &format!("Bearer {token}"))
            .call()
        {
            Ok(response) => Ok(Some(response.into_json()?)),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(error) => Err(anyhow!("firestore get {collection}: {error}")),
        }
    }

    /// Create a document under a chosen id. `Ok(false)` when the id was taken.
    ///
    /// **The atomic mint.** Firestore refuses a create whose `documentId`
    /// already exists, and it refuses it consistently rather than
    /// last-write-wins, so two replicas racing produce exactly one winner.
    fn create(&mut self, collection: &str, id: &str, fields: Value) -> Result<bool> {
        let token = self.token()?;
        let url = format!("{}/{collection}?documentId={}", self.base, encode(id));
        match self
            .agent
            .post(&url)
            .set("Authorization", &format!("Bearer {token}"))
            .send_json(json!({ "fields": fields }))
        {
            Ok(_) => Ok(true),
            Err(ureq::Error::Status(409, _)) => Ok(false),
            Err(error) => Err(anyhow!("firestore create {collection}: {error}")),
        }
    }

    /// Write a document, creating or replacing it.
    fn put(&mut self, collection: &str, id: &str, fields: Value) -> Result<()> {
        let token = self.token()?;
        let url = format!("{}/{collection}/{}", self.base, encode(id));
        self.agent
            .request("PATCH", &url)
            .set("Authorization", &format!("Bearer {token}"))
            .send_json(json!({ "fields": fields }))
            .map_err(|error| anyhow!("firestore put {collection}: {error}"))?;
        Ok(())
    }

    /// Delete a document only if it is there. `Ok(true)` when this call is the
    /// one that removed it.
    ///
    /// The precondition is what makes spending a challenge single-use across
    /// replicas: an unconditional delete succeeds against a missing document, so
    /// two callers would both believe they had spent the same nonce.
    fn delete_if_present(&mut self, collection: &str, id: &str) -> Result<bool> {
        let token = self.token()?;
        let name = format!("{}/{collection}/{}", self.resource, encode(id));
        // `:commit` hangs off `documents`, not off the database — the API is
        // `POST /v1/{database=projects/*/databases/*}/documents:commit`. Getting
        // this wrong answers 404, which reads as "no such document" and is
        // really "no such method".
        let url = format!("{}:commit", self.base);
        match self
            .agent
            .post(&url)
            .set("Authorization", &format!("Bearer {token}"))
            .send_json(json!({
                "writes": [{
                    "delete": name,
                    "currentDocument": { "exists": true }
                }]
            })) {
            Ok(_) => Ok(true),
            // FAILED_PRECONDITION: somebody else got there first.
            Err(ureq::Error::Status(400, _)) => Ok(false),
            Err(error) => Err(anyhow!("firestore delete {collection}: {error}")),
        }
    }
}

/// Percent-encode a path segment. Addresses and hex ids are already safe, but a
/// document id reaches here from the wire and a `/` in one would silently
/// address a different collection.
fn encode(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32 & 0xFF)
            }
        })
        .collect()
}

fn string(value: &Value, field: &str) -> Option<String> {
    value["fields"][field]["stringValue"]
        .as_str()
        .map(ToOwned::to_owned)
}

fn integer(value: &Value, field: &str) -> Option<u64> {
    value["fields"][field]["integerValue"]
        .as_str()
        .and_then(|raw| raw.parse().ok())
}

impl crate::registry::RegistryStore for FirestoreStore {
    fn binding(
        &mut self,
        label: &crate::registry::Label,
    ) -> Result<Option<mechanics::kinship::ProfileId>> {
        let Some(document) = self.get(REGISTRY_BINDINGS, label.as_str())? else {
            return Ok(None);
        };
        Ok(string(&document, "profile")
            .and_then(|value| mechanics::kinship::ProfileId::parse(&value)))
    }

    fn bind(
        &mut self,
        label: &crate::registry::Label,
        profile: &mechanics::kinship::ProfileId,
    ) -> Result<bool> {
        // The same atomic mint the address claim rests on: Firestore refuses
        // a create whose id exists, consistently, so a binding never moves
        // through this path however many replicas race.
        self.create(
            REGISTRY_BINDINGS,
            label.as_str(),
            json!({ "profile": { "stringValue": profile.as_str() } }),
        )
    }

    fn route(
        &mut self,
        label: &crate::registry::Label,
    ) -> Result<Option<crate::registry::Resolved>> {
        let Some(document) = self.get(REGISTRY_ROUTES, label.as_str())? else {
            return Ok(None);
        };
        let (Some(profile), Some(endpoint), Some(epoch)) = (
            string(&document, "profile"),
            string(&document, "endpoint"),
            integer(&document, "epoch"),
        ) else {
            return Ok(None);
        };
        Ok(Some(crate::registry::Resolved {
            label: label.clone(),
            profile,
            endpoint,
            epoch,
        }))
    }

    fn record_route(&mut self, resolved: &crate::registry::Resolved) -> Result<bool> {
        // Read-compare-put, exactly as `record` above accepts it: the epoch
        // guard exists to refuse replay, and the residual cross-replica
        // window is the one the directory already carries for publications.
        if let Some(held) = self.get(REGISTRY_ROUTES, resolved.label.as_str())? {
            if let Some(existing) = integer(&held, "epoch") {
                if resolved.epoch <= existing {
                    return Ok(false);
                }
            }
        }
        self.put(
            REGISTRY_ROUTES,
            resolved.label.as_str(),
            json!({
                "profile": { "stringValue": resolved.profile },
                "endpoint": { "stringValue": resolved.endpoint },
                "epoch": { "integerValue": resolved.epoch.to_string() },
            }),
        )?;
        Ok(true)
    }
}

impl Store for FirestoreStore {
    fn claim(&mut self, address: &Address, profile: &ProfileId) -> Result<bool> {
        let taken = self.create(
            ADDRESSES,
            address.as_str(),
            json!({ "profile": { "stringValue": profile.as_str() } }),
        )?;
        if !taken {
            return Ok(false);
        }
        // The reverse mapping is written second and is not part of the atomic
        // step. If this fails the address is claimed and unreachable from its
        // profile, which the next publish repairs by minting again — a leaked
        // address out of 2^46 is not worth a transaction.
        self.put(
            PROFILES,
            profile.as_str(),
            json!({ "address": { "stringValue": address.as_str() } }),
        )?;
        Ok(true)
    }

    fn address_of(&mut self, profile: &ProfileId) -> Result<Option<Address>> {
        let Some(doc) = self.get(PROFILES, profile.as_str())? else {
            return Ok(None);
        };
        Ok(string(&doc, "address").and_then(|raw| Address::parse(&raw).ok()))
    }

    fn record(&mut self, profile: &ProfileId, published: &Published) -> Result<bool> {
        let held = self.get(PROFILES, profile.as_str())?;
        let address = held.as_ref().and_then(|doc| string(doc, "address"));
        if let Some(existing) = held.as_ref().and_then(|doc| integer(doc, "epoch")) {
            if published.epoch < existing {
                return Ok(false);
            }
        }
        let mut fields = json!({
            "announcement": { "bytesValue": base64(&published.announcement) },
            "epoch": { "integerValue": published.epoch.to_string() },
        });
        if let Some(address) = address {
            fields["address"] = json!({ "stringValue": address });
        }
        self.put(PROFILES, profile.as_str(), fields)?;
        Ok(true)
    }

    fn published(&mut self, address: &Address) -> Result<Option<Published>> {
        let Some(holder) = self.get(ADDRESSES, address.as_str())? else {
            return Ok(None);
        };
        let Some(profile) = string(&holder, "profile") else {
            return Ok(None);
        };
        let Some(doc) = self.get(PROFILES, &profile)? else {
            return Ok(None);
        };
        let Some(announcement) = doc["fields"]["announcement"]["bytesValue"].as_str() else {
            return Ok(None);
        };
        let announcement = data_encoding::BASE64
            .decode(announcement.as_bytes())
            .context("a stored announcement is not base64")?;
        Ok(Some(Published {
            announcement,
            epoch: integer(&doc, "epoch").unwrap_or(0),
        }))
    }

    fn open(&mut self, challenge: &Challenge) -> Result<()> {
        let nonce = data_encoding::HEXLOWER.encode(&challenge.nonce);
        self.create(
            CHALLENGES,
            &nonce,
            json!({
                "device": { "stringValue": challenge.device.as_str() },
                "issuedAt": { "integerValue": challenge.issued_at.to_string() },
                // Read by the Firestore TTL policy tofu installs. Garbage
                // collection only — `spend` checks the age itself, because TTL
                // deletion is best-effort within 24 hours and an expiry this
                // service depends on cannot be someone else's schedule.
                "expireAt": { "timestampValue": rfc3339(challenge.issued_at + crate::bounds::CHALLENGE_TTL) },
            }),
        )?;
        Ok(())
    }

    fn spend(&mut self, nonce: &[u8; 32], now: u64) -> Result<Option<Challenge>> {
        let id = data_encoding::HEXLOWER.encode(nonce);
        let Some(doc) = self.get(CHALLENGES, &id)? else {
            return Ok(None);
        };
        // Conditional delete first: whoever wins this is the one caller that
        // spent it. Reading before deleting is safe because the read alone
        // grants nothing.
        if !self.delete_if_present(CHALLENGES, &id)? {
            return Ok(None);
        }
        let device = string(&doc, "device").and_then(|raw| DeviceId::parse(&raw));
        let issued_at = integer(&doc, "issuedAt").unwrap_or(0);
        let Some(device) = device else {
            return Ok(None);
        };
        if now.saturating_sub(issued_at) > crate::bounds::CHALLENGE_TTL {
            return Ok(None);
        }
        Ok(Some(Challenge {
            device,
            nonce: *nonce,
            issued_at,
        }))
    }

    fn open_for(&mut self, _device: &DeviceId, _now: u64) -> Result<usize> {
        // Always zero, and that is a decision rather than a stub.
        //
        // Counting a device's open challenges means querying a collection by a
        // field, which is a second index and a per-request query on the free
        // path. The ceiling it feeds is a courtesy bound on a *free,
        // unauthenticated* operation whose whole cost is one document write that
        // Firestore's TTL policy then collects for nothing. Spending is what
        // must be exact, and it is.
        //
        // The consequence, stated so nobody has to infer it: against this store
        // a caller may hold more than `MAX_CHALLENGES_PER_DEVICE` open at once.
        // Nothing downstream depends on that number — it bounds a nuisance, not
        // an authority.
        Ok(0)
    }

    fn note_resolve(&mut self, asker: &DeviceId, now: u64) -> Result<usize> {
        let id = data_encoding::HEXLOWER.encode(asker.as_str().as_bytes());
        let held = self.get(RESOLVES, &id)?;
        let window_start = now.saturating_sub(crate::bounds::RATE_WINDOW);
        let mut recent: Vec<u64> = held
            .as_ref()
            .and_then(|doc| {
                doc["fields"]["at"]["arrayValue"]["values"]
                    .as_array()
                    .cloned()
            })
            .unwrap_or_default()
            .iter()
            .filter_map(|value| value["integerValue"].as_str()?.parse().ok())
            .filter(|at: &u64| *at > window_start)
            .collect();
        recent.push(now);
        // Bounded so one asker cannot grow a document without limit; the count
        // is what matters and anything past the ceiling is already refused.
        recent.truncate(crate::bounds::MAX_RESOLVES_PER_WINDOW + 1);
        let values: Vec<Value> = recent
            .iter()
            .map(|at| json!({ "integerValue": at.to_string() }))
            .collect();
        self.put(
            RESOLVES,
            &id,
            json!({ "at": { "arrayValue": { "values": values } } }),
        )?;
        Ok(recent.len())
    }

    fn sweep(&mut self, _now: u64) -> Result<usize> {
        // Firestore's TTL policy collects expired challenges, and the rate
        // documents are small and self-truncating. Nothing to walk, which is
        // half of why this store is cheaper to operate than a timer over a disk.
        Ok(0)
    }
}

/// Base64, as Firestore's `bytesValue` wants it.
fn base64(bytes: &[u8]) -> String {
    data_encoding::BASE64.encode(bytes)
}

/// A unix second as RFC 3339 UTC, which is the only timestamp shape Firestore
/// accepts.
fn rfc3339(unix: u64) -> String {
    // Days since epoch, then the civil-date algorithm — no chrono, for the same
    // reason there is no cloud SDK.
    let days = i64::try_from(unix / 86_400).unwrap_or(0);
    let seconds = unix % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

/// Howard Hinnant's `civil_from_days`, which is the standard answer and is
/// exact over the whole range this will ever see.
///
/// The casts are the algorithm's, and every one of them is safe over the input
/// this gets: `days` comes from a unix second divided by 86,400, so it is
/// positive and about five orders of magnitude below where any of these could
/// wrap. Allowed here with a reason rather than in the workspace table, because
/// the reason is about this function and not about the codebase.
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "a unix-second day count is positive and far below every boundary here"
)]
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, encode, rfc3339};

    /// The timestamp is what a TTL policy reads, so a wrong one would leave
    /// challenges uncollected — visible as a bill rather than as a bug.
    #[test]
    fn a_unix_second_renders_as_the_utc_instant_firestore_expects() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(civil_from_days(19_675), (2023, 11, 14));
    }

    /// A document id arrives from the wire. One with a slash in it would
    /// address a different collection entirely.
    #[test]
    fn a_path_separator_cannot_escape_a_document_id() {
        assert_eq!(encode("act-ion-zoo-4417"), "act-ion-zoo-4417");
        assert_eq!(encode("a/b"), "a%2Fb");
        assert_eq!(encode("../addresses"), "..%2Faddresses");
    }
}
