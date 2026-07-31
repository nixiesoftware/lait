//! Reliable signals: bounded one-message events between peers.
//!
//! A signal is a thing that happened — somebody offered you a file, invited you
//! to collaborate, asked for your attention. It is reliable in the sense that it
//! is delivered or it fails loudly, and it is *not* durable in every other
//! sense: no journal entry, nothing replayed after a restart, and nothing that
//! becomes activity.
//!
//! That negative is the whole contract, and it is easy to break by accident.
//! One line — a `with_replica` here, a `publish` there — would journal nothing
//! and still emit an Observation, and `SpaceBridge::frame_for` turns any
//! Observation carrying scopes into `activity_advanced`. So the enforcement is
//! structural: **this module may not name the Replica writer or the Observation
//! ring**, and `tests/signal_is_not_durable.rs` parses this file and fails if it
//! does.
//!
//! Privacy cannot do that job. `Broadcaster::publish` is `pub(crate)` and this
//! module is inside that crate; `StationCore::with_replica` is outright `pub`.
//! The gate is what makes the rule real, which is why it lands before the code
//! it guards.

use replica::ids::WorldId;

/// Why a signal did not happen.
///
/// Local diagnostics, in this module rather than a shared error type — the same
/// shape `FreightError`, `FetchError`, `TransferError` and `ContentHostError`
/// all take. What crosses to a peer is deliberately coarser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalError {
    /// No declaration for this selector. A signal nobody declared is one
    /// nothing knows how to bound, which is why it is refused rather than
    /// forwarded.
    NotRegistered,
    /// Authorization refused.
    Denied,
    /// Past the declaration's own ceiling.
    TooLarge,
    /// Did not decode, or decoded to something the declaration forbids.
    Malformed,
    /// The peer did not answer inside the budget.
    Deadline,
    /// This connection is sending signals faster than it may.
    OverBudget,
    /// The peer said no, in its own words.
    PeerRefused,
    /// This connection was not granted the signal lane.
    LaneNotGranted,
}

impl SignalError {
    /// The stable kebab-case code, for a client that needs to branch.
    ///
    /// A second function rather than an arm on `ErrorDto::code_for`, which is
    /// monomorphic on `WorldError` — a signal failure is not a World failure and
    /// widening that function would make it one.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotRegistered => "signal-not-registered",
            Self::Denied => "signal-denied",
            Self::TooLarge => "signal-too-large",
            Self::Malformed => "signal-malformed",
            Self::Deadline => "signal-deadline",
            Self::OverBudget => "signal-over-budget",
            Self::PeerRefused => "signal-peer-refused",
            Self::LaneNotGranted => "signal-lane-not-granted",
        }
    }
}

impl std::fmt::Display for SignalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for SignalError {}

/// Whether a signal may be answered, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsePolicy {
    /// One-way. The sender learns only that it was delivered.
    ///
    /// Most signals are this. An answer is a second round trip and a second
    /// deadline, and a signal that does not need one should not pay for one.
    Forbidden,
    /// The receiver acknowledges, within the response deadline.
    Acknowledge,
}

/// What a peer must satisfy to send this signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalDemand {
    /// Being admitted to the session is enough. For signals that say nothing
    /// about any particular thing — a liveness ping.
    Session,
    /// A World's own demand, evaluated by mechanics at the pinned frontier.
    World { world: WorldId, demand: Vec<u8> },
}

/// Everything the substrate needs to know about one kind of signal.
///
/// A declaration is what makes a signal bounded, authorized, and answerable.
/// Nothing is sent or accepted without one — an undeclared signal is one
/// nothing knows how large it may be or who may send it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalDeclaration {
    /// The wire discriminant. Distinct across every declaration, and never
    /// reused once published.
    pub selector: u16,
    /// This signal's own ceiling, at or under the plane's.
    pub max_bytes: usize,
    pub demand: SignalDemand,
    pub response: ResponsePolicy,
}

/// The selectors the substrate itself owns.
///
/// A World's signals are registered separately and may not collide with these.
pub mod selector {
    pub const PING: u16 = 0x0001;
    pub const ACKNOWLEDGE: u16 = 0x0002;
    pub const ATTENTION: u16 = 0x0003;
    pub const SESSION_INVITE: u16 = 0x0004;
    pub const FILE_OFFER: u16 = 0x0005;
    /// A World's own signal, discriminated further inside the payload.
    pub const WORLD: u16 = 0x0006;
}

/// Every declaration the substrate ships.
///
/// Written as an exhaustive list rather than a lookup built at runtime, so a
/// selector added without a declaration is a compile error here rather than a
/// `NotRegistered` somebody meets in the field.
pub fn core_declarations() -> Vec<SignalDeclaration> {
    vec![
        SignalDeclaration {
            selector: selector::PING,
            max_bytes: 64,
            demand: SignalDemand::Session,
            // The one signal that exists to be answered.
            response: ResponsePolicy::Acknowledge,
        },
        SignalDeclaration {
            selector: selector::ACKNOWLEDGE,
            max_bytes: 64,
            demand: SignalDemand::Session,
            // Answering an answer is how a ping becomes a loop.
            response: ResponsePolicy::Forbidden,
        },
        SignalDeclaration {
            selector: selector::ATTENTION,
            max_bytes: 1024,
            demand: SignalDemand::Session,
            response: ResponsePolicy::Forbidden,
        },
        SignalDeclaration {
            selector: selector::SESSION_INVITE,
            max_bytes: 2048,
            demand: SignalDemand::Session,
            response: ResponsePolicy::Forbidden,
        },
        SignalDeclaration {
            selector: selector::FILE_OFFER,
            max_bytes: 4096,
            demand: SignalDemand::Session,
            // An offer is not an acceptance. Whether the receiver wants the
            // file is a decision a person makes later, not a protocol answer
            // due inside a deadline.
            response: ResponsePolicy::Forbidden,
        },
        SignalDeclaration {
            selector: selector::WORLD,
            max_bytes: crate::planes::bounds::MAX_SIGNAL_BYTES,
            demand: SignalDemand::Session,
            response: ResponsePolicy::Forbidden,
        },
    ]
}

/// Look up a declaration. `None` is `NotRegistered`, and refusing is correct:
/// a signal nobody declared is one nothing knows how to bound.
pub fn declaration_for(selector: u16) -> Option<SignalDeclaration> {
    core_declarations()
        .into_iter()
        .find(|declaration| declaration.selector == selector)
}

/// Read one reliable signal off its flow.
///
/// The wire is `stream_kind | u16 selector | u32 length | canonical body`, and
/// the **selector precedes the length** for one reason: a declaration's
/// `max_bytes` is only a pre-allocation ceiling if it is known before the
/// length is. Behind the length, the schema is known only after a buffer
/// already exists, and the per-signal maximum becomes decoration.
///
/// `max_bytes` is floored against the plane's own ceiling, so a bad declaration
/// table can lower the limit and never raise it.
///
/// The stream kind is read by the caller, which is what decides this is a
/// signal flow at all.
pub async fn read_signal(
    flow: &mut dyn comms::RecvFlow,
) -> Result<crate::planes::Signal, SignalError> {
    let selector = flow
        .read_exact(2)
        .await
        .map_err(|_| SignalError::Malformed)?;
    let selector = u16::from_le_bytes(
        <[u8; 2]>::try_from(selector.as_slice()).map_err(|_| SignalError::Malformed)?,
    );
    // Resolved before the length is read. An unknown selector is refused here,
    // with nothing allocated and no length even consulted.
    let declaration = declaration_for(selector).ok_or(SignalError::NotRegistered)?;
    let ceiling = declaration
        .max_bytes
        .min(crate::planes::bounds::MAX_SIGNAL_BYTES);

    let header = flow
        .read_exact(4)
        .await
        .map_err(|_| SignalError::Malformed)?;
    let len = u32::from_le_bytes(
        <[u8; 4]>::try_from(header.as_slice()).map_err(|_| SignalError::Malformed)?,
    ) as usize;
    if len > ceiling {
        // Refused by the declared length, before a buffer that size exists.
        return Err(SignalError::TooLarge);
    }
    let body = flow
        .read_exact(len)
        .await
        .map_err(|_| SignalError::Malformed)?;
    let signal =
        crate::planes::Signal::decode_canonical(&body).map_err(|_| SignalError::Malformed)?;
    // The body decoded, and it has to be the signal the selector promised —
    // otherwise a small declaration's ceiling could be used to smuggle a
    // different, larger-bounded shape past it.
    if signal.selector() != selector {
        return Err(SignalError::Malformed);
    }
    Ok(signal)
}

/// Frame one signal for sending, or refuse.
///
/// A `Result` rather than a substituted refusal. `FreightFrame::encode_bounded`
/// substitutes because the alternative there is telling a peer nothing at all;
/// here the alternative is sending something other than what was asked for,
/// which is worse than an error the caller can see.
pub fn frame_signal(signal: &crate::planes::Signal) -> Result<Vec<u8>, SignalError> {
    let selector = signal.selector();
    let declaration = declaration_for(selector).ok_or(SignalError::NotRegistered)?;
    // A bounds failure is a size failure and says so. Collapsing it into
    // `Malformed` would tell a caller its signal was ill-formed when what it
    // actually was is too big — two different things to do about it.
    signal.validate().map_err(|error| match error {
        crate::planes::PlaneWireError::Bounds => SignalError::TooLarge,
        _ => SignalError::Malformed,
    })?;
    let body = signal.encode();
    if body.len()
        > declaration
            .max_bytes
            .min(crate::planes::bounds::MAX_SIGNAL_BYTES)
    {
        return Err(SignalError::TooLarge);
    }
    let mut out = Vec::with_capacity(1 + 2 + 4 + body.len());
    out.push(crate::planes::stream_kind::RELIABLE_SIGNAL);
    out.extend_from_slice(&selector.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}
