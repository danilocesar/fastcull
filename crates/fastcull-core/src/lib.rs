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

pub mod burst;
pub mod cache;
pub mod catalog;
pub mod exif;
pub mod fileops;
pub mod filter;
pub mod grid;
pub mod iptc;
pub mod loupe;
pub mod pipeline;
pub mod pointer;
pub mod raw;
pub mod selection;
pub mod sidecar_writer;
pub mod viewassets;
pub mod xmp;
pub mod zoompan;

/// Scratch directories for unit tests, in one place.
///
/// Eight test modules had their own copy of this three-line scaffold,
/// and they had drifted: some cleaned the directory first, some did not,
/// some included the thread id (without which two tests in the same
/// binary share a directory and race each other).
#[cfg(test)]
pub(crate) mod testutil {
    use std::path::PathBuf;

    /// A fresh `<temp>/fastcull-<tag>-<pid>-<thread>` directory: the
    /// thread id keeps parallel tests in one binary apart, and the
    /// pre-clean means a crashed earlier run cannot poison this one.
    pub(crate) fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fastcull-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

/// Application version, shared by the CLI and the UI shell.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_semver_like() {
        // MAJOR.MINOR.PATCH with an optional -prerelease suffix — the
        // old dot-count assert rejected the first RC tag ("0.1.1-rc.1").
        let (base, pre) = super::VERSION
            .split_once('-')
            .unwrap_or((super::VERSION, "x"));
        let parts: Vec<_> = base.split('.').collect();
        assert_eq!(parts.len(), 3, "base must be MAJOR.MINOR.PATCH: {base}");
        assert!(
            parts.iter().all(|p| p.parse::<u32>().is_ok()),
            "numeric base: {base}"
        );
        assert!(!pre.is_empty(), "prerelease suffix must be non-empty");
    }
}
