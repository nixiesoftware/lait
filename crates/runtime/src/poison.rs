//! One policy for reading state that a panicked thread may have left
//! half-written.
//!
//! A poisoned lock is the only signal `std` gives that a previous holder
//! panicked while holding it — that the invariant the lock exists to protect
//! may not hold right now. This crate's roots deny `panic`, `unwrap_used` and
//! `expect_used` under `not(test)`, which means the ordinary way to take a lock
//! is unavailable and every site had to write its own recovery. Ninety-odd of
//! them did, in five different spellings, none documented, and nothing anywhere
//! in the workspace ever called `is_poisoned`. The policy was not "recover"; it
//! was whatever silenced the lint at that expression shape.
//!
//! So: state it once. Recovering is the right default here — a Station that
//! aborts every future read because one thread died is worse than one that
//! keeps serving — but recovering *silently* is not a policy, it is the absence
//! of one. `#[track_caller]` puts the call site in the line, so the report names
//! the lock rather than this module.
//!
//! Where recovery is NOT good enough, the answer is not a quieter version of
//! this: it is to refuse. The Replica already does exactly that for the same
//! hazard — see `replica`'s own `poisoned` flag, which is fail-stop and
//! documents that the operation must never be retried through it. Locks
//! guarding authority, admission or durable commit state belong on that path,
//! and this helper is what will tell you when one of them is actually hit.

use std::sync::{Mutex, MutexGuard};

/// Take a lock, recovering from — and reporting — poisoning.
pub trait LockRecovering<T: ?Sized> {
    /// Lock, or recover the guard from a panicked holder and say so.
    fn lock_recovering(&self) -> MutexGuard<'_, T>;
}

impl<T: ?Sized> LockRecovering<T> for Mutex<T> {
    #[track_caller]
    fn lock_recovering(&self) -> MutexGuard<'_, T> {
        // Captured out here: `Location::caller()` reads the `#[track_caller]`
        // frame, and the closure below is not one — inside it, this would
        // report this file instead of the site that took the lock.
        let site = std::panic::Location::caller();
        self.lock().unwrap_or_else(|poisoned| {
            tracing::error!(
                %site,
                "lock poisoned by an earlier panic — recovering, so what follows \
                 reads state that may be half-written"
            );
            poisoned.into_inner()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poisoned_lock_is_recovered_rather_than_propagated() {
        let lock = std::sync::Arc::new(Mutex::new(7u32));
        let poisoner = std::sync::Arc::clone(&lock);
        // Poison it for real: panic while holding the guard.
        let _ = std::thread::spawn(move || {
            let mut guard = poisoner.lock().expect("uncontended");
            *guard = 9;
            panic!("the holder dies here");
        })
        .join();
        assert!(lock.is_poisoned(), "the panic must have poisoned the lock");

        // The write the panicking thread made is still visible — which is the
        // whole reason this is a report and not a silent recovery.
        assert_eq!(*lock.lock_recovering(), 9);
    }

    #[test]
    fn an_unpoisoned_lock_is_taken_normally() {
        let lock = Mutex::new(1u32);
        *lock.lock_recovering() += 1;
        assert_eq!(*lock.lock_recovering(), 2);
        assert!(!lock.is_poisoned());
    }
}
