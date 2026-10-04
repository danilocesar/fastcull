//! FastCull engine: everything except the UI.
//!
//! This crate owns the RAW preview pipeline, the XMP sidecar model, IPTC
//! templates, burst grouping, filtering, the copy/rename engine, and the
//! decision functions the UI walks (the pointer machine, the grid layout,
//! the loupe's transit ladder). It has no UI dependencies so that all
//! behavior is exercisable from unit and integration tests.
//!
//! Module specifications live in `specs/modules/` at the repository root and
//! are the source of truth for behavior. Invariant that outranks everything
//! else: **a RAW file is never opened for writing** — all state goes to XMP
//! sidecars.

pub mod burst;
pub mod cache;
pub mod catalog;
pub mod clip;
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
pub mod settings;
pub mod sidecar_writer;
pub mod transit;
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
    use std::path::{Path, PathBuf};

    /// A scratch directory that goes when the test that made it ends — and
    /// STAYS when that test is panicking, so a red test's files are still
    /// there to be read (brief 010 R3, settings.md AC31: every passing run
    /// used to leave its dirs behind, 1,086 of them in /tmp on the
    /// development seat, 2026-10-03). It reads as the directory's `Path`.
    ///
    /// Hold it for the test's whole life — `let dir = scratch_dir(tag);` —
    /// never as a temporary (`scratch_dir(tag).join(…)`): a guard dropped
    /// early deletes the directory under the test, which then fails loudly
    /// on its first file, never silently. Not `Clone`: two guards of one
    /// directory would each delete it.
    pub(crate) struct ScratchDir(PathBuf);

    impl std::ops::Deref for ScratchDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for ScratchDir {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            // A test that panics is unwinding when its locals drop: its dir
            // is its evidence. A removal that fails (a file still open on
            // Windows, a directory a test left read-only) leaves the dir as
            // every run used to, and never turns a green test red.
            if !std::thread::panicking() {
                std::fs::remove_dir_all(&self.0).ok();
            }
        }
    }

    /// A fresh `<temp>/fastcull-<tag>-<pid>-<thread>` directory: the
    /// thread id keeps parallel tests in one binary apart, and the
    /// pre-clean means a crashed earlier run cannot poison this one.
    pub(crate) fn scratch_dir(tag: &str) -> ScratchDir {
        let dir = std::env::temp_dir().join(format!(
            "fastcull-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        ScratchDir(dir)
    }

    mod tests {
        use super::*;

        /// The guard removes its directory when the test ends, and keeps it
        /// when the test is panicking — a red test's evidence (brief 010
        /// R3, settings.md AC31). The panic below is on purpose, caught, and
        /// prints its message like any panic.
        ///
        /// Mutants (2026-10-03), each alone: `Drop`'s removal taken out →
        /// "a scratch dir outlived the test that made it" — red; the
        /// `panicking()` check taken out → "a panicking test's scratch dir
        /// was removed" — red.
        #[test]
        fn a_scratch_dir_goes_on_drop_and_stays_for_a_panicking_test() {
            let dir = scratch_dir("guard-drop");
            let gone = dir.to_path_buf();
            std::fs::write(dir.join("evidence"), b"green").unwrap();
            drop(dir);
            assert!(
                !gone.exists(),
                "a scratch dir outlived the test that made it: {}",
                gone.display()
            );

            let mut kept = None;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let dir = scratch_dir("guard-panic");
                std::fs::write(dir.join("evidence"), b"red").unwrap();
                kept = Some(dir.to_path_buf());
                panic!("a red test, on purpose: its scratch dir must stay");
            }));
            assert!(outcome.is_err(), "the closure did not panic");
            let kept = kept.expect("the panicking closure never made its dir");
            let evidence = std::fs::read(kept.join("evidence"));
            std::fs::remove_dir_all(&kept).ok();
            assert_eq!(
                evidence.ok().as_deref(),
                Some(&b"red"[..]),
                "a panicking test's scratch dir was removed — its evidence is gone: {}",
                kept.display()
            );
        }
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
