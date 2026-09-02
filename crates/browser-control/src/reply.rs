//! The reply shapes a browser backend serializes — every one a fact it can
//! honestly state, none a reading it pretends to have taken.
//!
//! The spaces reply is the load-bearing one. The daemon's `SpaceRow` carries
//! `path`, `origin`, a probe `status` (up/idle/missing/unknown) and a probe
//! `unnamed` taxonomy — registry-on-disk and live-probe readings a browser
//! backend cannot take. Fabricating them is the "Unnamed Space" defect. So
//! this crate cannot express them: [`ServedSpaceRow`] is a `kind: "served"`
//! shape with the fields a one-Space browser backend actually holds, and the
//! viewer discriminates on `kind` to keep the daemon-only affordances
//! (StatusDot, Forget, Prune) off it.
//!
//! The identity and member replies mirror the daemon's `WhoamiDto`/`MemberDto`
//! field-for-field on the wire (the viewer decodes one type), but a browser
//! backend leaves the daemon-only fields at their honest absence: the local
//! `name`/`alias` (from the daemon's config and address book), and the
//! sponsorship-wait heads (a host-plane act).

use serde::{Deserialize, Serialize};

/// One row of a browser backend's spaces reply: the single Space this Worker
/// serves. No `path`, no `origin`, no probe `status`, no `unnamed` taxonomy —
/// "served" is the construction fact, and its absence of those fields is what
/// keeps the daemon-only affordances from rendering against it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedSpaceRow {
    /// The discriminant. Always `"served"`; the daemon's rows carry no `kind`
    /// (or an `"orbit"` one), so the viewer's union splits on it.
    pub kind: ServedKind,
    /// The local handle for RPC routing — for a browser backend, the mount it
    /// serves under (there is exactly one).
    pub id: String,
    /// The replicated Space id (`ws_…`).
    pub space: String,
    /// The Catalog name, read live from the composed Session's own World —
    /// the same reading a docked Station gives. `None` when the World is not
    /// docked yet; there is deliberately no remembered fallback and no probe
    /// taxonomy, because a browser has different absences than a daemon's four.
    #[serde(default)]
    pub name: Option<String>,
    /// Whose key this backend signs with. A browser composes with a
    /// `LocalIdentity` from its own seed, so this is what it constructed, not
    /// a probe: `own` for a person's device, `agent` never (a browser is not
    /// a sponsored co-located identity).
    pub identity: ServedIdentity,
}

/// The `kind` discriminant, serialized as the bare string `"served"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServedKind {
    Served,
}

/// Whose key a served row signs with — the browser mirror of the daemon's
/// `SpaceIdentity`, kept identical on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServedIdentity {
    Own,
    Agent { name: String },
}

/// A browser backend's spaces reply. One served row (a Worker serves one
/// Space), plus the mount fact the page has no other way to learn — the
/// local-World precedent (`local_issues`) is exactly why it must be stated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedSpacesReply {
    pub spaces: Vec<ServedSpaceRow>,
    /// The World mount this backend answers at.
    pub world: String,
}

/// The identity projection, mirroring the daemon's `WhoamiDto` wire shape.
/// The daemon-only fields — local `name`, and the sponsorship-wait heads —
/// stay at their honest absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Whoami {
    #[serde(default)]
    pub actor: Option<String>,
    pub device: String,
    #[serde(default)]
    pub did: Option<String>,
    #[serde(default)]
    pub space: Option<String>,
    pub role: String,
    pub member: bool,
    pub can_write: bool,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub policy_admin: bool,
    #[serde(default)]
    pub sponsor: Option<String>,
    /// Absent for a browser backend: the local display name is the daemon's
    /// config nick, which does not exist here.
    #[serde(default)]
    pub name: Option<String>,
    /// The loud partial-view signal — a real reading from the pulled ledger's
    /// keyring, not a fabrication.
    pub partial_view: bool,
    #[serde(default)]
    pub divergence: Vec<String>,
    #[serde(default)]
    pub sponsorship_asked: bool,
    #[serde(default)]
    pub sponsorship_granted: bool,
    #[serde(default)]
    pub wait_heads: Vec<String>,
}

/// One member row, mirroring the daemon's `MemberDto`. `alias` — the daemon's
/// address-book decoration — stays empty; the viewer falls back to the did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub key: String,
    pub role: String,
    #[serde(default)]
    pub did: Option<String>,
    pub me: bool,
    #[serde(default)]
    pub sponsor: Option<String>,
    #[serde(default)]
    pub alias: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_served_row_names_its_kind_and_omits_daemon_readings() {
        let row = ServedSpaceRow {
            kind: ServedKind::Served,
            id: "issues".into(),
            space: "ws_abc".into(),
            name: Some("Live".into()),
            identity: ServedIdentity::Own,
        };
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["kind"], "served");
        // The daemon-only readings are absent from the wire, not null.
        for absent in ["path", "origin", "status", "unnamed", "seen", "last_opened"] {
            assert!(
                json.get(absent).is_none(),
                "{absent} leaked onto a served row"
            );
        }
        assert_eq!(json["identity"]["kind"], "own");
    }
}
