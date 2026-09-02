//! The refusal, with its words held to the truth.
//!
//! The viewer classifies refusals by `error_kind` and, failing that, by
//! regex over the message — so a refusal's words are load-bearing. Two
//! contracts, both pinned by tests:
//!
//! - **Never the wrong-mount refusal.** The native head's
//!   `"this head serves '…'"` + 404 + `not_found` triggers mount-refresh
//!   replays in `api.ts` and reroutes LivePlane editor writes; a browser
//!   refusal that imitated it would replay requests against a backend that
//!   answered honestly.
//! - **Never an outage costume.** The viewer's fallback regex reads
//!   `daemon`/`connect`/`offline` in a message with an unknown kind as a
//!   transient outage inviting "Reconnect" — permanently wrong for a
//!   by-design refusal. Hence the dedicated kind `not_hosted` (and a status
//!   the head never uses for routing, 501), so wording stops mattering.

use serde::{Deserialize, Serialize};

/// The `error_kind` every browser control refusal carries. The viewer grows
/// one typed case for it; an old viewer falls through to its fail-safe
/// unknown-kind handling.
pub const ERROR_KIND: &str = "not_hosted";

/// The HTTP-shaped status a browser refusal crosses with. 501, never 404:
/// the wrong-mount replay keys on 404 + `not_found`.
pub const STATUS: u16 = 501;

/// A control refusal as clone-safe data, in the viewer's refusal shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub status: u16,
    pub message: String,
    pub error_kind: String,
}

impl Refusal {
    /// Refuse a command that is the daemon's act by nature.
    pub fn daemon_only(cmd: &str) -> Self {
        Self {
            status: STATUS,
            message: format!(
                "'{cmd}' is an act of the device's own lait service, and this \
                 browser session serves the Space without one"
            ),
            error_kind: ERROR_KIND.to_string(),
        }
    }

    /// Refuse a reading this backend could take but does not yet. The words
    /// promise nothing.
    pub fn not_yet(cmd: &str) -> Self {
        Self {
            status: STATUS,
            message: format!("'{cmd}' is not answered in this browser session"),
            error_kind: ERROR_KIND.to_string(),
        }
    }

    /// Refuse a frame outside the vocabulary altogether.
    pub fn unclassified(cmd: &str) -> Self {
        Self {
            status: STATUS,
            message: format!("'{cmd}' is not a control request this backend understands"),
            error_kind: ERROR_KIND.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_refusal_wears_the_wrong_mount_costume_or_an_outage_one() {
        let refusals = [
            Refusal::daemon_only("member_remove"),
            Refusal::not_yet("status"),
            Refusal::unclassified("mystery"),
        ];
        for refusal in &refusals {
            assert!(
                !refusal.message.starts_with("this head serves '"),
                "{refusal:?} imitates the wrong-mount refusal the viewer replays on"
            );
            assert_ne!(refusal.status, 404);
            assert_eq!(refusal.error_kind, ERROR_KIND);
            // The viewer's outage regex: connect|daemon|network|fetch|offline.
            // `not_hosted` bypasses the regex entirely, but the words stay
            // clean anyway so an old viewer's fallback cannot read a refusal
            // as a reconnectable outage.
            for costume in ["connect", "daemon", "network", "fetch", "offline"] {
                assert!(
                    !refusal.message.contains(costume),
                    "{:?} contains {costume:?}, an outage word",
                    refusal.message
                );
            }
        }
    }
}
