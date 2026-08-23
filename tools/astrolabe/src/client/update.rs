//! What this machine knows about its own updating, and the one decision that
//! follows from it (CLIENT-47).
//!
//! The client is evergreen: a person has no update choice, staging runs
//! continuously in the daemon, and applying happens at a moment no client is
//! alive — the stub's launch window on Windows and Linux, the daemon's own
//! act on macOS. So there is exactly one thing left that a person can be
//! asked, and this module computes it: *when to restart*.
//!
//! ## The prompt is a request, not a consent
//!
//! Nothing here asks whether to take an update. A staged release applies at
//! the next natural boundary without anybody's permission, and on a machine
//! that is used and closed like an ordinary application, that boundary
//! arrives on its own and no prompt is ever drawn. What this module exists
//! for is the machine that *never* reaches one — a session left running for
//! days — where the only way the release lands is if somebody restarts.
//!
//! The escalation is Chrome's, and the numbers are theirs because they have
//! the fleet-scale evidence: quiet for two days, insistent at four, urgent at
//! seven, counted from when the release was *staged* rather than when it was
//! published. A release that has been ready for an hour and one that has been
//! ready for a week are different requests.
//!
//! ## Every absence keeps its own name
//!
//! The one thing this must never do is turn a machine that could not ask into
//! a machine that is up to date. "The channel could not be reached", "the
//! bytes did not verify", and "the pointer went backwards" are three
//! different facts and only the last two mean somebody should look. They are
//! carried through as themselves, and a machine that has never completed a
//! check has no standing at all — which is a fourth thing, and not zero.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use lait::update::watch::Standing;

/// Chrome's thresholds, counted from staging. Theirs because the fleet-scale
/// evidence behind them is theirs; inventing different numbers would be
/// inventing different evidence.
const INSISTENT_AFTER: Duration = Duration::from_secs(4 * 24 * 60 * 60);
const URGENT_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How hard to ask. Ordered, so a surface can compare without matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Urgency {
    /// Staged less than two days ago. Mentioned, easy to ignore.
    Quiet,
    /// Four days. Drawn to be noticed.
    Insistent,
    /// A week. Drawn to be acted on.
    Urgent,
}

impl Urgency {
    /// The urgency of a release staged `waited` ago.
    pub fn after(waited: Duration) -> Self {
        if waited >= URGENT_AFTER {
            Self::Urgent
        } else if waited >= INSISTENT_AFTER {
            Self::Insistent
        } else {
            Self::Quiet
        }
    }
}

/// Why a restart is not being asked for, when it is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Held {
    /// Declared in-flight work would be lost across a restart.
    ///
    /// The work is drained rather than discarded, and the person is told it
    /// is being waited for rather than told nothing.
    WorkInFlight {
        /// What is holding it, in the words a surface should say.
        what: Vec<String>,
    },
}

/// What, if anything, to put in front of a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Nothing to say. The ordinary state of an evergreen client.
    Nothing,
    /// A release is staged and waiting for a restart this machine will not
    /// take on its own.
    RestartRequested {
        /// The version that becomes live on restart.
        version: String,
        /// How hard to ask, by how long it has waited.
        urgency: Urgency,
    },
    /// A staged release is ready and something is holding the restart.
    ///
    /// Distinct from a request, because the answer is not "restart when you
    /// like" — it is "this is why we have not, and it is being waited for".
    Waiting {
        /// The version that becomes live once the hold clears.
        version: String,
        /// Why.
        why: Held,
    },
    /// Something happened that a person should see, and it is not an update.
    ///
    /// A verification failure means the host is compromised or a publish used
    /// the wrong key; a stale pointer means somebody replayed an old one.
    /// Neither is "up to date" and neither is "could not ask".
    Attention {
        /// What happened, in the words the feed used.
        why: String,
    },
}

/// The whole decision, as a pure function of what is known.
///
/// `now`, `in_flight`, and `running_version` are arguments
/// rather than ambient reads so the policy is testable across every axis
/// without a clock or a running client — which is the point of it being
/// separate from the surface that draws it.
pub fn intent(
    standing: Option<&Standing>,
    now: u64,
    in_flight: &[String],
    running_version: Option<&str>,
) -> Intent {
    let Some(standing) = standing else {
        // No check has ever completed here. Not "up to date", not "could not
        // ask" — nothing is known, and saying anything would be inventing it.
        return Intent::Nothing;
    };
    match standing {
        // The channel answered and holds nothing newer, or holds something
        // that is not on this machine yet. Neither is a person's business:
        // staging is silent and continuous.
        Standing::Current { .. } | Standing::Available { .. } => Intent::Nothing,
        Standing::ReinstallRequired { version, floor } => Intent::Attention {
            why: format!(
                "Astrolabe {version} requires a canonical reinstall (compatibility floor {floor}). Spaces and issue data are preserved; Worlds must be installed again from their signed channels."
            ),
        },
        // The channel could not be asked. A network is not news.
        Standing::CouldNotAsk { .. } => Intent::Nothing,
        // These two are news. A signature that did not verify means the host
        // is compromised or a publish used the wrong key; a pointer older
        // than one already believed means a replay. Rendering either as
        // "no update" is the silence the attack buys.
        Standing::Refused { why } | Standing::Stale { why } => {
            Intent::Attention { why: why.clone() }
        }
        // The macOS daemon can exchange the bundle while this process is
        // alive, so its durable standing remains Staged until the new bundle
        // actually launches. Once that version is this process, the standing
        // has served its purpose; restarting again would loop on a successful
        // apply while waiting for the next daemon sample.
        Standing::Staged { version, .. } if running_version == Some(version.as_str()) => {
            Intent::Nothing
        }
        Standing::Staged { version, .. } => {
            let waited = standing.staged_for(now).unwrap_or_default();
            if !in_flight.is_empty() {
                return Intent::Waiting {
                    version: version.clone(),
                    why: Held::WorkInFlight {
                        what: in_flight.to_vec(),
                    },
                };
            }
            Intent::RestartRequested {
                version: version.clone(),
                urgency: Urgency::after(waited),
            }
        }
    }
}

// --- The stub seam ---------------------------------------------------------
//
// The install-root vocabulary below is mirrored in `astrolabe-stub` rather
// than shared through a dependency — the same discipline as the stage
// manifest — and the staged-swap chain test welds the halves by running the
// real pair against these spellings.

/// Where a relaunch request is written, relative to the install root.
pub const RELAUNCH_REQUEST: &str = "relaunch.requested";

static RUNNING_VERSION: OnceLock<String> = OnceLock::new();

/// Identify the version of the desktop bundle hosting this core.
///
/// Tauri owns the distribution version and can override it at build time, so
/// the core cannot infer it from its own Cargo package version. The host calls
/// this once before starting the runtime.
pub fn identify_running_version(version: String) {
    let _ = RUNNING_VERSION.set(version);
}

pub(crate) fn running_version() -> Option<&'static str> {
    RUNNING_VERSION.get().map(String::as_str)
}

/// The stub that owns this executable's relaunch, when there is one.
///
/// `None` — a developer's build or a macOS bundle — means this process's
/// own relaunch is the apply window.
pub fn managing_stub_of(executable: &Path) -> Option<PathBuf> {
    let live = executable.parent()?;
    if live.file_name()? != "current" {
        return None;
    }
    let stub = live.parent()?.join(if cfg!(windows) {
        "astrolabe.exe"
    } else {
        "astrolabe"
    });
    stub.is_file().then_some(stub)
}

/// Ask the managing stub for the apply window on behalf of `version`.
///
/// `false` — no managing stub, or the root refused the write — means the
/// caller's own relaunch is the best remaining move.
pub fn request_relaunch(version: &str) -> bool {
    let Some(stub) = std::env::current_exe()
        .ok()
        .and_then(|exe| managing_stub_of(&exe))
    else {
        return false;
    };
    let Some(root) = stub.parent() else {
        return false;
    };
    std::fs::write(root.join(RELAUNCH_REQUEST), version).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 60 * 60;

    /// Staged at a fixed moment; the tests move `now` rather than the stage,
    /// so each reads as "and then N days passed".
    const STAGED_AT: u64 = 1_000 * DAY;

    fn staged() -> Standing {
        Standing::Staged {
            version: "0.9.0".into(),
            at: STAGED_AT,
        }
    }

    fn now_after(days: u64) -> u64 {
        STAGED_AT + days * DAY
    }

    #[test]
    fn a_machine_that_has_never_checked_says_nothing_rather_than_up_to_date() {
        assert_eq!(intent(None, now_after(0), &[], None), Intent::Nothing);
    }

    #[test]
    fn staging_is_silent_and_so_is_a_channel_that_could_not_be_asked() {
        for standing in [
            Standing::Current {
                channel_version: "0.8.0".into(),
            },
            Standing::Available {
                version: "0.9.0".into(),
            },
            Standing::CouldNotAsk {
                why: "no route".into(),
            },
        ] {
            assert_eq!(
                intent(Some(&standing), now_after(0), &[], None),
                Intent::Nothing,
                "{standing:?} drew something a person had to read"
            );
        }
    }

    /// The two that are news, and the reason they are: neither is an update
    /// and neither may be folded into silence.
    #[test]
    fn a_verification_failure_and_a_replayed_pointer_both_ask_for_attention() {
        for standing in [
            Standing::Refused {
                why: "feed signature verification failed: bad key".into(),
            },
            Standing::Stale {
                why: "feed answered with a stale pointer: older than believed".into(),
            },
        ] {
            let Intent::Attention { why } = intent(Some(&standing), now_after(0), &[], None) else {
                panic!("{standing:?} was not surfaced");
            };
            assert!(!why.is_empty());
        }
    }

    /// The escalation, at each of Chrome's thresholds. Counted from staging,
    /// so a release ready for an hour and one ready for a week are different
    /// requests.
    #[test]
    fn the_request_gets_more_insistent_the_longer_a_release_has_waited() {
        let staged = staged();
        for (days, expected) in [
            (0, Urgency::Quiet),
            (1, Urgency::Quiet),
            (2, Urgency::Quiet),
            (4, Urgency::Insistent),
            (6, Urgency::Insistent),
            (7, Urgency::Urgent),
            (30, Urgency::Urgent),
        ] {
            let Intent::RestartRequested { urgency, version } =
                intent(Some(&staged), now_after(days), &[], None)
            else {
                panic!("a staged release did not ask for a restart at {days} days");
            };
            assert_eq!(urgency, expected, "at {days} days");
            assert_eq!(version, "0.9.0");
        }
        // Ordered, so a surface may compare rather than match.
        assert!(Urgency::Quiet < Urgency::Insistent && Urgency::Insistent < Urgency::Urgent);
        assert!(INSISTENT_AFTER < URGENT_AFTER);
    }

    /// Declared work holds the restart, and the answer says so rather than
    /// asking a person to restart into losing it.
    #[test]
    fn declared_work_in_flight_holds_the_request_and_names_what_is_holding_it() {
        let staged = staged();
        let Intent::Waiting { version, why } = intent(
            Some(&staged),
            now_after(9),
            &["an unsent comment".to_string()],
            None,
        ) else {
            panic!("in-flight work did not hold the restart");
        };
        assert_eq!(version, "0.9.0");
        assert_eq!(
            why,
            Held::WorkInFlight {
                what: vec!["an unsent comment".to_string()]
            }
        );

        // And it holds regardless of how long the release has waited: the
        // floor is the only thing that overrides declared work, and the floor
        // is not this decision.
        let Intent::Waiting { .. } = intent(
            Some(&staged),
            now_after(90),
            &["an unsent comment".to_string()],
            None,
        ) else {
            panic!("a long wait overrode declared work");
        };
    }

    /// A compatibility boundary is not an ordinary update. It must remain
    /// unstaged and tell the person exactly how to cross it without implying
    /// that a restart can turn an old installation into the new layout.
    #[test]
    fn below_the_floor_requires_a_canonical_reinstall() {
        let crossing = Standing::ReinstallRequired {
            version: "0.10.0".into(),
            floor: "0.10.0".into(),
        };
        let Intent::Attention { why } = intent(Some(&crossing), now_after(0), &[], None) else {
            panic!("a compatibility boundary was presented as an ordinary update");
        };
        assert!(
            why.contains("canonical reinstall")
                && why.contains("Spaces")
                && why.contains("Worlds")
                && why.contains("0.10.0"),
            "the boundary did not name the safe crossing and preserved data: {why}"
        );
    }

    /// The staged record deliberately survives a successful macOS bundle
    /// exchange so the old process still takes its restart. The new process
    /// identifies itself as that staged release and must not take another.
    #[test]
    fn the_applied_version_does_not_restart_again_on_stale_staged_standing() {
        let staged = Standing::Staged {
            version: "0.9.0".into(),
            at: STAGED_AT,
        };
        assert_eq!(
            intent(Some(&staged), now_after(30), &[], Some("0.9.0")),
            Intent::Nothing,
            "the applied release looped"
        );
    }

    /// The stub seam, from the client's side: the entry inside `current/`
    /// resolves to the stub at the root, and nothing else does. The spelling
    /// agreement with the stub itself is held by the staged-swap chain test,
    /// which runs the real pair.
    #[test]
    fn only_the_installed_shape_has_a_managing_stub() {
        let root = tempfile::tempdir().expect("a scratch root");
        let name = if cfg!(windows) {
            "astrolabe.exe"
        } else {
            "astrolabe"
        };

        let current = root.path().join("current");
        std::fs::create_dir(&current).expect("the live tree");
        let entry = current.join(name);
        std::fs::write(&entry, b"the entry").expect("the entry");

        // No stub at the root yet: half the shape is no shape.
        assert_eq!(managing_stub_of(&entry), None);

        let stub = root.path().join(name);
        std::fs::write(&stub, b"the stub").expect("the stub");
        assert_eq!(managing_stub_of(&entry), Some(stub));

        // A developer's build tree resolves to nothing.
        assert_eq!(
            managing_stub_of(&root.path().join("target").join("debug").join(name)),
            None
        );
    }

    /// A clock that has gone backwards must not produce a negative age and a
    /// wrong urgency; it produces the quietest answer, which is the one that
    /// costs nothing if wrong.
    #[test]
    fn a_clock_behind_the_staging_time_is_quiet_rather_than_urgent() {
        let staged = staged();
        let Intent::RestartRequested { urgency, .. } = intent(Some(&staged), 0, &[], None) else {
            panic!("a staged release did not ask for a restart");
        };
        assert_eq!(urgency, Urgency::Quiet);
    }
}
