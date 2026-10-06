fn main() {
    build_version_suffix();
    embed_windows_icon();
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

/// The Windows executable's icon (specs/modules/app-icon.md, brief 013):
/// `assets/icon/fastcull.ico` becomes icon resource 1 of fastcull-app.exe,
/// which is what Explorer, the Start menu, a pinned taskbar and
/// SmartScreen's dialog show. The running window's icon is a separate
/// mechanism (`MainWindow.icon`, main.slint).
///
/// Gated twice. On the HOST, by `#[cfg(windows)]`: a build script is
/// compiled for the machine that builds, and the `winresource`
/// build-dependency exists only under `[target.'cfg(windows)'.
/// build-dependencies]`, which Cargo also evaluates against the host — so
/// the Linux build never compiles this body or the crate. On the TARGET, by
/// `CARGO_CFG_TARGET_OS`, so a Windows host building for another OS does not
/// run rc.exe for nothing. fastcull-cli deliberately gets no icon: an
/// iconless CLI reads as "not the one you double-click" (persona
/// 2026-10-06).
#[cfg(windows)]
fn embed_windows_icon() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // Built lexically, never with `canonicalize()`: on Windows that yields a
    // `\\?\` path, which rc.exe's ICON statement must not be handed.
    let manifest_dir = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    let ico = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/fastcull-app sits two levels below the workspace root")
        .join("assets")
        .join("icon")
        .join("fastcull.ico");
    assert!(ico.is_file(), "missing {}", ico.display());
    println!("cargo:rerun-if-changed={}", ico.display());
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().expect("a UTF-8 path to fastcull.ico"))
        // winresource writes a VERSIONINFO resource whether asked or not;
        // these are its user-visible strings (Task Manager, Explorer's
        // Details tab), chosen in app-icon.md rather than left at the
        // crate's default, the package NAME "fastcull-app". The version
        // fields stay the crate's own, from CARGO_PKG_VERSION.
        .set("FileDescription", "FastCull")
        .set("ProductName", "FastCull")
        // en-US (LANG_ENGLISH | SUBLANG_ENGLISH_US << 10).
        .set_language(0x0409);
    res.compile()
        .expect("compiling the Windows icon resource with the Windows SDK's rc.exe");
}

/// Not a Windows host: nothing to embed (see the Windows version above).
#[cfg(not(windows))]
fn embed_windows_icon() {}

/// About-dialog version suffix (issue #23, user decision 2026-07-27):
/// an OFFICIAL build — HEAD sitting exactly on the release tag
/// `v{CARGO_PKG_VERSION}` — shows plain "X.Y.Z"; anything else shows
/// "X.Y.Z-devel-YYYYMMDD-<short-hash>" (issue #26) so a bug report from a
/// dev build pins both WHICH commit and HOW OLD it is (untagged builds
/// otherwise all report the same X.Y.Z). No git (tarball build) → empty
/// suffix, plain version.
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
        // Issue #26: a dev build carries the commit DATE as well as the
        // hash — `X.Y.Z-devel-YYYYMMDD-<hash>`. The hash pins which code is
        // running; the date says how old it is without anyone having to go
        // and look the hash up, which is the whole point of a string people
        // paste into bug reports. Date first so builds from one branch sort
        // chronologically.
        //
        // COMMITTER date (%cd), not author date (%ad): a rebased or
        // cherry-picked commit keeps its original author date, which would
        // describe when the code was first written rather than when the
        // commit being run came into existence. And the commit date rather
        // than the BUILD date (user decision 2026-07-31) because it is
        // reproducible — the same commit always yields the same string, two
        // people on that commit report the same version, and it cannot go
        // stale when this script does not re-run, which a build date could.
        let date = git(&["show", "-s", "--format=%cd", "--date=format:%Y%m%d", "HEAD"])
            .filter(|d| d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()));
        match (date, git(&["rev-parse", "--short", "HEAD"])) {
            (Some(date), Some(hash)) => format!("-devel-{date}-{hash}"),
            // A hash with no usable date is still worth having.
            (None, Some(hash)) => format!("-devel-{hash}"),
            (_, None) => String::new(),
        }
    };
    println!("cargo:rustc-env=FASTCULL_VERSION_SUFFIX={suffix}");
    // Recompute when the checked-out commit moves (HEAD changes on branch
    // switch; the branch ref changes on commit) — and when a TAG appears,
    // because `git tag vX.Y.Z && cargo build` otherwise leaves a `-devel-`
    // string in a release binary. That is not hypothetical: it happened at
    // 0.5.0, and a version string nobody can trust defeats the point of
    // issue #26.
    //
    // Every path is checked for existence first. A `rerun-if-changed` path
    // that does NOT exist makes cargo re-run this script on every single
    // build, and this script recompiles main.slint — measured ~4.7 s of
    // pure tax per no-op build. Both missing-path cases are real here: in a
    // `git worktree` the branch ref lives in the common dir, and in a fresh
    // clone the refs are packed rather than loose (QE finding D5).
    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        let watch = |path: String| {
            if std::path::Path::new(&path).exists() {
                println!("cargo:rerun-if-changed={path}");
            }
        };
        watch(format!("{dir}/HEAD"));
        if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
            watch(format!("{dir}/{r}"));
        }
        watch(format!("{dir}/refs/tags"));
        watch(format!("{dir}/packed-refs"));
    }
}
