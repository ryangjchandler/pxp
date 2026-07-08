//! Feature transforms.
//!
//! Each feature is a self-contained pass that parses the source and returns a
//! list of [`Edit`](crate::emit::Edit)s. Passes never mutate the source or each
//! other's output — the driver collects every edit and splices once. New syntax
//! features slot in as new modules here.

pub mod generics;
pub mod short_closures;

use crate::emit::Edit;

/// Run every feature pass and collect their edits.
pub fn run_all(src: &[u8]) -> Vec<Edit> {
    let mut edits = Vec::new();
    edits.extend(short_closures::transform(src));
    edits.extend(generics::transform(src));
    edits
}
