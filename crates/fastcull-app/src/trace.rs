//! Trace facility: the FASTCULL_TRACE stall log every controller writes to
//! (one emit site, because tests grep the line format) — and, through that
//! same emit site, the condition a `FASTCULL_DRIVE` `wait:` step waits on.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// FASTCULL_TRACE=1: log UI-thread stalls to stderr (any handle_nav /
/// refresh phase over the trace threshold). Debug facility for hang
/// reports — zero cost when off.
fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FASTCULL_TRACE").is_some())
}

/// The substrings a driven run's `wait:` steps are watching for, each with
/// the flag its step polls.
static WAITS: Mutex<Vec<(String, Arc<AtomicBool>)>> = Mutex::new(Vec::new());

/// True once any `wait:` has been registered — i.e. in driven runs only.
/// Every emit site loads this one atomic and returns; a normal run never
/// takes the lock and never walks the list.
static WATCHING: AtomicBool = AtomicBool::new(false);

/// Register a `wait:<substring>` (harness.rs). Deliberately INDEPENDENT of
/// `FASTCULL_TRACE`: the switch decides what is printed, not what the app
/// can observe about itself, so a script may wait without asking for the
/// stall log. Registration happens at script-parse time, before the first
/// frame, because a wait must also be satisfied by a mark emitted long
/// BEFORE its own step comes due (issue #50 waits on a thumb that landed
/// seconds earlier).
///
/// For the script author, that is the whole contract and its limits:
///
/// * A wait asks "has this happened yet?", so PAST marks count — but the
///   NEXT occurrence of a mark already emitted once cannot be asked for.
///   Pick a substring unique to the state you mean (a geometry, an index,
///   a name) or keep that step on the clock. The fix, where the mark is
///   ours, is to put the thing that DIFFERS into the mark: the second
///   session's settle used to be the standing example of this limit, and
///   is now `wait:load settled gen 1` because the mark carries the
///   session generation (issue #62).
/// * "Past" starts at `harness::install`, which main.rs runs AFTER the
///   session dispatch and the first refresh — a mark emitted in that
///   window (the opening scan, the first layout) is never observed, so a
///   wait on one of those never fires.
/// * Only the APP is observed. Every line the harness writes about its own
///   script — the `drive: <action>` echo, the pointer/wheel echoes, the
///   modal-swallow line, the wait reports — goes out through
///   [`trace_mark_unobserved`], because each quotes the script's own text
///   and would otherwise let a wait be satisfied by a later step's echo.
///   `QEDUMP` lines stay observable: they report app state, not the script.
/// * A substring cannot contain `;` — the script's step separator splits
///   it first. (Same recorded limitation as `open:PATH`.)
pub(crate) fn watch_for(substring: &str) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    if let Ok(mut waits) = WAITS.lock() {
        waits.push((substring.to_string(), Arc::clone(&flag)));
        WATCHING.store(true, Ordering::Release);
    }
    flag
}

/// Is anything waiting? Lets the per-item emit site build its label when
/// tracing is off but a script is waiting on one of those labels.
fn watching() -> bool {
    WATCHING.load(Ordering::Acquire)
}

/// Raise the flag of every `wait:` this label satisfies. Called from the
/// one emit site, so a mark a script can wait on is exactly a mark the
/// trace log can show — there is no second, divergent list.
fn observe(label: &str) {
    if !watching() {
        return;
    }
    let Ok(waits) = WAITS.lock() else { return };
    for (needle, flag) in waits.iter() {
        if !flag.load(Ordering::Acquire) && label.contains(needle.as_str()) {
            flag.store(true, Ordering::Release);
        }
    }
}

fn trace_clock() -> u128 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis()
}

/// Start a stopwatch for `trace_slow` — None (and no cost) when tracing
/// is off. The `Option` IS the on/off switch, so a timed section never
/// calls `Instant::now` in a normal run.
pub(crate) fn trace_start() -> Option<std::time::Instant> {
    trace_enabled().then(std::time::Instant::now)
}

/// Report a stopwatch started by `trace_start` if the section was slow
/// enough to matter for a hang report.
pub(crate) fn trace_slow(label: &str, t0: Option<std::time::Instant>) {
    if let Some(t0) = t0 {
        let ms = t0.elapsed().as_millis();
        if ms > 20 {
            trace_mark(&format!("{label} took {ms} ms"));
        }
    }
}

/// The ONE place a trace line is emitted: tests grep these lines, so the
/// format lives in a single string.
fn emit(label: &str) {
    if trace_enabled() {
        eprintln!("fastcull-trace: [{}] {label}", trace_clock());
    }
}

/// Trace a mark — and answer any `wait:` it satisfies, so what a script
/// can wait for is exactly what the log can show.
pub(crate) fn trace_mark(label: &str) {
    emit(label);
    observe(label);
}

/// Trace a mark the `wait:` matcher must NOT see. One kind of caller: the
/// harness narrating its own script — the `drive: <action>` echo, the
/// pointer and wheel echoes, the modal-swallow line, and the wait reports.
/// Each quotes text taken from the script, so leaving them observable
/// would let a wait be satisfied by a later step's echo (or by its own),
/// when the whole point is to observe the APP.
pub(crate) fn trace_mark_unobserved(label: &str) {
    emit(label);
}

/// `trace_mark` for PER-ITEM sites: the label is built only if tracing is
/// on (or something is waiting on it). `trace_mark(&format!(…))` formats
/// first and discards second, which is free at a handful of call sites and
/// is not at one per image — a 50,000-file scan would pay 50,000
/// allocations for output nobody asked for, against this module's "zero
/// cost when off" promise.
pub(crate) fn trace_mark_with(label: impl FnOnce() -> String) {
    if trace_enabled() || watching() {
        trace_mark(&label());
    }
}
