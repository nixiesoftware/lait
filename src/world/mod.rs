//! Product-blind loading and lifecycle composition for independently installed
//! World releases.

pub mod installed;
pub mod lifecycle;

#[cfg(test)]
mod test;
#[cfg(test)]
pub use test::*;
