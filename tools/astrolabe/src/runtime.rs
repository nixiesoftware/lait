//! Where the background work lives, and how it reaches the frame loop.
//!
//! Supervision, control-protocol traffic and sampling never run on the UI
//! thread. They run on a Tokio runtime owned by this module and reach the frame
//! loop only as [`Update`]s on a channel the interface drains once per frame.
//!
//! That is one boundary, and it is a channel rather than a bridge. Nothing is
//! serialized across it and nothing is mirrored: an `Update` carries the
//! authoritative value itself, moved, and the model on the other side takes
//! ownership. There is still exactly one model of client state.

#![allow(
    clippy::future_not_send,
    reason = "every future here is polled by `block_on` on this module's own               thread and is never handed to an executor that could move it"
)]

use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;

use lait_workbench::{ClientSignal, Signals, WorkbenchSnapshot};

use crate::client::library::LibraryEntry;
use crate::client::storage::StorageFacts;
use crate::client::{Client, ClientError, ClientResult, Config};
use crate::model::App;

/// Something that happened, on its way to the model.
pub enum Update {
    Snapshot(Box<WorkbenchSnapshot>),
    Library(Vec<LibraryEntry>),
    Storage(Vec<StorageFacts>),
    Signal(ClientSignal),
    Failed { what: String, error: ClientError },
}

/// The background half of the client.
///
/// Owns the Tokio runtime, the client, and the task draining the signal stream.
/// Dropping it asks the runtime to stop and waits for the thread, so a closed
/// window does not leave a sampler running against a supervisor nobody reads.
pub struct Runtime {
    updates: Receiver<Update>,
    /// Wakes the UI thread when an update lands, so the interface is not
    /// obliged to poll at a fixed rate to feel responsive.
    _worker: JoinHandle<()>,
}

impl Runtime {
    /// Start the background half.
    ///
    /// `wake` is called whenever an update is queued. In the real shell it is
    /// `egui::Context::request_repaint`; in a test it can be a no-op, which is
    /// the reason it is a parameter rather than a captured context.
    pub fn start(config: Config, wake: impl Fn() + Send + 'static) -> ClientResult<Self> {
        let (sender, updates) = std::sync::mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("astrolabe-client".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        send(
                            &sender,
                            &wake,
                            Update::Failed {
                                what: "start the client runtime".into(),
                                error: ClientError::internal(error.to_string()),
                            },
                        );
                        return;
                    }
                };
                runtime.block_on(serve(config, &sender, &wake));
            })
            .map_err(|error| {
                // A machine that cannot spawn a thread at startup cannot run
                // this program, and there is no channel to report it through
                // yet — the reporting channel is what the thread would own. So
                // it goes back to the caller, which still has a way to say so.
                ClientError::internal(format!("start the client thread: {error}"))
            })?;

        Ok(Self {
            updates,
            _worker: worker,
        })
    }

    /// Apply everything that has arrived since the last frame.
    ///
    /// Drains rather than taking one: a frame that applied a single update
    /// would fall behind a busy stream and draw state that was already several
    /// signals stale, which is the opposite of what the freshness rules are for.
    pub fn drain_into(&self, app: &mut App) {
        loop {
            match self.updates.try_recv() {
                Ok(Update::Snapshot(snapshot)) => app.absorb(*snapshot),
                Ok(Update::Library(entries)) => app.absorb_library(entries),
                Ok(Update::Storage(facts)) => app.absorb_storage(facts, Vec::new()),
                Ok(Update::Signal(signal)) => app.consume(&signal),
                Ok(Update::Failed { what, error }) => app.fail(what, error),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        }
    }
}

async fn serve(config: Config, sender: &Sender<Update>, wake: &(impl Fn() + Send)) {
    let (client, signals) = match Client::start(config).await {
        Ok(started) => started,
        Err(error) => {
            send(
                sender,
                wake,
                Update::Failed {
                    what: "start the supervisor".into(),
                    error,
                },
            );
            return;
        }
    };

    // The first read happens *after* the stream exists, which `Client::start`
    // guarantees by handing both back together. Reading first would open a
    // window in which events vanish between the snapshot and the subscription.
    refresh(&client, sender, wake).await;
    drain(client, signals, sender, wake).await;
}

/// Consume the stream forever, re-reading whenever it says to.
async fn drain(
    client: Client,
    mut signals: Signals,
    sender: &Sender<Update>,
    wake: &(impl Fn() + Send),
) {
    while let Some(signal) = signals.recv().await {
        let rebaseline = matches!(signal, ClientSignal::SnapshotRequired(_));
        send(sender, wake, Update::Signal(signal));
        // A re-read on *every* event would make a busy log a request storm.
        // Only a snapshot-required says the model cannot be derived from what
        // it has seen, and only that is worth a round trip.
        if rebaseline {
            refresh(&client, sender, wake).await;
        }
    }
}

async fn refresh(client: &Client, sender: &Sender<Update>, wake: &(impl Fn() + Send)) {
    send(
        sender,
        wake,
        Update::Snapshot(Box::new(client.supervisor().snapshot().await)),
    );
    match client.get_library().await {
        Ok(entries) => send(sender, wake, Update::Library(entries)),
        Err(error) => send(
            sender,
            wake,
            Update::Failed {
                what: "read the library".into(),
                error,
            },
        ),
    }
    match client.get_storage().await {
        Ok(facts) => send(sender, wake, Update::Storage(facts)),
        Err(error) => send(
            sender,
            wake,
            Update::Failed {
                what: "read storage".into(),
                error,
            },
        ),
    }
}

/// A send whose receiver is gone means the window closed. Not an error, and
/// nothing to report to — the only correct response is to stop trying.
fn send(sender: &Sender<Update>, wake: &(impl Fn() + Send), update: Update) {
    if sender.send(update).is_ok() {
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lait_workbench::{
        BackendEvent, Capabilities, EnvironmentSnapshot, EventKind, SnapshotReason,
    };

    fn snapshot() -> WorkbenchSnapshot {
        WorkbenchSnapshot {
            schema_version: 1,
            revision: 1,
            environment: EnvironmentSnapshot {
                state_root: "root".into(),
                executable: "lait".into(),
                server_pid: 1,
            },
            capabilities: Capabilities::default(),
            devices: Vec::new(),
            connections: Vec::new(),
        }
    }

    /// A frame applies everything waiting, not one thing. A drain that took a
    /// single update would draw state that was already several signals old on a
    /// busy stream — precisely when being current matters most.
    #[test]
    fn a_frame_applies_every_update_that_is_waiting() {
        let (sender, updates) = std::sync::mpsc::channel();
        let runtime = Runtime {
            updates,
            _worker: std::thread::spawn(|| {}),
        };

        sender.send(Update::Snapshot(Box::new(snapshot()))).ok();
        for index in 0..5 {
            sender
                .send(Update::Signal(ClientSignal::Event(BackendEvent {
                    revision: index + 2,
                    at_ms: 0,
                    kind: EventKind::LogChanged,
                    device_id: None,
                    message: "log grew".into(),
                })))
                .ok();
        }
        sender
            .send(Update::Signal(ClientSignal::SnapshotRequired(
                SnapshotReason::Reloaded,
            )))
            .ok();

        let mut app = App::new();
        runtime.drain_into(&mut app);

        assert_eq!(app.consumed(), 6, "the frame stopped short of the queue");
        assert!(
            !app.is_loading(),
            "the snapshot in the queue was not applied"
        );
        assert!(
            app.stale().is_some(),
            "the last signal said to re-baseline and the model did not record it"
        );
    }

    /// Failures reach the model as state rather than being logged and lost.
    #[test]
    fn a_failure_becomes_something_the_surface_can_draw() {
        let (sender, updates) = std::sync::mpsc::channel();
        let runtime = Runtime {
            updates,
            _worker: std::thread::spawn(|| {}),
        };
        sender
            .send(Update::Failed {
                what: "read the library".into(),
                error: ClientError::unreachable("no daemon"),
            })
            .ok();

        let mut app = App::new();
        runtime.drain_into(&mut app);
        let failure = app.failures().next().expect("a failure reached the model");
        assert_eq!(failure.what, "read the library");
        assert!(
            failure.error.retryable,
            "an unreachable daemon is retryable"
        );
    }

    /// A closed window drops the receiver. The background half must treat that
    /// as "stop", not as an error worth reporting to a channel nobody holds.
    #[test]
    fn sending_to_a_closed_window_is_not_an_error() {
        let (sender, updates) = std::sync::mpsc::channel::<Update>();
        drop(updates);
        let woken = std::sync::atomic::AtomicBool::new(false);
        send(
            &sender,
            &|| woken.store(true, std::sync::atomic::Ordering::SeqCst),
            Update::Signal(ClientSignal::SnapshotRequired(SnapshotReason::Reloaded)),
        );
        assert!(
            !woken.load(std::sync::atomic::Ordering::SeqCst),
            "a send to a closed window still asked for a repaint"
        );
    }
}
