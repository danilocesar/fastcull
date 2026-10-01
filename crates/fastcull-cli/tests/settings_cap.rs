//! The CLI and the settings file (settings.md, "Performance › Thumbnail
//! cache cap"; ADR 0005: a knob the two binaries share comes from the same
//! file, with no flag surface of the CLI's own). The CLI's first test.

use std::ffi::OsStr;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// A sample RAW from `testdata/raws/`, which `testdata/fetch.sh` fills.
fn testdata(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/raws")
        .join(name);
    assert!(
        path.is_file(),
        "missing test file {path:?} — run testdata/fetch.sh first"
    );
    path
}

/// What one run of the CLI printed, and how it ended.
struct Run {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Run `fastcull-cli` with `envs` set and the variables in `unset` removed,
/// under a 90 s watchdog: a hung child is killed and reported, never waited
/// on forever (the app tests' rule). Both pipes are drained on threads so a
/// chatty child cannot fill one and block.
fn run_cli(args: &[&str], envs: &[(&str, &OsStr)], unset: &[&str]) -> Run {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fastcull-cli"));
    cmd.args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for var in unset {
        cmd.env_remove(var);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn fastcull-cli");
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            pipe.read_to_string(&mut text).ok();
            text
        })
    };
    let out = drain(Box::new(child.stdout.take().expect("stdout piped")));
    let err = drain(Box::new(child.stderr.take().expect("stderr piped")));
    let deadline = Instant::now() + Duration::from_secs(90);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            let (stdout, stderr) = (out.join().unwrap(), err.join().unwrap());
            panic!("fastcull-cli did not exit within 90 s:\n{stdout}\n{stderr}");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    Run {
        success: status.success(),
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}

/// AC11's CLI half (settings.md, "Performance › Thumbnail cache cap"): the
/// CLI trims the DEFAULT thumbnail cache to the file's cap — the same file
/// and the same number as the app — and its `cache:` line says where the
/// cap in force came from: the file only when one was read, otherwise the
/// default and why (QE 2026-10-01, D11).
///
/// The default cache is sandboxed the way the app's driven cache test does
/// it: `HOME` and `XDG_CACHE_HOME` point into this test's own scratch dir,
/// so the `directories` crate resolves the cache there and never the
/// user's real one, and the test refuses to run otherwise. That redirect
/// exists on Linux only — Windows' known-folder lookup ignores the
/// environment — so the test skips itself elsewhere at run time, and the
/// CLI's call site stays review-verified on Windows (settings.md, AC11).
/// `FASTCULL_CONFIG_DIR` points at a scratch config dir for the same
/// reason.
///
/// The cap run: 300 seeded thumbnails of 1 MiB each and `cache_cap =
/// "0.1"` — held at the 0.25 GB floor, 268,435,456 bytes, room for 256 of
/// them — then `thumbs` over one RAW. Three wording runs follow, each with
/// a fresh config dir and the sandbox kept on (`--no-cache` would print no
/// `cache:` line at all): no file, `FASTCULL_NO_CONFIG`, a broken file.
///
/// Mutants (2026-10-01): the CLI passing `cache::DEFAULT_CAP_BYTES` to
/// `default_cache_path` → all 300 seeded thumbnails survive — red;
/// `cap_source` returning ` from settings.toml` whatever was read → the
/// no-file wording — red.
#[test]
fn the_cli_honours_the_files_cache_cap_and_says_where_it_came_from() {
    if !cfg!(target_os = "linux") {
        eprintln!(
            "skipped: the default cache cannot be sandboxed off Linux (Windows' \
             known-folder lookup ignores HOME and XDG_CACHE_HOME); the CLI's call \
             site is review-verified there"
        );
        return;
    }
    let scratch = std::env::temp_dir().join(format!("fastcull-cli-cap-{}", std::process::id()));
    std::fs::remove_dir_all(&scratch).ok();
    // The seeded cache is ~300 MiB: gone however the test ends, a red run
    // included.
    struct RemoveOnDrop(PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }
    let _cleanup = RemoveOnDrop(scratch.clone());
    let home = scratch.join("home");
    let cache_home = home.join(".cache");
    for (var, dir) in [("HOME", &home), ("XDG_CACHE_HOME", &cache_home)] {
        assert!(
            dir.is_absolute() && dir.starts_with(&scratch),
            "refusing a run with the cache on: {var} must point under {} so the \
             default cache resolves into the sandbox, never the user's real one",
            scratch.display()
        );
    }
    let db = cache_home.join("fastcull").join("previews.db");
    let mtime = Some(SystemTime::now());
    let seeded = |i: usize| PathBuf::from(format!("/seed/{i:03}.ARW"));
    {
        let mut cache = fastcull_core::cache::PreviewCache::open(&db).expect("seed the cache");
        let blob = vec![0u8; 1 << 20];
        for i in 0..300 {
            cache
                .store(
                    &seeded(i),
                    1,
                    mtime,
                    &fastcull_core::exif::ExifSummary::default(),
                    &blob,
                )
                .expect("seed a row");
        }
    }
    let folder = scratch.join("folder");
    std::fs::create_dir_all(&folder).unwrap();
    let raw = testdata("A1_full_compressed.ARW");
    // A link, not a copy: the CLI only reads it (`thumbs` writes nothing
    // beside a RAW), and 63 MB need not be copied for that.
    #[cfg(unix)]
    std::os::unix::fs::symlink(&raw, folder.join("one.ARW")).unwrap();
    #[cfg(not(unix))]
    std::fs::copy(&raw, folder.join("one.ARW")).unwrap();
    let config = |tag: &str, text: Option<&str>| -> PathBuf {
        let dir = scratch.join(format!("config-{tag}"));
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(text) = text {
            std::fs::write(dir.join("settings.toml"), text).unwrap();
        }
        dir
    };
    // Every run keeps the cache sandbox on (`--no-cache` would print no
    // `cache:` line at all) and adds its own config variables.
    let thumbs = |config: &[(&str, &OsStr)], unset: &[&str]| -> Run {
        let mut envs: Vec<(&str, &OsStr)> = vec![
            ("HOME", home.as_os_str()),
            ("XDG_CACHE_HOME", cache_home.as_os_str()),
        ];
        envs.extend_from_slice(config);
        let run = run_cli(&["thumbs", folder.to_str().unwrap()], &envs, unset);
        assert!(
            run.success,
            "fastcull-cli thumbs failed:\n{}\n{}",
            run.stdout, run.stderr
        );
        run
    };

    // The cap: the file's 0.1 GB, held at the 0.25 GB floor.
    let capped = config("cap", Some("[performance]\ncache_cap = \"0.1\"\n"));
    let run = thumbs(&[("FASTCULL_CONFIG_DIR", capped.as_os_str())], &[]);
    let line = format!("cache: {} (cap 0.25 GB from settings.toml)", db.display());
    assert!(
        run.stdout.contains(&line),
        "the CLI did not report the sandboxed default cache under the file's cap \
         (expected `{line}`):\n{}",
        run.stdout
    );
    let remaining = {
        let mut cache = fastcull_core::cache::PreviewCache::open(&db).expect("reopen the cache");
        (0..300)
            .filter(|i| matches!(cache.lookup(&seeded(*i), 1, mtime), Ok(Some(_))))
            .count()
    };
    assert!(
        remaining <= 256,
        "{remaining} of the 300 seeded 1 MiB thumbnails survived a CLI run under a \
         0.25 GB cap — the file's cap was not enforced on the default cache:\n{}",
        run.stdout
    );

    // Where the cap came from, when it is not the file's.
    let wording = |run: &Run, source: &str| {
        let line = format!("cache: {} (cap 2 GB, the default — {source})", db.display());
        assert!(
            run.stdout.contains(&line),
            "expected `{line}`:\n{}\n{}",
            run.stdout,
            run.stderr
        );
    };
    let empty = config("none", None);
    wording(
        &thumbs(&[("FASTCULL_CONFIG_DIR", empty.as_os_str())], &[]),
        "no settings.toml",
    );
    // An inherited FASTCULL_CONFIG_DIR would win over FASTCULL_NO_CONFIG
    // (settings.md, "Hermetic"), so it goes.
    wording(
        &thumbs(
            &[("FASTCULL_NO_CONFIG", OsStr::new("1"))],
            &["FASTCULL_CONFIG_DIR"],
        ),
        "FASTCULL_NO_CONFIG is set",
    );
    let broken = config("broken", Some("[performance\n"));
    let run = thumbs(&[("FASTCULL_CONFIG_DIR", broken.as_os_str())], &[]);
    wording(&run, "settings.toml could not be read");
    assert!(
        run.stderr.contains(&format!(
            "fastcull: {} could not be read (",
            broken.join("settings.toml").display()
        )),
        "a settings.toml that would not read was not named on stderr:\n{}",
        run.stderr
    );
}
