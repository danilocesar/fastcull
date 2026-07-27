fn main() {
    build_version_suffix();
    // The Slint compiler is deeply recursive and build scripts are compiled
    // unoptimized; Windows gives the main thread 1 MB of stack (vs 8 MB on
    // Linux), which main.slint's growth overflowed (CI: STATUS_STACK_OVERFLOW
    // 0xc00000fd on every Windows run since M3). Standard workaround: compile
    // on a thread with an explicit, roomy stack.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| slint_build::compile("ui/main.slint"))
        .expect("spawning slint-build thread")
        .join()
        .expect("slint-build thread panicked")
        .expect("compiling ui/main.slint");
}

/// About-dialog version suffix (issue #23, user decision 2026-07-27):
/// an OFFICIAL build — HEAD sitting exactly on the release tag
/// `v{CARGO_PKG_VERSION}` — shows plain "X.Y.Z"; anything else shows
/// "X.Y.Z-devel-<short-hash>" so a bug report from a dev build pins
/// the commit (untagged builds otherwise all report the same X.Y.Z).
/// No git (tarball build) → empty suffix, plain version.
fn build_version_suffix() {
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git").args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    // Only trust a repo that is OURS: git discovers any enclosing
    // repository, and a tarball extracted inside an unrelated repo
    // would stamp that repo's hash into the version (QE finding). The
    // discovered toplevel must be this crate's workspace root.
    let ours = git(&["rev-parse", "--show-toplevel"])
        .map(std::path::PathBuf::from)
        .and_then(|top| top.canonicalize().ok())
        .zip(
            std::env::var("CARGO_MANIFEST_DIR")
                .ok()
                .map(|m| std::path::PathBuf::from(m).join("../.."))
                .and_then(|w| w.canonicalize().ok()),
        )
        .is_some_and(|(top, ws)| top == ws);
    if !ours {
        println!("cargo:rustc-env=FASTCULL_VERSION_SUFFIX=");
        return;
    }
    let on_release_tag = git(&["describe", "--tags", "--exact-match", "HEAD"])
        .is_some_and(|tag| tag == format!("v{version}"));
    let suffix = if on_release_tag {
        String::new()
    } else {
        match git(&["rev-parse", "--short", "HEAD"]) {
            Some(hash) => format!("-devel-{hash}"),
            None => String::new(),
        }
    };
    println!("cargo:rustc-env=FASTCULL_VERSION_SUFFIX={suffix}");
    // Recompute when the checked-out commit moves (HEAD file changes on
    // branch switch; the ref file changes on commit).
    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={dir}/{r}");
        }
    }
}
