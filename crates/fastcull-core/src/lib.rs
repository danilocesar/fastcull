//! FastCull engine: everything except the UI.
//!
//! This crate owns the RAW preview pipeline, the XMP sidecar model, IPTC
//! templates, burst grouping, filtering, and the copy/rename engine. It has no
//! UI dependencies so that all behavior is exercisable from unit and
//! integration tests.
//!
//! Module specifications live in `specs/modules/` at the repository root and
//! are the source of truth for behavior. Invariant that outranks everything
//! else: **a RAW file is never opened for writing** — all state goes to XMP
//! sidecars.

pub mod cache;
pub mod catalog;
pub mod exif;
pub mod filter;
pub mod grid;
pub mod loupe;
pub mod pipeline;
pub mod raw;
pub mod sidecar_writer;
pub mod viewassets;
pub mod xmp;
pub mod zoompan;

/// Application version, shared by the CLI and the UI shell.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_semver_like() {
        assert_eq!(super::VERSION.split('.').count(), 3);
    }
}
