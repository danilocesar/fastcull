//! Copy Picks bridge (M6, fileops.md): the copy dialog's callbacks, the
//! plan/preview rebuild behind them, and the burst regrouping that shares
//! the same "re-derive over the whole session, in true sort order" shape.

use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;

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
                        .as_ref()
                        .map(|d| d.to_string_lossy().into_owned())
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
                win.set_copy_dest(dir.to_string_lossy().into_owned().into());
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
            // fresh so sidecar existence, refresh mtimes and free space
            // are decided AFTER every pending write landed (gate HIGH
            // finding — a frozen at-open plan is never executed).
            if let Some(writer) = &st.session.writer {
                writer.flush();
            }
            copy_replan(&win, &mut st);
            let Some(plan) = st.copy.plan.take() else {
                return; // replan surfaced an error; the dialog shows it
            };
            let (handle, rx) = fastcull_core::fileops::execute(plan);
            st.copy.handle = Some(handle);
            st.copy.rx = Some(rx);
            win.set_copy_state(1);
            win.set_copy_progress("Starting…".into());
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

/// Rebuild the copy plan from the dialog's current inputs and publish the
/// preview properties (fileops.md dialog minimums).
fn copy_replan(win: &MainWindow, st: &mut AppState) {
    use fastcull_core::fileops::{plan, ExistsMode, PlanError};
    let sources = plan_sources(st);
    win.set_copy_error("".into());
    win.set_copy_ready(false);
    win.set_copy_preview("".into());
    win.set_copy_collisions("".into());
    win.set_copy_show_skip_toggle(false);
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
    let mode = if win.get_copy_skip_existing() {
        ExistsMode::Skip
    } else {
        ExistsMode::Rename
    };
    // The badge follows the disk from the moment the dialog looks (a copy
    // the user deleted by hand loses it here, and gets it back when the
    // copy lands again — persona decision 2026-08-21); the plan itself
    // re-checks every landed path on its own.
    st.copy.copies.refresh();
    match plan(&sources, &dest, template, mode, &st.copy.copies) {
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
                    human_bytes(p.total_bytes),
                    match p.free_bytes {
                        Some(free) => format!("{} free", human_bytes(free)),
                        None => "free space unknown".to_string(),
                    }
                )
                .into(),
            );
            let mut notes = Vec::new();
            if p.renamed > 0 {
                notes.push(format!("{} will be renamed (name collisions)", p.renamed));
            }
            if p.skipped > 0 {
                notes.push(format!("{} already at destination (skipped)", p.skipped));
            }
            if p.refreshed > 0 {
                notes.push(format!("{} sidecars will be refreshed", p.refreshed));
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
            let collided = p.renamed > 0 || p.skipped > 0 || p.refreshed > 0;
            win.set_copy_collisions(notes.join(" · ").into());
            win.set_copy_show_skip_toggle(collided);
            win.set_copy_ready(true);
            st.copy.plan = Some(p);
        }
        Err(
            e @ (PlanError::InsufficientSpace { .. }
            | PlanError::DestEqualsSource
            | PlanError::DestInsideSource
            | PlanError::DestExists(_)
            | PlanError::TemplateCollision { .. }
            | PlanError::Template(_)
            | PlanError::Io(_)),
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
