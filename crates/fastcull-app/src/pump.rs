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
use crate::trace::{trace_mark, trace_mark_with};
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
                        .session
                        .pipeline_rx
                        .as_ref()
                        .map(|rx| rx.try_iter().collect())
                        .unwrap_or_default();
                    for event in events {
                        match event {
                            SessionEvent::ThumbReady {
                                index, thumb_jpeg, ..
                            } => {
                                st.textures.thumb_jpegs.insert(index, thumb_jpeg);
                                st.session.thumbs_done += 1;
                                dirty = true;
                                // The thumb path has two stages and only
                                // this one touches the FILE: the embedded
                                // JPEG is read here, at scan time, and
                                // decoded into a texture much later (when
                                // the cell is near the view). A test that
                                // corrupts a file mid-run has to know the
                                // read already happened (issue #50).
                                trace_mark_with(|| format!("thumb bytes idx {index}"));
                            }
                            SessionEvent::Failed { index, .. } => {
                                st.textures.failed.insert(index);
                                st.session.thumbs_done += 1;
                                dirty = true;
                            }
                            SessionEvent::MetadataReady { index, exif, .. } => {
                                // Capture-time sort keys arrive progressively;
                                // the view re-sorts as they land (spec:
                                // keyless images sort after keyed ones).
                                if let Some(slot) = st.session.capture_keys.get_mut(index) {
                                    *slot = exif.sort_key();
                                    dirty = true;
                                }
                                if let Some(slot) = st.session.frame_meta.get_mut(index) {
                                    *slot = fastcull_core::burst::FrameMeta::from_summary(&exif);
                                    st.bursts.dirty = true;
                                }
                                // `{camera}` in both template engines. Kept
                                // as the MODEL alone (iptc-templates.md:
                                // "EXIF model string"), which is why this
                                // is not read off FrameMeta::camera — that
                                // one prefers the serial number.
                                if let Some(slot) = st.session.camera_models.get_mut(index) {
                                    *slot = exif.camera_model.clone();
                                }
                            }
                            SessionEvent::Sidecar { index, pick, iptc } => {
                                // Picks from a previous session/tool — never
                                // override what the user changed just now.
                                if !st.session.touched.contains(&index) {
                                    if let Some(slot) = st.session.picks.get_mut(index) {
                                        *slot = pick;
                                        dirty = true;
                                    }
                                }
                                // Same guard as picks: a sidecar read that
                                // raced the debounced writer must not
                                // revert a fresh panel edit (gate finding).
                                if !st.session.touched_iptc.contains(&index) {
                                    if let Some(slot) = st.session.iptc.get_mut(index) {
                                        *slot = *iptc;
                                    }
                                }
                            }
                        }
                    }
                    let at_loupe = st.at_loupe();
                    let failures: Vec<_> = st
                        .session
                        .sidecar_errs
                        .as_ref()
                        .map(|rx| rx.try_iter().collect())
                        .unwrap_or_default();
                    for _failure in failures {
                        // Count for the status bar only: the writer's drain
                        // already eprintlns the path+reason (QE finding —
                        // logging here too printed every failure twice).
                        st.session.sidecar_failures += 1;
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
                            CopyEvent::File {
                                index,
                                total,
                                name,
                                action,
                            } => {
                                if let Some(win) = win.upgrade() {
                                    // Say WHICH work this is: an overwrite
                                    // run starts by hashing what is
                                    // already at the destination, and a
                                    // progress line that counts to 148
                                    // saying "copying" reads as "it is
                                    // sending my whole export again"
                                    // (persona 2026-08-21).
                                    let verb = match action {
                                        fastcull_core::fileops::PlanAction::Replace => "Checking",
                                        _ => "Copying",
                                    };
                                    win.set_copy_progress(
                                        format!("{verb} {index} / {total} — {name}").into(),
                                    );
                                }
                            }
                            CopyEvent::Failed { .. } => {} // in the report
                            CopyEvent::Finished(report) => {
                                for (id, path) in &report.landed {
                                    st.copy.copies.record(*id, path.clone());
                                }
                                st.copy.handle = None;
                                st.copy.rx = None;
                                if let Some(win) = win.upgrade() {
                                    let lines = report_lines(&report);
                                    win.set_copy_report(lines.join("\n").into());
                                    win.set_copy_state(2);
                                }
                                dirty = true; // copied badges
                            }
                        }
                    }
                    // Export Frames as Video progress/report (M9).
                    let clip_events: Vec<_> = st
                        .clip
                        .rx
                        .as_ref()
                        .map(|rx| rx.try_iter().collect())
                        .unwrap_or_default();
                    for event in clip_events {
                        use fastcull_core::clip::ClipEvent;
                        match event {
                            ClipEvent::Frame { index, total, name } => {
                                if let Some(win) = win.upgrade() {
                                    win.set_clip_progress(
                                        format!("Writing {index} / {total} — {name}").into(),
                                    );
                                }
                            }
                            ClipEvent::Verifying { index, total } => {
                                // The read-back counts too: on a 4 GB
                                // export it takes tens of seconds, and a
                                // line frozen at "400 / 400" reads as a
                                // hang.
                                if let Some(win) = win.upgrade() {
                                    win.set_clip_progress(
                                        format!("Verifying {index} / {total}").into(),
                                    );
                                }
                            }
                            ClipEvent::Finished(report) => {
                                st.clip.handle = None;
                                st.clip.rx = None;
                                st.clip.running_dst = None;
                                // The ▶ badge (issue #56). CORE decides
                                // whether this run left a file to point
                                // at: it is the same question as the
                                // report's green light, so it has to be
                                // the same answer. `record` supersedes
                                // the same path, so an Overwrite with a
                                // different frame set drops the frames of
                                // the file it replaced.
                                let stashed = std::mem::take(&mut st.clip.running_frames);
                                if let Some((path, frames)) = report.frames_to_record(stashed) {
                                    st.clip.ledger.record(path, frames);
                                    // Follow the disk at the second of
                                    // the two re-check points.
                                    st.clip.ledger.refresh();
                                }
                                if let Some(win) = win.upgrade() {
                                    win.set_clip_report(
                                        clip_report_lines(&report).join("\n").into(),
                                    );
                                    win.set_clip_state(2);
                                }
                                dirty = true;
                            }
                        }
                    }
                    // A refusal explained in the status line expires on
                    // its own; the tick that notices has to ask for the
                    // repaint that removes it.
                    if st
                        .clip
                        .notice
                        .as_ref()
                        .is_some_and(|(_, at)| at.elapsed() >= crate::state::CLIP_NOTICE_LIFE)
                    {
                        st.clip.notice = None;
                        dirty = true;
                    }
                    let loupe_events: Vec<_> = st
                        .loupe_view
                        .rx
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
                                    st.textures.terminal_native.insert(index);
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
                                st.textures.failed.insert(index); // badge; core won't retry
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

                    if st.bursts.dirty {
                        st.bursts.dirty = false;
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
                st.textures
                    .images
                    .insert(index, slint::Image::from_rgb8(buf));
                // The thumb rung's ARMING moment, and the only observable
                // one: nothing evicts from `st.textures.images` within a
                // session, so from here on the loupe's thumb rescue has a
                // texture in hand for this image. The rung trace says the
                // rescue RENDERED, which is a different question — a test
                // about the gate that SKIPS the rescue can never see that
                // line and needs the arming fact separately (issue #50).
                trace_mark_with(|| format!("thumb landed idx {index}"));
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
                    && (st.textures.mids.len() < MIDS_CAP || st.textures.mids.contains_key(&index))
                {
                    st.textures.mids.insert(index, texture);
                    st.textures.va.note_held(index, long);
                }
            }
            kitchen::Done::Mid {
                index,
                buf,
                held_long,
            } => {
                if st.textures.mids.len() < MIDS_CAP || st.textures.mids.contains_key(&index) {
                    st.textures.mids.insert(index, slint::Image::from_rgb8(buf));
                    st.textures.va.note_held(index, held_long);
                }
            }
        }
    }
    drop(st);
    let _ = win;
    true
}

/// The video export's report lines (video-export.md, "Dialog").
///
/// A free function beside its Copy Picks twin, and for the same reason:
/// the honesty rules get a test of their own. "All checksums verified"
/// is not a decoration this function may add — the RULE lives in core
/// (`ClipReport::earned_the_green_light`) and this only decides which
/// line carries the sentence. The skipped and cadence wording is the
/// PLAN's own, so what the user reads afterwards is what they agreed to.
pub(crate) fn clip_report_lines(report: &fastcull_core::clip::ClipReport) -> Vec<String> {
    use crate::clip_bridge::{mirrored_note, seconds};
    let mut lines = Vec::new();
    // THE HEADLINE FIRST, and it is exactly one of four things. A run
    // that failed used to lead with "skipped — 1 frame: different size",
    // because the skip note was pushed before anything said what
    // happened — the Copy Picks rule this report inherits is that the
    // headline may never contradict the body, and a note about frames
    // left out is not a headline for a run that wrote nothing.
    if report.frames > 0 && report.path.is_some() {
        lines.push(format!(
            "Exported {} frames · {} · {} · {} → {}{}",
            report.frames,
            seconds(report.duration_ms),
            report
                .cadence
                .map(|c| c.text())
                .unwrap_or_else(|| "unknown cadence".into()),
            crate::copy_bridge::human_bytes(report.bytes),
            report.name,
            if report.earned_the_green_light() {
                ", all checksums verified"
            } else {
                ""
            }
        ));
        if report.replaced {
            lines.push("replaced the file that was already there".into());
        }
    } else if report.cancelled {
        // Unlike a cancelled copy, there are no "finished files" to keep:
        // this operation produces exactly one file, and a cancel means it
        // was never created.
        lines.push("Cancelled — nothing was written".into());
    } else if let Some(reason) = &report.failed {
        lines.push(format!("FAILED: {reason}"));
    } else {
        lines.push("Nothing was exported".into());
    }
    // ...then what else is worth knowing, whichever way it went.
    let skipped = fastcull_core::clip::skipped_text(&report.skipped);
    if !skipped.is_empty() {
        lines.push(skipped);
    }
    if report.mirrored > 0 {
        lines.push(mirrored_note(report.mirrored));
    }
    lines
}

/// The final report's lines, from what the run actually did.
///
/// A free function so the honesty rules have a test of their own: the
/// green light ("all checksums verified") appears only over files this run
/// copied or re-verified, and the headline never contradicts the body —
/// saying "nothing needed copying" over a run whose files FAILED states
/// the opposite of what happened (QE finding 2026-08-21).
pub(crate) fn report_lines(report: &fastcull_core::fileops::CopyReport) -> Vec<String> {
    // The rule for the green light lives in core (CLAUDE.md rule 5); this
    // function only decides which line carries the sentence.
    let verified = report.earned_the_green_light();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let mut lines = Vec::new();
    if report.copied > 0 {
        lines.push(format!(
            "{} copied{}",
            report.copied,
            if verified && report.identical == 0 {
                ", all checksums verified"
            } else {
                ""
            }
        ));
    }
    if report.identical > 0 {
        // The re-run's real answer, and a free "is my export still
        // bit-perfect?" pass before the card is wiped (persona).
        lines.push(format!(
            "{} already identical — re-verified in place{}",
            report.identical,
            if verified {
                ", all checksums verified"
            } else {
                ""
            }
        ));
    }
    if report.renamed > 0 {
        // One real example name: the report is what the user reads before
        // switching to darktable, and the names are how they find those
        // frames again.
        lines.push(match &report.renamed_example {
            Some(name) => format!("{} landed under new names ({name} …)", report.renamed),
            None => format!("{} landed under new names", report.renamed),
        });
    }
    if report.replaced > 0 {
        lines.push(format!("{} replaced", report.replaced));
    }
    if report.refreshed > 0 {
        lines.push(format!(
            "{} sidecar{} replaced beside an identical RAW",
            report.refreshed,
            plural(report.refreshed)
        ));
    }
    if report.foreign_sidecars_left > 0 {
        // Our RAW, someone else's .xmp: the user hears it here, not from
        // darktable months later (QE finding).
        let n = report.foreign_sidecars_left;
        lines.push(if n == 1 {
            "1 destination sidecar left in place — that pick has no sidecar of its own".to_string()
        } else {
            format!("{n} destination sidecars left in place — those picks have none of their own")
        });
    }
    if lines.is_empty() {
        lines.push(if report.failed.is_empty() {
            "Nothing needed copying".to_string()
        } else {
            format!(
                "Nothing was copied — {} file{} failed",
                report.failed.len(),
                plural(report.failed.len())
            )
        });
    }
    if report.cancelled {
        lines.push("cancelled — finished files remain".into());
    }
    for (name, reason) in &report.failed {
        lines.push(format!("FAILED {name}: {reason}"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::{clip_report_lines, report_lines};
    use fastcull_core::clip::{Cadence, CadenceSource, ClipReport, SkipReason, Skipped};
    use fastcull_core::fileops::CopyReport;

    fn exported() -> ClipReport {
        ClipReport {
            frames: 30,
            bytes: 344 << 20,
            path: Some(std::path::PathBuf::from("/out/DSC05010-DSC05039.mov")),
            name: "DSC05010-DSC05039.mov".into(),
            duration_ms: 990,
            cadence: Some(Cadence {
                sample_ms: 33,
                source: CadenceSource::Measured,
            }),
            all_verified: true,
            ..Default::default()
        }
    }

    /// The green light is EARNED. It appears over a run that landed a
    /// verified file and over nothing else — not a cancel, not a failure,
    /// not a run that wrote nothing.
    #[test]
    fn the_verified_line_of_a_video_export_is_earned() {
        let lines = clip_report_lines(&exported());
        assert_eq!(
            lines,
            [concat!(
                "Exported 30 frames · 1.0 s · 30.3 fps · 344.0 MB → ",
                "DSC05010-DSC05039.mov, all checksums verified"
            )]
        );
        for spoiled in [
            ClipReport {
                cancelled: true,
                ..exported()
            },
            ClipReport {
                failed: Some("No space left on device".into()),
                ..exported()
            },
            ClipReport {
                all_verified: false,
                ..exported()
            },
        ] {
            let lines = clip_report_lines(&spoiled);
            assert!(
                !lines.iter().any(|l| l.contains("checksums verified")),
                "green light over a spoiled run: {lines:?}"
            );
        }
    }

    /// A run that wrote nothing says so, and a failure names itself —
    /// the headline may never contradict the body (the Copy Picks rule,
    /// which this report inherits).
    #[test]
    fn a_video_export_that_wrote_nothing_says_so() {
        assert_eq!(
            clip_report_lines(&ClipReport::default()),
            ["Nothing was exported"]
        );
        let cancelled = clip_report_lines(&ClipReport {
            cancelled: true,
            ..Default::default()
        });
        // "finished files remain" would be a lie here: this operation
        // writes ONE file and never commits it unverified.
        assert_eq!(cancelled, ["Cancelled — nothing was written"]);
        let failed = clip_report_lines(&ClipReport {
            failed: Some("Permission denied".into()),
            ..Default::default()
        });
        assert_eq!(failed, ["FAILED: Permission denied"]);
    }

    /// The headline says WHAT HAPPENED, always — a run that failed used
    /// to lead with a note about the frames it had left out, which reads
    /// as a report of a successful export with a footnote.
    #[test]
    fn a_failed_export_leads_with_the_failure() {
        let lines = clip_report_lines(&ClipReport {
            failed: Some("Permission denied (os error 13)".into()),
            skipped: vec![Skipped {
                id: 3,
                name: "d.ARW".into(),
                reason: SkipReason::Size {
                    width: 380,
                    height: 285,
                },
            }],
            ..Default::default()
        });
        assert_eq!(lines[0], "FAILED: Permission denied (os error 13)");
        assert!(lines[1].starts_with("skipped — 1 frame"));
        // The same for a cancel.
        let lines = clip_report_lines(&ClipReport {
            cancelled: true,
            mirrored: 1,
            ..Default::default()
        });
        assert_eq!(lines[0], "Cancelled — nothing was written");
        assert!(lines[1].contains("mirrored"));
    }

    /// What the plan said it would leave out, the report repeats — in the
    /// same words, so the user is never told two different stories about
    /// the same frames.
    #[test]
    fn the_report_repeats_the_plans_own_words() {
        let lines = clip_report_lines(&ClipReport {
            replaced: true,
            mirrored: 2,
            skipped: vec![Skipped {
                id: 7,
                name: "DSC05020.ARW".into(),
                reason: SkipReason::Size {
                    width: 5616,
                    height: 3744,
                },
            }],
            cadence: Some(Cadence {
                sample_ms: 67,
                source: CadenceSource::NoTiming,
            }),
            ..exported()
        });
        assert!(lines[0].contains("timing not in the files — assumed 15 fps"));
        assert!(lines.contains(&"replaced the file that was already there".to_string()));
        assert!(lines
            .iter()
            .any(|l| l == "skipped — 1 frame: different size (5616×3744)"));
        assert!(lines.iter().any(|l| l.contains("2 frames were mirrored")));
    }

    fn failed(n: usize) -> Vec<(String, String)> {
        (0..n)
            .map(|i| (format!("f{i}.ARW"), "Permission denied".into()))
            .collect()
    }

    /// The headline may never contradict the body (QE finding): a run whose
    /// files failed did not "need no copying", and a run that replaced a
    /// file and then failed its sidecar did not either.
    #[test]
    fn the_headline_says_what_happened() {
        let lines = report_lines(&CopyReport {
            all_verified: false,
            failed: failed(2),
            ..Default::default()
        });
        assert_eq!(lines[0], "Nothing was copied — 2 files failed");
        assert!(lines.iter().any(|l| l.starts_with("FAILED f0.ARW")));
        assert!(
            !lines.iter().any(|l| l.contains("checksums verified")),
            "no green light over a run that copied nothing: {lines:?}"
        );

        let lines = report_lines(&CopyReport {
            all_verified: false,
            failed: failed(1),
            ..Default::default()
        });
        assert_eq!(lines[0], "Nothing was copied — 1 file failed");

        // A genuinely empty run still says the calm thing.
        let lines = report_lines(&CopyReport {
            all_verified: true,
            ..Default::default()
        });
        assert_eq!(lines, vec!["Nothing needed copying".to_string()]);
    }

    /// The green light follows what was verified — copied files AND files
    /// found byte-identical at the destination — and nothing else.
    #[test]
    fn the_verified_sentence_follows_what_was_verified() {
        let lines = report_lines(&CopyReport {
            copied: 3,
            all_verified: true,
            ..Default::default()
        });
        assert_eq!(lines[0], "3 copied, all checksums verified");

        let lines = report_lines(&CopyReport {
            identical: 145,
            all_verified: true,
            ..Default::default()
        });
        assert_eq!(
            lines[0],
            "145 already identical — re-verified in place, all checksums verified"
        );

        // Cancelled, or with a failure, the green light is withheld.
        for report in [
            CopyReport {
                copied: 3,
                all_verified: true,
                cancelled: true,
                ..Default::default()
            },
            CopyReport {
                copied: 3,
                all_verified: false,
                failed: failed(1),
                ..Default::default()
            },
        ] {
            let lines = report_lines(&report);
            assert!(
                !lines.iter().any(|l| l.contains("checksums verified")),
                "{lines:?}"
            );
        }
    }

    /// Our RAW beside a sidecar that is not ours is said out loud.
    #[test]
    fn a_foreign_sidecar_left_in_place_is_reported() {
        let lines = report_lines(&CopyReport {
            copied: 1,
            replaced: 1,
            foreign_sidecars_left: 1,
            all_verified: true,
            ..Default::default()
        });
        assert!(
            lines.iter().any(|l| l
                == "1 destination sidecar left in place — that pick has no sidecar of its own"),
            "{lines:?}"
        );
        let lines = report_lines(&CopyReport {
            copied: 2,
            foreign_sidecars_left: 2,
            all_verified: true,
            ..Default::default()
        });
        assert!(
            lines.iter().any(|l| l
                == "2 destination sidecars left in place — those picks have none of their own"),
            "{lines:?}"
        );
    }
}
