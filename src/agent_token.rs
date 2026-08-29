//! The credential an agent presents to reach this device over HTTP.
//!
//! # Why an agent needs one at all
//!
//! An agent surface reached over stdio needs no credential: the editor spawned
//! the process, the process inherited the environment, and the operating system
//! decided who could do that. A long-lived endpoint has none of those and has
//! to be told who is calling.
//!
//! # Derived, never stored
//!
//! This is not a secret minted beside an agent and filed next to it. It is
//! derived from the agent's own seed, which means three things fall out rather
//! than needing to be built:
//!
//! - **Revoking the agent revokes the token.** Removing an agent removes its
//!   seed, and a token that cannot be re-derived cannot be verified. There is
//!   no second list to keep in step, and no window where a revoked agent still
//!   holds a live credential because something forgot to sweep.
//! - **There is nothing extra to leak.** A device already holding the seed can
//!   act as the agent outright; the token grants nothing it did not.
//! - **It is stable.** The same agent presents the same token across restarts,
//!   which is the whole point — a binding written once keeps working, and
//!   nobody is sent to re-copy a string because a daemon restarted.
//!
//! # What it is not
//!
//! It is not authority. It says *which agent is calling* and nothing else:
//! what that agent may do is its standing in the Space, which the address book
//! and its sponsor decide and which this cannot widen. An agent presenting a
//! valid token and holding no membership is exactly as unable to act as one
//! presenting nothing — it simply gets told which agent it is while being
//! refused.
//!
//! # Reach
//!
//! Loopback today, and the reason is written down here rather than assumed at
//! the callsite: a bearer credential on a loopback socket is guarded by the
//! operating system's process boundary, and the same credential on a network
//! socket is guarded by nothing until there is transport security under it.
//! [`Reach`] exists so that widening it is a decision somebody makes in one
//! place, against this paragraph, rather than a bind address quietly changing.

use std::path::Path;

use anyhow::{Context, Result};

/// BLAKE3's key-derivation context.
///
/// `derive_key` rather than hashing a prefix and the seed together: BLAKE3's
/// KDF mode sets a different flag word in the compression function, so a
/// derived key cannot collide with a plain or keyed hash of *anything*.
/// `H(prefix ‖ seed)` is safe only while no other BLAKE3 callsite in this tree
/// ever hashes a concatenation that reproduces those bytes — an invariant
/// nobody can check, and one that grows more callsites over time.
const CONTEXT: &str = "lait 2026 agent-mcp-token";

/// The file recording how many times an agent's token has been rotated.
const EPOCH_FILE: &str = "token-epoch";

/// Where an agent's credential may be presented from.
///
/// Loopback is the only variant this build honours. It is an enum rather than
/// a bool because the second case is not "loopback plus", it is a different
/// threat model — a bearer token on a network socket needs transport security
/// and an answer for every device that can reach the port, and neither exists
/// yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// This machine only, over 127.0.0.1.
    Loopback,
}

impl Reach {
    /// What a listener should bind. One place, so widening reach is one edit
    /// with this module's reasoning next to it.
    pub fn bind_address(self) -> &'static str {
        match self {
            Reach::Loopback => "127.0.0.1",
        }
    }
}

/// The token for one co-located agent at one epoch.
///
/// The epoch is what makes this rotatable without destroying anything.
/// Without it the only revocation was deleting the agent — which takes its
/// ed25519 identity, its sponsorship and everything it has authored with it.
/// That is a far larger hammer than "this leaked, issue a new one", and a
/// remedy nobody will reach for is not a remedy. A token lands in an editor's
/// configuration file, which is exactly the artefact people commit and sync.
///
/// Still nothing to sweep: the epoch is derived *from*, not a list of issued
/// tokens to keep in step. Bumping it invalidates every token ever issued for
/// the agent, because none of them can be re-derived.
pub fn derive(seed: &[u8; 32], epoch: u32) -> String {
    let mut material = [0u8; 36];
    material[..32].copy_from_slice(seed);
    material[32..].copy_from_slice(&epoch.to_be_bytes());
    blake3::derive_key(CONTEXT, &material).iter().fold(
        String::with_capacity(64),
        |mut hex, byte| {
            use std::fmt::Write;
            let _ = write!(hex, "{byte:02x}");
            hex
        },
    )
}

/// This agent's current epoch. Absent, unreadable or malformed reads as 0 —
/// the epoch a never-rotated agent is at, which is the conservative answer:
/// a token that verifies is one the holder was issued, and a corrupted file
/// must not silently rotate somebody out.
pub fn epoch(home: &Path, name: &str) -> u32 {
    let Ok(path) = epoch_path(home, name) else {
        return 0;
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

/// Rotate this agent's token, returning the new epoch.
///
/// Every token issued for this agent stops verifying. The agent keeps its
/// identity, its sponsorship and its history.
pub fn rotate(home: &Path, name: &str) -> Result<u32> {
    let path = epoch_path(home, name)?;
    let next = epoch(home, name).saturating_add(1);
    if let Some(parent) = path.parent() {
        mechanics::secretfs::create_private_dir(parent)
            .with_context(|| format!("make agent '{name}' private"))?;
    }
    mechanics::secretfs::write_private(
        &path,
        next.to_string().as_bytes(),
        mechanics::secretfs::Create::Replace,
        mechanics::secretfs::Wrap::Portable,
    )
    .with_context(|| format!("record agent '{name}' token epoch"))?;
    Ok(next)
}

fn epoch_path(home: &Path, name: &str) -> Result<std::path::PathBuf> {
    plain_agent_name(name)?;
    Ok(crate::registry::agents_base(home)
        .join(name)
        .join(EPOCH_FILE))
}

/// The token for a provisioned agent by name, or `None` when no such agent is
/// provisioned under this home.
///
/// `None` is "there is no such agent", which is the same answer a wrong name
/// gets. A caller must not distinguish them for a presenter it has not
/// authenticated yet.
pub fn for_agent(home: &Path, name: &str) -> Option<String> {
    let path = seed_path(home, name).ok()?;
    let hex = std::fs::read_to_string(path).ok()?;
    let mut seed = [0u8; 32];
    let decoded = data_encoding::HEXLOWER.decode(hex.trim().as_bytes()).ok()?;
    if decoded.len() != seed.len() {
        return None;
    }
    seed.copy_from_slice(&decoded);
    Some(derive(&seed, epoch(home, name)))
}

/// Which provisioned agent, if any, a presented token belongs to.
///
/// Every provisioned agent is re-derived and compared in constant time, so a
/// caller learns which agent is calling without the token ever being stored to
/// compare against. The cost is linear in agents on a device, which is a
/// handful — this is not a directory of users.
pub fn identify(home: &Path, presented: &str) -> Option<String> {
    let presented = presented.trim();
    if presented.is_empty() {
        return None;
    }
    for name in provisioned(home) {
        if let Some(expected) = for_agent(home, &name) {
            if constant_time_eq(expected.as_bytes(), presented.as_bytes()) {
                return Some(name);
            }
        }
    }
    None
}

/// Every agent name provisioned under this home.
pub fn provisioned(home: &Path) -> Vec<String> {
    let base = crate::registry::agents_base(home);
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| seed_path(home, name).map(|p| p.is_file()).unwrap_or(false))
        .collect();
    names.sort();
    names
}

/// Windows normalises these away, so a file named for one is a file named for
/// something else. Checked case-insensitively and against the stem, because
/// `CON.txt` is `CON` too.
const RESERVED_ON_WINDOWS: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Whether a name may be joined into a path under this home, and used as an
/// identity.
///
/// **One validator, because there were two and they disagreed in both
/// directions.** The structural one — "is this exactly one path component" —
/// admitted `scribe.`, and Win32 strips trailing dots and spaces during
/// normalisation, so `agents\scribe.\secret.key` opens `agents\scribe\secret.key`.
/// The name is wire-supplied through `act_as`, so that was an
/// identity-selection bypass: ask to act as `scribe.` and load scribe's seed.
/// Meanwhile the character rule here rejected names the structural one let
/// through, so such an agent provisioned fine and could never authenticate —
/// which reads to a person as "my token is wrong".
///
/// Deliberately not narrowed to the grammar a *new* name should have. This
/// runs on every load of an already-provisioned agent, and an agent's seed is
/// its identity, its sponsorship and everything it has authored. Refusing to
/// load one because its name would not be chosen today destroys more than it
/// protects. What is refused is what is genuinely unsafe: anything that is not
/// one plain component, anything Windows renames on the way to the filesystem,
/// and anything outside ASCII, where two different names can normalise to one.
pub fn plain_agent_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("an agent name may not be empty");
    }
    // Structural first: one ordinary component, no separators, no `..`.
    let mut parts = std::path::Path::new(name).components();
    let single =
        matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none();
    if !single {
        anyhow::bail!("'{name}' is not a plain agent name");
    }
    if !name.is_ascii() {
        anyhow::bail!("'{name}' must be ASCII: two names that look different can normalise to one");
    }
    if name.chars().any(|ch| ch.is_ascii_control()) {
        anyhow::bail!("'{name}' may not carry control characters");
    }
    // What Win32 strips on the way to the filesystem. A name it renames is a
    // name that opens somebody else's file.
    if name.ends_with('.') || name.ends_with(' ') || name.starts_with(' ') {
        anyhow::bail!("'{name}' may not begin or end with a space, or end with '.'");
    }
    if name.starts_with('.') {
        anyhow::bail!("'{name}' may not begin with '.'");
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    if RESERVED_ON_WINDOWS.contains(&stem.as_str()) {
        anyhow::bail!("'{name}' is a name Windows reserves for a device");
    }
    Ok(())
}

fn seed_path(home: &Path, name: &str) -> Result<std::path::PathBuf> {
    // Re-proved here rather than trusted from the caller: this joins a name
    // into a path, and a name Windows renames would read somebody else's seed.
    plain_agent_name(name)?;
    Ok(crate::registry::agents_base(home)
        .join(name)
        .join("secret.key"))
}

/// Compare without leaking where two byte strings first differ.
///
/// Hand-rolled rather than pulled in: it is six lines, and the alternative is a
/// dependency whose whole job is these six lines.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Read the token a caller presented, from either header a client may use.
///
/// `Authorization: Bearer …` is what an MCP client sends. `X-Lait-Agent-Token`
/// exists for callers that cannot set `Authorization` and is checked second, so
/// the standard spelling wins when both are present.
pub fn presented<'a>(authorization: Option<&'a str>, fallback: Option<&'a str>) -> Option<&'a str> {
    if let Some(value) = authorization {
        if let Some(rest) = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
        {
            return Some(rest.trim());
        }
    }
    fallback.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provision(home: &Path, name: &str, seed: [u8; 32]) {
        let path = seed_path(home, name).expect("a plain name");
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the agent directory");
        std::fs::write(&path, data_encoding::HEXLOWER.encode(&seed)).expect("the seed");
    }

    #[test]
    fn a_token_is_stable_for_one_agent_and_differs_between_agents() {
        let home = tempfile::tempdir().expect("a home");
        provision(home.path(), "scribe", [7u8; 32]);
        provision(home.path(), "auditor", [9u8; 32]);

        let first = for_agent(home.path(), "scribe").expect("a token");
        assert_eq!(
            first,
            for_agent(home.path(), "scribe").expect("the same token"),
            "a binding written once keeps working across restarts"
        );
        assert_ne!(first, for_agent(home.path(), "auditor").expect("a token"));
    }

    /// The property the whole derivation exists for. There is no second list to
    /// sweep, so there is no window where a removed agent still holds a live
    /// credential.
    #[test]
    fn removing_an_agent_revokes_its_token_with_nothing_to_sweep() {
        let home = tempfile::tempdir().expect("a home");
        provision(home.path(), "scribe", [7u8; 32]);
        let token = for_agent(home.path(), "scribe").expect("a token");
        assert_eq!(identify(home.path(), &token).as_deref(), Some("scribe"));

        std::fs::remove_dir_all(crate::registry::agents_base(home.path()).join("scribe"))
            .expect("the sponsor removes the agent");
        assert!(
            identify(home.path(), &token).is_none(),
            "the token cannot be re-derived, so it cannot be verified"
        );
    }

    #[test]
    fn an_unknown_token_identifies_nobody() {
        let home = tempfile::tempdir().expect("a home");
        provision(home.path(), "scribe", [7u8; 32]);
        assert!(identify(home.path(), "not-a-token").is_none());
        assert!(identify(home.path(), "").is_none());
        assert!(identify(home.path(), "   ").is_none());
    }

    /// A name reaches this from a request. One carrying a separator would read
    /// a seed that is not the caller's to ask about.
    #[test]
    fn a_name_that_is_not_a_plain_segment_reads_no_seed() {
        let home = tempfile::tempdir().expect("a home");
        provision(home.path(), "scribe", [7u8; 32]);
        assert!(seed_path(home.path(), "../scribe").is_err());
        assert!(seed_path(home.path(), "a/b").is_err());
        assert!(seed_path(home.path(), "").is_err());
        assert!(for_agent(home.path(), "../scribe").is_none());
    }

    /// The Windows bypass. Win32 strips trailing dots during normalisation, so
    /// `agents\scribe.\secret.key` opens `agents\scribe\secret.key` — and the
    /// name is wire-supplied through `act_as`. Asking to act as `scribe.` would
    /// have loaded scribe's seed and minted scribe's token.
    #[test]
    fn a_name_windows_would_rename_is_refused_everywhere() {
        let home = tempfile::tempdir().expect("a home");
        provision(home.path(), "scribe", [7u8; 32]);
        assert!(plain_agent_name("scribe.").is_err(), "trailing dot");
        assert!(plain_agent_name("scribe ").is_err(), "trailing space");
        assert!(plain_agent_name(" scribe").is_err(), "leading space");
        assert!(plain_agent_name("con").is_err(), "a reserved device name");
        assert!(plain_agent_name("CON.txt").is_err(), "reserved by its stem");
        assert!(
            for_agent(home.path(), "scribe.").is_none(),
            "and it reads no seed"
        );
    }

    /// Not narrowed to what a new name should look like. This runs on every
    /// load of an already-provisioned agent, and refusing one destroys its
    /// identity, its sponsorship and everything it authored.
    #[test]
    fn a_name_somebody_already_uses_still_loads() {
        for name in ["Claude", "my_agent", "agent-1", "agent.1", "a"] {
            assert!(plain_agent_name(name).is_ok(), "{name} must still load");
        }
        assert!(plain_agent_name("café").is_err(), "but not outside ASCII");
    }

    #[test]
    fn the_standard_header_wins_and_a_blank_one_is_not_a_token() {
        assert_eq!(presented(Some("Bearer abc"), None), Some("abc"));
        assert_eq!(presented(Some("Bearer abc"), Some("def")), Some("abc"));
        assert_eq!(presented(None, Some("def")), Some("def"));
        assert_eq!(presented(None, Some("   ")), None);
        assert_eq!(presented(Some("Basic abc"), None), None);
    }

    /// The remedy that was missing. A token lands in an editor's configuration
    /// file — the artefact people commit and sync — and before this the only
    /// revocation was deleting the agent, which takes its identity, its
    /// sponsorship and everything it authored with it. Nobody reaches for that.
    #[test]
    fn rotating_invalidates_every_issued_token_and_keeps_the_identity() {
        let home = tempfile::tempdir().expect("a home");
        provision(home.path(), "scribe", [7u8; 32]);
        let leaked = for_agent(home.path(), "scribe").expect("a token");
        assert_eq!(identify(home.path(), &leaked).as_deref(), Some("scribe"));

        assert_eq!(rotate(home.path(), "scribe").expect("rotated"), 1);
        assert!(
            identify(home.path(), &leaked).is_none(),
            "the token that leaked stops verifying"
        );
        let issued = for_agent(home.path(), "scribe").expect("a new token");
        assert_eq!(
            identify(home.path(), &issued).as_deref(),
            Some("scribe"),
            "and the agent is still the same agent, still sponsored"
        );
        assert_ne!(leaked, issued);
    }

    /// A corrupted or missing epoch reads as 0 rather than rotating somebody
    /// out. The conservative direction: a token that verifies is one its holder
    /// was issued.
    #[test]
    fn an_unreadable_epoch_is_the_one_a_never_rotated_agent_is_at() {
        let home = tempfile::tempdir().expect("a home");
        provision(home.path(), "scribe", [7u8; 32]);
        assert_eq!(epoch(home.path(), "scribe"), 0);
        std::fs::write(
            crate::registry::agents_base(home.path())
                .join("scribe")
                .join("token-epoch"),
            b"not a number",
        )
        .expect("a corrupted epoch");
        assert_eq!(epoch(home.path(), "scribe"), 0);
    }

    /// The derivation is domain separated, so this token can never be the same
    /// bytes as another use of the same seed.
    #[test]
    fn the_derivation_is_domain_separated_from_the_raw_seed() {
        let seed = [3u8; 32];
        let token = derive(&seed, 0);
        // Two inequalities are two assertions, not domain separation. What
        // gives the property is BLAKE3's KDF mode, which sets a different flag
        // word so a derived key cannot collide with a plain or keyed hash of
        // anything — including a future callsite in this tree that happens to
        // hash the same bytes. These stay as the two collisions somebody would
        // reach for first.
        assert_ne!(token, data_encoding::HEXLOWER.encode(&seed));
        assert_ne!(token, blake3::hash(&seed).to_hex().to_string());
        assert_ne!(token, derive(&seed, 1), "and an epoch is part of it");
        assert_eq!(token.len(), 64);
    }
}
