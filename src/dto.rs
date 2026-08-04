//! Space-substrate projections: the DTOs the navigation shell returns for
//! identity, membership, authority, and pinned remotes.
//!
//! These describe the **Space**, not any World inside it — a Space has one
//! membership list, one ACL, and one set of remotes however many Worlds it
//! hosts. They lived in the Issues product crate, which made the shell depend
//! on a product to describe its own substrate and inverted the layering the
//! World seam exists to enforce. Nothing in `products/` or `crates/` ever
//! referenced them; every consumer was the shell.
//!
//! Wire shapes are unchanged by the move: only the Rust path differs, so the
//! `--json` DTO and the viewer's hand-maintained mirror stay byte-identical.

use serde::{Deserialize, Serialize};

/// A space member projection. Roles come from the
/// signed ACL graph — the only cryptographically-verified identity in the system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberDto {
    /// The member's **actor id** (`act_…`), a self-certifying identity over a set
    /// of device keys rather than a raw key. Kept
    /// as `key` for wire compatibility across client projections.
    pub key: String,
    /// `admin` | `member` | `viewer` — the coarse label of this member's ACL
    /// grant set. A **sponsored** member (an agent) is not a separate role; it
    /// reads as `member`/`viewer` per its grants, and `sponsor` (below) is what
    /// marks it as sponsored. One surface: an agent is a member, rendered.
    pub role: String,
    /// The member's `did:key` — the self-certifying, offline interop form of one
    /// of its device keys (`z6Mk…`). `None` only if no device resolves for the
    /// actor. Deterministic: a pure function of the key, synced-safe (unlike the
    /// local `alias`).
    #[serde(default)]
    pub did: Option<String>,
    /// Whether this is us (this device speaks for the actor).
    pub me: bool,
    /// For an agent, the sponsoring actor; `None` for humans. The agent's
    /// standing dies with this sponsor.
    #[serde(default)]
    pub sponsor: Option<String>,
    /// Local petname you've assigned (empty if none). A private, never-synced
    /// label — the trusted half of the local-petname identity model.
    #[serde(default)]
    pub alias: String,
}

/// The one-shot "who am I, and am I whole?" projection (the MCP `whoami` tool,
/// the viewer's identity panel). Answers the three questions that cost us a full multi-node
/// session by inference — *which identity, what may it do, is its view
/// complete* — as a glance, never a deduction. The observability half of the
/// Agent Experience initiative (`docs/plans/09`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhoamiDto {
    /// This node's **actor id** (`act_…`), if this device resolves to one yet.
    /// `None` before admission (a fresh joiner whose inception hasn't landed).
    #[serde(default)]
    pub actor: Option<String>,
    /// This node's **device id** (the hex ed25519 key this daemon signs with).
    pub device: String,
    /// This device's `did:key` — the self-certifying, offline interop handle.
    #[serde(default)]
    pub did: Option<String>,
    /// The space id this node is bound to.
    #[serde(default)]
    pub space: Option<String>,
    /// The coarse ACL role: `admin` | `member` | `viewer`, or `none` if this
    /// actor holds no membership here yet.
    pub role: String,
    /// Whether this actor is a member of the space at all.
    pub member: bool,
    /// Whether this actor holds content-write standing (can author issues).
    pub can_write: bool,
    /// Effective scoped capability names (sorted, deduped).
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Whether this actor holds policy-admin (the meta-capability that gates
    /// inviting and policy management).
    pub policy_admin: bool,
    /// If this identity is **sponsored** (an agent), the sponsoring actor id;
    /// its standing dies with this sponsor. `None` for an ordinary human member.
    #[serde(default)]
    pub sponsor: Option<String>,
    /// The display name this node knows for itself (local alias; the synced
    /// display-name plane is the multi-member follow-on). `None` if unnamed.
    #[serde(default)]
    pub name: Option<String>,
    /// **Loud** partial-view signal: `true` when this node knows its view is
    /// incomplete — an authorized key epoch it cannot open, or missing history —
    /// so some content is invisible to it. A delegated agent must NOT author
    /// against a `true` here (it could "close" issues it cannot see). `sync`
    /// reports the same signal; the daemon *enforces* it at the authoring gate.
    pub partial_view: bool,
    /// Human-readable lines describing any partial-view divergence (empty when
    /// whole) — what epoch/history is missing, so it is never an inference.
    #[serde(default)]
    pub divergence: Vec<String>,
}

/// One rendered row of the membership audit log (`Request::MemberLog`): the signed
/// ACL DAG replayed in causal order with each operation's verdict.
/// This is **cryptographic provenance** (who was authorized to do what),
/// distinct from the advisory activity feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberLogEntry {
    /// The op's content-address (its DAG node id).
    pub op: String,
    /// The signing author's key (verified — the signature covers the op).
    pub actor: String,
    /// "add_member" | "remove_member" | "set_role" | "add_agent" | "unknown".
    pub kind: String,
    /// The subject key the op acts on (absent for an undecodable op).
    #[serde(default)]
    pub subject: Option<String>,
    /// "admin" | "member" for role-bearing ops.
    #[serde(default)]
    pub role: Option<String>,
    /// Whether replay honored the op (false = unauthorized or undecodable).
    pub authorized: bool,
}

/// A pinned seed ("remote") projection for `seed ls` / `remote ls`. A seed
/// is a bootstrap + backfill anchor, never a trust authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedDto {
    /// The seed's endpoint id (== its ed25519 key, 64-hex).
    pub id: String,
    /// Advisory nick (empty when pinned by bare id).
    pub nick: String,
    /// The space id the seed serves.
    pub space: String,
    /// "online" | "away" | "offline" from the live presence map.
    pub state: String,
    /// Whether the seed is currently reachable.
    pub online: bool,
}
