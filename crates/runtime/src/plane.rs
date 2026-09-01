#![allow(
    clippy::as_conversions,
    clippy::expect_used,
    reason = "plane framing converts only values already bounded by its wire ceilings"
)]
//! The runtime's delivery-plane protocol records.
//!
//! Freight moves exact requested bytes. Live hosts bounded realtime sessions —
//! cursors, presence, and small reliable signals. They are separate ALPNs
//! because they have different admission, timeouts, memory profiles, shutdown
//! semantics, and compatibility lifetimes: a file fetch should not keep a
//! realtime session alive, and a cursor bug should not block artifact recovery.
//!
//! **On the name.** The docket calls the second plane "Session". This crate
//! already has `session`, meaning a *World* session — a client docked against a
//! Station, committing transactions. Two unrelated things under one name is
//! worse for a reader than one module diverging from the prose, so the plane is
//! `live` here and the encoded ALPN keeps `lait/session/1` exactly as
//! specified. Nothing on the wire moves.
//!
//! This module is shapes and bounds only. There is no handler, no connection,
//! and no routing: S0 freezes what S1 and later build against, and freezing it
//! first is what stops three planes growing three bespoke framings.

use serde::{Deserialize, Serialize};

pub mod contact;
// The plane handlers drive real connections; the guest carve keeps only the
// vocabulary (bounds, stream kinds, the contact shim) — see `lib.rs`.
#[cfg(not(target_arch = "wasm32"))]
pub mod exec;
#[cfg(not(target_arch = "wasm32"))]
pub mod freight;
#[cfg(not(target_arch = "wasm32"))]
pub mod live;

#[cfg(not(target_arch = "wasm32"))]
pub use crate::contact_driver::{CommsOptions, GossipOptions, MAX_CONTACTS_IN_FLIGHT};
#[cfg(not(target_arch = "wasm32"))]
pub use crate::lifecycle::Activation;

/// Typed stream kinds within one live connection.
///
/// One byte, and allocated values are never silently reused. `0x03` and `0x04`
/// are the native media pair: Group streams and bounded control/feedback.
pub mod stream_kind {
    /// Long-lived bidirectional framed control.
    pub const CONTROL: u8 = 0x01;
    /// One bounded message per short stream.
    pub const RELIABLE_SIGNAL: u8 = 0x02;
    /// One Group header plus length-delimited encoded Frames on a uni stream.
    pub const MEDIA_GROUP: u8 = 0x03;
    /// Bounded media control and feedback on a short bidirectional flow.
    pub const MEDIA_CONTROL: u8 = 0x04;

    /// Whether this build implements a kind.
    pub fn is_implemented(kind: u8) -> bool {
        matches!(
            kind,
            CONTROL | RELIABLE_SIGNAL | MEDIA_GROUP | MEDIA_CONTROL
        )
    }

    /// Media lanes are granted as a pair and only when the media feature was
    /// negotiated. Keeping this predicate separate makes that admission rule
    /// explicit without teaching `comms` what a lane means.
    pub fn is_media(kind: u8) -> bool {
        matches!(kind, MEDIA_GROUP | MEDIA_CONTROL)
    }
}

/// The longest human-readable text a signal may carry.
///
/// A display name or a media type, not a message. Both are shown to a person
/// and neither is content — a signal that wanted to carry prose would be a
/// signal carrying a Body's job.
pub const MAX_SIGNAL_TEXT_BYTES: usize = 256;

/// What a peer is invited to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InviteKind {
    Collaborate,
}

/// One reliable signal.
///
/// Bounded, one per stream, and never durable. The shapes are closed: a World
/// that needs its own says so through `WorldSignal`, whose payload the
/// substrate carries and does not interpret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Signal {
    /// Are you there. The one signal that expects an answer.
    Ping { nonce: [u8; 16] },
    /// I am. Never itself acknowledged — that is how a ping becomes a loop.
    Acknowledge { nonce: [u8; 16] },
    /// Look at this.
    Attention { scope: crate::transient::Target },
    /// Come and work on this with me.
    SessionInvite {
        kind: InviteKind,
        scope: crate::transient::Target,
    },
    /// I have a file you may want.
    ///
    /// An offer, not a transfer. It names content the sender holds; whether the
    /// receiver wants it is a decision a person makes afterwards, which is why
    /// this expects no answer.
    FileOffer {
        content: [u8; 32],
        plaintext_len: u64,
        /// What to call it, as the sender sees it.
        ///
        /// Sanitised **on use**, never on decode. A decode-time rewrite makes
        /// `encode(decode(x)) == x` false, and canonical re-encode equality is
        /// what every shape on this plane rests on.
        display_name: String,
        media_type: String,
    },
    /// A World's own signal. Opaque here.
    WorldSignal {
        world: String,
        schema: String,
        payload: Vec<u8>,
    },
}

impl Signal {
    /// Which declaration governs this signal.
    pub fn selector(&self) -> u16 {
        use crate::signal::selector;
        match self {
            Self::Ping { .. } => selector::PING,
            Self::Acknowledge { .. } => selector::ACKNOWLEDGE,
            Self::Attention { .. } => selector::ATTENTION,
            Self::SessionInvite { .. } => selector::SESSION_INVITE,
            Self::FileOffer { .. } => selector::FILE_OFFER,
            Self::WorldSignal { .. } => selector::WORLD,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard signal")
    }

    /// Decode one signal, in the order that makes each check protect the next.
    ///
    /// Deliberately no `encode_bounded`. A frame that is too large to send is a
    /// caller's error and gets a `Result`; substituting a refusal — which is
    /// right for `FreightFrame`, where the alternative is telling a peer
    /// nothing — would silently send something other than what was asked for.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() > bounds::MAX_SIGNAL_BYTES {
            return Err(WireError::TooLarge);
        }
        let signal: Self = postcard::from_bytes(bytes).map_err(|_| WireError::NonCanonical)?;
        if signal.encode() != bytes {
            return Err(WireError::NonCanonical);
        }
        signal.validate()?;
        Ok(signal)
    }

    pub fn validate(&self) -> Result<(), WireError> {
        let text = |value: &str| {
            if value.len() > MAX_SIGNAL_TEXT_BYTES {
                return Err(WireError::Bounds);
            }
            // A control character in a name lands in a header, a filename, or a
            // terminal. None of those are places a peer chooses what happens.
            if value.chars().any(|c| c.is_control()) {
                return Err(WireError::NonCanonical);
            }
            Ok(())
        };
        match self {
            Self::Ping { .. } | Self::Acknowledge { .. } => Ok(()),
            Self::Attention { scope } | Self::SessionInvite { scope, .. } => {
                scope.validate_wire().map_err(|_| WireError::Bounds)
            }
            Self::FileOffer {
                display_name,
                media_type,
                ..
            } => {
                text(display_name)?;
                text(media_type)
            }
            Self::WorldSignal {
                world,
                schema,
                payload,
            } => {
                // Parsed through the real grammars rather than a length check:
                // a World id and a schema id have shapes, and something that is
                // merely short is not therefore one.
                replica::body::WorldId::parse(world).ok_or(WireError::NonCanonical)?;
                replica::body::SchemaId::parse(schema).ok_or(WireError::NonCanonical)?;
                if payload.len() > bounds::MAX_SIGNAL_BYTES {
                    return Err(WireError::Bounds);
                }
                Ok(())
            }
        }
    }
}

/// Bounds that constrain each other, checked at compile time.
///
/// Moved out of the fixtures for the reason clippy names: a comparison of two
/// constants is decided when this file compiles, so making it a test defers a
/// build error into a test run. Here it stops the build at the file the numbers
/// live in.
mod bound_consistency {
    use super::bounds;

    const _: () = assert!(
        bounds::MAX_DATAGRAM_BYTES <= 1_200,
        "a datagram must fit the smallest path anyone measures, not the largest"
    );
    const _: () = assert!(
        bounds::MAX_SIGNAL_BYTES < bounds::MAX_CONTROL_FRAME_BYTES,
        "one signal must fit inside a control frame, or it can never be sent"
    );
}

// The admission vocabulary — Plane, Capability, Open/Accept/Refusal, the
// wire bounds — lives in the `contact` crate now, so a browser initiator and
// this runtime speak from one set of types. Every prior path still works.
pub use ::contact::admission::{
    bounds, datagram_fits, feature, Accept, Capability, Open, Plane, Refusal, WireError, EXEC_ALPN,
    EXEC_PROTOCOL_VERSION, FREIGHT_ALPN, FREIGHT_PROTOCOL_VERSION, LIVE_ALPN,
    LIVE_PROTOCOL_VERSION, SPACE_ID_LEN,
};

/// What Freight asks for and answers with.
///
/// Requests are exact. There is no "list what you have" and no remote path: a
/// peer asks for one chunk of one content it already knows the id of, and a
/// provider may refuse without saying whether authorization, policy, load,
/// absence, or incomplete proof material caused it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FreightFrame {
    /// Do you hold any of these chunks of this content?
    Have {
        content_id: [u8; 32],
        wanted: Vec<u32>,
    },
    /// Which of them are servable right now. A chunk counts only when its
    /// ciphertext *and* a validated proof are both resident.
    Available {
        content_id: [u8; 32],
        chunks: Vec<u32>,
    },
    /// Send me this chunk, from this offset.
    ///
    /// `resume_leaf` is the leaf hash a partial transfer already validated. A
    /// provider whose leaf differs is rejected before a byte is appended, so a
    /// resumed transfer cannot be steered onto different content.
    GetChunk {
        content_id: [u8; 32],
        chunk_index: u32,
        offset: u64,
        max_len: u32,
        resume_leaf: Option<[u8; 32]>,
    },
    /// Bounded leaf metadata and proof, ahead of the raw bytes.
    ChunkHeader {
        content_id: [u8; 32],
        chunk_index: u32,
        proof: Vec<u8>,
        total_len: u32,
    },
    /// No. Deliberately without a reason.
    Refused,
}

impl FreightFrame {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard freight frame")
    }

    /// Encode, substituting a coarse refusal for anything that would exceed
    /// what the peer is allowed to read.
    ///
    /// A bound checked only on receive turns a local mistake into a remote
    /// protocol error: the peer refuses a frame we should never have written,
    /// and the failure is attributed to the wrong side. A provider that finds
    /// itself about to answer with something oversized says no instead — which
    /// is a legal answer, and the only one that keeps the refusal coarse.
    pub fn encode_bounded(&self) -> Vec<u8> {
        let bytes = self.encode();
        if self.validate().is_err() || bytes.len() > bounds::MAX_CONTROL_FRAME_BYTES {
            return FreightFrame::Refused.encode();
        }
        bytes
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() > bounds::MAX_CONTROL_FRAME_BYTES {
            return Err(WireError::TooLarge);
        }
        let frame: Self = postcard::from_bytes(bytes).map_err(|_| WireError::NonCanonical)?;
        if frame.encode() != bytes {
            return Err(WireError::NonCanonical);
        }
        frame.validate()?;
        Ok(frame)
    }

    pub fn validate(&self) -> Result<(), WireError> {
        match self {
            FreightFrame::Have { wanted, .. } => {
                if wanted.len() > MAX_WANTED_CHUNKS {
                    return Err(WireError::Bounds);
                }
            }
            FreightFrame::Available { chunks, .. } => {
                if chunks.len() > MAX_WANTED_CHUNKS {
                    return Err(WireError::Bounds);
                }
            }
            FreightFrame::GetChunk { max_len, .. } => {
                if *max_len as usize > bounds::MAX_CHUNK_FRAME_BYTES {
                    return Err(WireError::Bounds);
                }
            }
            FreightFrame::ChunkHeader {
                proof, total_len, ..
            } => {
                if proof.len() > MAX_PROOF_BYTES
                    || *total_len as usize > bounds::MAX_CHUNK_FRAME_BYTES
                {
                    return Err(WireError::Bounds);
                }
            }
            FreightFrame::Refused => {}
        }
        Ok(())
    }
}

/// Maximum chunk indices one `Have`/`Available` may name.
pub const MAX_WANTED_CHUNKS: usize = 4096;
/// Maximum encoded proof sidecar.
///
/// The same number the resident cache accepts, and deliberately not a second
/// one: a sidecar that arrives inside the wire bound and then cannot be stored
/// would be a transfer that verifies and fails.
pub const MAX_PROOF_BYTES: usize = replica::content::MAX_PROOF_BYTES;
