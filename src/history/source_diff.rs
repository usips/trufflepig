//! Conservative declaration correspondence and session fingerprints over
//! `crate::line_diff` changes.

mod correspondence;
mod fingerprints;
#[cfg(test)]
mod tests;

pub use correspondence::{DeclarationCorrespondence, DeclarationRelation, correspond_declarations};
pub use fingerprints::{
    FingerprintFact, FingerprintLines, correspond_fingerprints, diff_fingerprints,
    fingerprint_lines,
};
