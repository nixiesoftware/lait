//! The exec plane: direct Station work and bounded lifecycle flows.
//!
//! This is the E3 foundation: `lait/exec/1` is registered, an opening is
//! judged by the shared admission path — Space and Station binding, protocol
//! generation, operator policy, member standing, replay recognition, and
//! revocation-driven close all come from `plane_driver`/`admission` — and an
//! admitted connection is held. The typed `control`, `output`, `input`, and
//! `link` flows are owned by the remaining E3 deliverables; until they land,
//! every flow a peer opens is stopped loudly rather than left to time out,
//! so an admitted peer learns "nothing is served here yet" at its first
//! write, not at its deadline.
//!
//! What this deliberately does not do yet: read a byte of any flow, name a
//! Spec or Build, mint an Attempt, or answer a readiness challenge. An
//! admitted connection confers standing to converse, never work — a `Try`
//! remains an authorized durable command, and remote incorporation of a
//! committed Run stays inert.

use std::sync::Arc;

use crate::admission::AdmittedPeer;
use crate::lifecycle::CancelToken;
use crate::plane_driver::PlaneService;

/// The application error code a refused flow is stopped with.
///
/// One value, stable on purpose: a peer on this generation reading this code
/// knows the plane is mounted but the flow vocabulary is not served, which is
/// a different fact from a reset connection or a missing ALPN.
pub const FLOW_NOT_SERVED: u32 = 0x4E53; // "NS"

/// The exec plane's connection service.
///
/// Stateless in the foundation: everything a connection is entitled to was
/// decided at admission, and nothing served yet accumulates per-peer state.
#[derive(Debug, Default, Clone, Copy)]
pub struct Service;

impl Service {
    pub fn new() -> Self {
        Self
    }
}

impl PlaneService for Service {
    async fn serve(
        &self,
        connection: Arc<dyn comms::Connection>,
        _peer: AdmittedPeer,
        cancel: CancelToken,
    ) {
        // Hold the admitted connection and refuse every flow loudly. Both
        // accept queues are polled so a peer probing with either flow shape
        // gets the same answer. The cancel check rides the poll interval:
        // the driver also races this future against revocation, so the beat
        // here only bounds how long a quiet shutdown waits.
        loop {
            if cancel.is_cancelled() {
                return;
            }
            tokio::select! {
                accepted = connection.accept_bi() => match accepted {
                    Ok(Some((mut send, mut recv))) => {
                        send.reset(FLOW_NOT_SERVED);
                        recv.stop(FLOW_NOT_SERVED);
                    }
                    Ok(None) | Err(_) => return,
                },
                accepted = connection.accept_uni() => match accepted {
                    Ok(Some(mut recv)) => recv.stop(FLOW_NOT_SERVED),
                    Ok(None) | Err(_) => return,
                },
                () = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
            }
        }
    }
}
