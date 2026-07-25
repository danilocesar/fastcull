//! Dedicated sidecar-writer thread (`specs/01-architecture.md` threading
//! model): pick mutations are debounced (≤1 s after the last change per
//! image), writes are ordered and never lost — flushed on drop (panic-safe)
//! and on demand via the barrier the copy engine will use in M6.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender};
use std::time::{Duration, Instant};

use crate::catalog::PickState;
use crate::xmp::{write_keywords, write_pick};

/// Debounce window: a burst of re-marks on one image becomes one write.
const DEBOUNCE: Duration = Duration::from_millis(700);

enum Msg {
    Mark(PathBuf, PickState),
    Keywords(PathBuf, Vec<String>),
    Flush(SyncSender<()>),
}

/// Handle to the writer thread. Dropping flushes everything and joins.
pub struct SidecarWriter {
    tx: Option<Sender<Msg>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// A failed sidecar write, surfaced to the UI (a cull that silently does
/// not persist would betray the user — gate finding).
#[derive(Debug, Clone)]
pub struct WriteFailure {
    pub path: PathBuf,
    pub reason: String,
}

impl SidecarWriter {
    pub fn start() -> (Self, Receiver<WriteFailure>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let (err_tx, err_rx) = std::sync::mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("sidecar-writer".into())
            .spawn(move || writer_loop(rx, err_tx))
            .expect("spawn sidecar writer");
        (
            Self {
                tx: Some(tx),
                handle: Some(handle),
            },
            err_rx,
        )
    }

    /// Queue a pick for `raw_path`; the sidecar lands within the debounce
    /// window (or at flush/shutdown, whichever is first).
    pub fn mark(&self, raw_path: PathBuf, pick: PickState) {
        if let Some(tx) = &self.tx {
            tx.send(Msg::Mark(raw_path, pick)).ok();
        }
    }

    /// Queue a keyword-list write for `raw_path` (M5: the IPTC panel and
    /// template apply route here — ALL sidecar writes serialize through
    /// this one thread; parallel raw `write_*` calls are corruption-safe
    /// since the unique-temp fix but still last-writer-wins per property).
    pub fn keywords(&self, raw_path: PathBuf, keywords: Vec<String>) {
        if let Some(tx) = &self.tx {
            tx.send(Msg::Keywords(raw_path, keywords)).ok();
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

/// One queued write: the latest value per (path, property) wins.
enum PendingWrite {
    Pick(PickState),
    Keywords(Vec<String>),
}

fn writer_loop(rx: Receiver<Msg>, err_tx: Sender<WriteFailure>) {
    // Latest value per (path, property) + its write deadline; later
    // mutations supersede. Picks and keywords are separate entries so a
    // keyword edit never delays a pick write past its window.
    let mut pending: HashMap<(PathBuf, u8), (PendingWrite, Instant)> = HashMap::new();
    loop {
        let timeout = pending
            .values()
            .map(|(_, deadline)| deadline.saturating_duration_since(Instant::now()))
            .min()
            .unwrap_or(Duration::from_secs(3600));
        match rx.recv_timeout(timeout) {
            Ok(Msg::Mark(path, pick)) => {
                pending.insert(
                    (path, 0),
                    (PendingWrite::Pick(pick), Instant::now() + DEBOUNCE),
                );
            }
            Ok(Msg::Keywords(path, kws)) => {
                pending.insert(
                    (path, 1),
                    (PendingWrite::Keywords(kws), Instant::now() + DEBOUNCE),
                );
            }
            Ok(Msg::Flush(ack)) => {
                drain(&mut pending, /*only_due=*/ false, &err_tx);
                ack.send(()).ok();
            }
            Err(RecvTimeoutError::Timeout) => {
                drain(&mut pending, /*only_due=*/ true, &err_tx);
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Shutdown: nothing may be lost.
                drain(&mut pending, false, &err_tx);
                return;
            }
        }
    }
}

fn drain(
    pending: &mut HashMap<(PathBuf, u8), (PendingWrite, Instant)>,
    only_due: bool,
    err_tx: &Sender<WriteFailure>,
) {
    let now = Instant::now();
    let due: Vec<(PathBuf, u8)> = pending
        .iter()
        .filter(|(_, (_, deadline))| !only_due || *deadline <= now)
        .map(|(k, _)| k.clone())
        .collect();
    for key in due {
        if let Some((write, _)) = pending.remove(&key) {
            let path = &key.0;
            let result = match &write {
                PendingWrite::Pick(pick) => write_pick(path, *pick),
                PendingWrite::Keywords(kws) => write_keywords(path, kws),
            };
            if let Err(e) = result {
                // A sidecar failure must never take the writer down; it is
                // surfaced to the UI (and logged for headless callers).
                eprintln!("fastcull: sidecar write failed for {}: {e}", path.display());
                err_tx
                    .send(WriteFailure {
                        path: path.clone(),
                        reason: e.to_string(),
                    })
                    .ok();
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
        let (writer, _errs) = SidecarWriter::start();
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
        let (writer, _errs) = SidecarWriter::start();
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

    /// Keyword writes serialize through the same thread as picks and
    /// coalesce per property: interleaved marks and keyword edits on one
    /// image land as the LAST value of each, no corruption, no loss.
    #[test]
    fn keywords_and_picks_serialize_and_coalesce() {
        let dir = tmp();
        let raw = dir.join("k.ARW");
        let (writer, _errs) = SidecarWriter::start();
        for i in 0..50 {
            writer.mark(
                raw.clone(),
                if i % 2 == 0 {
                    PickState::Picked
                } else {
                    PickState::Rejected
                },
            );
            writer.keywords(raw.clone(), vec![format!("kw{i}")]);
        }
        writer.flush();
        let state = read_sidecar(&sidecar_path(&raw)).unwrap();
        assert_eq!(state.pick, PickState::Rejected, "last mark wins");
        assert_eq!(state.keywords, vec!["kw49"], "last keyword list wins");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn drop_flushes_everything() {
        let dir = tmp();
        let raws: Vec<PathBuf> = (0..20).map(|i| dir.join(format!("c{i}.ARW"))).collect();
        {
            let (writer, _errs) = SidecarWriter::start();
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
        let (writer, _errs) = SidecarWriter::start();
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

#[cfg(test)]
mod error_tests {
    use super::*;
    use std::path::PathBuf;

    /// A write failure must reach the error channel (gate finding: culling
    /// a read-only card looked fully successful).
    #[test]
    fn failed_writes_are_surfaced() {
        let (writer, errs) = SidecarWriter::start();
        writer.mark(
            PathBuf::from("/proc/definitely/not/writable/x.ARW"),
            PickState::Picked,
        );
        writer.flush();
        let failure = errs
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("failure must be surfaced");
        assert!(failure.path.to_string_lossy().contains("x.ARW"));
    }
}
