//! Noticing that a letter arrived.
//!
//! The hop delivers; this observes. Without it a mailbox is something nobody
//! checks — the whole plane is a letter in a box on a street the recipient never
//! walks down. AUTH's own infrastructure record names the gap: "No push
//! notification path of any kind — an input to carrier design, since a recipient
//! who was offline learns nothing arrived until something asks." This is the
//! *asking*, on a timer, which is the floor AUTH-19 leaves in place until a real
//! push path is decided.
//!
//! # Three facts, kept apart
//!
//! [`Standing`] is `Waiting`, `Quiet`, or `Unreachable`, and folding any two would
//! be the false-disconnection defect at the surface a person actually reads. In
//! particular `Unreachable` is never `Quiet`: "we could not ask" rendered as
//! "nobody wrote to you" tells a person the opposite of the truth exactly when
//! somebody is trying to reach them.
//!
//! # The cadence, and why it is jittered
//!
//! A fleet restarted together by a deploy must not arrive at the carrier
//! together, so the first delay and every steady delay carry full jitter — the
//! same shape `update::watch` uses, and for the same reason. On failure the
//! period backs off exponentially so a carrier that is down is not hammered by
//! synchronised retries, and resets the moment it answers again.
//!
//! # This module names no filesystem and no clock
//!
//! Like the rest of the crate. `now` and the jitter fraction are arguments, so
//! the loop is deterministic under test; persistence is a callback the daemon
//! supplies, because where a standing is written is the daemon's business and not
//! the seam's.

use std::time::Duration;

use mechanics::ids::DeviceId;
use serde::{Deserialize, Serialize};

use crate::{Carrier, Missed};

/// How often a healthy poll asks, before jitter.
///
/// Minutes, not hours: a person waiting to hear is the case this serves, and the
/// carrier's cost is one signed request per identity per period. Tighter would
/// chase latency the carrier's own store-and-forward does not promise; looser
/// would make "you have mail" arrive long after the mail did.
pub const POLL_PERIOD: Duration = Duration::from_secs(5 * 60);

/// The width of the jitter added to every delay.
///
/// A full period, so the steady cadence is `[POLL_PERIOD, 2·POLL_PERIOD)` and the
/// fleet spreads across it rather than pulsing at the boundary.
pub const POLL_SPREAD: Duration = Duration::from_secs(5 * 60);

/// The longest a backed-off poll waits, however many failures precede it.
///
/// A carrier down for an hour should still be re-checked within the hour, or a
/// recipient whose carrier recovered learns nothing until far too late.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60 * 60);

/// What the last poll established about a mailbox.
///
/// Persisted so a restart does not forget, and read by the client on its ordinary
/// refresh — the same silent-standing-file shape as the update watcher, which
/// keeps this observable without a push path the tree does not have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "standing")]
pub enum Standing {
    /// Mail is waiting. `count` is how many, `checked_at` is when it was seen.
    Waiting { count: usize, checked_at: u64 },
    /// The carrier answered and is holding nothing.
    Quiet { checked_at: u64 },
    /// The carrier could not be asked. **Not** an empty mailbox.
    ///
    /// `since` dates from the *first* failure in the current run of them, so a
    /// surface can say how long the dark has lasted rather than resetting the
    /// clock on every retry — the same discipline `ObservationHealth` keeps.
    ///
    /// It does not carry the last known count. The mail is not lost — it is on the
    /// carrier — only the local number is, and the next answer restores it. What
    /// must survive is that this is *not* `Quiet`, and that it does.
    Unreachable { why: String, since: u64 },
}

impl Standing {
    /// Whether a person has correspondence to collect, as far as the last poll
    /// could tell. `Unreachable` answers `false` — uncertainty is not a
    /// notification — but the surface must still show that it could not check
    /// rather than implying quiet.
    pub fn has_mail(&self) -> bool {
        matches!(self, Self::Waiting { count, .. } if *count > 0)
    }
}

/// Fold one poll's answer into a new standing, given the one before it.
///
/// The prior standing is threaded for exactly one reason: `Unreachable::since`
/// must date from the first failure, so a run of failures keeps its start rather
/// than forgetting how long it has been dark.
pub fn fold(prior: Option<&Standing>, answer: Missed, now: u64) -> Standing {
    match answer {
        Missed::Held(waiting) if waiting.is_empty() => Standing::Quiet { checked_at: now },
        Missed::Held(waiting) => Standing::Waiting {
            count: waiting.len(),
            checked_at: now,
        },
        Missed::Unasked(why) => {
            let since = match prior {
                Some(Standing::Unreachable { since, .. }) => *since,
                _ => now,
            };
            Standing::Unreachable { why, since }
        }
    }
}

/// Ask a carrier once, and fold the answer.
///
/// The whole poll, small enough to read: build nothing, decide nothing, just ask
/// and record. It is separate from the loop so the algorithm can be tested
/// without a timer and the loop can be tested without a carrier.
pub fn poll_once(
    carrier: &mut dyn Carrier,
    device: &DeviceId,
    prior: Option<&Standing>,
    now: u64,
) -> Standing {
    fold(prior, carrier.collect(device, now), now)
}

/// The delay before the next poll, jittered, backing off while failing.
///
/// `draw` is a number in `[0, 1)` supplied by the caller, so this is a pure
/// function a test can pin. `consecutive_failures` is 0 when the last poll was
/// answered (whether or not it held mail) and counts up while it could not be
/// asked.
pub fn next_delay(consecutive_failures: u32, draw: f64) -> Duration {
    // Healthy: the steady period plus full jitter, so the fleet spreads.
    // Failing: double the period per failure up to the cap, then jitter that —
    // synchronised retries against a down carrier are the thing jitter exists to
    // prevent, and a cap keeps a recovered carrier from being noticed hours late.
    let base = if consecutive_failures == 0 {
        POLL_PERIOD
    } else {
        // `saturating` throughout: this is on a daemon's forever-loop and must not
        // panic on a pathological failure count.
        let shift = consecutive_failures.min(16);
        POLL_PERIOD.saturating_mul(1u32 << shift).min(MAX_BACKOFF)
    };
    base.saturating_add(POLL_SPREAD.mul_f64(draw.clamp(0.0, 1.0)))
}

/// A number in `[0, 1)` for jitter, from `getrandom`.
///
/// Degrades to the midpoint rather than failing: a poll that refused to run
/// because entropy was briefly unavailable would be a mailbox that stops being
/// checked over a number that only needs to be roughly spread. Copied in shape
/// from `update::watch::draw` because the reasoning is identical.
pub fn draw() -> f64 {
    let mut bytes = [0u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        return 0.5;
    }
    f64::from(u32::from_le_bytes(bytes)) / f64::from(u32::MAX)
}

/// Poll a carrier until told to stop, persisting each standing.
///
/// The loop, split from its parts for the reason `update::watch` splits its own:
/// every piece here was unit-tested and the *loop* was the untested half, so
/// nothing asserted a running daemon ever reaches its poll — the composition this
/// tree keeps getting wrong while every part is correct.
///
/// `ask` performs one carrier collect and returns the raw answer; the loop owns
/// folding it, the failure count, and the cadence. `persist` is handed each new
/// standing. `clock` supplies `now`. All three are arguments so the loop is
/// deterministic under a paused test clock and names no wall clock of its own.
///
/// The first delay is jittered too, so a fleet restarted together by a deploy does
/// not arrive at the carrier together either.
pub async fn serve<A, P, C>(
    mut ask: A,
    mut persist: P,
    mut clock: C,
    mut stop: tokio::sync::watch::Receiver<bool>,
) where
    A: FnMut() -> Missed + Send,
    P: FnMut(&Standing) + Send,
    C: FnMut() -> u64 + Send,
{
    let mut prior: Option<Standing> = None;
    let mut failures: u32 = 0;
    let mut delay = POLL_SPREAD.mul_f64(draw());
    loop {
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            _ = stop.changed() => return,
        }
        if *stop.borrow() {
            return;
        }
        let standing = fold(prior.as_ref(), ask(), clock());
        failures = if matches!(standing, Standing::Unreachable { .. }) {
            failures.saturating_add(1)
        } else {
            0
        };
        persist(&standing);
        prior = Some(standing);
        delay = next_delay(failures, draw());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::MemCarrier;
    use crate::Sealed;
    use mechanics::actor::device_from_seed;

    /// The loop reaches its poll, persists what it found, and stops when told.
    ///
    /// Time is paused, so this asserts the composition without waiting real
    /// minutes: advance past the first jittered delay, and exactly one poll must
    /// have happened and been persisted.
    #[tokio::test(start_paused = true)]
    async fn the_loop_polls_persists_and_stops() {
        use std::sync::{Arc, Mutex};

        let me = device(1);
        let mut carrier = MemCarrier::new();
        carrier.deliver_for_test(&sealed_to(&me), &device(2), 0);
        let carrier = Arc::new(Mutex::new(carrier));

        let seen: Arc<Mutex<Vec<Standing>>> = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = tokio::sync::watch::channel(false);

        let ask_carrier = Arc::clone(&carrier);
        let me_for_ask = me.clone();
        let seen_for_persist = Arc::clone(&seen);
        let handle = tokio::spawn(serve(
            move || ask_carrier.lock().unwrap().collect(&me_for_ask, 1),
            move |standing| seen_for_persist.lock().unwrap().push(standing.clone()),
            || 1,
            rx,
        ));

        // Let the spawned loop reach its first `sleep` before time is advanced —
        // under a paused clock a task that has not been polled has no timer to
        // fire. Then advance past the maximum first delay (a full spread) and let
        // the poll run.
        tokio::task::yield_now().await;
        tokio::time::advance(POLL_SPREAD + Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let after_first = seen.lock().unwrap().clone();
        assert_eq!(
            after_first.len(),
            1,
            "one poll must have happened and persisted"
        );
        assert_eq!(
            after_first[0],
            Standing::Waiting {
                count: 1,
                checked_at: 1
            },
            "the persisted standing is what the poll found"
        );

        tx.send(true).expect("signal stop");
        handle.await.expect("the loop returns on stop");
    }

    fn device(n: u8) -> DeviceId {
        device_from_seed(&[n; 32])
    }

    fn sealed_to(recipient: &DeviceId) -> Sealed {
        Sealed {
            recipient: recipient.clone(),
            bytes: vec![0u8; 8],
            expires_at: u64::MAX,
            construction: 1,
        }
    }

    /// A poll reports the three facts, and never folds two of them.
    #[test]
    fn a_poll_tells_waiting_quiet_and_unreachable_apart() {
        let me = device(1);
        let mut carrier = MemCarrier::new();

        // Nothing yet: Quiet, not Unreachable.
        let quiet = poll_once(&mut carrier, &me, None, 100);
        assert_eq!(quiet, Standing::Quiet { checked_at: 100 });
        assert!(!quiet.has_mail());

        // A letter lands (deposited straight into the mailbox for the test — the
        // deposit path has its own coverage).
        carrier.deliver_for_test(&sealed_to(&me), &device(2), 100);
        let waiting = poll_once(&mut carrier, &me, Some(&quiet), 200);
        assert_eq!(
            waiting,
            Standing::Waiting {
                count: 1,
                checked_at: 200
            }
        );
        assert!(waiting.has_mail());

        // The carrier goes dark: Unreachable, and it is NOT Quiet.
        carrier.seal_off("no route");
        let dark = poll_once(&mut carrier, &me, Some(&waiting), 300);
        match &dark {
            Standing::Unreachable { why, since } => {
                assert_eq!(why, "no route");
                assert_eq!(*since, 300, "the dark started now");
            }
            other => panic!("a carrier that could not be asked reported {other:?}"),
        }
        assert!(!dark.has_mail());
        assert_ne!(
            dark,
            Standing::Quiet { checked_at: 300 },
            "unreachable must never equal quiet"
        );
    }

    /// A run of failures keeps the time the dark began.
    #[test]
    fn unreachable_dates_from_the_first_failure_not_the_latest() {
        let me = device(1);
        let mut carrier = MemCarrier::new();
        carrier.seal_off("down");

        let first = poll_once(&mut carrier, &me, None, 1000);
        let second = poll_once(&mut carrier, &me, Some(&first), 2000);
        let third = poll_once(&mut carrier, &me, Some(&second), 3000);

        for standing in [&second, &third] {
            match standing {
                Standing::Unreachable { since, .. } => assert_eq!(
                    *since, 1000,
                    "the dark began at the first failure and must not reset"
                ),
                other => panic!("expected unreachable, got {other:?}"),
            }
        }
    }

    /// The steady cadence is jittered within one period-width.
    #[test]
    fn a_healthy_poll_waits_one_period_plus_jitter() {
        // draw = 0 → exactly the period; draw ≈ 1 → nearly period + spread.
        assert_eq!(next_delay(0, 0.0), POLL_PERIOD);
        let jittered = next_delay(0, 0.999);
        assert!(jittered > POLL_PERIOD);
        assert!(jittered < POLL_PERIOD + POLL_SPREAD + Duration::from_secs(1));
    }

    /// Failure backs off, monotonically, up to the cap.
    #[test]
    fn failure_backs_off_and_caps() {
        // Compare at draw = 0 so the jitter does not blur the base.
        let one = next_delay(1, 0.0);
        let two = next_delay(2, 0.0);
        let three = next_delay(3, 0.0);
        assert!(
            one > POLL_PERIOD,
            "one failure already waits longer than healthy"
        );
        assert!(two > one);
        assert!(three > two);

        // Far out, it is capped, not unbounded — and does not panic.
        let far = next_delay(1000, 0.0);
        assert_eq!(far, MAX_BACKOFF);
    }
}
