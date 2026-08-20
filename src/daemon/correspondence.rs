//! Identity-scoped correspondence on the daemon.
//!
//! Correspondence was the desktop client's alone: `crates/correspondence` was
//! reached by exactly one crate, and the plane — the device seeds, the kinship
//! registry, the mailbox — was built inside the Astrolabe process. Two things
//! follow from that, and both are wrong.
//!
//! A **World cannot send.** A World's head talks to this daemon; Astrolabe sits
//! above Worlds and launches them, so a World has no upward call. Offering
//! "invite this person" from a tracker was structurally impossible, not merely
//! unbuilt.
//!
//! And **mail arrives only while a window is open.** The one process able to
//! collect was the one a person closes.
//!
//! So the plane lives here, beside the address book, for the same reason the
//! address book does: it is the identity's, not any surface's. Astrolabe asks
//! for it over the control plane like every other caller, and stops being the
//! place it happens to live.
//!
//! What does **not** move down here is judgement. This service carries letters
//! and holds who a person has learned. It admits nobody to anything: an
//! invitation it delivers is verified at the Space that issued it, and delivery
//! was never admission.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::control::{Request, Response};

/// Whether a request belongs to this service.
///
/// Named rather than matched at the call site so the dispatch in
/// [`crate::daemon::host`] reads as one line, the way the book's does.
pub fn is_correspondence_request(request: &Request) -> bool {
    matches!(
        request,
        Request::ReachShare
            | Request::ReachLearn { .. }
            | Request::ReachView
            | Request::CorrespondSend { .. }
            | Request::CorrespondCollect
            | Request::CorrespondInvite { .. }
    )
}

/// The identity's correspondence plane, and the durable state under it.
pub struct CorrespondenceService {
    identity: PathBuf,
    /// `None` until a carrier is configured. Correspondence with no carrier is
    /// not an empty mailbox — every operation refuses in words, because the two
    /// are different facts and only one is worth acting on.
    plane: Mutex<Option<Plane>>,
}

/// What a configured plane holds.
struct Plane {
    /// Where mail is carried. The hosted Post's base URL.
    base: String,
}

impl CorrespondenceService {
    /// Open the service for one identity. Carrying nothing yet is normal: a
    /// carrier is configured, and until one is this refuses honestly.
    pub fn open(identity: &Path) -> Self {
        Self {
            identity: identity.to_path_buf(),
            plane: Mutex::new(None),
        }
    }

    /// Where this identity's durable correspondence state lives.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.identity
    }

    /// Point this service at a carrier.
    pub fn carry_over(&self, base: String) {
        if let Ok(mut plane) = self.plane.lock() {
            *plane = Some(Plane { base });
        }
    }

    /// Whether a carrier is configured.
    #[must_use]
    pub fn carrying(&self) -> bool {
        self.plane.lock().is_ok_and(|plane| plane.is_some())
    }

    /// Answer one control-plane request.
    pub async fn handle(&self, request: Request) -> Response {
        let Ok(plane) = self.plane.lock() else {
            return Response::err("the correspondence plane is poisoned");
        };
        let Some(_plane) = plane.as_ref() else {
            return Response::err("correspondence is not connected to a carrier");
        };
        match request {
            Request::ReachShare
            | Request::ReachLearn { .. }
            | Request::ReachView
            | Request::CorrespondSend { .. }
            | Request::CorrespondCollect
            | Request::CorrespondInvite { .. } => {
                Response::err("correspondence is not connected to a carrier")
            }
            _ => Response::err("not a correspondence request"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(tag: &str) -> CorrespondenceService {
        let dir = std::env::temp_dir().join(format!("corr-svc-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        CorrespondenceService::open(&dir)
    }

    #[test]
    fn every_correspondence_request_reaches_this_service_and_nothing_else_does() {
        assert!(is_correspondence_request(&Request::ReachShare));
        assert!(is_correspondence_request(&Request::ReachView));
        assert!(is_correspondence_request(&Request::CorrespondCollect));
        assert!(is_correspondence_request(&Request::ReachLearn {
            announcement: "x".into()
        }));
        assert!(is_correspondence_request(&Request::CorrespondSend {
            to: "prf_x".into(),
            body: "hi".into()
        }));
        assert!(is_correspondence_request(&Request::CorrespondInvite {
            to: "prf_x".into(),
            link: "lait://join/aa".into()
        }));
        assert!(!is_correspondence_request(&Request::BookList));
    }

    /// No carrier is not an empty mailbox. A service that answered "nothing
    /// waiting" here would be the false-disconnection defect one layer down.
    #[tokio::test]
    async fn with_no_carrier_every_operation_refuses_in_words() {
        let service = service("nocarrier");
        assert!(!service.carrying());
        let answer = service.handle(Request::CorrespondCollect).await;
        assert!(
            matches!(&answer, Response::Error { message, .. } if message.contains("carrier")),
            "{answer:?}"
        );
    }
}
