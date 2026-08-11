//! The engine pump: the 33 ms tick that drains every worker channel
//! (thumbnails/metadata/sidecars, copy progress, loupe decodes) into
//! `AppState`, plus the kitchen-completion adoption both it and the
//! worker's own nudge go through.

use std::cell::RefCell;
use std::rc::Rc;

use fastcull_core::loupe::is_top_rung;
use fastcull_core::pipeline::SessionEvent;
use slint::ComponentHandle;

use crate::copy_bridge::recompute_bursts;
use crate::kitchen;
use crate::loupe_ctrl::{insert_fullres, route_warm, WarmCtx, WarmJob};
use crate::nav::recompute_view_keep_cursor;
use crate::presenter::refresh;
use crate::state::{AppState, MIDS_CAP};
use crate::trace::trace_mark;
use crate::MainWindow;

/// Wire the kitchen's completion nudge: a finished texture is adopted as
/// soon as the UI thread is idle, without waiting for the next tick.
pub(crate) fn wire(window: &MainWindow, state: &Rc<RefCell<AppState>>) {
    {
        // Kitchen completions: adopt immediately (the worker's nudge), so
        // a finished texture never waits for the 33 ms pump.
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_kitchen_ready(move || {
            let Some(win) = win.upgrade() else { return };
            if drain_kitchen(&win, &state) {
                refresh(&win, &state);
            }
        });
    }
}

/// Start the 33 ms engine pump. Returns the timer: dropping it stops the
/// pump, so main.rs holds it for the life of the window.
pub(crate) fn start(window: &MainWindow, state: &Rc<RefCell<AppState>>) -> slint::Timer {
    // Engine event pump: drain pending events every 33 ms; refresh once if
    // anything relevant arrived. Receivers live in AppState so File > Open
    // Folder can swap the session under a running pump.
    let timer = slint::Timer::default();
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(33),
            move || {
                let mut dirty = false;
                {
                    let mut st = state.borrow_mut();
                    let events: Vec<SessionEvent> = st
                        .pipeline_rx
                        .as_ref()
                        .map(|rx| rx.try_iter().collect())
                        .unwrap_or_default();
                    for event in events {
                        match event {
                            SessionEvent::ThumbReady {
                                index, thumb_jpeg, ..
                            } => {
                                st.thumb_jpegs.insert(index, thumb_jpeg);
                                st.thumbs_done += 1;
                                dirty = true;
                            }
                            SessionEvent::Failed { index, .. } => {
                                st.failed.insert(index);
                                st.thumbs_done += 1;
                                dirty = true;
                            }
                            SessionEvent::MetadataReady { index, exif, .. } => {
                                // Capture-time sort keys arrive progressively;
                                // the view re-sorts as they land (spec:
                                // keyless images sort after keyed ones).
                                if let Some(slot) = st.capture_keys.get_mut(index) {
                                    *slot = exif.sort_key();
                                    dirty = true;
                                }
                                if let Some(slot) = st.frame_meta.get_mut(index) {
                                    *slot = fastcull_core::burst::FrameMeta::from_summary(&exif);
                                    st.burst_dirty = true;
                                }
                            }
                            SessionEvent::Sidecar { index, pick, iptc } => {
                                // Picks from a previous session/tool — never
                                // override what the user changed just now.
                                if !st.touched.contains(&index) {
                                    if let Some(slot) = st.picks.get_mut(index) {
                                        *slot = pick;
                                        dirty = true;
                                    }
                                }
                                // Same guard as picks: a sidecar read that
                                // raced the debounced writer must not
                                // revert a fresh panel edit (gate finding).
                                if !st.touched_iptc.contains(&index) {
                                    if let Some(slot) = st.iptc.get_mut(index) {
                                        *slot = *iptc;
                                    }
                                }
                            }
                        }
                    }
                    let at_loupe = st.at_loupe();
                    let failures: Vec<_> = st
                        .sidecar_errs
                        .as_ref()
                        .map(|rx| rx.try_iter().collect())
                        .unwrap_or_default();
                    for _failure in failures {
                        // Count for the status bar only: the writer's drain
                        // already eprintlns the path+reason (QE finding —
                        // logging here too printed every failure twice).
                        st.sidecar_failures += 1;
                        dirty = true;
                    }
                    // Copy Picks progress/report (M6).
                    let copy_events: Vec<_> = st
                        .copy
                        .rx
                        .as_ref()
                        .map(|rx| rx.try_iter().collect())
                        .unwrap_or_default();
                    for event in copy_events {
                        use fastcull_core::fileops::CopyEvent;
                        match event {
                            CopyEvent::File { index, total, name } => {
                                if let Some(win) = win.upgrade() {
                                    win.set_copy_progress(
                                        format!("{index} / {total} — {name}").into(),
                                    );
                                }
                            }
                            CopyEvent::Failed { .. } => {} // in the report
                            CopyEvent::Finished(report) => {
                                if let Some(dest) = st.copy.dest.clone() {
                                    for id in &report.copied_ids {
                                        st.copy.copied_to.insert(*id, dest.clone());
                                    }
                                }
                                st.copy.handle = None;
                                st.copy.rx = None;
                                if let Some(win) = win.upgrade() {
                                    // The green light to format a card
                                    // appears ONLY when this run actually
                                    // verified copies (gate finding: an
                                    // all-skipped run verified nothing).
                                    let verified_line = report.copied > 0
                                        && report.all_verified
                                        && report.failed.is_empty()
                                        && !report.cancelled;
                                    let mut lines = vec![if report.copied == 0 {
                                        "Nothing needed copying".to_string()
                                    } else {
                                        format!(
                                            "{} copied{}",
                                            report.copied,
                                            if verified_line {
                                                ", all checksums verified"
                                            } else {
                                                ""
                                            }
                                        )
                                    }];
                                    if report.skipped > 0 {
                                        lines.push(format!("{} skipped", report.skipped));
                                    }
                                    if report.refreshed > 0 {
                                        lines.push(format!(
                                            "{} sidecars refreshed",
                                            report.refreshed
                                        ));
                                    }
                                    if report.cancelled {
                                        lines.push("cancelled — finished files remain".into());
                                    }
                                    for (name, reason) in &report.failed {
                                        lines.push(format!("FAILED {name}: {reason}"));
                                    }
                                    win.set_copy_report(lines.join("\n").into());
                                    win.set_copy_state(2);
                                }
                                dirty = true; // copied badges
                            }
                        }
                    }
                    let loupe_events: Vec<_> = st
                        .loupe_rx
                        .as_ref()
                        .map(|rx| rx.try_iter().collect())
                        .unwrap_or_default();
                    for event in loupe_events {
                        match event {
                            fastcull_core::loupe::LoupeEvent::Ready {
                                index,
                                image,
                                terminal,
                            } => {
                                let long = image.width.max(image.height);
                                trace_mark(&format!("loupe ready idx {index} long {long}"));
                                let job = route_warm(
                                    long,
                                    terminal,
                                    at_loupe,
                                    // Ignored by Announced (see route_warm):
                                    // a freshly announced decode supersedes
                                    // whatever is held, so don't pay for the
                                    // lookup on this per-event path.
                                    false,
                                    WarmCtx::Announced,
                                );
                                if let Some(WarmJob::Wrap { terminal: true }) = job {
                                    // Metadata now; the texture follows
                                    // when the kitchen serves it.
                                    st.terminal_native.insert(index);
                                }
                                match job {
                                    // Mid rung: kitchen copies it off-thread.
                                    Some(WarmJob::Wrap { terminal }) => {
                                        st.kitchen.submit_wrap(index, image.clone(), terminal)
                                    }
                                    // Full rung: the fill runs on the kitchen
                                    // worker; the core LRU keeps the pixels.
                                    Some(WarmJob::Full) => {
                                        st.kitchen.submit_full(index, image.clone())
                                    }
                                    None => {}
                                }
                                dirty = true;
                            }
                            fastcull_core::loupe::LoupeEvent::Failed { index, .. } => {
                                st.failed.insert(index); // badge; core won't retry
                                dirty = true;
                            }
                        }
                    }
                    if dirty {
                        // Picks/keys may have changed membership or order.
                        // ENGINE-driven: must not move an untouched cursor
                        // once the folder has loaded (issue #25).
                        recompute_view_keep_cursor(&mut st, false);
                    }

                    if st.burst_dirty {
                        st.burst_dirty = false;
                        recompute_bursts(&mut st);
                    }
                }
                // Kitchen fallback drain: the worker's event-loop nudge is
                // the fast path; this tick-time drain catches completions
                // whose nudge raced a busy loop iteration.
                let kitchen_dirty = win
                    .upgrade()
                    .map(|win| drain_kitchen(&win, &state))
                    .unwrap_or(false);
                if dirty || kitchen_dirty {
                    if let Some(win) = win.upgrade() {
                        refresh(&win, &state);
                    }
                }
            },
        );
    }
    timer
}

/// Adopt every texture the kitchen has finished — UNBUDGETED, per the
/// persona condition: the wrap is O(1), so rationing adoption would turn
/// "one tick later" into a visible trickle-in. Returns whether anything
/// was adopted (callers refresh on true).
fn drain_kitchen(win: &MainWindow, state: &Rc<RefCell<AppState>>) -> bool {
    let mut st = state.borrow_mut();
    let at_loupe = st.at_loupe();
    let done = st.kitchen.drain();
    if done.is_empty() {
        return false;
    }
    trace_mark(&format!("kitchen: adopting {} done", done.len()));
    for d in done {
        match d {
            kitchen::Done::Thumb { index, buf } => {
                st.images.insert(index, slint::Image::from_rgb8(buf));
            }
            kitchen::Done::Full { index, buf } => {
                // The 150 MB texture is only held while the loupe can use
                // it (same gate as the old event-time adoption, re-checked
                // NOW because the user may have left the loupe while it
                // cooked); the core LRU keeps the pixels either way.
                if at_loupe {
                    let texture = slint::Image::from_rgb8(buf);
                    insert_fullres(&mut st, index, texture);
                }
            }
            kitchen::Done::Wrap {
                index,
                buf,
                terminal,
            } => {
                let long = buf.width().max(buf.height());
                let texture = slint::Image::from_rgb8(buf);
                if terminal {
                    // The file's best rung IS this texture (issue #8).
                    insert_fullres(&mut st, index, texture.clone());
                }
                // Size-only, like route_warm's Announced arm: this asks
                // "may this artifact enter the mid cache", which is about
                // the pixels, not about the file's ladder being topped
                // out — hence the explicit `false` for terminal.
                if !is_top_rung(long, false)
                    && (st.mids.len() < MIDS_CAP || st.mids.contains_key(&index))
                {
                    st.mids.insert(index, texture);
                    st.va.note_held(index, long);
                }
            }
            kitchen::Done::Mid {
                index,
                buf,
                held_long,
            } => {
                if st.mids.len() < MIDS_CAP || st.mids.contains_key(&index) {
                    st.mids.insert(index, slint::Image::from_rgb8(buf));
                    st.va.note_held(index, held_long);
                }
            }
        }
    }
    drop(st);
    let _ = win;
    true
}
