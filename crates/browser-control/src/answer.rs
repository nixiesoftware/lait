//! The answerers for the `Answered` commands, over the pulled ledger.
//!
//! These are the browser mirror of the daemon's `hosting.rs` whoami/members,
//! reading the same facts from the same `mechanics::space::Authority` the
//! daemon reads — one evaluation, not a second policy model. What the daemon
//! adds and a browser cannot are exactly the fields left absent in
//! [`crate::reply`]: the local config nick, the address-book alias, the
//! host-plane sponsorship wait.
//!
//! They take `contact::authority::SharedLedgerAuthority` — the pulled ledger
//! plus the device seed and keyring — because the facts (role, capabilities,
//! partial view) live on the ledger's ACL state and the keyring, below the
//! `AuthorityView` seam the Session queries through. The Worker composition
//! root (slice 8) calls these; here they are unit-tested against a real
//! founder ledger.

use mechanics::actor::{device_from_seed, did_key_from_device};
use mechanics::membership as acl;

use crate::reply::{Member, Whoami};

type Ledger = contact::authority::SharedLedgerAuthority;

fn lock(ledger: &Ledger) -> std::sync::MutexGuard<'_, contact::authority::LedgerAuthority> {
    ledger
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// "Who am I, and is my view whole?" — from the pulled ledger and the keyring
/// this device holds. The local display `name` is absent (no daemon config).
#[allow(
    clippy::significant_drop_tightening,
    reason = "one consistent read of the ledger — the whoami facts must come \
              from a single locked view, not a guard reacquired per field"
)]
pub fn whoami(ledger: &Ledger) -> Whoami {
    let mut guard = lock(ledger);
    let seed = guard.seed;
    let device = device_from_seed(&seed);
    let did = did_key_from_device(&device);
    let space = Some(guard.space.as_str().to_string());

    // The loud partial-view signal: authorized epochs this device holds no
    // sealed key for. A real reading of the replicated ACL against the
    // keyring the pull populated — never an inference from counts.
    guard.refresh_keyring();
    let held: std::collections::BTreeSet<[u8; 16]> = guard.keyring.keys().copied().collect();
    let actor = guard.ledger.actor_plane().actor_of_device(&device).cloned();
    let acl_state = guard.ledger.acl_state().ok();
    let divergence = match &acl_state {
        Some(state) => state
            .epochs()
            .into_iter()
            .filter(|epoch| !held.contains(&epoch.id))
            .map(|epoch| {
                format!(
                    "authorized key epoch gen {} ({}) is not sealed to this device — \
                     content under it is invisible here; sync with a peer first",
                    epoch.gen,
                    data_encoding::HEXLOWER.encode(&epoch.id),
                )
            })
            .collect(),
        None => Vec::new(),
    };
    let partial_view = !divergence.is_empty();

    let (actor_str, role, member, can_write, capabilities, policy_admin, sponsor) =
        match (&actor, &acl_state) {
            (Some(actor), Some(state)) => {
                let mut names: Vec<String> = state
                    .effective_assignments(actor)
                    .into_iter()
                    .map(|(_, assignment)| assignment.capability.name)
                    .collect();
                names.sort();
                names.dedup();
                (
                    Some(actor.as_str().to_string()),
                    acl::role_label(&state.grants(actor)).to_string(),
                    state.is_member(actor),
                    state.can_write(actor),
                    names,
                    state.is_policy_admin(actor),
                    state.sponsor_of(actor).map(|s| s.as_str().to_string()),
                )
            }
            _ => (
                None,
                "none".to_string(),
                false,
                false,
                Vec::new(),
                false,
                None,
            ),
        };

    Whoami {
        actor: actor_str,
        device: device.as_str().to_string(),
        did,
        space,
        role,
        member,
        can_write,
        capabilities,
        policy_admin,
        sponsor,
        name: None,
        partial_view,
        divergence,
        sponsorship_asked: false,
        sponsorship_granted: false,
        wait_heads: Vec::new(),
    }
}

/// The membership roster — one row per member, from the replicated ACL. The
/// `alias` (the daemon's address-book decoration) stays empty; the viewer
/// falls back to the did.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the roster and the 'me' mark must read one consistent locked view"
)]
pub fn members(ledger: &Ledger) -> Vec<Member> {
    let mut guard = lock(ledger);
    let me = {
        let device = device_from_seed(&guard.seed);
        guard.ledger.actor_plane().actor_of_device(&device).cloned()
    };
    let plane = guard.ledger.actor_plane();
    let Ok(acl_state) = guard.ledger.acl_state() else {
        return Vec::new();
    };
    acl_state
        .members()
        .into_iter()
        .map(|(actor, grants)| {
            let did = plane
                .devices_of(&actor)
                .into_iter()
                .min()
                .and_then(|device| did_key_from_device(&device));
            Member {
                key: actor.as_str().to_string(),
                role: acl::role_label(&grants).to_string(),
                did,
                me: me.as_ref() == Some(&actor),
                sponsor: acl_state.sponsor_of(&actor).map(|s| s.as_str().to_string()),
                alias: String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use contact::authority::{LedgerAuthority, SharedLedgerAuthority};
    use mechanics::actor::incept_single;
    use mechanics::ids::{ActorId, SpaceId, SystemUlidSource};
    use mechanics::space::{Authority, Effect, Genesis};

    use super::*;

    /// A one-member founder ledger on an in-memory medium, and the founder's
    /// seed — the smallest real ledger the answerers can read.
    fn founder_ledger() -> SharedLedgerAuthority {
        let seed = [9u8; 32];
        let space = SpaceId::mint(&SystemUlidSource);
        let (inception, actor) = incept_single(&seed, &space, [1u8; 16], [2u8; 16], None);
        let genesis = Genesis {
            space_id: space.clone(),
            founding_actors: vec![actor],
            salt: [0u8; 16],
            recovery_root: [0u8; 32],
        };
        let mut ledger =
            Authority::create_on(Arc::new(journal::MemMedium::new()), genesis).expect("ledger");
        ledger
            .commit_batch(&[Effect::Actor(inception).encode()], &[])
            .expect("founder inception lands");
        SharedLedgerAuthority::new(LedgerAuthority::new(space, ledger, seed))
    }

    #[test]
    fn whoami_reads_the_founder_as_a_whole_member() {
        let ledger = founder_ledger();
        let who = whoami(&ledger);
        assert!(who.member, "the founder is a member");
        assert!(who.actor.is_some(), "the device resolves to an actor");
        assert!(who.did.is_some());
        assert!(
            !who.partial_view,
            "a founder holds its own epoch keys: {:?}",
            who.divergence
        );
        // The daemon-only local name is absent, not fabricated.
        assert_eq!(who.name, None);
    }

    #[test]
    fn members_lists_the_founder_once_marked_me() {
        let ledger = founder_ledger();
        let roster = members(&ledger);
        assert_eq!(roster.len(), 1, "one founder");
        assert!(roster[0].me, "this device speaks for the founder");
        assert_eq!(roster[0].alias, "", "no address book browser-side");
        assert_ne!(roster[0].key, String::new());
    }

    #[test]
    fn an_unknown_device_is_no_member() {
        // A ledger whose seed maps to no admitted actor: honest "not a member",
        // never a fabricated standing.
        let space = SpaceId::mint(&SystemUlidSource);
        let (inception, actor) = incept_single(&[1u8; 32], &space, [3u8; 16], [4u8; 16], None);
        let genesis = Genesis {
            space_id: space.clone(),
            founding_actors: vec![actor],
            salt: [0u8; 16],
            recovery_root: [0u8; 32],
        };
        let mut ledger =
            Authority::create_on(Arc::new(journal::MemMedium::new()), genesis).unwrap();
        ledger
            .commit_batch(&[Effect::Actor(inception).encode()], &[])
            .unwrap();
        // A DIFFERENT seed — a stranger holding the same public ledger.
        let stranger = SharedLedgerAuthority::new(LedgerAuthority::new(space, ledger, [7u8; 32]));
        let who = whoami(&stranger);
        assert!(!who.member);
        assert_eq!(who.role, "none");
        let _ = ActorId::parse; // keep the import honest across refactors
    }
}
