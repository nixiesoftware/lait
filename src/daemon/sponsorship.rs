//! Pending agent-sponsorship asks, identity-scoped.
//!
//! An unsponsored `LAIT_AGENT` attach is a decision for the person on this
//! machine, not a Space fact and not a World signal. The ask lives next to the
//! address book so every Orbit this identity serves can raise one without
//! opening a Station, and so a second consumer of World Signals never has to
//! exist.
//!
//! Raising is idempotent: the first `whoami` as a named agent that is not yet
//! a member files the ask; later ones leave the timestamp alone, so a client
//! that diffs the list does not re-interrupt somebody about the same wait.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use crate::control::{SponsorshipAsk, SponsorshipWake, WaitReply};
use crate::dto::WhoamiDto;

/// Durable pending asks for one identity.
pub(crate) struct SponsorshipAsks {
    path: PathBuf,
    lock: Mutex<()>,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct OnDisk {
    #[serde(default)]
    asks: Vec<SponsorshipAsk>,
    #[serde(default)]
    wakes: Vec<SponsorshipWake>,
}

struct Loaded {
    asks: Vec<SponsorshipAsk>,
    wakes: Vec<SponsorshipWake>,
}

impl SponsorshipAsks {
    pub(crate) fn open(identity_dir: &Path) -> Self {
        Self {
            path: identity_dir.join("sponsorship-asks.json"),
            lock: Mutex::new(()),
        }
    }

    pub(crate) fn list(&self) -> Vec<SponsorshipAsk> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.load().asks
    }

    /// File an ask for `(space, name)`. Returns whether this was the first time.
    pub(crate) fn raise(&self, space: &str, name: &str, actor: Option<&str>) -> bool {
        if !usable_name(name) || space.is_empty() {
            return false;
        }
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut disk = self.load();
        if let Some(existing) = disk
            .asks
            .iter_mut()
            .find(|ask| ask.space == space && ask.name == name)
        {
            if existing.actor.is_none() {
                if let Some(actor) = actor {
                    existing.actor = Some(actor.to_owned());
                    self.save(&disk);
                }
            }
            return false;
        }
        disk.asks.push(SponsorshipAsk {
            space: space.to_owned(),
            name: name.to_owned(),
            actor: actor.map(str::to_owned),
            asked_at_ms: now_ms(),
        });
        disk.asks.sort_by(|left, right| {
            left.asked_at_ms
                .cmp(&right.asked_at_ms)
                .then_with(|| left.space.cmp(&right.space))
                .then_with(|| left.name.cmp(&right.name))
        });
        self.save(&disk);
        true
    }

    /// Drop the ask for `(space, name)` if it is there. Returns whether one was.
    pub(crate) fn take(&self, space: &str, name: &str) -> bool {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut disk = self.load();
        let before = disk.asks.len();
        disk.asks
            .retain(|ask| !(ask.space == space && ask.name == name));
        if disk.asks.len() == before {
            return false;
        }
        self.save(&disk);
        true
    }

    /// Approve `(space, name)`: drop the ask and file a wake the agent can Watch.
    pub(crate) fn grant(&self, space: &str, name: &str, actor: Option<&str>) {
        if !usable_name(name) || space.is_empty() {
            return;
        }
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut disk = self.load();
        disk.asks
            .retain(|ask| !(ask.space == space && ask.name == name));
        if !disk
            .wakes
            .iter()
            .any(|wake| wake.space == space && wake.name == name)
        {
            disk.wakes.push(SponsorshipWake {
                space: space.to_owned(),
                name: name.to_owned(),
                actor: actor.map(str::to_owned),
                granted_at_ms: now_ms(),
            });
        }
        self.save(&disk);
    }

    fn load(&self) -> Loaded {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(_) => {
                return Loaded {
                    asks: Vec::new(),
                    wakes: Vec::new(),
                }
            }
        };
        match serde_json::from_slice::<OnDisk>(&bytes) {
            Ok(disk) => Loaded {
                asks: disk.asks,
                wakes: disk.wakes,
            },
            Err(_) => Loaded {
                asks: Vec::new(),
                wakes: Vec::new(),
            },
        }
    }

    fn save(&self, disk: &Loaded) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let body = match serde_json::to_vec_pretty(&OnDisk {
            asks: disk.asks.clone(),
            wakes: disk.wakes.clone(),
        }) {
            Ok(body) => body,
            Err(_) => return,
        };
        let _ = fs::write(&self.path, body);
    }
}

/// Apply a `whoami` as `act_as` to the pending-ask file and to the DTO itself.
///
/// A member is no longer waiting: a leftover ask is cleared, and a wake is
/// delivered once (`sponsorship_granted`) then consumed. An unsponsored named
/// agent files an ask and is given the heads to Watch.
pub(crate) fn note_whoami(
    asks: &SponsorshipAsks,
    space: &str,
    act_as: &str,
    whoami: &mut WhoamiDto,
) {
    if whoami.member {
        let _ = asks.take(space, act_as);
        if let Some(wake) = take_wake(asks, space, act_as) {
            whoami.sponsorship_granted = true;
            whoami.wait_heads = vec![granted_head(&wake)];
        } else {
            whoami.sponsorship_granted = false;
            whoami.wait_heads.clear();
        }
        whoami.sponsorship_asked = false;
        return;
    }
    if !usable_name(act_as) {
        return;
    }
    let _ = asks.raise(space, act_as, whoami.actor.as_deref());
    whoami.sponsorship_asked = true;
    whoami.sponsorship_granted = false;
    whoami.wait_heads = asks
        .list()
        .into_iter()
        .find(|ask| ask.space == space && ask.name == act_as)
        .map(|ask| vec![ask_head(&ask)])
        .unwrap_or_default();
}

/// Exec Watch against the host-plane sponsorship wait.
///
/// Known heads in; `Unchanged` if they still match. A grant moves the heads
/// to a `Granted` wake, which this reading consumes.
pub(crate) fn watch(
    asks: &SponsorshipAsks,
    space: &str,
    name: &str,
    known_heads: &[String],
) -> WaitReply {
    if !usable_name(name) || space.is_empty() {
        return WaitReply::Idle;
    }
    if let Some(wake) = peek_wake(asks, space, name) {
        let heads = vec![granted_head(&wake)];
        if !known_heads.is_empty() && known_heads == heads.as_slice() {
            return WaitReply::Unchanged { heads };
        }
        let _ = take_wake(asks, space, name);
        return WaitReply::Granted {
            heads,
            space: space.to_owned(),
            name: name.to_owned(),
            actor: wake.actor,
        };
    }
    let Some(ask) = asks
        .list()
        .into_iter()
        .find(|ask| ask.space == space && ask.name == name)
    else {
        return WaitReply::Idle;
    };
    let heads = vec![ask_head(&ask)];
    if !known_heads.is_empty() && known_heads == heads.as_slice() {
        return WaitReply::Unchanged { heads };
    }
    WaitReply::Waiting {
        heads,
        space: space.to_owned(),
        name: name.to_owned(),
    }
}

fn peek_wake(asks: &SponsorshipAsks, space: &str, name: &str) -> Option<SponsorshipWake> {
    let _guard = asks
        .lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    asks.load()
        .wakes
        .into_iter()
        .find(|wake| wake.space == space && wake.name == name)
}

fn take_wake(asks: &SponsorshipAsks, space: &str, name: &str) -> Option<SponsorshipWake> {
    let _guard = asks
        .lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut disk = asks.load();
    let index = disk
        .wakes
        .iter()
        .position(|wake| wake.space == space && wake.name == name)?;
    let wake = disk.wakes.remove(index);
    asks.save(&disk);
    Some(wake)
}

fn ask_head(ask: &SponsorshipAsk) -> String {
    format!("ask:{}", ask.asked_at_ms)
}

fn granted_head(wake: &SponsorshipWake) -> String {
    format!("ok:{}", wake.granted_at_ms)
}

/// File an ask when `whoami` as `act_as` never produced a DTO (the seed is
/// missing). Returns whether the name was usable, so the caller can rewrite
/// the denial whether this was the first ask or a retry.
pub(crate) fn note_denied(asks: &SponsorshipAsks, space: &str, act_as: &str) -> bool {
    if !usable_name(act_as) {
        return false;
    }
    let _ = asks.raise(space, act_as, None);
    true
}

fn usable_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lait-asks-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp");
        dir
    }

    fn empty_whoami(member: bool, actor: Option<&str>) -> WhoamiDto {
        WhoamiDto {
            actor: actor.map(str::to_owned),
            device: "0".repeat(64),
            did: None,
            space: Some("ws_one".into()),
            role: if member {
                "member".into()
            } else {
                "none".into()
            },
            member,
            can_write: member,
            capabilities: Vec::new(),
            policy_admin: false,
            sponsor: None,
            name: Some("grok".into()),
            partial_view: false,
            divergence: Vec::new(),
            sponsorship_asked: false,
            sponsorship_granted: false,
            wait_heads: Vec::new(),
        }
    }

    #[test]
    fn raising_the_same_ask_twice_does_not_change_it() {
        let dir = scratch("idempotent");
        let asks = SponsorshipAsks::open(&dir);
        assert!(asks.raise("ws_one", "grok", Some("act_one")));
        let first = asks.list();
        assert_eq!(first.len(), 1);
        assert!(!asks.raise("ws_one", "grok", Some("act_one")));
        let again = asks.list();
        assert_eq!(first, again, "a second whoami re-dated the same ask");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_shaped_name_is_not_an_ask() {
        let dir = scratch("path");
        let asks = SponsorshipAsks::open(&dir);
        assert!(!asks.raise("ws_one", "../escape", None));
        assert!(!asks.raise("ws_one", "", None));
        assert!(asks.list().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn taking_an_ask_forgets_it_and_taking_again_is_idle() {
        let dir = scratch("take");
        let asks = SponsorshipAsks::open(&dir);
        assert!(asks.raise("ws_one", "grok", None));
        assert!(asks.take("ws_one", "grok"));
        assert!(asks.list().is_empty());
        assert!(!asks.take("ws_one", "grok"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn whoami_as_an_unsponsored_agent_files_an_ask_and_says_so() {
        let dir = scratch("raise");
        let asks = SponsorshipAsks::open(&dir);
        let mut whoami = empty_whoami(false, None);
        note_whoami(&asks, "ws_one", "grok", &mut whoami);
        assert!(whoami.sponsorship_asked);
        assert_eq!(asks.list().len(), 1);
        assert_eq!(asks.list()[0].name, "grok");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn whoami_as_a_member_clears_a_stale_ask() {
        let dir = scratch("clear");
        let asks = SponsorshipAsks::open(&dir);
        assert!(asks.raise("ws_one", "grok", None));
        let mut whoami = empty_whoami(true, Some("act_one"));
        note_whoami(&asks, "ws_one", "grok", &mut whoami);
        assert!(!whoami.sponsorship_asked);
        assert!(asks.list().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_denied_whoami_still_files_an_ask() {
        let dir = scratch("denied");
        let asks = SponsorshipAsks::open(&dir);
        assert!(note_denied(&asks, "ws_one", "grok"));
        assert!(note_denied(&asks, "ws_one", "grok"));
        assert_eq!(asks.list().len(), 1);
        assert!(!note_denied(&asks, "ws_one", "../x"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn grant_moves_the_wait_heads_and_watch_consumes_them() {
        let dir = scratch("grant");
        let asks = SponsorshipAsks::open(&dir);
        assert!(asks.raise("ws_one", "grok", None));
        let first = watch(&asks, "ws_one", "grok", &[]);
        let WaitReply::Waiting { heads, .. } = first else {
            panic!("expected waiting, got {first:?}");
        };
        assert_eq!(
            watch(&asks, "ws_one", "grok", &heads),
            WaitReply::Unchanged {
                heads: heads.clone()
            }
        );
        asks.grant("ws_one", "grok", Some("act_one"));
        assert!(asks.list().is_empty());
        let granted = watch(&asks, "ws_one", "grok", &heads);
        assert!(matches!(granted, WaitReply::Granted { .. }), "{granted:?}");
        assert_eq!(watch(&asks, "ws_one", "grok", &[]), WaitReply::Idle);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn asks_survive_reopening_the_file() {
        let dir = scratch("persist");
        let first = SponsorshipAsks::open(&dir);
        assert!(first.raise("ws_one", "grok", Some("act_one")));
        let second = SponsorshipAsks::open(&dir);
        let listed = second.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].actor.as_deref(), Some("act_one"));
        let _ = fs::remove_dir_all(&dir);
    }
}
