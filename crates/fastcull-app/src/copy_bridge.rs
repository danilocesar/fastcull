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
            win.invoke_dbg_focus_claim("copy-dialog".into());
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
        window.on_copy_answer_new_only(move || {
            let Some(win) = win.upgrade() else { return };
            let mut st = state.borrow_mut();
            answer_clash_question(&win, &mut st, ClashPolicy::NewOnly);
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
            win.invoke_dbg_focus_claim("copy-close".into());
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
                    st.session.camera_models.get(id).and_then(|c| c.as_deref()),
                ),
            })
        })
        .collect()
}

/// A byte count in the units a person reads (fileops.md, dialog
/// minimums, "Sizes on screen"): five binary tiers chosen by threshold —
/// bytes below 1,024, then KB, MB, GB and TB from 2^10, 2^20, 2^30 and
/// 2^40 — with one decimal on every tiered value and none on a plain
/// byte count.
///
/// The tier is picked FIRST and the value rounded inside it (brief 006
/// D6), so one byte short of a megabyte is `1024.0 KB` and never
/// `1.0 MB`: no line may claim a boundary the count has not reached.
/// Above the TB tier the number simply runs on (`1024.0 TB`) — there is
/// no PB tier, because no volume a photographer copies to needs one.
/// Binary, because `df -h`, Windows Explorer and a NAS dashboard are
/// (the user, brief 006 OQ1: "I don't compare them").
///
/// This is the ONE formatter both dialogs share: the copy summary line
/// and the Keep both row's cost, the copy's free-space refusal, and the
/// video export's plan line, refusal and report line.
pub(crate) fn human_bytes(b: u64) -> String {
    const KB: u64 = 1 << 10;
    const MB: u64 = 1 << 20;
    const GB: u64 = 1 << 30;
    const TB: u64 = 1 << 40;
    if b >= TB {
        format!("{:.1} TB", b as f64 / TB as f64)
    } else if b >= GB {
        format!("{:.1} GB", b as f64 / GB as f64)
    } else if b >= MB {
        format!("{:.1} MB", b as f64 / MB as f64)
    } else if b >= KB {
        format!("{:.1} KB", b as f64 / KB as f64)
    } else {
        format!("{b} B")
    }
}

/// The free-space refusal in the units a person reads — the video
/// dialog's sentence shape (`clip_bridge::no_room_for_it`) with this
/// dialog's subject (fileops.md, plan-time errors; brief 006 R2). Core
/// keeps its byte counts, which are right for core and useless on
/// screen: until 2026-09-12 this dialog printed them straight through
/// and the user counted digits to learn whether the shortfall was
/// 100 MB or 100 GB.
fn no_room_for_the_copy(needed: u64, free: u64) -> String {
    format!(
        "The copy needs {} and there is {} free at the destination.",
        human_bytes(needed),
        human_bytes(free)
    )
}

/// What the copy dialog prints for a plan-time refusal. ONE function for
/// both paths that can refuse — the plan preview and the drop-back after
/// an answer — so neither can drift back to core's raw text.
///
/// Only the free-space refusal is re-worded. Every other `PlanError`
/// keeps core's `Display` text (fileops.md: "Every other `PlanError`
/// keeps the text it has"), which is what the `other` arm below does:
/// in Rust, `to_string()` on an error calls exactly that `Display`.
pub(crate) fn copy_error_text(e: &fastcull_core::fileops::PlanError) -> String {
    use fastcull_core::fileops::PlanError;
    match e {
        PlanError::InsufficientSpace { needed, free } => no_room_for_the_copy(*needed, *free),
        other => other.to_string(),
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
pub(crate) fn short_dest(p: &std::path::Path) -> String {
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
    // Numbered from here, not from the report: the mark this feeds is
    // "the Nth copy has finished", and N must already be this run's when
    // the events start arriving.
    st.copy.runs = st.copy.runs.saturating_add(1);
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
    // The FIRST row (fileops.md §6, brief 005): the safe, common answer.
    // "New" first, so `N` reads as New and not as No. ALWAYS offered
    // (Manager decision D6) — with nothing new it says so, and answering
    // it copies nothing: a row that appears and disappears moves `B` and
    // `O` under the pointer on exactly the destructive question.
    win.set_copy_confirm_new_only(
        match free {
            0 => format!(
                "New only — nothing new to copy, leave the {clashes} already here untouched"
            ),
            n => format!("New only — copy the {n}, leave the {clashes} already here untouched"),
        }
        .into(),
    );
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
    // Bytes belong on THIS answer row and no other: it is the only
    // answer whose cost is knowable up front (the overwrite answer
    // re-checks identical files instead of re-sending them, so a
    // worst-case number on it would be a cost the user never pays). The
    // plan line above and the free-space refusal state sizes too — they
    // are not answer rows.
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
    // lives in a file of exactly that name (persona finding 2026-08-21).
    // Since 2026-09-12 it names the way out in the SAME breath: the word
    // that makes the user stop is "(darktable)", and the answer that does
    // not do it belongs in the next clause, not two rows above (persona
    // C4, Manager D7).
    win.set_copy_confirm_warning(
        concat!(
            "Overwriting also replaces those files' .xmp sidecars — edits made ",
            "at the destination by another app (darktable) are lost. New only ",
            "leaves them alone."
        )
        .into(),
    );
    win.set_copy_confirm_nudge("Pick one: N, B, O or Esc.".into());
    win.set_copy_confirm_nudged(false);
    win.set_copy_state(3);
}

/// The user answered: flush again, REPLAN with the chosen policy, and run
/// only that fresh plan (fileops.md rule 3 — the plan built before the
/// question is never executed). A policy that no longer fits (free space,
/// a destination that moved) drops back to the plan preview with the
/// error on it — in the dialog's words, through `copy_error_text` —
/// having copied nothing.
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
            if p.seq_meets_clashes {
                // The {seq} re-run trap (the user's decision on brief 005
                // OQ1: warn on the plan line, no refusal — every answer
                // stays available). The new picks renumber everything
                // after them, so the names found occupied here may belong
                // to other frames. A plain literal, NOT a `format!`: the
                // braces are the text the user typed.
                notes.push(
                    "{seq} numbers the whole session — the names already here may now belong to other frames"
                        .to_string(),
                );
            }
            if p.shared_name > 0 {
                // Not a question — two picks that share a name always get
                // a suffix (user decision 2026-08-22) — but the user still
                // has to see that names on disk will differ from the ones
                // in the grid. `shared_name`, never `renamed`: the latter
                // also counts suffixes taken because the DESTINATION held
                // the name, and this sentence would then be false about
                // them (gate finding 2026-08-22).
                notes.push(if p.shared_name == 1 {
                    "1 pick shares a name with another — it gets a suffix".to_string()
                } else {
                    format!(
                        "{} picks share a name with another — those get a suffix",
                        p.shared_name
                    )
                });
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
            | PlanError::TemplateMakesAHiddenName { .. }
            | PlanError::Template(_)),
        ) => {
            win.set_copy_summary(format!("{} picked images.", sources.len()).into());
            win.set_copy_error(copy_error_text(&e).into());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every size either dialog shows, at the boundary of each tier
    /// (fileops.md, dialog minimums, "Sizes on screen"; brief 006 AC1).
    /// The pairs are exact strings on purpose: this is the text a user
    /// reads at a glance, and a tier that rounds into its neighbour or
    /// loses its decimal is a wrong number, not a cosmetic wobble.
    #[test]
    fn a_byte_count_reads_in_its_tier_with_one_decimal() {
        for (bytes, want) in [
            // Below the first tier there is no decimal at all: a count of
            // bytes is exact, and `1023.0 B` would imply a rounding that
            // did not happen.
            (0u64, "0 B"),
            (1023, "1023 B"),
            (1 << 10, "1.0 KB"),
            // The tier is chosen by THRESHOLD and the value rounded
            // INSIDE it (brief 006 D6), so one byte short of a megabyte
            // is `1024.0 KB` and never `1.0 MB`: no line may claim a
            // boundary the count has not reached.
            ((1 << 20) - 1, "1024.0 KB"),
            (1 << 20, "1.0 MB"),
            (1 << 30, "1.0 GB"),
            ((1u64 << 40) - 1, "1024.0 GB"),
            (1u64 << 40, "1.0 TB"),
            (12u64 << 40, "12.0 TB"),
            // Above the TB tier the number runs on — there is no PB tier
            // (fileops.md "Sizes on screen") — and the largest count a
            // `u64` can hold still prints, without a panic or an overflow.
            (1u64 << 50, "1024.0 TB"),
            (u64::MAX, "16777216.0 TB"),
            // The two figures issue #88 was opened on: a ~1 MB clash on
            // the Keep both row printed `1029480 B`, and a 1.2 TB NAS's
            // free space printed `1228.8 GB`.
            (1_029_480, "1005.4 KB"),
            (1_319_413_953_331, "1.2 TB"),
            // The values the two existing readers of a size string pin
            // (clip_bridge's video refusal, pump's report line): they sit
            // inside their tiers under the old three-tier rule and the
            // new five-tier one alike, which is why those tests stay
            // green with no change.
            (4_823_456_789, "4.5 GB"),
            (344 << 20, "344.0 MB"),
        ] {
            assert_eq!(human_bytes(bytes), want, "{bytes}");
        }
    }

    /// The plan-time refusal the dialog prints, and the line the user
    /// reads on the night the card is nearly full (fileops.md,
    /// plan-time errors; brief 006 AC2). The first assertion is the twin
    /// of `clip_bridge::the_two_untestable_messages_now_have_a_test` —
    /// the two dialogs say the same thing about the same situation.
    ///
    /// The WIRING — that both replan paths actually reach this function
    /// — is what the driven round
    /// `the_copy_refusal_reaches_the_dialog_on_the_drop_back_after_keep_both`
    /// pins; on Windows, where that round cannot run (NTFS allocates on
    /// `set_len`), this test is what pins the sentence.
    #[test]
    fn the_copy_refusal_reads_in_units_a_person_reads() {
        use fastcull_core::fileops::PlanError;

        assert_eq!(
            no_room_for_the_copy(7_834_567_890, 1_234_567_890),
            "The copy needs 7.3 GB and there is 1.1 GB free at the destination."
        );
        assert_eq!(
            copy_error_text(&PlanError::InsufficientSpace {
                needed: 7_834_567_890,
                free: 1_234_567_890
            }),
            "The copy needs 7.3 GB and there is 1.1 GB free at the destination."
        );
        // Core's developer-facing form never leaks through this function,
        // whatever the numbers are.
        assert!(
            !copy_error_text(&PlanError::InsufficientSpace { needed: 1, free: 0 })
                .contains("bytes")
        );
        // Every other refusal keeps core's words (fileops.md): this
        // sentence is the space one's alone.
        assert_eq!(
            copy_error_text(&PlanError::DestNotADirectory),
            "the destination is not a folder"
        );
    }
}
