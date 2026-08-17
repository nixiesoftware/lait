//! A process that stays alive, for measuring what happens to a running
//! program when the tree it lives in is replaced.
//!
//! It exists because the obvious subject — a copy of `/bin/sleep` — is not a
//! valid one: macOS kills a copied platform binary within a few hundred
//! milliseconds on code-signing grounds, which looks exactly like "the swap
//! killed it" and is not. A binary this workspace built and the linker
//! ad-hoc-signed runs happily from anywhere, which is what makes it a subject
//! rather than a confound.
fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(5);
    std::thread::sleep(std::time::Duration::from_secs(seconds));
}
