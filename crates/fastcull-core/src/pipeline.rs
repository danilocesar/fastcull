//! Priority thumbnail pipeline: turns a scanned session into grid thumbs and
//! EXIF metadata on all cores (`specs/modules/raw-pipeline.md`).
//!
//! Queue contract: three priorities (`Visible` > `Prefetch` > `Background`),
//! background jobs run in sequential file order (card-reader friendly),
//! `promote`/`set_visible` reprioritizes queued jobs without re-enqueueing,
//! duplicates coalesce, in-flight jobs are never cancelled (they are ≤150 ms).
//!
//! Per image the pipeline emits `MetadataReady` (EXIF via the in-tree TIFF walker) and
//! `ThumbReady` (embedded preview → decode → 320 px → JPEG q80), or `Failed`
//! once if the thumb path is impossible — metadata failure alone does not
//! fail an image, the thumb is the essential asset. Results are cached
//! (path, size, mtime); cache hits skip every RAW read.
//!
//! M1 scope: grid thumbs only. FitPreview/FullRes assets and their LRU memory
//! budget join in M4 (loupe); the queue and event plumbing are built for that
//! extension.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::cache::PreviewCache;
use crate::exif::ExifSummary;
use crate::raw::{find_embedded_jpegs, read_jpeg};

/// Adaptive read pool (raw-pipeline.md, user requirement 2026-07-25): a pool
/// manager owns how many workers may READ concurrently (decode stays fully
/// parallel) and adapts that number to the medium's measured behavior — a
/// fixed 4 fixed the 32-readers-on-a-microSD hang but could not react when
/// the medium degraded further mid-session.
///
/// Floor 4 (user decision 2026-07-25): the empirically proven-safe value is
/// always available — NAS/network mounts must never be throttled below it.
const POOL_MIN_READERS: usize = 4;

/// Cap (user decision 2026-07-25): "if the loader is not stuck, we can add
/// more up to the number of CPU cores" — growth beyond the floor is earned
/// probe by probe, so slow media never sees the high end.
fn pool_cap() -> usize {
    std::thread::available_parallelism().map_or(POOL_MIN_READERS, |n| n.get().max(POOL_MIN_READERS))
}

/// Pure clamp arithmetic: no clock, no threads. Grow below / shrink above
/// form a hysteresis dead-band (a single threshold ping-pongs at
/// equilibrium); shrinks are throttled by the pool to one per
/// shrink-threshold window. AIMD: +1 per fast probe, HALVE on a slow or
/// stalled read — with the cap at core count (possibly 32), one-step shrink
/// would need ~28 slow probes to recover from a warm-cache-pumped limit;
/// halving reaches the floor in 3 windows.
struct PoolController {
    limit: usize,
    floor: usize,
    cap: usize,
    grow_below: Duration,
    shrink_above: Duration,
}

impl PoolController {
    fn new() -> Self {
        // FASTCULL_MAX_READERS=N (user request 2026-07-25, raw-pipeline.md):
        // debug/testing override REPLACING the adaptive cap — N above the
        // core count raises the ceiling (spec: an override, not merely a
        // limiter). N <= 4 also lowers the floor, so N=1 pins a single
        // reader and N=4 restores the old fixed-4 behavior; unset = adaptive.
        let over = std::env::var("FASTCULL_MAX_READERS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n >= 1);
        Self::with_override(over)
    }

    fn with_override(max_readers: Option<usize>) -> Self {
        let (floor, cap) = match max_readers {
            Some(n) => (POOL_MIN_READERS.min(n), n),
            None => (POOL_MIN_READERS, pool_cap()),
        };
        let cap = cap.max(floor);
        Self {
            limit: POOL_MIN_READERS.clamp(floor, cap),
            floor,
            cap,
            grow_below: Duration::from_millis(200),
            shrink_above: Duration::from_millis(500),
        }
    }

    /// Additive increase: one more reader, clamped at the cap.
    fn grow_one(&mut self) {
        self.limit = (self.limit + 1).min(self.cap);
    }

    /// Multiplicative decrease, clamped at the floor.
    fn shrink_halve(&mut self) {
        self.limit = (self.limit / 2).max(self.floor);
    }
}

/// Reads larger than this never feed the controller: the non-A1 fallback can
/// use a multi-MB full-res JPEG as grid source, and a healthy medium serving
/// one legitimately takes >200 ms — it must not pin the limit at the floor.
const POOL_SAMPLE_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// How often blocked waiters re-check for a stalled probe (they may be the
/// only threads touching the pool while everything else is stuck in I/O).
const POOL_STALL_RECHECK: Duration = Duration::from_millis(100);

/// One granted read: start time + whether it is excluded from control
/// decisions (>2 MB payload — validator H1: exclusion must cover EVERY
/// decision path, stall included).
struct ActiveRead {
    started: Instant,
    excluded: bool,
}

struct PoolState {
    controller: PoolController,
    /// EVERY in-flight read, by permit id. The stall check watches the
    /// oldest of these, not just the probe: stuck reads never produce
    /// samples, so the probe alone has survivorship bias (live incident
    /// 2026-07-25: 0 ms page-cache-warm probes pumped the limit 4 -> 22
    /// while every cold read sat wedged on a saturated microSD).
    active: HashMap<u64, ActiveRead>,
    /// Waiting tickets, min (priority, arrival seq) first: a freed slot goes
    /// to the highest-priority waiter — visible thumbs beat background
    /// prefetch even at the floor (validator finding: FIFO handoff starves
    /// visible-first exactly when the medium degrades).
    waiters: std::collections::BinaryHeap<std::cmp::Reverse<(u8, u64)>>,
    next_ticket: u64,
    /// Permit id of the designated probe. The probe paces GROWTH (one
    /// growth decision per completed probe); shrink signals come from the
    /// whole active set.
    probe: Option<u64>,
    /// Shrinks are throttled to one per shrink-threshold window: a wedged
    /// card walks cap -> floor in ~3 windows (halving) without a collapse
    /// cascade from many simultaneous slow observations.
    last_shrink: Option<Instant>,
}

impl PoolState {
    fn oldest_active_age(&self, now: Instant) -> Option<Duration> {
        self.active
            .values()
            .filter(|r| !r.excluded)
            .map(|r| now.duration_since(r.started))
            .max()
    }

    fn shrink_window_open(&self, now: Instant) -> bool {
        self.last_shrink
            .is_none_or(|t| now.duration_since(t) > self.controller.shrink_above)
    }

    /// Stall shrink: the oldest non-excluded in-flight read exceeding the
    /// shrink threshold halves the limit — decided WITHOUT waiting for any
    /// completion, at most once per window.
    fn check_stall(&mut self) {
        let now = Instant::now();
        let Some(age) = self.oldest_active_age(now) else {
            return;
        };
        if age <= self.controller.shrink_above || !self.shrink_window_open(now) {
            return;
        }
        let before = self.controller.limit;
        self.controller.shrink_halve();
        if self.controller.limit != before {
            self.last_shrink = Some(now);
            eprintln!(
                "fastcull: read pool {before} -> {} workers (read stalled for {} ms; {} reading)",
                self.controller.limit,
                age.as_millis(),
                self.active.len()
            );
        }
    }
}

/// The pool manager: workers ask it for release before entering a read
/// section. Shrink is non-preemptive — reads in progress always finish, the
/// slot is simply not re-released.
struct ReadPool {
    state: Mutex<PoolState>,
    cv: Condvar,
}

impl ReadPool {
    fn new() -> Self {
        Self {
            state: Mutex::new(PoolState {
                controller: PoolController::new(),
                active: HashMap::new(),
                waiters: std::collections::BinaryHeap::new(),
                next_ticket: 0,
                probe: None,
                last_shrink: None,
            }),
            cv: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PoolState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    fn set_limit_for_test(&self, limit: usize) {
        self.lock().controller.limit = limit;
        self.cv.notify_all();
    }

    #[cfg(test)]
    fn current_limit(&self) -> usize {
        self.lock().controller.limit
    }

    #[cfg(test)]
    fn waiting_count(&self) -> usize {
        self.lock().waiters.len()
    }

    #[cfg(test)]
    fn set_thresholds_for_test(&self, grow_below: Duration, shrink_above: Duration) {
        let mut state = self.lock();
        state.controller.grow_below = grow_below;
        state.controller.shrink_above = shrink_above;
    }

    /// Growth-asserting tests must not depend on the host's core count:
    /// on 4-core CI runners the real cap equals the floor and growth is
    /// impossible (the warm-probe regression test failed there for exactly
    /// that reason while passing on 32-core dev machines).
    #[cfg(test)]
    fn set_cap_for_test(&self, cap: usize) {
        self.lock().controller.cap = cap;
    }

    /// Wait for release at `priority` (lower = more urgent). `sampled` marks
    /// a preview-read section eligible to become the probe; the EXIF section
    /// passes false (EXIF/decode parse CPU would contaminate the sample).
    fn acquire(&self, priority: u8, sampled: bool) -> ReadPermit<'_> {
        let mut state = self.lock();
        let seq = state.next_ticket;
        state.next_ticket += 1;
        let ticket = (priority, seq);
        state.waiters.push(std::cmp::Reverse(ticket));
        loop {
            state.check_stall();
            if state.active.len() < state.controller.limit
                && state.waiters.peek() == Some(&std::cmp::Reverse(ticket))
            {
                state.waiters.pop();
                let started = Instant::now();
                state.active.insert(
                    seq,
                    ActiveRead {
                        started,
                        excluded: false,
                    },
                );
                if sampled && state.probe.is_none() {
                    state.probe = Some(seq);
                }
                drop(state);
                // Other freed slots may still be grantable to later tickets.
                self.cv.notify_all();
                return ReadPermit {
                    pool: self,
                    id: seq,
                    started,
                };
            }
            // Timed wait: stall detection must keep running even when no
            // release ever arrives (everything stuck in uninterruptible I/O).
            state = self
                .cv
                .wait_timeout(state, POOL_STALL_RECHECK)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

struct ReadPermit<'a> {
    pool: &'a ReadPool,
    id: u64,
    started: Instant,
}

impl ReadPermit<'_> {
    /// Payload size, once known (before the bulk read): >2 MB excludes this
    /// read from ALL control decisions — growth, shrink, and stall alike
    /// (validator H1: a large read legitimately runs long; it must neither
    /// vouch for nor indict the medium). Immediate, not at drop: the stall
    /// check runs concurrently from other pool interactions.
    fn set_bytes(&self, bytes: u64) {
        if bytes > POOL_SAMPLE_MAX_BYTES {
            if let Some(read) = self.pool.lock().active.get_mut(&self.id) {
                read.excluded = true;
            }
        }
    }
}

impl Drop for ReadPermit<'_> {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        let mut state = self.pool.lock();
        let entry = state.active.remove(&self.id);
        // Every release is a pool touch: stalled reads must be noticed even
        // when no waiters are blocked (validator L1).
        state.check_stall();
        if state.probe == Some(self.id) {
            state.probe = None;
            let excluded = entry.is_none_or(|r| r.excluded);
            let now = Instant::now();
            if !excluded && elapsed < state.controller.grow_below {
                // "If the loader is not stuck, we can add more" (user rule):
                // a fast probe only vouches for the medium when NO other
                // in-flight read is older than the grow threshold — warm
                // page-cache probes must not outvote wedged cold reads
                // (survivorship bias, live incident 2026-07-25).
                let stuck = state
                    .oldest_active_age(now)
                    .is_some_and(|age| age > state.controller.grow_below);
                if !stuck {
                    let before = state.controller.limit;
                    state.controller.grow_one();
                    if state.controller.limit != before {
                        // Debug visibility (user request 2026-07-25): stderr,
                        // consistent with every other FastCull diagnostic;
                        // "N reading" = reads actually in flight right now.
                        eprintln!(
                            "fastcull: read pool {before} -> {} workers (probe read {} ms; {} reading)",
                            state.controller.limit,
                            elapsed.as_millis(),
                            state.active.len()
                        );
                    }
                }
            } else if !excluded
                && elapsed > state.controller.shrink_above
                && state.shrink_window_open(now)
            {
                let before = state.controller.limit;
                state.controller.shrink_halve();
                if state.controller.limit != before {
                    state.last_shrink = Some(now);
                    eprintln!(
                        "fastcull: read pool {before} -> {} workers (probe read {} ms; {} reading)",
                        state.controller.limit,
                        elapsed.as_millis(),
                        state.active.len()
                    );
                }
            }
        }
        drop(state);
        // Grow and release must wake ALL waiters: with priority tickets only
        // the head may proceed, and notify_one could wake the wrong thread
        // (lost-wakeup class this codebase has been bitten by before).
        self.pool.cv.notify_all();
    }
}

/// Grid thumbnails: long edge in pixels.
pub const THUMB_LONG_EDGE: u32 = 320;
/// Grid thumbnails: JPEG re-encode quality.
pub const THUMB_JPEG_QUALITY: u8 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Visible = 0,
    Prefetch = 1,
    Background = 2,
}

/// What the pipeline needs to know about one image (mirrors `ImageRecord`).
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: Option<SystemTime>,
}

/// Progress events, delivered on the receiver returned by [`Pipeline::start`].
/// Indexes refer to the job list passed at start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    MetadataReady {
        index: usize,
        exif: ExifSummary,
        from_cache: bool,
    },
    ThumbReady {
        index: usize,
        thumb_jpeg: Vec<u8>,
        width: u32,
        height: u32,
        from_cache: bool,
    },
    Failed {
        index: usize,
        reason: String,
    },
    /// A pre-existing sidecar was found at load (M1-deferred criterion,
    /// approved for M3): picks — and since M5, the full IPTC state — from a
    /// previous session (or another tool) reappear. Emitted only when the
    /// sidecar file exists.
    Sidecar {
        index: usize,
        pick: crate::catalog::PickState,
        iptc: Box<crate::iptc::IptcData>,
    },
}

struct QueueState {
    /// Min-heap on (priority, sequence): lowest priority value first, then
    /// insertion order — which is file order for the initial background fill.
    heap: BinaryHeap<std::cmp::Reverse<(u8, u64, usize)>>,
    /// Source of truth for each queued job's current priority; heap entries
    /// that disagree are stale and skipped on pop (lazy deletion). Absence
    /// means done or in flight — promote() ignores those.
    queued: HashMap<usize, u8>,
    in_flight: HashSet<usize>,
    next_seq: u64,
}

struct Shared {
    state: Mutex<QueueState>,
    wakeup: Condvar,
    jobs: Vec<JobSpec>,
    cache_path: Option<PathBuf>,
    events: Sender<SessionEvent>,
    shutdown: AtomicBool,
    read_pool: ReadPool,
}

/// Handle to the running pipeline. Dropping it stops the workers (queued jobs
/// are abandoned, in-flight jobs finish).
pub struct Pipeline {
    shared: Arc<Shared>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Pipeline {
    /// Start `num_threads` workers over `jobs`; all jobs begin at
    /// `Background` priority in list order. `cache_path` is the SQLite
    /// preview cache (None disables caching, e.g. for tests).
    pub fn start(
        jobs: Vec<JobSpec>,
        cache_path: Option<PathBuf>,
        num_threads: usize,
    ) -> (Self, Receiver<SessionEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut state = QueueState {
            heap: BinaryHeap::new(),
            queued: HashMap::new(),
            in_flight: HashSet::new(),
            next_seq: 0,
        };
        for index in 0..jobs.len() {
            let seq = state.next_seq;
            state.next_seq += 1;
            state.queued.insert(index, Priority::Background as u8);
            state
                .heap
                .push(std::cmp::Reverse((Priority::Background as u8, seq, index)));
        }
        // Create/validate the cache DB once, before workers race to open
        // their own handles: schema creation is not concurrency-safe on a
        // fresh file (two workers racing it left one silently cache-less).
        let cache_path = cache_path.filter(|p| match PreviewCache::open(p) {
            Ok(_) => true,
            Err(e) => {
                eprintln!("fastcull: preview cache disabled ({e}) — thumbnails will not persist");
                false
            }
        });
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            wakeup: Condvar::new(),
            jobs,
            cache_path,
            events: tx,
            shutdown: AtomicBool::new(false),
            read_pool: ReadPool::new(),
        });
        let workers = (0..num_threads.max(1))
            .map(|_| {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || worker_loop(&shared))
            })
            .collect();
        (Self { shared, workers }, rx)
    }

    /// Promote `indexes` to `priority`. Completed and in-flight jobs are
    /// skipped; queued jobs are reprioritized (never duplicated in effect).
    /// Lowering a priority is intentionally impossible — a job that was once
    /// visible stays at its highest requested urgency.
    pub fn promote(&self, indexes: impl IntoIterator<Item = usize>, priority: Priority) {
        // Drain the caller's iterator BEFORE taking the lock: caller code
        // must never run under our mutex (a panicking iterator would poison
        // it — QE finding).
        let indexes: Vec<usize> = indexes.into_iter().collect();
        let mut state = lock_state(&self.shared);
        for index in indexes {
            let Some(current) = state.queued.get(&index).copied() else {
                continue; // done, in flight, or out of range
            };
            if (priority as u8) < current {
                state.queued.insert(index, priority as u8);
                let seq = state.next_seq;
                state.next_seq += 1;
                state
                    .heap
                    .push(std::cmp::Reverse((priority as u8, seq, index)));
            }
        }
        drop(state);
        self.shared.wakeup.notify_all();
    }

    /// The UI's viewport changed: everything in `range` becomes `Visible`.
    pub fn set_visible(&self, range: std::ops::Range<usize>) {
        self.promote(range, Priority::Visible);
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        // Serialize with the workers' check-then-wait window: a worker that
        // saw shutdown == false while holding the lock must reach
        // Condvar::wait before our notify, or it sleeps forever (lost-wakeup
        // deadlock found by validator + reproduced by qe-engineer). Taking
        // and releasing the state lock between the store and the notify
        // closes that window; promote() is safe for the same reason.
        // Poison-tolerant: panicking in a destructor would abort the process.
        drop(lock_state(&self.shared));
        self.shared.wakeup.notify_all();
        for worker in self.workers.drain(..) {
            worker.join().ok();
        }
    }
}

/// Lock the queue state, tolerating poison: our own critical sections never
/// panic (caller iterators are drained outside the lock), so a poisoned
/// mutex only means some thread died mid-panic — the queue data is still a
/// consistent snapshot and shutdown must keep working.
fn lock_state(shared: &Shared) -> std::sync::MutexGuard<'_, QueueState> {
    shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn worker_loop(shared: &Shared) {
    // One cache handle per worker (schema already created by start());
    // handles contend via WAL + busy timeout. A failed open degrades this
    // worker to cache-less operation — loudly, never silently.
    let mut cache = shared.cache_path.as_deref().and_then(|p| {
        PreviewCache::open(p)
            .map_err(|e| eprintln!("fastcull: worker running without preview cache ({e})"))
            .ok()
    });

    loop {
        let (index, priority) = {
            let mut state = lock_state(shared);
            loop {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                // Pop entries until one matches its recorded priority.
                match state.heap.pop() {
                    Some(std::cmp::Reverse((prio, _seq, index))) => {
                        if state.queued.get(&index) == Some(&prio) {
                            state.queued.remove(&index);
                            state.in_flight.insert(index);
                            break (index, prio);
                        }
                        // Stale entry (reprioritized or already taken): skip.
                    }
                    None => {
                        state = shared
                            .wakeup
                            .wait(state)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                }
            }
        };

        // Contain panics from decoder internals on hostile files: the image
        // gets its Failed event and the worker survives; without this a
        // panicking job would strand the index in_flight and kill the worker.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            process_job(shared, &mut cache, index, priority);
        }));
        if outcome.is_err() {
            shared
                .events
                .send(SessionEvent::Failed {
                    index,
                    reason: "internal error (panic) while processing this file".into(),
                })
                .ok();
        }

        let mut state = lock_state(shared);
        state.in_flight.remove(&index);
    }
}

fn process_job(shared: &Shared, cache: &mut Option<PreviewCache>, index: usize, priority: u8) {
    let spec = &shared.jobs[index];
    let send = |event: SessionEvent| {
        shared.events.send(event).ok();
    };

    // Sidecar-at-open: read fresh on every load (never cached — another
    // tool may have changed it between sessions).
    let sc = crate::xmp::sidecar_path(&spec.path);
    if sc.exists() {
        match crate::xmp::read_sidecar(&sc) {
            Ok(state) => send(SessionEvent::Sidecar {
                index,
                pick: state.pick,
                iptc: Box::new(state.iptc),
            }),
            // A malformed sidecar must not vanish silently: the previous
            // cull is gone from view and the user deserves a trace.
            Err(e) => eprintln!("fastcull: unreadable sidecar {}: {e}", sc.display()),
        }
    }

    // Cache hit: no RAW reads at all.
    if let Some(cache) = cache.as_mut() {
        if let Ok(Some(hit)) = cache.lookup(&spec.path, spec.size, spec.mtime) {
            if let Some((w, h)) = jpeg_dimensions(&hit.thumb_jpeg) {
                send(SessionEvent::MetadataReady {
                    index,
                    exif: hit.exif,
                    from_cache: true,
                });
                send(SessionEvent::ThumbReady {
                    index,
                    thumb_jpeg: hit.thumb_jpeg,
                    width: w,
                    height: h,
                    from_cache: true,
                });
                return;
            }
            // Undecodable cached thumb: fall through to re-extraction.
        }
    }

    let exif = {
        // Pool-managed but never sampled: this section mixes file reads with
        // parse/decode CPU (spec: only the preview read feeds the controller).
        let _permit = shared.read_pool.acquire(priority, false);
        crate::exif::read_exif_summary(&spec.path).ok()
    };
    if let Some(exif) = &exif {
        send(SessionEvent::MetadataReady {
            index,
            exif: exif.clone(),
            from_cache: false,
        });
    }

    match make_grid_thumb_gated(spec, Some((&shared.read_pool, priority))) {
        Ok((thumb_jpeg, width, height)) => {
            // Cache the thumb even when the EXIF read failed (as an
            // all-None summary): the zero-RAW-reads-on-reopen guarantee
            // outranks metadata completeness (recorded in catalog-cache.md).
            if let Some(cache) = cache.as_mut() {
                let exif_for_cache = exif.unwrap_or_default();
                cache
                    .store(
                        &spec.path,
                        spec.size,
                        spec.mtime,
                        &exif_for_cache,
                        &thumb_jpeg,
                    )
                    .ok();
            }
            send(SessionEvent::ThumbReady {
                index,
                thumb_jpeg,
                width,
                height,
                from_cache: false,
            });
        }
        Err(reason) => send(SessionEvent::Failed { index, reason }),
    }
}

/// Extract the grid-source preview, decode, resize to 320 px, re-encode q80.
/// Public so benches and the CLI can exercise the exact hot path.
pub fn make_grid_thumb(spec: &JobSpec) -> Result<(Vec<u8>, u32, u32), String> {
    make_grid_thumb_gated(spec, None)
}

fn make_grid_thumb_gated(
    spec: &JobSpec,
    pool: Option<(&ReadPool, u8)>,
) -> Result<(Vec<u8>, u32, u32), String> {
    // Read phase under the pool manager; decode below runs fully parallel.
    // This is the probe-eligible section: open + IFD walk + payload read.
    let (previews, jpeg_bytes) = {
        let permit = pool.map(|(p, priority)| p.acquire(priority, true));
        let mut file = std::fs::File::open(&spec.path).map_err(|e| format!("open: {e}"))?;
        let previews = find_embedded_jpegs(&mut file).map_err(|e| format!("parse: {e}"))?;
        let source = previews
            .grid_source()
            .ok_or(crate::raw::NO_USABLE_PREVIEW)?
            .clone();
        if let Some(permit) = &permit {
            permit.set_bytes(source.len);
        }
        let bytes = read_jpeg(&mut file, &source).map_err(|e| format!("read: {e}"))?;
        (previews, bytes)
    };

    // Issue #31: zune-jpeg 0.4 zero-fills a truncated scan and reports
    // success — a cut-off preview must become Failed (spec acceptance
    // criterion), not a mostly-blank thumb. Dimension claims on this path
    // are already bounded by zune's default 16384-per-side limit (268 MP,
    // stricter than raw::MAX_DECODED_PIXELS), so only stream completeness
    // needs checking here.
    if !crate::raw::scan_is_terminated(&jpeg_bytes) {
        return Err("truncated JPEG preview (scan reaches no end-of-image marker)".into());
    }
    // Force RGB output so grayscale/CMYK previews also become valid thumbs.
    let options = zune_jpeg::zune_core::options::DecoderOptions::default()
        .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGB);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(&jpeg_bytes, options);
    let pixels = decoder.decode().map_err(|e| format!("decode: {e}"))?;
    let (src_w, src_h) = decoder
        .dimensions()
        .ok_or("decoded JPEG reports no dimensions")?;
    let (src_w, src_h) = (
        u32::try_from(src_w).map_err(|_| "width overflow")?,
        u32::try_from(src_h).map_err(|_| "height overflow")?,
    );
    // Soft-rotate to display orientation (spec: every rung).
    let (pixels, src_w, src_h) =
        crate::raw::apply_orientation(pixels, src_w, src_h, previews.orientation);

    let (dst_w, dst_h) = fit_long_edge(src_w, src_h, THUMB_LONG_EDGE);
    let src_image = fast_image_resize::images::Image::from_vec_u8(
        src_w,
        src_h,
        pixels,
        fast_image_resize::PixelType::U8x3,
    )
    .map_err(|e| format!("resize input: {e}"))?;
    let mut dst_image =
        fast_image_resize::images::Image::new(dst_w, dst_h, fast_image_resize::PixelType::U8x3);
    fast_image_resize::Resizer::new()
        .resize(&src_image, &mut dst_image, None)
        .map_err(|e| format!("resize: {e}"))?;

    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, THUMB_JPEG_QUALITY)
        .encode(
            dst_image.buffer(),
            dst_w as u16,
            dst_h as u16,
            jpeg_encoder::ColorType::Rgb,
        )
        .map_err(|e| format!("encode: {e}"))?;
    Ok((out, dst_w, dst_h))
}

fn fit_long_edge(w: u32, h: u32, long_edge: u32) -> (u32, u32) {
    if w.max(h) <= long_edge {
        return (w, h);
    }
    if w >= h {
        (
            long_edge,
            (u64::from(h) * u64::from(long_edge) / u64::from(w)).max(1) as u32,
        )
    } else {
        (
            (u64::from(w) * u64::from(long_edge) / u64::from(h)).max(1) as u32,
            long_edge,
        )
    }
}

/// Cheap SOF sniff on an in-memory JPEG (for cached thumbs).
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut decoder = zune_jpeg::JpegDecoder::new(bytes);
    decoder.decode_headers().ok()?;
    decoder
        .dimensions()
        .and_then(|(w, h)| Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_long_edge_preserves_aspect() {
        assert_eq!(fit_long_edge(1616, 1080, 320), (320, 213));
        assert_eq!(fit_long_edge(1080, 1616, 320), (213, 320));
        assert_eq!(fit_long_edge(100, 50, 320), (100, 50)); // never upscale
        assert_eq!(fit_long_edge(8640, 2, 320), (320, 1)); // extreme aspect
    }

    /// Issue #31: a bare JPEG session file whose scan was cut off (dying
    /// card, interrupted copy) must yield Failed — not the mostly-blank
    /// thumb zune-jpeg 0.4's zero-fill "success" produced. FAILS ON
    /// PRE-FIX CODE (make_grid_thumb returned Ok there).
    #[test]
    fn truncated_bare_jpeg_yields_failed_not_a_blank_thumb() {
        let dir = std::env::temp_dir().join(format!("fastcull-hostile31-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let intact = crate::raw::jpeg_hostile::encoded(320, 200);
        let path = dir.join("truncated.jpg");
        std::fs::write(&path, crate::raw::jpeg_hostile::truncate_scan(&intact, 64)).unwrap();
        let spec = JobSpec {
            path: path.clone(),
            size: 0,
            mtime: None,
        };
        let err = make_grid_thumb(&spec).expect_err("truncated scan must fail the thumb");
        assert!(err.contains("truncated"), "reason names the cause: {err}");
        // And the intact twin still thumbs fine through the same path.
        let ok_path = dir.join("intact.jpg");
        std::fs::write(&ok_path, &intact).unwrap();
        let spec = JobSpec {
            path: ok_path,
            size: 0,
            mtime: None,
        };
        make_grid_thumb(&spec).expect("intact stream still thumbs");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The queue contract, tested deterministically without threads: pop
    /// order honors priority then insertion order, promotion reprioritizes
    /// without duplication, done/in-flight jobs are not re-queued.
    #[test]
    fn queue_pops_promoted_jobs_first() {
        let jobs: Vec<JobSpec> = (0..100)
            .map(|i| JobSpec {
                path: PathBuf::from(format!("/j{i}")),
                size: 0,
                mtime: None,
            })
            .collect();
        // Zero worker threads is not constructible via start(); emulate the
        // pop loop directly on QueueState through a started-but-starved pool:
        // instead, drive the same lazy-deletion algorithm.
        let mut state = QueueState {
            heap: BinaryHeap::new(),
            queued: HashMap::new(),
            in_flight: HashSet::new(),
            next_seq: 0,
        };
        for index in 0..jobs.len() {
            let seq = state.next_seq;
            state.next_seq += 1;
            state.queued.insert(index, Priority::Background as u8);
            state
                .heap
                .push(std::cmp::Reverse((Priority::Background as u8, seq, index)));
        }
        // Promote 90..95 to Visible, 10..12 to Prefetch (same algorithm as
        // Pipeline::promote).
        for (range, prio) in [(90..95, Priority::Visible), (10..12, Priority::Prefetch)] {
            for index in range {
                let current = state.queued[&index];
                if (prio as u8) < current {
                    state.queued.insert(index, prio as u8);
                    let seq = state.next_seq;
                    state.next_seq += 1;
                    state.heap.push(std::cmp::Reverse((prio as u8, seq, index)));
                }
            }
        }
        let mut order = Vec::new();
        while let Some(std::cmp::Reverse((prio, _seq, index))) = state.heap.pop() {
            if state.queued.get(&index) == Some(&prio) {
                state.queued.remove(&index);
                order.push(index);
            }
        }
        assert_eq!(order.len(), 100, "each job exactly once");
        assert_eq!(&order[0..5], &[90, 91, 92, 93, 94], "visible first");
        assert_eq!(&order[5..7], &[10, 11], "prefetch second");
        // Background keeps sequential file order (0,1,2,... minus promoted).
        assert_eq!(&order[7..10], &[0, 1, 2]);
        assert!(order[7..].windows(2).all(|w| w[0] < w[1]));
    }

    // ---- Adaptive read pool (raw-pipeline.md, 2026-07-25) ----

    fn test_controller(cap: usize) -> PoolController {
        PoolController {
            limit: POOL_MIN_READERS,
            floor: POOL_MIN_READERS,
            cap,
            grow_below: Duration::from_millis(200),
            shrink_above: Duration::from_millis(500),
        }
    }

    /// FASTCULL_MAX_READERS override semantics (spec: caps the adaptive
    /// range; at or below the floor it pins the pool to a fixed size).
    #[test]
    fn controller_override_caps_and_pins() {
        let c = PoolController::with_override(Some(1));
        assert_eq!((c.limit, c.floor, c.cap), (1, 1, 1), "N=1 pins one reader");
        let c = PoolController::with_override(Some(4));
        assert_eq!((c.limit, c.floor, c.cap), (4, 4, 4), "N=4 = old fixed gate");
        let c = PoolController::with_override(Some(6));
        assert_eq!((c.limit, c.floor, c.cap), (4, 4, 6), "N>4 caps growth only");
        let c = PoolController::with_override(None);
        assert_eq!((c.limit, c.floor), (4, 4));
        assert_eq!(c.cap, pool_cap());
    }

    #[test]
    fn controller_starts_at_floor_and_clamps_both_ends() {
        let mut c = test_controller(6);
        assert_eq!(c.limit, POOL_MIN_READERS, "initial limit is the proven 4");
        // Additive growth, +1 per fast probe, clamped at the cap.
        for expected in [5, 6, 6] {
            c.grow_one();
            assert_eq!(c.limit, expected);
        }
        // Multiplicative decrease: from a core-count cap (32) a choking
        // card reaches the floor in 3 halvings, not 28 single steps, and
        // never goes below 4 (user decision: NAS keeps the proven floor).
        let mut c = test_controller(32);
        c.limit = 32;
        for expected in [16, 8, 4, 4] {
            c.shrink_halve();
            assert_eq!(c.limit, expected);
        }
    }

    /// Live incident 2026-07-25 regression: 0 ms page-cache-warm probes
    /// pumped the limit 4 -> 22 while every cold read sat wedged on the
    /// microSD. "If the loader is not stuck, we can add more" — a stuck
    /// read anywhere vetoes growth, no matter how fast the probe was.
    #[test]
    fn pool_warm_probe_cannot_outvote_stuck_read() {
        let pool = ReadPool::new();
        pool.set_thresholds_for_test(Duration::from_millis(150), Duration::from_secs(60));
        pool.set_cap_for_test(POOL_MIN_READERS + 2); // headroom even on 4-core CI
        let stuck = pool.acquire(0, false);
        std::thread::sleep(Duration::from_millis(300)); // older than grow_below
        drop(pool.acquire(0, true)); // ~0 ms warm probe
        assert_eq!(
            pool.current_limit(),
            POOL_MIN_READERS,
            "growth must be vetoed while any read is stuck"
        );
        drop(stuck);
        drop(pool.acquire(0, true)); // nothing stuck anymore: growth resumes
        assert_eq!(pool.current_limit(), POOL_MIN_READERS + 1);
    }

    /// Dead-band: a probe between the thresholds moves nothing.
    #[test]
    fn pool_dead_band_holds() {
        let pool = ReadPool::new();
        pool.set_thresholds_for_test(Duration::from_millis(1), Duration::from_secs(60));
        let probe = pool.acquire(0, true);
        std::thread::sleep(Duration::from_millis(150));
        drop(probe);
        assert_eq!(pool.current_limit(), POOL_MIN_READERS);
    }

    #[test]
    fn pool_cap_is_at_least_the_floor() {
        assert!(pool_cap() >= POOL_MIN_READERS);
        assert_eq!(PoolController::new().limit, POOL_MIN_READERS);
    }

    /// The in-tree concurrency invariant (spec: replaces the fixed-gate era's
    /// manual fd proof): concurrent readers never exceed the current limit,
    /// and the limit never exceeds the cap.
    #[test]
    fn pool_concurrency_never_exceeds_limit() {
        let pool = std::sync::Arc::new(ReadPool::new());
        pool.set_limit_for_test(2);
        let current = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (pool, current, max_seen) = (pool.clone(), current.clone(), max_seen.clone());
                std::thread::spawn(move || {
                    for _ in 0..5 {
                        let _permit = pool.acquire(Priority::Background as u8, false);
                        let now = current.fetch_add(1, Ordering::SeqCst) + 1;
                        max_seen.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(2));
                        current.fetch_sub(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let max = max_seen.load(Ordering::SeqCst);
        assert!(max <= 2, "observed {max} concurrent readers at limit 2");
        assert_eq!(
            pool.current_limit(),
            2,
            "no probes ran; limit must not move"
        );
    }

    /// Validator finding (severe): at low limits a freed slot must go to the
    /// highest-priority waiter, not FIFO — visible beats background.
    #[test]
    fn pool_releases_highest_priority_waiter_first() {
        let pool = std::sync::Arc::new(ReadPool::new());
        // Disarm the stall shrink: under heavy parallel test load the held
        // permit could cross a 500 ms default threshold and mutate the limit
        // mid-assertion.
        pool.set_thresholds_for_test(Duration::from_millis(200), Duration::from_secs(60));
        pool.set_limit_for_test(1);
        let held = pool.acquire(Priority::Background as u8, false);
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));

        let spawn_waiter = |prio: Priority, tag: &'static str| {
            let (pool, order) = (pool.clone(), order.clone());
            std::thread::spawn(move || {
                let _permit = pool.acquire(prio as u8, false);
                order.lock().unwrap().push(tag);
            })
        };
        // Background arrives FIRST, visible second — priority must win over
        // arrival order.
        let b = spawn_waiter(Priority::Background, "background");
        wait_until(|| pool.waiting_count() == 1);
        let v = spawn_waiter(Priority::Visible, "visible");
        wait_until(|| pool.waiting_count() == 2);

        drop(held);
        b.join().unwrap();
        v.join().unwrap();
        assert_eq!(*order.lock().unwrap(), vec!["visible", "background"]);
    }

    #[test]
    fn pool_probe_grows_shrinks_and_excludes_large_reads() {
        let pool = ReadPool::new();
        pool.set_thresholds_for_test(Duration::from_millis(150), Duration::from_secs(60));
        pool.set_limit_for_test(2);
        // Fast probe completion grows.
        drop(pool.acquire(0, true));
        assert_eq!(pool.current_limit(), 3);
        // A >2 MB read never feeds the controller (non-A1 fallback path).
        let large = pool.acquire(0, true);
        large.set_bytes(3 * 1024 * 1024);
        drop(large);
        assert_eq!(pool.current_limit(), 3, "large read must not be sampled");
        // Non-sampled sections (EXIF) never probe.
        drop(pool.acquire(0, false));
        assert_eq!(pool.current_limit(), 3);
        // Only one probe at a time: with a probe outstanding, a second
        // sampled acquire is not a probe, so its fast completion is ignored.
        let probe = pool.acquire(0, true);
        drop(pool.acquire(0, true));
        assert_eq!(pool.current_limit(), 3);
        drop(probe); // fast → +1
        assert_eq!(pool.current_limit(), 4);
    }

    /// Validator H1 regression: a >2 MB probe read must feed NO decision —
    /// the stall path in particular must not shrink on it, or the non-A1
    /// large-preview fallback pins a healthy medium at the floor.
    #[test]
    fn pool_large_probe_never_stall_shrinks() {
        let pool = ReadPool::new();
        pool.set_thresholds_for_test(Duration::from_millis(1), Duration::from_millis(150));
        pool.set_limit_for_test(16);
        let probe = pool.acquire(0, true);
        probe.set_bytes(3 * 1024 * 1024); // known large before the bulk read
        std::thread::sleep(Duration::from_millis(300)); // well past shrink_above
        drop(pool.acquire(0, false)); // pool touch runs the stall check
        assert_eq!(
            pool.current_limit(),
            16,
            "excluded probe must not stall-shrink"
        );
        drop(probe); // slow completion must also feed nothing
        assert_eq!(
            pool.current_limit(),
            16,
            "excluded probe must not shrink on completion"
        );
    }

    /// The single-quiet-reader scenario: a slow probe completion must shrink
    /// on its own even when NO other pool touch ever ran the stall check
    /// (validator round-3 observation: this branch was previously untested).
    #[test]
    fn pool_slow_completion_shrinks_without_other_touches() {
        let pool = ReadPool::new();
        pool.set_thresholds_for_test(Duration::from_millis(1), Duration::from_millis(150));
        pool.set_limit_for_test(16);
        let probe = pool.acquire(0, true);
        std::thread::sleep(Duration::from_millis(300));
        // Drop is the FIRST pool touch since the acquire: the permit removes
        // itself before the stall check, so the completion branch decides.
        drop(probe);
        assert_eq!(pool.current_limit(), 8, "slow completion must halve");
    }

    /// Spec: a stalled read is a slow signal, decided WITHOUT waiting for
    /// completion; the shrink throttle (one per shrink-threshold window)
    /// then suppresses the immediate second shrink when the same probe's
    /// slow completion lands moments later.
    #[test]
    fn pool_stalled_probe_shrinks_once() {
        let pool = std::sync::Arc::new(ReadPool::new());
        pool.set_thresholds_for_test(Duration::from_millis(1), Duration::from_millis(150));
        pool.set_limit_for_test(16);
        let probe = pool.acquire(0, true);
        std::thread::sleep(Duration::from_millis(300));
        // Any pool interaction notices the stall — here a blocked-waiter
        // recheck is simulated by a plain acquire/release at prio 0.
        drop(pool.acquire(0, false));
        assert_eq!(
            pool.current_limit(),
            8,
            "stall must halve before completion"
        );
        // Completion is long past shrink_above, but the throttle window
        // (150 ms, reopened microseconds ago) suppresses a second shrink.
        drop(probe);
        assert_eq!(pool.current_limit(), 8, "one shrink per window");
    }

    fn wait_until(cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(Instant::now() < deadline, "wait_until timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
