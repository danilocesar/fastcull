//! THE INTEROP TEST (specs/modules/xmp-sidecars.md): darktable itself, fully
//! sandboxed, must read the ratings FastCull writes.
//!
//! Hard rule (CLAUDE.md): darktable-cli ALWAYS runs with a throwaway
//! --configdir/--library — never the user's real config or database.
//!
//! Empirical mapping (darktable 5.4.1, probed 2026-07-25): library.db
//! `images.flags & 7` = star rating (picked → 1); rejected sets flag bit 3
//! (0x8) with rating bits zeroed.

use std::path::PathBuf;

use fastcull_core::catalog::PickState;
use fastcull_core::xmp::{write_keywords, write_pick};

fn darktable_cli() -> Option<&'static str> {
    let ok = std::process::Command::new("darktable-cli")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("darktable round-trip skipped: darktable-cli not installed");
    }
    ok.then_some("darktable-cli")
}

fn testdata(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/raws")
        .join(name);
    assert!(path.is_file(), "missing {path:?} — run testdata/fetch.sh");
    path
}

#[test]
fn darktable_reads_fastcull_picks_and_rejects() {
    let Some(cli) = darktable_cli() else { return };

    let base = std::env::temp_dir().join(format!("fastcull-dtrt-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let cfg = base.join("cfg"); // throwaway darktable config — hard rule
    let work = base.join("work");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let library = base.join("library.db");

    // Two copies of a real A1 file: one picked, one rejected — sidecars
    // written by the SAME code path the app uses.
    let source = testdata("A1_full_compressed.ARW");
    let picked = work.join("picked.ARW");
    let rejected = work.join("rejected.ARW");
    std::fs::copy(&source, &picked).unwrap();
    std::fs::copy(&source, &rejected).unwrap();
    write_pick(&picked, PickState::Picked).unwrap();
    write_pick(&rejected, PickState::Rejected).unwrap();
    // Keywords half of the round-trip (M5): written by the same code path
    // the template apply uses; includes Unicode and a pipe hierarchy.
    write_keywords(
        &picked,
        &["owl".into(), "são joão".into(), "Nature|Birds".into()],
    )
    .unwrap();

    for (input, out) in [(&picked, "p.jpg"), (&rejected, "r.jpg")] {
        let status = std::process::Command::new(cli)
            .arg(input)
            .arg(base.join(out))
            .args(["--core", "--configdir"])
            .arg(&cfg)
            .arg("--library")
            .arg(&library)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run darktable-cli");
        assert!(status.success(), "darktable-cli failed for {input:?}");
    }

    let conn = rusqlite::Connection::open(&library).unwrap();
    let flags_of = |name: &str| -> i64 {
        conn.query_row(
            "SELECT flags FROM images WHERE filename = ?1",
            [name],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("{name} not in darktable library: {e}"))
    };
    let picked_flags = flags_of("picked.ARW");
    let rejected_flags = flags_of("rejected.ARW");

    assert_eq!(picked_flags & 7, 1, "picked must be 1 star in darktable");
    assert_eq!(picked_flags & 0x8, 0, "picked must not be rejected");
    assert_ne!(rejected_flags & 0x8, 0, "rejected bit must be set");
    assert_eq!(rejected_flags & 7, 0, "rejected carries no stars");

    // Keywords: darktable stores tag names in <configdir>/data.db and the
    // image<->tag links in library.db (tagged_images). All three of our
    // keywords — plain, Unicode, pipe-hierarchy — must be attached to
    // picked.ARW.
    conn.execute(
        "ATTACH DATABASE ?1 AS data",
        [cfg.join("data.db").to_str().unwrap()],
    )
    .unwrap();
    let tag_count: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT t.name) FROM tagged_images ti \
             JOIN images i ON i.id = ti.imgid \
             JOIN data.tags t ON t.id = ti.tagid \
             WHERE i.filename = 'picked.ARW' \
             AND t.name IN ('owl', 'são joão', 'Nature|Birds')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        tag_count, 3,
        "darktable must import all three FastCull keywords as tags"
    );

    std::fs::remove_dir_all(&base).ok();
}
