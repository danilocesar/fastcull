//! Trace facility: the FASTCULL_TRACE stall log every controller writes to
//! (one emit site, because tests grep the line format).

/// FASTCULL_TRACE=1: log UI-thread stalls to stderr (any handle_nav /
/// refresh phase over the trace threshold). Debug facility for hang
/// reports — zero cost when off.
fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FASTCULL_TRACE").is_some())
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
pub(crate) fn trace_mark(label: &str) {
    if trace_enabled() {
        eprintln!("fastcull-trace: [{}] {label}", trace_clock());
    }
}
