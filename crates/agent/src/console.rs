//! Crash-safe operation identity for an agent's Console.
//!
//! A command sent through correspondence is an effect, not an exactly-once
//! transaction. This ledger gives it a stable identity and a deliberately
//! asymmetric recovery rule: `Accepted` work may still be claimed for first
//! dispatch, while a `Dispatched` operation becomes `OutcomeUnknown` after a
//! restart. It is never silently run a second time.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt as _;
use mechanics::kinship::ProfileId;
use serde::{Deserialize, Serialize};

use crate::{Error, OwnershipBond};

const MAGIC: &[u8; 8] = b"LAITCON1";
const ENVELOPE_VERSION: u8 = 1;
const LEDGER_VERSION: u16 = 3;
const LEGACY_LEDGER_VERSION: u16 = 2;
const PREFIX: usize = 8 + 1 + 4;
const MAX_WRAPPED_OVERHEAD: u64 = 16 * 1024;
// V2 writers were limited to 32 MiB. Keep bounded migration headroom for the
// V3 coordinate tombstones materialized while decoding a maximally sized V2
// ledger, so the first post-migration mutation cannot fail solely because the
// safer representation is slightly larger.
const MAX_LEDGER_BYTES: usize = 40 * 1024 * 1024;
pub const MAX_CONSOLE_OPERATIONS: usize = 4_096;
pub const MAX_CONSUMED_CONSOLE_OPERATIONS: usize = 8_192;
pub const MAX_CONSOLE_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_CONSOLE_REPLY_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_CONSOLE_COORDINATE_BYTES: usize = 512;

/// Stable id supplied by the correspondence adapter and retained on retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ConsoleOperationId(pub [u8; 16]);

/// Exact, non-retargetable execution coordinates accepted with an owner
/// message. Presentation names never select a different World or Build later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleExecutionBinding {
    pub space: String,
    pub world: String,
    pub world_implementation: [u8; 32],
    pub spec: String,
    pub spec_version: u32,
    pub build: [u8; 32],
    pub image: String,
    pub enforcement: [u8; 32],
    pub run: [u8; 16],
}

impl ConsoleExecutionBinding {
    fn validate(&self) -> Result<(), Error> {
        for (name, value) in [
            ("console Space", self.space.as_str()),
            ("console World", self.world.as_str()),
            ("console Spec", self.spec.as_str()),
            ("console image", self.image.as_str()),
        ] {
            if value.is_empty() || value.len() > MAX_CONSOLE_COORDINATE_BYTES {
                return Err(Error::Bound(name));
            }
        }
        if self.spec_version == 0 {
            return Err(Error::Invalid("console Spec version"));
        }
        Ok(())
    }
}

/// Immutable facts bound before an effect can be dispatched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleOperationInput {
    pub id: ConsoleOperationId,
    pub sender: ProfileId,
    pub agent: ProfileId,
    pub generation: u64,
    pub sequence: u64,
    pub payload: Vec<u8>,
    pub accepted_at: u64,
    pub execution: ConsoleExecutionBinding,
}

impl ConsoleOperationInput {
    fn validate(&self) -> Result<(), Error> {
        if self.sender == self.agent {
            return Err(Error::Invalid("console sender and agent are not distinct"));
        }
        if self.payload.is_empty() {
            return Err(Error::Invalid("empty console input"));
        }
        if self.payload.len() > MAX_CONSOLE_INPUT_BYTES {
            return Err(Error::Bound("console input"));
        }
        self.execution.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleCompletion {
    pub attempt: [u8; 16],
    pub transcript_cursor: u64,
    pub exit_code: Option<i32>,
    pub completed_at: u64,
}

fn validate_reply_body(body: &[u8]) -> Result<(), Error> {
    if body.is_empty() || body.len() > MAX_CONSOLE_REPLY_BYTES {
        return Err(Error::Bound("console reply"));
    }
    Ok(())
}

/// Reply delivery is a separate effect from execution. A crash after claiming
/// a send is honest uncertainty; it never authorizes a duplicate message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsoleReplyStanding {
    None,
    Prepared {
        body: Vec<u8>,
        prepared_at: u64,
    },
    Sending {
        body: Vec<u8>,
        prepared_at: u64,
        claimed_at: u64,
    },
    Sent {
        deposit_id: String,
        sent_at: u64,
    },
    OutcomeUnknown {
        observed_at: u64,
    },
}

/// The only effect lifecycle the Console may report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsoleStanding {
    Accepted,
    Dispatched {
        dispatched_at: u64,
        attempt: Option<[u8; 16]>,
        transcript_cursor: u64,
    },
    Completed(ConsoleCompletion),
    Failed {
        attempt: [u8; 16],
        class: String,
        observed_at: u64,
    },
    OutcomeUnknown {
        transcript_cursor: u64,
        observed_at: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleOperation {
    pub input: ConsoleOperationInput,
    pub standing: ConsoleStanding,
    pub reply: ConsoleReplyStanding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LedgerState {
    version: u16,
    consumed: BTreeMap<ConsoleOperationId, (u64, u64)>,
    operations: Vec<ConsoleOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyLedgerStateV2 {
    version: u16,
    operations: Vec<ConsoleOperation>,
}

impl Default for LedgerState {
    fn default() -> Self {
        Self {
            version: LEDGER_VERSION,
            consumed: BTreeMap::new(),
            operations: Vec::new(),
        }
    }
}

impl LedgerState {
    fn validate(&self) -> Result<(), Error> {
        if self.version != LEDGER_VERSION {
            return Err(Error::UnsupportedVersion {
                artifact: "console ledger",
                found: self.version,
            });
        }
        if self.operations.len() > MAX_CONSOLE_OPERATIONS {
            return Err(Error::Bound("console operations"));
        }
        if self.consumed.len() > MAX_CONSUMED_CONSOLE_OPERATIONS {
            return Err(Error::Bound("consumed console operations"));
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut coordinates = std::collections::BTreeSet::new();
        for operation in &self.operations {
            operation.input.validate()?;
            if let ConsoleStanding::Failed { class, .. } = &operation.standing {
                if class.is_empty() || class.len() > MAX_CONSOLE_COORDINATE_BYTES {
                    return Err(Error::Bound("console failure class"));
                }
            }
            match &operation.reply {
                ConsoleReplyStanding::Prepared { body, .. }
                | ConsoleReplyStanding::Sending { body, .. } => {
                    if body.is_empty() || body.len() > MAX_CONSOLE_REPLY_BYTES {
                        return Err(Error::Bound("console reply"));
                    }
                }
                ConsoleReplyStanding::Sent { deposit_id, .. } => {
                    if deposit_id.is_empty() || deposit_id.len() > MAX_CONSOLE_COORDINATE_BYTES {
                        return Err(Error::Bound("console reply deposit id"));
                    }
                }
                ConsoleReplyStanding::None | ConsoleReplyStanding::OutcomeUnknown { .. } => {}
            }
            if !ids.insert(operation.input.id) {
                return Err(Error::Corrupt("duplicate console operation id"));
            }
            if self.consumed.get(&operation.input.id)
                != Some(&(operation.input.generation, operation.input.sequence))
            {
                return Err(Error::Corrupt("active console operation is not consumed"));
            }
            if !coordinates.insert((operation.input.generation, operation.input.sequence)) {
                return Err(Error::Corrupt("duplicate console operation sequence"));
            }
        }
        Ok(())
    }
}

/// Private ledger rooted at `<agent home>/agent/console`.
pub struct ConsoleLedger {
    dir: PathBuf,
    state: PathBuf,
    temporary: PathBuf,
    lock: PathBuf,
}

impl ConsoleLedger {
    #[must_use]
    pub fn at(agent_home: &Path) -> Self {
        let dir = agent_home.join("agent").join("console");
        Self {
            state: dir.join("operations.bin"),
            temporary: dir.join("operations.tmp"),
            lock: dir.join("operations.lock"),
            dir,
        }
    }

    pub fn list(&self) -> Result<Vec<ConsoleOperation>, Error> {
        self.with_state(false, |state| Ok((state.operations.clone(), false)))
    }

    /// One bounded snapshot for inbox de-duplication. Callers can skip all
    /// historical correspondence without reopening the ledger per message.
    pub fn known_operation_ids(&self) -> Result<BTreeSet<ConsoleOperationId>, Error> {
        self.with_state(false, |state| {
            Ok((state.consumed.keys().copied().collect(), false))
        })
    }

    /// Bind an id before dispatch. Repeating the exact same operation returns
    /// its current disposition; reusing its id or sequence for different work
    /// is an explicit collision.
    pub fn accept(
        &self,
        ownership: &OwnershipBond,
        input: ConsoleOperationInput,
    ) -> Result<ConsoleOperation, Error> {
        ownership.verify_signatures()?;
        input.validate()?;
        if &input.sender != ownership.owner() || &input.agent != ownership.agent() {
            return Err(Error::Unauthorized);
        }
        self.with_state(true, |state| {
            if let Some(held) = state
                .operations
                .iter()
                .find(|operation| operation.input.id == input.id)
            {
                return if held.input == input {
                    Ok((held.clone(), false))
                } else {
                    Err(Error::Invalid("console operation id collision"))
                };
            }
            if state.consumed.contains_key(&input.id) {
                return Err(Error::Invalid("console operation was already consumed"));
            }
            if state
                .consumed
                .values()
                .any(|held| held == &(input.generation, input.sequence))
            {
                return Err(Error::Invalid("console operation sequence collision"));
            }
            if state.operations.len() == MAX_CONSOLE_OPERATIONS {
                return Err(Error::Bound("console operations"));
            }
            if state.consumed.len() == MAX_CONSUMED_CONSOLE_OPERATIONS {
                return Err(Error::Bound("consumed console operations"));
            }
            let operation = ConsoleOperation {
                input,
                standing: ConsoleStanding::Accepted,
                reply: ConsoleReplyStanding::None,
            };
            state.consumed.insert(
                operation.input.id,
                (operation.input.generation, operation.input.sequence),
            );
            state.operations.push(operation.clone());
            Ok((operation, true))
        })
    }

    /// Atomically claim first dispatch. A duplicate call observes the standing
    /// already recorded and never authorizes another dispatch.
    pub fn claim_dispatch(
        &self,
        id: ConsoleOperationId,
        dispatched_at: u64,
    ) -> Result<ConsoleOperation, Error> {
        self.transition(id, |standing| match standing {
            ConsoleStanding::Accepted => Ok((
                ConsoleStanding::Dispatched {
                    dispatched_at,
                    attempt: None,
                    transcript_cursor: 0,
                },
                true,
            )),
            held => Ok((held.clone(), false)),
        })
    }

    /// Bind the exact Runtime Attempt observed after the dispatch claim. A
    /// second, different Attempt is retargeting and is refused.
    pub fn bind_attempt(
        &self,
        id: ConsoleOperationId,
        attempt: [u8; 16],
    ) -> Result<ConsoleOperation, Error> {
        self.transition(id, |standing| match standing {
            ConsoleStanding::Dispatched {
                dispatched_at,
                attempt: None,
                transcript_cursor,
            } => Ok((
                ConsoleStanding::Dispatched {
                    dispatched_at: *dispatched_at,
                    attempt: Some(attempt),
                    transcript_cursor: *transcript_cursor,
                },
                true,
            )),
            ConsoleStanding::Dispatched {
                attempt: Some(held),
                ..
            } if held == &attempt => Ok((standing.clone(), false)),
            ConsoleStanding::Dispatched { .. } => {
                Err(Error::Invalid("console Attempt changed after dispatch"))
            }
            _ => Err(Error::Invalid("console operation is not dispatched")),
        })
    }

    /// Persist observed output progress without treating it as completion.
    pub fn advance_cursor(
        &self,
        id: ConsoleOperationId,
        transcript_cursor: u64,
    ) -> Result<ConsoleOperation, Error> {
        self.transition(id, |standing| match standing {
            ConsoleStanding::Dispatched {
                dispatched_at,
                attempt,
                transcript_cursor: held,
            } if transcript_cursor >= *held => Ok((
                ConsoleStanding::Dispatched {
                    dispatched_at: *dispatched_at,
                    attempt: *attempt,
                    transcript_cursor,
                },
                transcript_cursor != *held,
            )),
            ConsoleStanding::Dispatched { .. } => {
                Err(Error::Invalid("console transcript cursor moved backwards"))
            }
            _ => Err(Error::Invalid("console operation is not dispatched")),
        })
    }

    pub fn prepare_reply(
        &self,
        id: ConsoleOperationId,
        body: Vec<u8>,
        prepared_at: u64,
    ) -> Result<ConsoleOperation, Error> {
        validate_reply_body(&body)?;
        self.transition_operation(id, |operation| {
            if !matches!(
                operation.standing,
                ConsoleStanding::Completed(_)
                    | ConsoleStanding::Failed { .. }
                    | ConsoleStanding::OutcomeUnknown { .. }
            ) {
                return Err(Error::Invalid("console execution is not terminal"));
            }
            match &operation.reply {
                ConsoleReplyStanding::None => {
                    operation.reply = ConsoleReplyStanding::Prepared { body, prepared_at };
                    Ok(true)
                }
                ConsoleReplyStanding::Prepared {
                    body: held,
                    prepared_at: at,
                } if held == &body && *at == prepared_at => Ok(false),
                _ => Err(Error::Invalid("console reply is already prepared")),
            }
        })
    }

    /// Claim the one outbound effect before touching correspondence.
    pub fn claim_reply_send(
        &self,
        id: ConsoleOperationId,
        claimed_at: u64,
    ) -> Result<ConsoleOperation, Error> {
        self.transition_operation(id, |operation| match &operation.reply {
            ConsoleReplyStanding::Prepared { body, prepared_at } => {
                operation.reply = ConsoleReplyStanding::Sending {
                    body: body.clone(),
                    prepared_at: *prepared_at,
                    claimed_at,
                };
                Ok(true)
            }
            ConsoleReplyStanding::Sending { .. }
            | ConsoleReplyStanding::Sent { .. }
            | ConsoleReplyStanding::OutcomeUnknown { .. } => Ok(false),
            ConsoleReplyStanding::None => Err(Error::Invalid("console reply is not prepared")),
        })
    }

    pub fn mark_reply_sent(
        &self,
        id: ConsoleOperationId,
        deposit_id: String,
        sent_at: u64,
    ) -> Result<ConsoleOperation, Error> {
        if deposit_id.is_empty() || deposit_id.len() > MAX_CONSOLE_COORDINATE_BYTES {
            return Err(Error::Bound("console reply deposit id"));
        }
        self.transition_operation(id, |operation| match &operation.reply {
            ConsoleReplyStanding::Sending { .. } => {
                operation.reply = ConsoleReplyStanding::Sent {
                    deposit_id: deposit_id.clone(),
                    sent_at,
                };
                Ok(true)
            }
            ConsoleReplyStanding::Sent {
                deposit_id: held,
                sent_at: at,
            } if held == &deposit_id && *at == sent_at => Ok(false),
            _ => Err(Error::Invalid("console reply send was not claimed")),
        })
    }

    /// Seal an already-claimed correspondence deposit whose outcome cannot be
    /// proven. An opaque transport error is not evidence that no deposit took
    /// place, so the body must not be retried.
    pub fn mark_reply_outcome_unknown(
        &self,
        id: ConsoleOperationId,
        observed_at: u64,
    ) -> Result<ConsoleOperation, Error> {
        self.transition_operation(id, |operation| match &operation.reply {
            ConsoleReplyStanding::Sending { .. } => {
                operation.reply = ConsoleReplyStanding::OutcomeUnknown { observed_at };
                Ok(true)
            }
            ConsoleReplyStanding::OutcomeUnknown { observed_at: held } if *held == observed_at => {
                Ok(false)
            }
            _ => Err(Error::Invalid("console reply send was not claimed")),
        })
    }

    pub fn complete(
        &self,
        id: ConsoleOperationId,
        completion: ConsoleCompletion,
    ) -> Result<ConsoleOperation, Error> {
        self.transition(id, |standing| match standing {
            ConsoleStanding::Dispatched {
                attempt: Some(held),
                transcript_cursor,
                ..
            } if held == &completion.attempt
                && completion.transcript_cursor >= *transcript_cursor =>
            {
                Ok((ConsoleStanding::Completed(completion.clone()), true))
            }
            ConsoleStanding::Dispatched { attempt: None, .. } => {
                Err(Error::Invalid("console Attempt is not bound"))
            }
            ConsoleStanding::Dispatched {
                attempt: Some(held),
                ..
            } if held != &completion.attempt => {
                Err(Error::Invalid("console completion Attempt changed"))
            }
            ConsoleStanding::Dispatched { .. } => {
                Err(Error::Invalid("console completion cursor moved backwards"))
            }
            ConsoleStanding::Completed(held) if held == &completion => {
                Ok((standing.clone(), false))
            }
            _ => Err(Error::Invalid("console operation is not dispatched")),
        })
    }

    /// Record terminal execution and its deterministic outbound reply in one
    /// ledger replacement. A crash can therefore observe neither fact or both,
    /// never a completed command that has permanently lost its reply.
    pub fn complete_with_reply(
        &self,
        id: ConsoleOperationId,
        completion: ConsoleCompletion,
        body: Vec<u8>,
        prepared_at: u64,
    ) -> Result<ConsoleOperation, Error> {
        validate_reply_body(&body)?;
        self.transition_operation(id, |operation| {
            let standing_changed = match &operation.standing {
                ConsoleStanding::Dispatched {
                    attempt: Some(held),
                    transcript_cursor,
                    ..
                } if held == &completion.attempt
                    && completion.transcript_cursor >= *transcript_cursor =>
                {
                    operation.standing = ConsoleStanding::Completed(completion.clone());
                    true
                }
                ConsoleStanding::Dispatched { attempt: None, .. } => {
                    return Err(Error::Invalid("console Attempt is not bound"))
                }
                ConsoleStanding::Dispatched {
                    attempt: Some(held),
                    ..
                } if held != &completion.attempt => {
                    return Err(Error::Invalid("console completion Attempt changed"))
                }
                ConsoleStanding::Dispatched { .. } => {
                    return Err(Error::Invalid("console completion cursor moved backwards"))
                }
                ConsoleStanding::Completed(held) if held == &completion => false,
                _ => return Err(Error::Invalid("console operation is not dispatched")),
            };
            let reply_changed = match &operation.reply {
                ConsoleReplyStanding::None => {
                    operation.reply = ConsoleReplyStanding::Prepared { body, prepared_at };
                    true
                }
                ConsoleReplyStanding::Prepared {
                    body: held,
                    prepared_at: at,
                } if held == &body && *at == prepared_at => false,
                _ => return Err(Error::Invalid("console reply is already prepared")),
            };
            Ok(standing_changed || reply_changed)
        })
    }

    pub fn mark_outcome_unknown(
        &self,
        id: ConsoleOperationId,
        transcript_cursor: u64,
        observed_at: u64,
    ) -> Result<ConsoleOperation, Error> {
        self.transition(id, |standing| match standing {
            ConsoleStanding::Dispatched {
                transcript_cursor: held,
                ..
            } if transcript_cursor >= *held => Ok((
                ConsoleStanding::OutcomeUnknown {
                    transcript_cursor,
                    observed_at,
                },
                true,
            )),
            ConsoleStanding::Dispatched { .. } => {
                Err(Error::Invalid("console unknown cursor moved backwards"))
            }
            ConsoleStanding::OutcomeUnknown {
                transcript_cursor: held,
                observed_at: at,
            } if *held == transcript_cursor && *at == observed_at => Ok((standing.clone(), false)),
            _ => Err(Error::Invalid("console operation is not dispatched")),
        })
    }

    pub fn fail(
        &self,
        id: ConsoleOperationId,
        attempt: [u8; 16],
        class: String,
        observed_at: u64,
    ) -> Result<ConsoleOperation, Error> {
        if class.is_empty() || class.len() > MAX_CONSOLE_COORDINATE_BYTES {
            return Err(Error::Bound("console failure class"));
        }
        self.transition(id, |standing| match standing {
            ConsoleStanding::Dispatched {
                attempt: Some(held),
                ..
            } if held == &attempt => Ok((
                ConsoleStanding::Failed {
                    attempt,
                    class: class.clone(),
                    observed_at,
                },
                true,
            )),
            ConsoleStanding::Failed {
                attempt: held,
                class: held_class,
                observed_at: held_at,
            } if held == &attempt && held_class == &class && *held_at == observed_at => {
                Ok((standing.clone(), false))
            }
            ConsoleStanding::Dispatched { attempt: None, .. } => {
                Err(Error::Invalid("console Attempt is not bound"))
            }
            ConsoleStanding::Dispatched { .. } => {
                Err(Error::Invalid("console failure Attempt changed"))
            }
            _ => Err(Error::Invalid("console operation is not dispatched")),
        })
    }

    /// Failure and its explanatory reply are one durable transition for the
    /// same reason as [`Self::complete_with_reply`].
    pub fn fail_with_reply(
        &self,
        id: ConsoleOperationId,
        attempt: [u8; 16],
        class: String,
        observed_at: u64,
        body: Vec<u8>,
        prepared_at: u64,
    ) -> Result<ConsoleOperation, Error> {
        if class.is_empty() || class.len() > MAX_CONSOLE_COORDINATE_BYTES {
            return Err(Error::Bound("console failure class"));
        }
        validate_reply_body(&body)?;
        self.transition_operation(id, |operation| {
            let standing_changed = match &operation.standing {
                ConsoleStanding::Dispatched {
                    attempt: Some(held),
                    ..
                } if held == &attempt => {
                    operation.standing = ConsoleStanding::Failed {
                        attempt,
                        class: class.clone(),
                        observed_at,
                    };
                    true
                }
                ConsoleStanding::Failed {
                    attempt: held,
                    class: held_class,
                    observed_at: held_at,
                } if held == &attempt && held_class == &class && *held_at == observed_at => false,
                ConsoleStanding::Dispatched { attempt: None, .. } => {
                    return Err(Error::Invalid("console Attempt is not bound"))
                }
                ConsoleStanding::Dispatched { .. } => {
                    return Err(Error::Invalid("console failure Attempt changed"))
                }
                _ => return Err(Error::Invalid("console operation is not dispatched")),
            };
            let reply_changed = match &operation.reply {
                ConsoleReplyStanding::None => {
                    operation.reply = ConsoleReplyStanding::Prepared { body, prepared_at };
                    true
                }
                ConsoleReplyStanding::Prepared {
                    body: held,
                    prepared_at: at,
                } if held == &body && *at == prepared_at => false,
                _ => return Err(Error::Invalid("console reply is already prepared")),
            };
            Ok(standing_changed || reply_changed)
        })
    }

    /// Startup reconciliation for a backend that cannot prove the prior
    /// process outcome. Accepted work remains eligible for first dispatch;
    /// every dispatched effect becomes unknown and is never replayed.
    pub fn recover_unreconciled(&self, observed_at: u64) -> Result<Vec<ConsoleOperation>, Error> {
        self.with_state(true, |state| {
            let mut changed = false;
            for operation in &mut state.operations {
                if let ConsoleStanding::Dispatched {
                    transcript_cursor, ..
                } = operation.standing
                {
                    operation.standing = ConsoleStanding::OutcomeUnknown {
                        transcript_cursor,
                        observed_at,
                    };
                    changed = true;
                }
                if matches!(operation.reply, ConsoleReplyStanding::Sending { .. }) {
                    operation.reply = ConsoleReplyStanding::OutcomeUnknown { observed_at };
                    changed = true;
                }
            }
            Ok((state.operations.clone(), changed))
        })
    }

    /// Recover only the correspondence reply outbox. Runtime dispatches are
    /// left intact until their exact durable Run can be inspected.
    pub fn recover_reply_sends(&self, observed_at: u64) -> Result<Vec<ConsoleOperation>, Error> {
        self.with_state(true, |state| {
            let mut changed = false;
            for operation in &mut state.operations {
                if matches!(operation.reply, ConsoleReplyStanding::Sending { .. }) {
                    operation.reply = ConsoleReplyStanding::OutcomeUnknown { observed_at };
                    changed = true;
                }
            }
            Ok((state.operations.clone(), changed))
        })
    }

    /// Drop heavyweight settled records while retaining their durable ids as
    /// replay tombstones. The tombstone horizon matches Reach's bounded inbox,
    /// so every correspondence item that can still be presented remains
    /// recognizable after compaction.
    pub fn compact_finalized(&self) -> Result<usize, Error> {
        self.with_state(true, |state| {
            let before = state.operations.len();
            state.operations.retain(|operation| {
                let terminal = matches!(
                    operation.standing,
                    ConsoleStanding::Completed(_)
                        | ConsoleStanding::Failed { .. }
                        | ConsoleStanding::OutcomeUnknown { .. }
                );
                let reply_settled = matches!(
                    operation.reply,
                    ConsoleReplyStanding::Sent { .. } | ConsoleReplyStanding::OutcomeUnknown { .. }
                );
                !(terminal && reply_settled)
            });
            let removed = before.saturating_sub(state.operations.len());
            Ok((removed, removed != 0))
        })
    }

    fn transition(
        &self,
        id: ConsoleOperationId,
        change: impl FnOnce(&ConsoleStanding) -> Result<(ConsoleStanding, bool), Error>,
    ) -> Result<ConsoleOperation, Error> {
        self.with_state(true, |state| {
            let operation = state
                .operations
                .iter_mut()
                .find(|operation| operation.input.id == id)
                .ok_or(Error::Invalid("console operation does not exist"))?;
            let (standing, changed) = change(&operation.standing)?;
            operation.standing = standing;
            Ok((operation.clone(), changed))
        })
    }

    fn transition_operation(
        &self,
        id: ConsoleOperationId,
        change: impl FnOnce(&mut ConsoleOperation) -> Result<bool, Error>,
    ) -> Result<ConsoleOperation, Error> {
        self.with_state(true, |state| {
            let operation = state
                .operations
                .iter_mut()
                .find(|operation| operation.input.id == id)
                .ok_or(Error::Invalid("console operation does not exist"))?;
            let changed = change(operation)?;
            Ok((operation.clone(), changed))
        })
    }

    fn with_state<T>(
        &self,
        prepare: bool,
        access: impl FnOnce(&mut LedgerState) -> Result<(T, bool), Error>,
    ) -> Result<T, Error> {
        if prepare || self.dir.exists() {
            mechanics::secretfs::create_private_dir(&self.dir)
                .map_err(|error| Error::Storage(error.to_string()))?;
        } else {
            return access(&mut LedgerState::default()).map(|(value, _)| value);
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.lock)?;
        lock.lock_exclusive()?;
        let mut state = self.load_unlocked()?;
        let (value, changed) = access(&mut state)?;
        state.validate()?;
        if changed {
            self.write_unlocked(&state)?;
        }
        drop(lock);
        Ok(value)
    }

    fn load_unlocked(&self) -> Result<LedgerState, Error> {
        if self.state.exists() {
            return read_path(&self.state);
        }
        if self.temporary.exists() {
            let recovered = read_path(&self.temporary)?;
            mechanics::secretfs::persist_replace(&self.temporary, &self.state)?;
            return Ok(recovered);
        }
        Ok(LedgerState::default())
    }

    fn write_unlocked(&self, state: &LedgerState) -> Result<(), Error> {
        let bytes = encode(state)?;
        mechanics::secretfs::write_private(
            &self.temporary,
            &bytes,
            mechanics::secretfs::Create::Replace,
            mechanics::secretfs::Wrap::Portable,
        )
        .map_err(|error| Error::Storage(error.to_string()))?;
        if read_path(&self.temporary)? != *state {
            return Err(Error::Corrupt(
                "temporary console ledger changed while writing",
            ));
        }
        mechanics::secretfs::persist_replace(&self.temporary, &self.state)?;
        Ok(())
    }
}

fn read_path(path: &Path) -> Result<LedgerState, Error> {
    let metadata = fs::metadata(path)?;
    let max = u64::try_from(MAX_LEDGER_BYTES)
        .map_err(|_| Error::Bound("console ledger envelope"))?
        .checked_add(MAX_WRAPPED_OVERHEAD)
        .ok_or(Error::Bound("console ledger envelope"))?;
    if metadata.len() > max {
        return Err(Error::Bound("console ledger envelope"));
    }
    let bytes = mechanics::secretfs::read_private(path)
        .map_err(|error| Error::Storage(error.to_string()))?
        .ok_or(Error::Corrupt("console ledger disappeared while reading"))?;
    if bytes.len() > MAX_LEDGER_BYTES {
        return Err(Error::Bound("console ledger envelope"));
    }
    decode(&bytes)
}

fn encode(state: &LedgerState) -> Result<Vec<u8>, Error> {
    state.validate()?;
    let body = postcard::to_stdvec(state).map_err(|_| Error::Corrupt("console ledger encode"))?;
    let body_len = u32::try_from(body.len()).map_err(|_| Error::Bound("console ledger body"))?;
    let total = PREFIX
        .checked_add(body.len())
        .and_then(|length| length.checked_add(32))
        .ok_or(Error::Bound("console ledger envelope"))?;
    if total > MAX_LEDGER_BYTES {
        return Err(Error::Bound("console ledger envelope"));
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(MAGIC);
    out.push(ENVELOPE_VERSION);
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(blake3::hash(&out).as_bytes());
    Ok(out)
}

fn decode(bytes: &[u8]) -> Result<LedgerState, Error> {
    if bytes.len() < PREFIX + 32 || bytes.get(..8) != Some(MAGIC.as_slice()) {
        return Err(Error::Corrupt("console ledger envelope"));
    }
    let envelope = *bytes
        .get(8)
        .ok_or(Error::Corrupt("console ledger version"))?;
    if envelope != ENVELOPE_VERSION {
        return Err(Error::UnsupportedVersion {
            artifact: "console ledger envelope",
            found: u16::from(envelope),
        });
    }
    let body_len = bytes
        .get(9..13)
        .and_then(|part| <[u8; 4]>::try_from(part).ok())
        .map(u32::from_le_bytes)
        .and_then(|length| usize::try_from(length).ok())
        .ok_or(Error::Corrupt("console ledger length"))?;
    let body_end = PREFIX
        .checked_add(body_len)
        .ok_or(Error::Corrupt("console ledger length"))?;
    let expected = body_end
        .checked_add(32)
        .ok_or(Error::Corrupt("console ledger length"))?;
    if expected != bytes.len() {
        return Err(Error::Corrupt("console ledger length"));
    }
    let digest = bytes
        .get(body_end..expected)
        .ok_or(Error::Corrupt("console ledger digest"))?;
    let signed = bytes
        .get(..body_end)
        .ok_or(Error::Corrupt("console ledger digest"))?;
    if blake3::hash(signed).as_bytes() != digest {
        return Err(Error::Corrupt("console ledger digest"));
    }
    let body = bytes
        .get(PREFIX..body_end)
        .ok_or(Error::Corrupt("console ledger body"))?;
    let state: LedgerState = if let Ok(state) = postcard::from_bytes::<LedgerState>(body) {
        if postcard::to_stdvec(&state).map_err(|_| Error::Corrupt("console ledger encode"))? != body
        {
            return Err(Error::Corrupt("non-canonical console ledger"));
        }
        state
    } else {
        let legacy: LegacyLedgerStateV2 =
            postcard::from_bytes(body).map_err(|_| Error::Corrupt("console ledger decode"))?;
        if legacy.version != LEGACY_LEDGER_VERSION {
            return Err(Error::UnsupportedVersion {
                artifact: "console ledger",
                found: legacy.version,
            });
        }
        if postcard::to_stdvec(&legacy).map_err(|_| Error::Corrupt("console ledger encode"))?
            != body
        {
            return Err(Error::Corrupt("non-canonical console ledger"));
        }
        LedgerState {
            version: LEDGER_VERSION,
            consumed: legacy
                .operations
                .iter()
                .map(|operation| {
                    (
                        operation.input.id,
                        (operation.input.generation, operation.input.sequence),
                    )
                })
                .collect(),
            operations: legacy.operations,
        }
    };
    state.validate()?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OwnershipRole, OwnershipTerms};

    fn profile(byte: u8) -> ProfileId {
        ProfileId::from_digest([byte; 16])
    }

    fn input(id: u8, sequence: u64) -> ConsoleOperationInput {
        ConsoleOperationInput {
            id: ConsoleOperationId([id; 16]),
            sender: profile(1),
            agent: profile(2),
            generation: 7,
            sequence,
            payload: b"printf hello".to_vec(),
            accepted_at: 10,
            execution: ConsoleExecutionBinding {
                space: "agent-system".into(),
                world: "lait.console".into(),
                world_implementation: [3; 32],
                spec: "lait.console.command".into(),
                spec_version: 1,
                build: [4; 32],
                image: "sha256:fixed".into(),
                enforcement: [5; 32],
                run: [6; 16],
            },
        }
    }

    fn ownership() -> OwnershipBond {
        let agent_seed = [2; 32];
        let owner_seed = [1; 32];
        let terms = OwnershipTerms::new(profile(2), profile(1), 1, [9; 16]);
        OwnershipBond::assemble(
            terms.clone(),
            terms
                .sign(OwnershipRole::Agent, &agent_seed)
                .expect("agent half"),
            terms
                .sign(OwnershipRole::Owner, &owner_seed)
                .expect("owner half"),
            &[mechanics::actor::device_from_seed(&agent_seed)],
            &[mechanics::actor::device_from_seed(&owner_seed)],
        )
        .expect("ownership")
    }

    #[test]
    fn restart_never_replays_a_dispatched_effect() {
        let root = tempfile::tempdir().expect("temporary home");
        let ledger = ConsoleLedger::at(root.path());
        let accepted = ledger.accept(&ownership(), input(3, 1)).expect("accept");
        assert_eq!(accepted.standing, ConsoleStanding::Accepted);
        let dispatched = ledger
            .claim_dispatch(ConsoleOperationId([3; 16]), 11)
            .expect("dispatch");
        assert!(matches!(
            dispatched.standing,
            ConsoleStanding::Dispatched { .. }
        ));
        ledger
            .advance_cursor(ConsoleOperationId([3; 16]), 9)
            .expect("advance output cursor");

        let restarted = ConsoleLedger::at(root.path());
        let recovered = restarted.recover_unreconciled(12).expect("recover");
        assert!(matches!(
            recovered[0].standing,
            ConsoleStanding::OutcomeUnknown {
                observed_at: 12,
                transcript_cursor: 9,
            }
        ));
        let duplicate = restarted
            .claim_dispatch(ConsoleOperationId([3; 16]), 13)
            .expect("observe prior disposition");
        assert!(matches!(
            duplicate.standing,
            ConsoleStanding::OutcomeUnknown { .. }
        ));
    }

    #[test]
    fn exact_redelivery_is_idempotent_but_id_and_sequence_collisions_fail() {
        let root = tempfile::tempdir().expect("temporary home");
        let ledger = ConsoleLedger::at(root.path());
        let bond = ownership();
        let first = input(3, 1);
        assert_eq!(
            ledger.accept(&bond, first.clone()).expect("first"),
            ledger.accept(&bond, first).expect("same")
        );

        let mut collision = input(3, 1);
        collision.payload = b"different".to_vec();
        assert!(matches!(
            ledger.accept(&bond, collision),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            ledger.accept(&bond, input(4, 1)),
            Err(Error::Invalid(_))
        ));
        let mut wrong_sender = input(5, 2);
        wrong_sender.sender = profile(8);
        assert!(matches!(
            ledger.accept(&bond, wrong_sender),
            Err(Error::Unauthorized)
        ));
    }

    #[test]
    fn only_dispatched_work_can_complete_and_completion_is_idempotent() {
        let root = tempfile::tempdir().expect("temporary home");
        let ledger = ConsoleLedger::at(root.path());
        ledger.accept(&ownership(), input(3, 1)).expect("accept");
        let completion = ConsoleCompletion {
            attempt: [8; 16],
            transcript_cursor: 42,
            exit_code: Some(0),
            completed_at: 15,
        };
        assert!(ledger
            .complete(ConsoleOperationId([3; 16]), completion.clone())
            .is_err());
        ledger
            .claim_dispatch(ConsoleOperationId([3; 16]), 11)
            .expect("dispatch");
        ledger
            .bind_attempt(ConsoleOperationId([3; 16]), [8; 16])
            .expect("bind Attempt");
        let first = ledger
            .complete(ConsoleOperationId([3; 16]), completion.clone())
            .expect("complete");
        let second = ledger
            .complete(ConsoleOperationId([3; 16]), completion)
            .expect("repeat");
        assert_eq!(first, second);
    }

    #[test]
    fn terminal_execution_and_reply_are_committed_together() {
        let root = tempfile::tempdir().expect("temporary home");
        let ledger = ConsoleLedger::at(root.path());
        ledger.accept(&ownership(), input(3, 1)).expect("accept");
        ledger
            .claim_dispatch(ConsoleOperationId([3; 16]), 11)
            .expect("dispatch");
        ledger
            .bind_attempt(ConsoleOperationId([3; 16]), [8; 16])
            .expect("bind Attempt");
        let completed = ledger
            .complete_with_reply(
                ConsoleOperationId([3; 16]),
                ConsoleCompletion {
                    attempt: [8; 16],
                    transcript_cursor: 42,
                    exit_code: Some(0),
                    completed_at: 15,
                },
                b"reply".to_vec(),
                16,
            )
            .expect("complete and prepare reply");
        assert!(matches!(completed.standing, ConsoleStanding::Completed(_)));
        assert!(matches!(
            completed.reply,
            ConsoleReplyStanding::Prepared {
                body,
                prepared_at: 16
            } if body == b"reply"
        ));

        let restarted = ConsoleLedger::at(root.path());
        let durable = restarted.list().expect("read durable pair");
        assert!(matches!(durable[0].standing, ConsoleStanding::Completed(_)));
        assert!(matches!(
            durable[0].reply,
            ConsoleReplyStanding::Prepared { .. }
        ));
    }

    #[test]
    fn settled_operations_compact_to_durable_replay_tombstones() {
        let root = tempfile::tempdir().expect("temporary home");
        let ledger = ConsoleLedger::at(root.path());
        let bond = ownership();
        let original = input(3, 1);
        ledger.accept(&bond, original.clone()).expect("accept");
        ledger
            .claim_dispatch(original.id, 11)
            .expect("claim dispatch");
        ledger
            .bind_attempt(original.id, [8; 16])
            .expect("bind Attempt");
        ledger
            .complete_with_reply(
                original.id,
                ConsoleCompletion {
                    attempt: [8; 16],
                    transcript_cursor: 42,
                    exit_code: Some(0),
                    completed_at: 12,
                },
                b"reply".to_vec(),
                13,
            )
            .expect("finish");
        ledger
            .claim_reply_send(original.id, 14)
            .expect("claim reply");
        ledger
            .mark_reply_sent(original.id, "deposit".into(), 15)
            .expect("settle reply");
        assert_eq!(ledger.compact_finalized().expect("compact"), 1);
        assert!(ledger.list().expect("active operations").is_empty());
        assert!(ledger
            .known_operation_ids()
            .expect("tombstones")
            .contains(&original.id));
        assert!(matches!(
            ledger.accept(&bond, original),
            Err(Error::Invalid("console operation was already consumed"))
        ));
    }

    #[test]
    fn version_two_ledgers_migrate_every_operation_to_a_tombstone() {
        let operation = ConsoleOperation {
            input: input(3, 1),
            standing: ConsoleStanding::Accepted,
            reply: ConsoleReplyStanding::None,
        };
        let legacy = LegacyLedgerStateV2 {
            version: LEGACY_LEDGER_VERSION,
            operations: vec![operation.clone()],
        };
        let body = postcard::to_stdvec(&legacy).expect("encode legacy ledger");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(ENVELOPE_VERSION);
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());

        let migrated = decode(&bytes).expect("migrate canonical v2 ledger");
        assert_eq!(migrated.version, LEDGER_VERSION);
        assert_eq!(migrated.operations, vec![operation.clone()]);
        assert!(migrated.consumed.contains_key(&operation.input.id));
    }

    #[test]
    fn restart_recovers_only_uncertain_reply_sends_not_runtime_dispatches() {
        let root = tempfile::tempdir().expect("temporary home");
        let ledger = ConsoleLedger::at(root.path());
        let bond = ownership();
        ledger.accept(&bond, input(3, 1)).expect("first accept");
        ledger
            .claim_dispatch(ConsoleOperationId([3; 16]), 11)
            .expect("first dispatch");
        ledger.accept(&bond, input(4, 2)).expect("second accept");
        ledger
            .claim_dispatch(ConsoleOperationId([4; 16]), 11)
            .expect("second dispatch");
        ledger
            .bind_attempt(ConsoleOperationId([4; 16]), [8; 16])
            .expect("bind second Attempt");
        ledger
            .complete(
                ConsoleOperationId([4; 16]),
                ConsoleCompletion {
                    attempt: [8; 16],
                    transcript_cursor: 42,
                    exit_code: Some(0),
                    completed_at: 12,
                },
            )
            .expect("complete second operation");
        ledger
            .prepare_reply(ConsoleOperationId([4; 16]), b"reply".to_vec(), 13)
            .expect("prepare reply");
        ledger
            .claim_reply_send(ConsoleOperationId([4; 16]), 14)
            .expect("claim external send");

        let restarted = ConsoleLedger::at(root.path());
        let recovered = restarted.recover_reply_sends(15).expect("recover sends");
        let recovered_send = recovered
            .iter()
            .find(|operation| operation.input.id == ConsoleOperationId([4; 16]))
            .expect("sending operation retained");
        assert!(matches!(
            recovered_send.reply,
            ConsoleReplyStanding::OutcomeUnknown { observed_at: 15 }
        ));
        let operations = restarted.list().expect("ledger remains readable");
        let dispatched = operations
            .iter()
            .find(|operation| operation.input.id == ConsoleOperationId([3; 16]))
            .expect("dispatched operation retained");
        assert!(matches!(
            dispatched.standing,
            ConsoleStanding::Dispatched { .. }
        ));
    }

    #[test]
    fn opaque_reply_failure_seals_only_the_claimed_send() {
        let root = tempfile::tempdir().expect("temporary home");
        let ledger = ConsoleLedger::at(root.path());
        let bond = ownership();
        ledger.accept(&bond, input(5, 1)).expect("accept");
        ledger
            .claim_dispatch(ConsoleOperationId([5; 16]), 2)
            .expect("dispatch");
        ledger
            .bind_attempt(ConsoleOperationId([5; 16]), [9; 16])
            .expect("bind Attempt");
        ledger
            .complete_with_reply(
                ConsoleOperationId([5; 16]),
                ConsoleCompletion {
                    attempt: [9; 16],
                    transcript_cursor: 4,
                    exit_code: Some(0),
                    completed_at: 3,
                },
                b"reply".to_vec(),
                3,
            )
            .expect("complete and prepare reply");
        ledger
            .claim_reply_send(ConsoleOperationId([5; 16]), 4)
            .expect("claim send");

        let sealed = ledger
            .mark_reply_outcome_unknown(ConsoleOperationId([5; 16]), 5)
            .expect("seal opaque send failure");
        assert!(matches!(
            sealed.reply,
            ConsoleReplyStanding::OutcomeUnknown { observed_at: 5 }
        ));
        assert_eq!(ledger.compact_finalized().expect("compact"), 1);
    }
}
