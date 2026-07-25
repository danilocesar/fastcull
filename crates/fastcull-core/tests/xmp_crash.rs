//! Atomicity crash harness (xmp-sidecars.md acceptance criterion): kill -9
//! a process mid-write-storm; every surviving sidecar must parse.
#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use fastcull_core::catalog::PickState;
use fastcull_core::xmp::{read_sidecar, write_pick};

/// Child mode: storm sidecar writes forever (parent kills us).
#[test]
fn crash_child_write_storm() {
    let Some(dir) = std::env::var_os("FASTCULL_CRASH_DIR") else {
        return; // not in child mode: nothing to do
    };
    let dir = PathBuf::from(dir);
    let picks = [PickState::Picked, PickState::Rejected, PickState::Unmarked];
    let mut i = 0usize;
    loop {
        let raw = dir.join(format!("img{:02}.ARW", i % 20));
        let _ = write_pick(&raw, picks[i % 3]);
        i += 1;
    }
}

#[test]
fn killed_writer_never_leaves_a_corrupt_sidecar() {
    let base = std::env::temp_dir().join(format!("fastcull-crash-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    std::fs::create_dir_all(&base).unwrap();
    let exe = std::env::current_exe().unwrap();

    for round in 0..15 {
        let mut child = std::process::Command::new(&exe)
            .args(["--exact", "crash_child_write_storm", "--nocapture"])
            .env("FASTCULL_CRASH_DIR", &base)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(15 + (round * 13) % 80));
        // SIGKILL: no destructors, no flushes — the rename must protect us.
        libc_kill(child.id() as i32);
        child.wait().ok();
    }

    let mut checked = 0;
    for entry in std::fs::read_dir(&base).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "xmp") {
            read_sidecar(&path).unwrap_or_else(|e| panic!("corrupt sidecar {path:?}: {e}"));
            checked += 1;
        }
    }
    assert!(checked > 0, "storm never produced sidecars");
    std::fs::remove_dir_all(&base).ok();
}

fn libc_kill(pid: i32) {
    // SIGKILL == 9; avoid a libc dependency for one syscall.
    std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .ok();
}
