//! Copy Picks bridge (M6, fileops.md): the copy dialog's callbacks, the
//! plan/preview rebuild behind them, and the burst regrouping that shares
//! the same "re-derive over the whole session, in true sort order" shape.

use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;

use fastcull_core::fileops::ClashPolicy;

use crate::focus::refocus_topmost_deferred;
use crate::session::{load_ui_prefs, save_ui_prefs};
use crate::state::AppState;
use crate::MainWindow;

/// Wire the Copy Picks dialog (fileops.md): open/plan/start/cancel/close
/// and the destination picker.
pub(crate) fn wire(window: &MainWindow, state: &Rc<RefCell<AppState>>) {
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_copy_open(move || {
            let Some(win) = win.upgrade() else { return };
            // Opening the dialog COVERS any focused panel field — the same
            // focus-continuity rule as the Help modals (issue #41, gate
            // finding: this menu route survived only by init-timing luck,
            // the RUN14 note; the deferred claim routes to the dialog's
            // own scope via focus-keys once copy-visible is set below).
            refocus_topmost_deferred(&win);
            {
                let mut st = state.borrow_mut();
                if st.copy.handle.is_some() {
                    // A copy is running: just re-show the dialog.
                    win.set_copy_visible(true);
                    return;
                }
                // THE BARRIER, part 1 (gate HIGH finding: planning before
                // flushing froze `sidecar exists?` answers from BEFORE
                // the debounced write landed — a fresh first-ever pick
                // shipped its RAW without the sidecar while reporting
                // verified). Flush here so the PREVIEW is truthful;
                // copy_start flushes AND replans again.
                if let Some(writer) = &st.session.writer {
                    writer.flush();
                }
                let (dest, template) = load_ui_prefs();
                if st.copy.dest.is_none() {
                    st.copy.dest = dest;
                }
                // The remembered template is OFFERED, never pre-applied
                // (fileops.md "never silently pre-applied"; gate finding).
                win.set_copy_last_template(template.into());
                win.set_copy_template("".into());
                win.set_copy_dest(
                    st.copy
                        .dest
                        .as_deref()
                        .map(short_dest)
                        .unwrap_or_default()
                        .into(),
                );
                win.set_copy_state(0);
                win.set_copy_report("".into());
                win.set_copy_visible(true);
                copy_replan(&win, &mut st);
            }
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_copy_pick_dest(move || {
            let Some(win) = win.upgrade() else { return };
            // Blocking rfd picker (same recorded limitation as Open
            // Folder); the native dialog allows creating a folder.
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                let mut st = state.borrow_mut();
                st.copy.dest = Some(dir.clone());
                save_ui_prefs(Some(&dir), win.get_copy_template().as_str());
                win.set_copy_dest(short_dest(&dir).into());
                copy_replan(&win, &mut st);
            }
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_copy_replan(move || {
            let Some(win) = win.upgrade() else { return };
            let mut st = state.borrow_mut();
            save_ui_prefs(st.copy.dest.as_deref(), win.get_copy_template().as_str());
            copy_replan(&win, &mut st);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_copy_start(move || {
            let Some(win) = win.upgrade() else { return };
            let mut st = state.borrow_mut();
            // THE BARRIER, part 2: flush FIRST, then rebuild the plan
            // fresh so sidecar existence and free space are decided AFTER
            // every pending write landed (gate HIGH finding — a frozen
            // at-open plan is never executed).
            if let Some(writer) = &st.session.writer {
                writer.flush();
            }
            copy_replan(&win, &mut st);
            let Some(plan) = st.copy.plan.take() else {
                return; // replan surfaced an error; the dialog shows it
            };
            if plan.clashes == 0 {
                // Nothing at the destination is in the way: today's flow,
                // unchanged, no question.
                start_copy(&win, &mut st, plan);
                return;
            }
            // THE CLASH QUESTION (fileops.md). The plan built here is
            // DROPPED, deliberately: the answer is a policy, and only a
            // plan built WITH that policy — after another flush — may run.
            show_clash_question(&win, &plan);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_copy_answer_keep_both(move || {
            let Some(win) = win.upgrade() else { return };
            let mut st = state.borrow_mut();
            answer_clash_question(&win, &mut st, ClashPolicy::CreateCopies);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_copy_answer_overwrite(move || {
            let Some(win) = win.upgrade() else { return };
            let mut st = state.borrow_mut();
            answer_clash_question(&win, &mut st, ClashPolicy::Overwrite);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_copy_answer_cancel(move || {
            let Some(win) = win.upgrade() else { return };
            // Cancel copies NOTHING — not even the clash-free files (user
            // decision 2026-08-21; Esc means the same). The dialog goes
            // back to its plan preview with the destination and template
            // intact, so "cancel, then copy somewhere else" is one step.
            let mut st = state.borrow_mut();
            win.set_copy_state(0);
            copy_replan(&win, &mut st);
        });
    }
    {
        let state = Rc::clone(state);
        window.on_copy_cancel(move || {
            let st = state.borrow();
            if let Some(handle) = &st.copy.handle {
                handle.cancel();
            }
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_copy_close(move || {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                if st.copy.handle.is_none() {
                    // keep plan state tidy between opens
                    st.copy.plan = None;
                }
            }
            win.set_copy_visible(false);
            // The copied badges follow what the dialog just learned about
            // the disk: a hand-deleted copy lost its badge in
            // `copy_replan`, and the grid under the dialog must show that
            // as soon as it is visible again (persona: "the six are the
            // badge-less cells in the Picked view").
            crate::presenter::refresh(&win, &state);
            win.invoke_focus_grid();
        });
    }
    {
        let state = Rc::clone(state);
        window.on_copy_open_dest_folder(move || {
            let st = state.borrow();
            if let Some(dest) = &st.copy.dest {
                #[cfg(target_os = "windows")]
                let cmd = "explorer";
                #[cfg(not(target_os = "windows"))]
                let cmd = "xdg-open";
                std::process::Command::new(cmd).arg(dest).spawn().ok();
            }
        });
    }
}

/// Picked images in SESSION SORT ORDER (fileops.md: scope is "everything
/// with a star", filter-independent; `{seq}` follows the session sort).
fn plan_sources(st: &AppState) -> Vec<fastcull_core::fileops::PlanSource> {
    let all_query = fastcull_core::filter::ViewQuery {
        filter: fastcull_core::filter::PickFilter::All,
        ..st.grid.query
    };
    // `{seq}` is baked into PERMANENT FILENAMES on disk — the one
    // irreversible artifact this app produces — and both fileops.md and
    // docs/copy-picks.md promise it follows the session sort, so a copy
    // started mid-load must not encode a transient view state forever.
    let ordered = fastcull_core::filter::view_true_sort(
        &st.session.picks,
        &st.session.labels,
        &st.session.capture_keys,
        &all_query,
    );
    ordered
        .into_iter()
        .filter(|id| {
            matches!(
                st.session.picks.get(*id),
                Some(fastcull_core::catalog::PickState::Picked)
            )
        })
        .filter_map(|id| {
            let path = st.session.paths.get(id)?.clone();
            let meta = std::fs::metadata(&path).ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let mtime = meta
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::UNIX_EPOCH);
            let name = st.session.labels.get(id).cloned().unwrap_or_default();
            Some(fastcull_core::fileops::PlanSource {
                id,
                path,
                size,
                ctx: fastcull_core::iptc::ExpandContext::from_sort_key(
                    st.session.capture_keys.get(id).and_then(|k| k.as_deref()),
                    mtime,
                    &name,
                    None,
                ),
            })
        })
        .collect()
}

fn human_bytes(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
    } else if b >= 1 << 20 {
        format!("{:.1} MB", b as f64 / (1u64 << 20) as f64)
    } else {
        format!("{b} B")
    }
}

/// The destination as the dialog shows it: the whole path while it is
/// short, otherwise its TAIL — `…/2026-08-21-osprey/selects`.
///
/// Slint's `overflow: elide` cuts the END of a string, which on a real
/// path throws away the only part that tells two shoots apart and keeps
/// the home-directory prefix every folder shares (persona 2026-08-21:
/// "showing me the useless half of the path is a trust failure" — and
/// this dialog's new job is asking permission to replace files in THIS
/// folder). The full path is still one click away under "Open
/// destination".
fn short_dest(p: &std::path::Path) -> String {
    let full = p.to_string_lossy().into_owned();
    if full.chars().count() <= 52 {
        return full;
    }
    let tail: std::path::PathBuf = {
        let mut last: Vec<_> = p.components().rev().take(2).collect();
        last.reverse();
        last.iter().collect()
    };
    format!("…/{}", tail.display())
}

/// Hand a plan to the copy worker and put the dialog in its running state.
fn start_copy(win: &MainWindow, st: &mut AppState, plan: fastcull_core::fileops::CopyPlan) {
    let (handle, rx) = fastcull_core::fileops::execute(plan);
    st.copy.handle = Some(handle);
    st.copy.rx = Some(rx);
    win.set_copy_state(1);
    win.set_copy_progress("Starting…".into());
}

/// Put the dialog into its question state (fileops.md, "The clash
/// question"): ONE question for the whole run, stating where, how many,
/// what still copies normally, and what each answer costs.
///
/// Wording settled with the persona at implementation time (fileops.md
/// §6). Counted in PICKS, not files — 148 picks are 296 files on disk,
/// and a count the user cannot reconcile is a count they stop trusting.
fn show_clash_question(win: &MainWindow, plan: &fastcull_core::fileops::CopyPlan) {
    use fastcull_core::fileops::PlanAction;
    let total = plan.jobs.len();
    let clashes = plan.clashes;
    let free = total.saturating_sub(clashes);
    let clashing: Vec<String> = plan
        .jobs
        .iter()
        .filter(|j| j.action == PlanAction::Clash)
        .filter_map(|j| j.dst_raw.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .collect();
    let dest = win.get_copy_dest();
    // Built line by line rather than as one escaped literal: the middle
    // line is the destination, and this text is the last thing the user
    // reads before a file can be replaced.
    let others = match free {
        0 => String::new(),
        1 => "The other 1 copies normally. ".to_string(),
        n => format!("The other {n} copy normally. "),
    };
    win.set_copy_confirm(
        [
            format!("{clashes} of your {total} picks already have files with these names in"),
            dest.to_string(),
            format!("{others}Choose once for the whole run:"),
        ]
        .join("\n")
        .into(),
    );
    // Three names, never a table: on a two-body night this is how the
    // user confirms the clashes are the other camera and not their own
    // export (persona; the 148-row table stays cut).
    win.set_copy_confirm_examples(if clashing.is_empty() {
        "".into()
    } else {
        format!(
            "e.g. {}{}",
            clashing
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            if clashing.len() > 3 { " …" } else { "" }
        )
        .into()
    });
    // The name core says the FIRST clashing pick would really land under
    // — `_1` when `_1` is free, `_2` when it is not (gate finding: the
    // question promised a name the copy would not use on the second
    // keep-both into the same folder).
    win.set_copy_confirm_keep_both(
        match &plan.keep_both_example {
            Some(name) => format!("Keep both — the {clashes} land as {name}"),
            None => format!("Keep both — the {clashes} land under a new name"),
        }
        .into(),
    );
    // Bytes belong HERE and nowhere else: this is the only answer whose
    // cost is knowable up front (the overwrite answer re-checks identical
    // files instead of re-sending them, so a worst-case number on it
    // would be a cost the user never pays).
    win.set_copy_confirm_keep_both_cost(format!("+{}", human_bytes(plan.clash_bytes)).into());
    win.set_copy_confirm_overwrite(
        format!("Overwrite those {clashes} — identical files are re-checked, not re-sent").into(),
    );
    win.set_copy_confirm_cancel(
        match free {
            0 => "Cancel — copy nothing at all".to_string(),
            n => format!("Cancel — copy nothing at all, not even the {n}"),
        }
        .into(),
    );
    // The one thing this question destroys, said out loud: a sidecar at
    // the destination is byte-replaced, and darktable's history stack
    // lives in a file of exactly that name (persona finding 2026-08-21 —
    // relayed to the user as an open question about merging instead).
    win.set_copy_confirm_warning(
        concat!(
            "Overwriting also replaces those files' .xmp sidecars — edits made ",
            "at the destination by another app (darktable) are lost."
        )
        .into(),
    );
    win.set_copy_confirm_nudge("Pick one: B, O or Esc.".into());
    win.set_copy_confirm_nudged(false);
    win.set_copy_state(3);
}

/// The user answered: flush again, REPLAN with the chosen policy, and run
/// only that fresh plan (fileops.md rule 3 — the plan built before the
/// question is never executed). A policy that no longer fits (free space,
/// a destination that moved) drops back to the plan preview with the
/// error on it, having copied nothing.
fn answer_clash_question(win: &MainWindow, st: &mut AppState, policy: ClashPolicy) {
    if let Some(writer) = &st.session.writer {
        writer.flush();
    }
    copy_replan_with(win, st, policy);
    match st.copy.plan.take() {
        Some(plan) => start_copy(win, st, plan),
        None => {
            win.set_copy_state(0);
        }
    }
}

/// Rebuild the copy plan from the dialog's current inputs and publish the
/// preview properties (fileops.md dialog minimums). `pub(crate)` because a
/// session swap must re-derive the preview it left on screen.
pub(crate) fn copy_replan(win: &MainWindow, st: &mut AppState) {
    copy_replan_with(win, st, ClashPolicy::Ask);
}

fn copy_replan_with(win: &MainWindow, st: &mut AppState, policy: ClashPolicy) {
    use fastcull_core::fileops::{plan, PlanError};
    let sources = plan_sources(st);
    win.set_copy_error("".into());
    win.set_copy_ready(false);
    win.set_copy_preview("".into());
    win.set_copy_collisions("".into());
    st.copy.plan = None;
    if sources.is_empty() {
        win.set_copy_summary("No picked images — nothing to copy.".into());
        return;
    }
    let Some(dest) = st.copy.dest.clone() else {
        win.set_copy_summary(
            format!("{} picked images. Choose a destination.", sources.len()).into(),
        );
        return;
    };
    let template_raw = win.get_copy_template().to_string();
    let template = (!template_raw.trim().is_empty()).then_some(template_raw.as_str());
    // The badge follows the disk from the moment the dialog looks (a copy
    // the user deleted by hand loses it here, and gets it back when the
    // copy lands again — persona decision 2026-08-21); the plan itself
    // re-reads the destination on its own.
    st.copy.copies.refresh();
    match plan(&sources, &dest, template, policy, &st.copy.copies) {
        Ok(p) => {
            if template.is_some() {
                let preview: Vec<String> = p
                    .jobs
                    .iter()
                    .take(3)
                    .map(|j| {
                        j.dst_raw
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect();
                win.set_copy_preview(format!("→ {}", preview.join(", ")).into());
            }
            win.set_copy_summary(
                format!(
                    "{} picked · {} to copy · {}",
                    sources.len(),
                    // Before the answer this is the WORST CASE — every
                    // pick going out, which is what both answers may cost
                    // (fileops.md rule 3).
                    human_bytes(p.total_bytes + p.clash_bytes),
                    match p.free_bytes {
                        Some(free) => format!("{} free", human_bytes(free)),
                        None => "free space unknown".to_string(),
                    }
                )
                .into(),
            );
            let mut notes = Vec::new();
            if p.clashes > 0 {
                // The split, not just the clash count: "3 new · 148
                // already exist here" diagnoses the situation before the
                // question is even asked (persona), and cross-session —
                // when the ✓ badges are gone — it is the ONLY signal that
                // the folder already holds this shoot.
                notes.push(format!(
                    "{} new · {} already exist here — Copy will ask what to do",
                    p.jobs.len().saturating_sub(p.clashes),
                    p.clashes
                ));
            }
            if p.renamed > 0 {
                // Not a question — two picks that share a name always get
                // a suffix (user decision 2026-08-22) — but the user still
                // has to see that names on disk will differ from the ones
                // in the grid.
                notes.push(format!(
                    "{} share a name with another pick — those get a suffix",
                    p.renamed
                ));
            }
            if p.recopied > 0 {
                // The one signal that Enter is about to put back what the
                // user removed by hand (persona: in a 200 MB plan a 70 MB
                // difference is invisible).
                notes.push(format!(
                    "{} copied earlier but gone from the destination — copying again",
                    p.recopied
                ));
            }
            win.set_copy_collisions(notes.join(" · ").into());
            win.set_copy_ready(true);
            st.copy.plan = Some(p);
        }
        Err(
            e @ (PlanError::InsufficientSpace { .. }
            | PlanError::DestEqualsSource
            | PlanError::DestInsideSource
            | PlanError::DestNotADirectory
            | PlanError::TemplateMakesAPath { .. }
            | PlanError::Template(_)),
        ) => {
            win.set_copy_summary(format!("{} picked images.", sources.len()).into());
            win.set_copy_error(e.to_string().into());
        }
    }
}

/// Rebuild burst grouping (M7, burst-grouping.md): always over CAPTURE
/// order of the WHOLE session (the spec's input contract) regardless of
/// the UI's filter/sort; results are indexed by image id for the grid
/// badge, the status position, and the `[`/`]` boundary keys.
pub(crate) fn recompute_bursts(st: &mut AppState) {
    let capture_query = fastcull_core::filter::ViewQuery {
        filter: fastcull_core::filter::PickFilter::All,
        sort: fastcull_core::filter::SortKey::CaptureTime,
        ascending: true,
    };
    // A burst is a fact about capture times, so grouping over issue #25's
    // provisional filename order would invent groups. Grouping over
    // partly-loaded keys is already approximate and is redone as metadata
    // streams (bursts.dirty).
    let order = fastcull_core::filter::view_true_sort(
        &st.session.picks,
        &st.session.labels,
        &st.session.capture_keys,
        &capture_query,
    );
    let frames: Vec<fastcull_core::burst::FrameMeta> = order
        .iter()
        .map(|id| st.session.frame_meta.get(*id).cloned().unwrap_or_default())
        .collect();
    let grouping =
        fastcull_core::burst::group(&frames, &fastcull_core::burst::BurstConfig::default());
    let n = st.session.labels.len();
    // Rebuilt from scratch every time, so the three parallel vectors are
    // re-sized through the one constructor that owns their length. The
    // dirty flag is the caller's (the pump clears it before calling), not
    // ours to reset.
    st.bursts = crate::state::BurstIndex {
        dirty: st.bursts.dirty,
        ..crate::state::BurstIndex::new(n)
    };
    let positions = grouping.positions(); // one O(n) pass, not per-frame
    for (pos_in_order, id) in order.iter().enumerate() {
        st.bursts.group_of[*id] = grouping.group[pos_in_order];
        // Badge goes on the group's FIRST frame (position 1) — with
        // interleaved bodies members need not be contiguous.
        if let Some((1, size)) = positions[pos_in_order] {
            st.bursts.badge[*id] = size;
        }
        st.bursts.pos[*id] = positions[pos_in_order];
    }
}
