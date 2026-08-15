//! The Swift bindgen, built from the same pinned uniffi the library uses so
//! the generated Swift can never be produced by a different generator version
//! than the scaffolding it must match.

fn main() {
    uniffi::uniffi_bindgen_swift()
}
