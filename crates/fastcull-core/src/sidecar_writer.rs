//! Dedicated sidecar-writer thread (`specs/01-architecture.md` threading
//! model): pick mutations are debounced (≤1 s after the last change per
//! image), writes are ordered and never lost — flushed on drop (panic-safe)
//! and on demand via the barrier the copy engine will use in M6.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender};
use std::time::{Duration, Instant};

use crate::catalog::PickState;
use crate::xmp::write_pick;

/// Debounce window: a burst of re-marks on one image becomes one write.
const DEBOUNCE: Duration = Duration::from_millis(700);

enum Msg {
    Mark(PathBuf, PickState),
    Flush(SyncSender<()>),
}

/// Handle to the writer thread. Dropping flushes everything and joins.
pub struct SidecarWriter {
    tx: Option<Sender<Msg>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SidecarWriter {
    pub fn start() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("sidecar-writer".into())
            .spawn(move || writer_loop(rx))
            .expect("spawn sidecar writer");
        Self {
            tx: Some(tx),
            handle: Some(handle),
        }
    }

    /// Queue a pick for `raw_path`; the sidecar lands within the debounce
    /// window (or at flush/shutdown, whichever is first).
    pub fn mark(&self, raw_path: PathBuf, pick: PickState) {
        if let Some(tx) = &self.tx {
            tx.send(Msg::Mark(raw_path, pick)).ok();
        }
    }

    /// Barrier: returns once every queued write has hit disk. The copy
    /// engine calls this before planning (a pick made a moment ago must be
    /// in the copied sidecar).
    pub fn flush(&self) {
        if let Some(tx) = &self.tx {
            let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(0);
            if tx.send(Msg::Flush(ack_tx)).is_ok() {
                ack_rx.recv().ok();
            }
        }
    }
}

impl Drop for SidecarWriter {
    fn drop(&mut self) {
        drop(self.tx.take()); // channel close = shutdown signal
        if let Some(handle) = self.handle.take() {
            handle.join().ok(); // writer drains all pending before exiting
        }
    }
}

fn writer_loop(rx: Receiver<Msg>) {
    // Latest pick per path + its write deadline; later marks supersede.
    let mut pending: HashMap<PathBuf, (PickState, Instant)> = HashMap::new();
    loop {
        let timeout = pending
            .values()
            .map(|(_, deadline)| deadline.saturating_duration_since(Instant::now()))
            .min()
            .unwrap_or(Duration::from_secs(3600));
        match rx.recv_timeout(timeout) {
            Ok(Msg::Mark(path, pick)) => {
                pending.insert(path, (pick, Instant::now() + DEBOUNCE));
            }
            Ok(Msg::Flush(ack)) => {
                drain(&mut pending, /*only_due=*/ false);
                ack.send(()).ok();
            }
            Err(RecvTimeoutError::Timeout) => {
                drain(&mut pending, /*only_due=*/ true);
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Shutdown: nothing may be lost.
                drain(&mut pending, false);
                return;
            }
        }
    }
}

fn drain(pending: &mut HashMap<PathBuf, (PickState, Instant)>, only_due: bool) {
    let now = Instant::now();
    let due: Vec<PathBuf> = pending
        .iter()
        .filter(|(_, (_, deadline))| !only_due || *deadline <= now)
        .map(|(p, _)| p.clone())
        .collect();
    for path in due {
        if let Some((pick, _)) = pending.remove(&path) {
            if let Err(e) = write_pick(&path, pick) {
                // A sidecar failure must never take the writer down; the
                // mark stays visible in-session and the error is logged.
                eprintln!("fastcull: sidecar write failed for {}: {e}", path.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xmp::{read_sidecar, sidecar_path};

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fastcull-scw-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn flush_barrier_makes_marks_durable_immediately() {
        let dir = tmp();
        let raw = dir.join("a.ARW");
        let writer = SidecarWriter::start();
        writer.mark(raw.clone(), PickState::Picked);
        writer.flush(); // no debounce wait
        assert_eq!(
            read_sidecar(&sidecar_path(&raw)).unwrap().pick,
            PickState::Picked
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rapid_remarks_coalesce_to_the_last_value() {
        let dir = tmp();
        let raw = dir.join("b.ARW");
        let writer = SidecarWriter::start();
        for pick in [
            PickState::Picked,
            PickState::Rejected,
            PickState::Unmarked,
            PickState::Rejected,
        ] {
            writer.mark(raw.clone(), pick);
        }
        writer.flush();
        assert_eq!(
            read_sidecar(&sidecar_path(&raw)).unwrap().pick,
            PickState::Rejected
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn drop_flushes_everything() {
        let dir = tmp();
        let raws: Vec<PathBuf> = (0..20).map(|i| dir.join(format!("c{i}.ARW"))).collect();
        {
            let writer = SidecarWriter::start();
            for raw in &raws {
                writer.mark(raw.clone(), PickState::Picked);
            }
            // No flush: Drop must not lose a single mark.
        }
        for raw in &raws {
            assert_eq!(
                read_sidecar(&sidecar_path(raw)).unwrap().pick,
                PickState::Picked,
                "{raw:?}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn debounce_writes_without_flush_within_window() {
        let dir = tmp();
        let raw = dir.join("d.ARW");
        let writer = SidecarWriter::start();
        writer.mark(raw.clone(), PickState::Picked);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if read_sidecar(&sidecar_path(&raw)).unwrap().pick == PickState::Picked {
                break;
            }
            assert!(Instant::now() < deadline, "debounced write never landed");
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(writer);
        std::fs::remove_dir_all(&dir).ok();
    }
}
