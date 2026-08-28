//! Export Frames as Video bridge (M9, video-export.md): the dialog's
//! callbacks and the plan/preview rebuild behind them.
//!
//! Deliberately the same shape as `copy_bridge.rs` — the two exports ask
//! the same clash question, use the same destination row, and put their
//! answer through the same replan-then-run rule — so that a reader who
//! knows one knows the other. What differs is what they produce: Copy
//! Picks moves N pairs of files, this writes ONE.
//!
//! Two things this bridge deliberately does NOT do, because the export
//! never touches the user's culling state: it does not flush the sidecar
//! writer (nothing it reads comes from a sidecar) and it never writes a
//! mark. The frames it exports are usually rejects.

use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;

use fastcull_core::clip::{self, ClipPlan};
use fastcull_core::fileops::ClashPolicy;

use crate::copy_bridge::{human_bytes, short_dest};
use crate::focus::refocus_topmost_deferred;
use crate::session::{load_clip_dest, save_clip_dest};
use crate::state::AppState;
use crate::MainWindow;

/// Wire the dialog: open/plan/start/cancel/close, the destination picker,
/// and the three answers to the clash question.
pub(crate) fn wire(window: &MainWindow, state: &Rc<RefCell<AppState>>) {
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_clip_open(move || {
            let Some(win) = win.upgrade() else { return };
            // Opening COVERS any focused panel field — the same focus
            // continuity rule as the Copy Picks dialog (issue #41).
            refocus_topmost_deferred(&win);
            let mut st = state.borrow_mut();
            if st.clip.handle.is_some() {
                // An export is running: just re-show the dialog.
                win.set_clip_visible(true);
                return;
            }
            // "Never a silent grey item" (video-export.md): the keystroke
            // works whether or not the menu item is enabled, and when
            // there is nothing to export it SAYS SO instead of doing
            // nothing visible.
            let frames = clip_scope(&st).len();
            if let Some(reason) = clip::unavailable_reason(frames) {
                st.clip.notice = Some((
                    format!("Export Frames as Video: {reason}"),
                    std::time::Instant::now(),
                ));
                drop(st);
                crate::presenter::refresh(&win, &state);
                return;
            }
            st.clip.notice = None;
            if st.clip.dest.is_none() {
                // Seeded from the Copy Picks folder until a video folder
                // has been chosen (video-export.md, persona 2026-08-27).
                st.clip.dest = load_clip_dest();
            }
            win.set_clip_dest(
                st.clip
                    .dest
                    .as_deref()
                    .map(short_dest)
                    .unwrap_or_default()
                    .into(),
            );
            win.set_clip_state(0);
            win.set_clip_report("".into());
            win.set_clip_visible(true);
            clip_replan(&win, &mut st);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_clip_pick_dest(move || {
            let Some(win) = win.upgrade() else { return };
            // Blocking rfd picker, as everywhere else in this app.
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                let mut st = state.borrow_mut();
                st.clip.dest = Some(dir.clone());
                // From now on this folder is remembered on its own, and
                // a Copy Picks destination change no longer moves it.
                save_clip_dest(&dir);
                win.set_clip_dest(short_dest(&dir).into());
                clip_replan(&win, &mut st);
            }
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_clip_start(move || {
            let Some(win) = win.upgrade() else { return };
            let mut st = state.borrow_mut();
            // Replan against the disk as it is NOW: the destination may
            // have gained the name since the dialog opened.
            clip_replan(&win, &mut st);
            let Some(plan) = st.clip.plan.take() else {
                return; // the replan surfaced an error; the dialog shows it
            };
            if plan.action != fastcull_core::clip::ClipAction::Clash {
                start_export(&win, &mut st, plan);
                return;
            }
            // THE CLASH QUESTION. This plan is dropped on purpose: the
            // answer is a policy, and only a plan built WITH that policy
            // may run (fileops.md rule 3).
            show_clash_question(&win, &plan);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_clip_answer_keep_both(move || {
            let Some(win) = win.upgrade() else { return };
            answer(&win, &mut state.borrow_mut(), ClashPolicy::CreateCopies);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_clip_answer_overwrite(move || {
            let Some(win) = win.upgrade() else { return };
            answer(&win, &mut state.borrow_mut(), ClashPolicy::Overwrite);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_clip_answer_cancel(move || {
            let Some(win) = win.upgrade() else { return };
            // Cancel writes nothing and returns to the plan with the
            // destination intact, so "cancel, then export somewhere else"
            // is one step (the Copy Picks rule).
            let mut st = state.borrow_mut();
            win.set_clip_state(0);
            clip_replan(&win, &mut st);
        });
    }
    {
        let state = Rc::clone(state);
        window.on_clip_cancel(move || {
            if let Some(handle) = &state.borrow().clip.handle {
                handle.cancel();
            }
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_clip_close(move || {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                if st.clip.handle.is_none() {
                    st.clip.plan = None;
                }
            }
            win.set_clip_visible(false);
            crate::presenter::refresh(&win, &state);
            win.invoke_focus_grid();
        });
    }
    {
        let state = Rc::clone(state);
        window.on_clip_open_dest_folder(move || {
            let st = state.borrow();
            if let Some(dest) = &st.clip.dest {
                #[cfg(target_os = "windows")]
                let cmd = "explorer";
                #[cfg(not(target_os = "windows"))]
                let cmd = "xdg-open";
                std::process::Command::new(cmd).arg(dest).spawn().ok();
            }
        });
    }
}

/// The image ids the export would take: the selection when there is one,
/// otherwise the burst under the cursor. The RULE lives in core
/// (`clip::scope`, CLAUDE.md rule 5); this only supplies the three inputs.
pub(crate) fn clip_scope(st: &AppState) -> Vec<usize> {
    let selected = if st.grid.selection.is_empty() {
        Vec::new()
    } else {
        st.grid.selection.batch(&st.grid.view, st.grid.cursor)
    };
    clip::scope(&selected, st.grid.cursor, &st.bursts.group_of)
}

/// The scope as export inputs. Capture time and its precision come from
/// the same `FrameMeta` burst grouping uses, so "capture order" means one
/// thing across the app.
fn clip_sources(st: &AppState) -> Vec<clip::ClipSource> {
    clip_scope(st)
        .into_iter()
        .filter_map(|id| {
            let meta = st.session.frame_meta.get(id)?;
            Some(clip::ClipSource {
                id,
                path: st.session.paths.get(id)?.clone(),
                name: st.session.labels.get(id).cloned().unwrap_or_default(),
                time_ms: meta.time_ms,
                has_subsec: meta.has_subsec,
            })
        })
        .collect()
}

/// Hand a plan to the writer and put the dialog in its running state.
fn start_export(win: &MainWindow, st: &mut AppState, plan: ClipPlan) {
    let (handle, rx) = clip::execute(plan);
    st.clip.handle = Some(handle);
    st.clip.rx = Some(rx);
    win.set_clip_state(1);
    win.set_clip_progress("Starting…".into());
}

/// The user answered: replan with the chosen policy and run only that
/// fresh plan (fileops.md rule 3). A policy that no longer fits — the
/// destination moved, the disk filled — drops back to the plan preview
/// with the error on it, having written nothing.
fn answer(win: &MainWindow, st: &mut AppState, policy: ClashPolicy) {
    clip_replan_with(win, st, policy);
    match st.clip.plan.take() {
        Some(plan) => start_export(win, st, plan),
        None => win.set_clip_state(0),
    }
}

/// The clash question, in this dialog's own words. One file, so the
/// counts Copy Picks needs are gone; what stays is the rule — nothing is
/// replaced without the Overwrite answer, and the destructive answer is
/// not on Enter.
fn show_clash_question(win: &MainWindow, plan: &ClipPlan) {
    let name = plan.file_name();
    win.set_clip_confirm(
        [
            format!("A file called {name} is already in"),
            win.get_clip_dest().to_string(),
            "Choose what to do with it:".to_string(),
        ]
        .join("\n")
        .into(),
    );
    win.set_clip_confirm_keep_both(
        match &plan.keep_both_example {
            Some(free) => format!("Keep both — the video lands as {free}"),
            None => "Keep both — the video lands under a new name".to_string(),
        }
        .into(),
    );
    win.set_clip_confirm_keep_both_cost(format!("+{}", human_bytes(plan.total_bytes)).into());
    win.set_clip_confirm_overwrite(format!("Overwrite — replace {name}").into());
    win.set_clip_confirm_cancel("Cancel — write nothing".into());
    win.set_clip_confirm_nudge("Pick one: B, O or Esc.".into());
    win.set_clip_confirm_nudged(false);
    win.set_clip_state(3);
}

/// Rebuild the plan from the dialog's current inputs and publish the
/// preview. `pub(crate)` because a session swap must re-derive the
/// preview it left on screen.
pub(crate) fn clip_replan(win: &MainWindow, st: &mut AppState) {
    clip_replan_with(win, st, ClashPolicy::Ask);
}

fn clip_replan_with(win: &MainWindow, st: &mut AppState, policy: ClashPolicy) {
    use fastcull_core::clip::ClipError;
    let sources = clip_sources(st);
    win.set_clip_error("".into());
    win.set_clip_ready(false);
    win.set_clip_skipped("".into());
    st.clip.plan = None;
    let frames = sources.len();
    if let Some(reason) = clip::unavailable_reason(frames) {
        win.set_clip_summary(format!("Nothing to export — {reason}.").into());
        return;
    }
    let Some(dest) = st.clip.dest.clone() else {
        win.set_clip_summary(format!("{frames} frames. Choose a destination.").into());
        return;
    };
    match clip::plan(&sources, &dest, policy) {
        Ok(p) => {
            win.set_clip_summary(plan_line(&p).into());
            win.set_clip_skipped(skipped_line(&p).into());
            win.set_clip_ready(true);
            st.clip.plan = Some(p);
        }
        Err(e) => {
            win.set_clip_summary(format!("{frames} frames selected.").into());
            // Every one of these is a refusal BEFORE anything is written,
            // and each says which one it is.
            win.set_clip_error(
                match &e {
                    ClipError::TooFewFrames { kept } => format!(
                        "Only {kept} of those frames can share one video — {}",
                        skipped_reasons(&sources)
                    ),
                    other => other.to_string(),
                }
                .into(),
            );
        }
    }
}

/// The one plan line (video-export.md, "Dialog"): what the file will be,
/// where it will land, and whether it fits — before a byte is written.
fn plan_line(p: &ClipPlan) -> String {
    format!(
        "{} frames · {}×{} · {} · {} · {} → {} · {}",
        p.frames.len(),
        p.width,
        p.height,
        // The cadence's own words, from core: a measured rate is just the
        // rate, and the two fallbacks explain themselves in the SAME
        // sentence the report will use.
        p.cadence.text(),
        seconds(p.duration_ms()),
        human_bytes(p.total_bytes),
        p.file_name(),
        match p.free_bytes {
            Some(free) => format!("{} free", human_bytes(free)),
            None => "free space unknown".to_string(),
        }
    )
}

/// What is being left out, and why — the same sentence the report gives.
pub(crate) fn skipped_line(p: &ClipPlan) -> String {
    let mut parts = Vec::new();
    let skipped = p.skipped_text();
    if !skipped.is_empty() {
        parts.push(skipped);
    }
    if p.mirrored > 0 {
        parts.push(mirrored_note(p.mirrored));
    }
    parts.join(" · ")
}

/// A mirrored frame keeps its rotation and loses its flip: the picture is
/// right, the mirroring is not, and the user hears it here rather than
/// finding out in the editor.
pub(crate) fn mirrored_note(n: usize) -> String {
    if n == 1 {
        "1 frame was mirrored in the camera — exported un-mirrored".to_string()
    } else {
        format!("{n} frames were mirrored in the camera — exported un-mirrored")
    }
}

/// When too few frames can share one track, say WHY rather than just how
/// many: the reason is the whole message ("they are different sizes").
fn skipped_reasons(sources: &[clip::ClipSource]) -> String {
    match sources.len() {
        0 | 1 => "a video needs at least two frames".to_string(),
        _ => "they do not share one frame size and orientation, and this export \
              never scales or rotates pixels"
            .to_string(),
    }
}

/// A duration in seconds with one decimal — "1.0 s".
pub(crate) fn seconds(ms: u64) -> String {
    format!("{:.1} s", ms as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duration_reads_in_seconds() {
        assert_eq!(seconds(990), "1.0 s");
        assert_eq!(seconds(99), "0.1 s");
        assert_eq!(seconds(12_340), "12.3 s");
    }

    #[test]
    fn the_mirrored_note_counts_properly() {
        assert!(mirrored_note(1).starts_with("1 frame was"));
        assert!(mirrored_note(3).starts_with("3 frames were"));
    }
}
