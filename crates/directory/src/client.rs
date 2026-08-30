//! Reaching a directory over HTTP.
//!
//! The other half of [`crate::http`], and the reason [`crate::Directory`] is a
//! trait: a caller composes `publish_as` / `resolve_as` against the trait and
//! never learns whether the answer came from a socket or from a `MemStore` in
//! the same process. That is the shape `correspondence::Carrier` has, for the
//! same reason.
//!
//! # This client trusts nothing it is told
//!
//! `resolve` hands back the announcement's own bytes. It does not decode them,
//! does not look inside, and has no opinion about whose they are — anchoring is
//! the *reader's* job, and a client that pre-digested the answer would be
//! inviting its caller to trust this crate's parsing instead of the
//! publisher's signature. That is the difference between a mirror and an
//! authority, expressed as an absence.

use std::time::Duration;

use mechanics::ids::DeviceId;

use crate::{
    address::Address, http::Refused, wire::Challenge, Directory, Issued, Refusal, SignedPublish,
    SignedResolve,
};

/// How long any single call may take.
///
/// A directory that hangs is a directory that is down, and this sits on the
/// daemon's request path. [`Refusal::Unavailable`] is a better answer than a
/// stalled reach operation, and — this being the whole discipline of the plane
/// above — it is never rendered as "nobody is there".
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// A directory somewhere else.
pub struct Remote {
    base: String,
    agent: ureq::Agent,
}

impl Remote {
    /// Point at a directory's base URL — `https://post.foundation.pub`, with
    /// the `/directory` prefix supplied here rather than by every caller.
    #[must_use]
    pub fn at(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            agent: ureq::AgentBuilder::new().timeout(CALL_TIMEOUT).build(),
        }
    }

    /// Turn a transport outcome into a refusal.
    ///
    /// A refused request carries its reason in the body, and that reason is the
    /// service's own vocabulary — decoded back into [`Refusal`] so a caller
    /// matches on one enum whichever side of the wire produced it. Anything
    /// that is not a refusal is [`Refusal::Unavailable`], which the layer above
    /// must never render as a "no".
    fn refusal(error: ureq::Error) -> Refusal {
        match error {
            ureq::Error::Status(_, response) => response.into_json::<Refused>().map_or_else(
                |_| Refusal::Unavailable("unreadable refusal".into()),
                Into::into,
            ),
            ureq::Error::Transport(transport) => Refusal::Unavailable(transport.to_string()),
        }
    }
}

impl From<Refused> for Refusal {
    fn from(refused: Refused) -> Self {
        match refused {
            Refused::NotAvailable => Self::NotAvailable,
            Refused::Malformed => Self::Malformed,
            Refused::NotAuthentic => Self::NotAuthentic,
            Refused::StaleChallenge => Self::StaleChallenge,
            Refused::TooFast => Self::TooFast,
            Refused::TooLarge => Self::TooLarge,
            Refused::Unavailable => Self::Unavailable("the directory said so".into()),
        }
    }
}

impl Directory for Remote {
    fn challenge(&mut self, device: &DeviceId, _now: u64) -> Result<Challenge, Refusal> {
        // `now` is the service's to decide. A client that sent its own would be
        // asking the service to trust a clock it cannot check, and the whole
        // point of a challenge is that its freshness is the issuer's fact.
        self.agent
            .get(&format!(
                "{}/directory/challenge?device={}",
                self.base,
                device.as_str()
            ))
            .call()
            .map_err(Self::refusal)?
            .into_json()
            .map_err(|error| Refusal::Unavailable(format!("challenge: {error}")))
    }

    fn publish(&mut self, request: &SignedPublish, _now: u64) -> Result<Issued, Refusal> {
        let answered: crate::http::Published = self
            .agent
            .post(&format!("{}/directory/publish", self.base))
            .send_json(serde_json::to_value(request).map_err(|_| Refusal::TooLarge)?)
            .map_err(Self::refusal)?
            .into_json()
            .map_err(|error| Refusal::Unavailable(format!("publish: {error}")))?;
        // Parsed rather than trusted. A service that answered with something
        // this build could not spell back would have handed a person an address
        // they could not type, and finding that out here costs one parse.
        //
        // The receipt is carried through unchecked, deliberately: verifying a
        // mark needs the leaf the *publisher* recomputes from what it signed,
        // and a client that pre-judged it here would be inviting its caller to
        // trust this crate's opinion instead of the marker's signature.
        Ok(Issued {
            address: Address::parse(&answered.address)?,
            receipt: answered.receipt,
        })
    }

    fn resolve(&mut self, request: &SignedResolve, _now: u64) -> Result<Vec<u8>, Refusal> {
        let answered: crate::http::Resolved = self
            .agent
            .post(&format!("{}/directory/resolve", self.base))
            .send_json(serde_json::to_value(request).map_err(|_| Refusal::TooLarge)?)
            .map_err(Self::refusal)?
            .into_json()
            .map_err(|error| Refusal::Unavailable(format!("resolve: {error}")))?;
        let bytes = data_encoding::HEXLOWER_PERMISSIVE
            .decode(answered.announcement.as_bytes())
            .map_err(|_| Refusal::NotAuthentic)?;
        if bytes.len() > crate::bounds::MAX_PUBLISH_BYTES {
            return Err(Refusal::TooLarge);
        }
        Ok(bytes)
    }
}
