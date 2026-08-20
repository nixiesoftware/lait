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

use correspondence::Carrier;
use mechanics::kinship::{Audience, ProfileId};

use crate::control::{Request, Response};

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The correspondents a plane holds, as address spellings.
fn correspondents(reach: &correspondence::plane::ReachPlane) -> Vec<String> {
    let mine = reach.profile().clone();
    reach
        .registry_profiles()
        .into_iter()
        .filter(|profile| profile != &mine)
        .map(|profile| profile.as_str().to_owned())
        .collect()
}

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

/// What a configured plane holds: the identity's reach, and something to carry
/// by. The carrier is boxed because which contractor is carrying is a
/// deployment choice rather than an architecture commitment — memory in tests,
/// a hosted Post today, a direct peer later — and the plane never learns which.
struct Plane {
    reach: correspondence::plane::ReachPlane,
    carrier: Box<dyn Carrier + Send>,
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

    /// Stand the plane up over a carrier.
    ///
    /// Founding is deterministic from this identity's own seeds, so the address
    /// a person hands out names them on every start. Durable reach state is
    /// read here and written back by anything that changes it.
    pub fn carry_over(&self, carrier: Box<dyn Carrier + Send>, now: u64) -> Result<(), String> {
        let seeds = crate::config::load_or_create_kinship_seeds(&self.identity)
            .map_err(|error| error.to_string())?;
        let held = addressbook::ReachStore::at(&self.identity)
            .load()
            .map_err(|error| error.to_string())?;
        let reach = correspondence::plane::ReachPlane::restore(seeds, held, now)
            .map_err(|error| format!("{error}"))?;
        let mut plane = self
            .plane
            .lock()
            .map_err(|_| "the correspondence plane is poisoned".to_string())?;
        *plane = Some(Plane { reach, carrier });
        Ok(())
    }

    /// Write the plane's durable half back where it was read from.
    fn keep(&self, plane: &Plane) -> Result<(), String> {
        addressbook::ReachStore::at(&self.identity)
            .save(&plane.reach.state())
            .map_err(|error| error.to_string())
    }

    /// This identity's reach, as every answer reports it.
    fn reach_view(&self, plane: &Plane) -> Response {
        Response::Reach(Box::new(crate::control::ReachView {
            announcement: plane
                .reach
                .card(&plane.reach.standing())
                .and_then(|card| card.render().ok()),
            profile: plane.reach.profile().as_str().to_owned(),
            correspondents: correspondents(&plane.reach),
        }))
    }

    /// Whether a carrier is configured.
    #[must_use]
    pub fn carrying(&self) -> bool {
        self.plane.lock().is_ok_and(|plane| plane.is_some())
    }

    /// Answer one control-plane request.
    pub async fn handle(&self, request: Request) -> Response {
        let Ok(mut held) = self.plane.lock() else {
            return Response::err("the correspondence plane is poisoned");
        };
        let Some(plane) = held.as_mut() else {
            // Not an empty mailbox. A caller has to be able to tell "nobody
            // wrote to you" from "we are carrying nothing at all", and only one
            // of those is worth acting on.
            return Response::err("correspondence is not connected to a carrier");
        };
        let now = now_secs();

        match request {
            Request::ReachView => self.reach_view(plane),

            Request::ReachShare => {
                let reader = plane.reach.standing();
                match plane.reach.announce(Audience::Public, &reader) {
                    Ok(_) => match self.keep(plane) {
                        Ok(()) => self.reach_view(plane),
                        Err(error) => Response::err(error),
                    },
                    Err(error) => Response::err(format!("{error}")),
                }
            }

            Request::ReachLearn { announcement } => {
                let Ok(parsed) = addressbook::Announcement::parse(&announcement) else {
                    return Response::err("that is not an announcement");
                };
                let reader = plane.reach.standing();
                match plane.reach.learn(parsed, &reader) {
                    Ok(_) => match self.keep(plane) {
                        Ok(()) => self.reach_view(plane),
                        Err(error) => Response::err(error),
                    },
                    Err(error) => Response::err(format!("{error}")),
                }
            }

            Request::CorrespondSend { to, body } => {
                let Some(profile) = ProfileId::parse(&to) else {
                    return Response::err("that is not an address");
                };
                let content = correspondence::Content::Message { body };
                match plane
                    .reach
                    .send_content(&mut *plane.carrier, &profile, content, now)
                {
                    Ok(_) => self.reach_view(plane),
                    Err(error) => Response::err(format!("{error}")),
                }
            }

            Request::CorrespondInvite { to, link } => {
                let Some(profile) = ProfileId::parse(&to) else {
                    return Response::err("that is not an address");
                };
                let body = link
                    .trim()
                    .strip_prefix("lait://join/")
                    .unwrap_or_else(|| link.trim());
                let cleaned: String = body.chars().filter(|c| !c.is_whitespace()).collect();
                let Ok(coordinates) =
                    data_encoding::BASE32_NOPAD.decode(cleaned.to_uppercase().as_bytes())
                else {
                    return Response::err("that is not an invite link");
                };
                // Opaque the whole way. This service carries an invitation and
                // never judges one: it verifies at the Space that issued it, and
                // delivery was never admission.
                let content = correspondence::Content::Invitation { coordinates };
                match plane
                    .reach
                    .send_content(&mut *plane.carrier, &profile, content, now)
                {
                    Ok(_) => self.reach_view(plane),
                    Err(error) => Response::err(format!("{error}")),
                }
            }

            Request::CorrespondCollect => {
                let collected = plane.reach.collect(&mut *plane.carrier, now);
                if let Err(error) = self.keep(plane) {
                    return Response::err(error);
                }
                match collected.unasked {
                    // A carrier that could not be asked is reported, never
                    // folded into "nothing was waiting".
                    Some(why) => Response::err(format!("the carrier could not be asked: {why}")),
                    None => self.reach_view(plane),
                }
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

    fn reach(response: &Response) -> &crate::control::ReachView {
        match response {
            Response::Reach(view) => view,
            other => panic!("expected a reach view, got {other:?}"),
        }
    }

    /// The daemon carries correspondence with no service anywhere.
    ///
    /// This is what the contractor seam is for. `MemCarrier` stands in for the
    /// Post the way `comms::mem::MemTransport` stands in for a network, so the
    /// plane can be exercised where it now lives without standing anything up —
    /// and a test that needed a hosted Post to prove the daemon holds a mailbox
    /// would be proving the Post instead.
    #[tokio::test]
    async fn the_daemon_carries_a_letter_between_two_identities_with_no_service() {
        let root = std::env::temp_dir().join(format!("corr-mem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (ada_home, grace_home) = (root.join("ada"), root.join("grace"));
        for home in [&ada_home, &grace_home] {
            std::fs::create_dir_all(home).unwrap();
            crate::config::load_or_create_identity(home).expect("identity");
        }

        // One carrier, two services: the shared store two people deposit into,
        // which is exactly the Post's role without the Post.
        let shared = correspondence::SharedMem::new();
        let ada = CorrespondenceService::open(&ada_home);
        let grace = CorrespondenceService::open(&grace_home);
        let now = now_secs();
        ada.carry_over(Box::new(shared.clone()), now).expect("ada");
        grace
            .carry_over(Box::new(shared.clone()), now)
            .expect("grace");

        // Each publishes; each takes the other in. Nothing else is shared.
        let ada_card = reach(&ada.handle(Request::ReachShare).await)
            .announcement
            .clone()
            .expect("ada publishes");
        let grace_view = reach(&grace.handle(Request::ReachShare).await).clone();
        let grace_card = grace_view.announcement.clone().expect("grace publishes");

        let learned = reach(
            &grace
                .handle(Request::ReachLearn {
                    announcement: ada_card,
                })
                .await,
        )
        .clone();
        assert_eq!(learned.correspondents.len(), 1, "Grace holds Ada");
        let ada_address = learned.correspondents[0].clone();

        ada.handle(Request::ReachLearn {
            announcement: grace_card,
        })
        .await;

        let sent = ada
            .handle(Request::CorrespondSend {
                to: grace_view.profile.clone(),
                body: "carried by the daemon".into(),
            })
            .await;
        assert!(
            matches!(sent, Response::Reach(_)),
            "the send is answered with what changed, got {sent:?}"
        );

        let collected = grace.handle(Request::CorrespondCollect).await;
        assert!(
            matches!(collected, Response::Reach(_)),
            "a collect that reached the carrier answers, got {collected:?}"
        );
        assert_eq!(
            reach(&collected).correspondents,
            vec![ada_address],
            "and Grace still holds exactly the one correspondent she learned"
        );

        let _ = std::fs::remove_dir_all(&root);
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
