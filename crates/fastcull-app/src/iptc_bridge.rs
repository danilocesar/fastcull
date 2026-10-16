//! IPTC panel bridge (M5, iptc-templates.md): the panel's callbacks (field
//! commit/clear, keywords, templates, revert, dock toggle), the model
//! rebuild that feeds it, and the app-side view of the core field table.

use std::cell::RefCell;
use std::rc::Rc;

use fastcull_core::iptc::IptcField;
use slint::{ComponentHandle, VecModel};

use crate::focus::refocus_topmost_deferred;
use crate::nav::reveal_cursor;
use crate::presenter::refresh;
use crate::session::reload_templates;
use crate::state::AppState;
use crate::{IptcFieldRow, KeywordChip, MainWindow};

/// Wire the IPTC panel: the dock toggle plus every editing callback.
pub(crate) fn wire(window: &MainWindow, state: &Rc<RefCell<AppState>>) {
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_iptc_toggle(move || {
            let Some(win) = win.upgrade() else { return };
            // Read BEFORE the toggle: `K` raises this flag and then calls
            // us, and the keyword field clears it again when its `init`
            // takes focus — by the time the panel is up the flag says
            // nothing about which key opened it (issue #64).
            let wants_keywords = win.get_iptc_focus_keywords();
            {
                let mut st = state.borrow_mut();
                st.iptc_panel.visible = !st.iptc_panel.visible;
                if st.iptc_panel.visible {
                    // The panel's open: one of templates.toml's three read
                    // moments (iptc-templates.md). The close reads nothing
                    // and writes nothing — not even the dropdown's index,
                    // which is what keeps the choice across it (D11).
                    reload_templates(&mut st, false);
                }
                // Publish the new dock state BEFORE any geometry read:
                // grid-width is a binding on it, and revealing against
                // the STALE width mis-anchored the viewport and let the
                // follow-scroll claim swap the photo (issue #16).
                win.set_iptc_visible(st.iptc_panel.visible);
            }
            // The dock reflows the grid: anchor on the cursor so the
            // viewport doesn't land somewhere new (persona gap 1).
            reveal_cursor(&win, &state);
            // Closing the panel DESTROYS its editors; if one was focused,
            // focus lands on no element and the keyboard dies (issue #41
            // D1 — the user's live hit, via View > IPTC Panel; the menu's
            // own restore targets the destroyed editor and strands the
            // keys). The mid-edit text is discarded with the editor (user
            // decision). The OPEN half is gated instead of skipped
            // (issue #64): with `K` the keyword field must keep the
            // focus its own `init` just took, but every other open has
            // nobody to protect.
            if !win.get_iptc_visible() {
                // BOTH claims, and neither is redundant. The synchronous
                // one closes the ownerless window for the keyboard route
                // (`I`), where nothing follows to override it — measured
                // at 19-22 ms before this. The deferred one is the only
                // one that survives the MENU route, where Slint's MenuBar
                // restores focus to the destroyed editor after the
                // activation returns. The range covers the keyword field
                // as well as the rows: closing destroys the whole panel.
                win.invoke_dbg_focus_claim("panel-close".into());
                crate::focus::reclaim_destroyed_editors(
                    &win,
                    1..=crate::focus::FIELD_ROWS + 1,
                    crate::focus::Reclaim::TopmostScope,
                );
                refocus_topmost_deferred(&win);
            } else if !wants_keywords {
                // A BELT for the #41 family, not the diagnosed cause: the
                // measured strand is the rows rebuild, and the reclaim
                // above closes it synchronously. This covers whatever
                // else an open may leave holding nothing — an `I` from a
                // real key dispatch mutates the item tree from inside
                // Slint's own delivery to the focus item, which is where
                // the 1-in-8 of issue #64 was first seen. Gated on the
                // flag because with `K` the keyword field's `init` claims
                // focus during instantiation and this claim would steal
                // it straight back.
                win.invoke_dbg_focus_claim("panel-open".into());
                refocus_topmost_deferred(&win);
            }
        });
    }
    {
        // Manual field commit: same tri-state as templates, but in the
        // PANEL bare emptiness PRESERVES (persona IN-MY-WAY rule) — an
        // empty commit is a no-op; clearing is the explicit control.
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_iptc_field_committed(move |i, text, return_focus| {
            let Some(win) = win.upgrade() else { return };
            // Sanitize at the commit boundary (NFC + control-strip + trim:
            // raw controls make the XMP packet invalid, QE-proven).
            let text = fastcull_core::iptc::sanitize_text(text.as_str());
            if !text.is_empty() {
                let mut st = state.borrow_mut();
                let batch = st.grid.selection.batch(&st.grid.view, st.grid.cursor);
                // No-op guard (gate finding): a value-unchanged commit —
                // Enter as "back to the grid", or the G7 click-away
                // double-fire — must not clobber the shared revert slot
                // or rewrite sidecars.
                let unchanged = batch.iter().all(|id| {
                    st.session
                        .iptc
                        .get(*id)
                        .is_some_and(|d| iptc_field_get(d, i as usize) == Some(&text))
                });
                if !batch.is_empty() && !unchanged {
                    let snaps: Vec<_> = batch
                        .iter()
                        .filter_map(|id| st.session.iptc.get(*id).cloned())
                        .collect();
                    for id in &batch {
                        if let Some(d) = st.session.iptc.get_mut(*id) {
                            iptc_field_set(d, i as usize, Some(text.clone()));
                        }
                    }
                    let label = format!(
                        "{} on {} image(s)",
                        iptc_field_label(i as usize),
                        batch.len()
                    );
                    commit_batch_mutation(&mut st, &batch, snaps, &label);
                }
            }
            if return_focus {
                win.invoke_dbg_focus_claim("field-commit".into());
                win.invoke_focus_grid(); // G4: cursor stays, grid gets keys
            }
            refresh(&win, &state);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_iptc_field_clear(move |i| {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                let batch = st.grid.selection.batch(&st.grid.view, st.grid.cursor);
                let all_unset = batch.iter().all(|id| {
                    st.session
                        .iptc
                        .get(*id)
                        .is_some_and(|d| iptc_field_get(d, i as usize).is_none())
                });
                if !batch.is_empty() && !all_unset {
                    let snaps: Vec<_> = batch
                        .iter()
                        .filter_map(|id| st.session.iptc.get(*id).cloned())
                        .collect();
                    for id in &batch {
                        if let Some(d) = st.session.iptc.get_mut(*id) {
                            iptc_field_set(d, i as usize, None);
                        }
                    }
                    let label = format!(
                        "clear {} on {} image(s)",
                        iptc_field_label(i as usize),
                        batch.len()
                    );
                    commit_batch_mutation(&mut st, &batch, snaps, &label);
                }
            }
            refresh(&win, &state);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_iptc_keyword_added(move |text| {
            let Some(win) = win.upgrade() else { return };
            let kws: Vec<String> = text
                .split(',')
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty())
                .collect();
            if !kws.is_empty() {
                let mut st = state.borrow_mut();
                let batch = st.grid.selection.batch(&st.grid.view, st.grid.cursor);
                // No-op guard (gate N2): re-entering an already-present
                // keyword — easy via the G7 click-away — must not clobber
                // the shared revert slot or rewrite sidecars. Dry-run on
                // clones; commit only when something actually changes.
                let changed = batch.iter().any(|id| {
                    st.session.iptc.get(*id).is_some_and(|d| {
                        let mut probe = d.clone();
                        probe.add_keywords(kws.iter().cloned());
                        probe.keywords != d.keywords
                    })
                });
                if !batch.is_empty() && changed {
                    let snaps: Vec<_> = batch
                        .iter()
                        .filter_map(|id| st.session.iptc.get(*id).cloned())
                        .collect();
                    for id in &batch {
                        if let Some(d) = st.session.iptc.get_mut(*id) {
                            d.add_keywords(kws.iter().cloned());
                        }
                    }
                    let label = format!("keywords on {} image(s)", batch.len());
                    commit_batch_mutation(&mut st, &batch, snaps, &label);
                }
            }
            refresh(&win, &state);
        });
    }
    {
        // Chip X: removes the keyword from EVERY batch image — revert-
        // covered (persona: never un-revertible batch destruction).
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_iptc_keyword_removed(move |chip_index| {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                let batch = st.grid.selection.batch(&st.grid.view, st.grid.cursor);
                // Rebuild the chip order exactly as the panel shows it
                // (first-seen across the batch in view order).
                let mut order: Vec<String> = Vec::new();
                for id in &batch {
                    if let Some(d) = st.session.iptc.get(*id) {
                        for kw in &d.keywords {
                            if !order.contains(kw) {
                                order.push(kw.clone());
                            }
                        }
                    }
                }
                if let Some(kw) = order.get(chip_index as usize).cloned() {
                    let snaps: Vec<_> = batch
                        .iter()
                        .filter_map(|id| st.session.iptc.get(*id).cloned())
                        .collect();
                    for id in &batch {
                        if let Some(d) = st.session.iptc.get_mut(*id) {
                            d.keywords.retain(|k| *k != kw);
                        }
                    }
                    let label = format!("remove '{kw}' from {} image(s)", batch.len());
                    commit_batch_mutation(&mut st, &batch, snaps, &label);
                }
            }
            refresh(&win, &state);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_iptc_apply_template(move |tpl_index| {
            let Some(win) = win.upgrade() else { return };
            let (name, outcome) = {
                let mut st = state.borrow_mut();
                // The NAME the dropdown showed at the click, from the cache
                // of the window's names model — set in the same block as the
                // model itself (`refresh_iptc_panel`), so the index and the
                // name come from one list. Never the ComboBox's
                // `current-value`, which a deferred handler derives. An index
                // past the list (unreachable: Apply is disabled on an empty
                // list) reads as a name no template has, so it ends in the
                // vanished-name warning, never in a silent return.
                let name = st
                    .iptc_panel
                    .cache
                    .names
                    .get(tpl_index.max(0) as usize)
                    .cloned()
                    .unwrap_or_default();
                let outcome = apply_by_name(&mut st, &name);
                (name, outcome)
            };
            // Every click ends in ONE mark naming its outcome
            // (test-harness.md, "Template apply"): the `wait:` target of
            // every Apply test — `templates loaded from` is emitted by the
            // panel's open too, and a wait is never "next", so the outcome
            // is what tells two clicks in one run apart.
            crate::trace::trace_mark_with(|| format!("template apply '{name}': {outcome}"));
            refresh(&win, &state);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_iptc_revert(move || {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                let ids = std::mem::take(&mut st.iptc_panel.revert_ids);
                let mut images: Vec<_> = ids
                    .iter()
                    .filter_map(|id| st.session.iptc.get(*id).cloned())
                    .collect();
                if st.iptc_panel.revert.revert_into(&mut images) {
                    for (id, data) in ids.iter().zip(images) {
                        if let Some(slot) = st.session.iptc.get_mut(*id) {
                            *slot = data.clone();
                        }
                        if let (Some(path), Some(writer)) =
                            (st.session.paths.get(*id), &st.session.writer)
                        {
                            writer.iptc(path.clone(), data);
                        }
                    }
                }
                st.iptc_panel.revert_label.clear();
            }
            refresh(&win, &state);
        });
    }
}

/// The panel field rows: the core table, in its declaration order, which
/// IS the display order. The row index is the callback contract with the
/// UI (iptc-field-committed / iptc-field-clear), so it must stay the
/// core order — hence indexing `IptcField::ALL` rather than keeping a
/// parallel list here.
///
/// An out-of-range index (a UI/core disagreement) reads as "no value" and
/// writes nowhere, exactly as the hand-written match arms did.
fn iptc_field_label(i: usize) -> &'static str {
    IptcField::ALL.get(i).map_or("field", |f| f.label())
}

fn iptc_field_get(d: &fastcull_core::iptc::IptcData, i: usize) -> Option<&String> {
    IptcField::ALL.get(i).and_then(|f| f.get(d))
}

fn iptc_field_set(d: &mut fastcull_core::iptc::IptcData, i: usize, v: Option<String>) {
    if let Some(f) = IptcField::ALL.get(i) {
        f.set(d, v);
    }
}

/// Populate the panel models for the current batch (selection in view
/// order, or the cursor). Field rows get the tri-state UI mapping: common
/// value across the batch = shown; differing values = `mixed`; unset
/// everywhere = untouched. Keyword chips show the batch UNION with
/// coverage counts on multi-selections — the chip rule, stated in
/// iptc-templates.md "The panel's models" (persona: an un-revertible
/// batch-destructive X is unacceptable — removal arms the shared slot).
pub(crate) fn refresh_iptc_panel(win: &MainWindow, st: &mut AppState) {
    win.set_iptc_visible(st.iptc_panel.visible);
    if !st.iptc_panel.visible {
        return;
    }
    let batch = st.grid.selection.batch(&st.grid.view, st.grid.cursor);
    win.set_iptc_batch_label(
        match batch.len() {
            0 => "No image".to_string(),
            1 => st.session.labels.get(batch[0]).cloned().unwrap_or_default(),
            n => format!("{n} images selected"),
        }
        .into(),
    );
    win.set_iptc_warning(st.templates.warnings.join("\n").into());
    // Build plain-data snapshots first; the Slint models are rebuilt ONLY
    // when content changed (gate finding: unconditional rebuilds tore the
    // field editors down mid-typing on every 33 ms engine tick) — and the
    // rows and the chips at the first refresh after a session swap too
    // (issue #114, the swap rule below).
    let rows: Vec<(String, String, bool)> = (0..IptcField::ALL.len())
        .map(|i| {
            let mut vs = batch.iter().filter_map(|id| {
                st.session
                    .iptc
                    .get(*id)
                    .map(|d| iptc_field_get(d, i).cloned())
            });
            let head = vs.next().flatten();
            let mixed = {
                let mut vs = batch.iter().filter_map(|id| {
                    st.session
                        .iptc
                        .get(*id)
                        .map(|d| iptc_field_get(d, i).cloned())
                });
                let h = vs.next().flatten();
                vs.any(|v| v != h)
            };
            (
                iptc_field_label(i).to_string(),
                if mixed {
                    String::new()
                } else {
                    head.unwrap_or_default()
                },
                mixed,
            )
        })
        .collect();
    let mut chip_data: Vec<(String, usize)> = Vec::new();
    for id in &batch {
        if let Some(d) = st.session.iptc.get(*id) {
            for kw in &d.keywords {
                match chip_data.iter_mut().find(|(t, _)| t == kw) {
                    Some((_, n)) => *n += 1,
                    None => chip_data.push((kw.clone(), 1)),
                }
            }
        }
    }
    let total = batch.len();
    let chips: Vec<(String, String)> = chip_data
        .into_iter()
        .map(|(text, n)| {
            let cov = if total > 1 {
                format!("{n}/{total}")
            } else {
                String::new()
            };
            (text, cov)
        })
        .collect();
    let names: Vec<String> = st.templates.list.iter().map(|t| t.name.clone()).collect();

    let session_gen = win.get_session_gen();
    // THE SWAP RULE (iptc-templates.md "The panel's models", issue #114).
    // `seen_gen` is -1 after `begin_session` (and at launch) and is stamped
    // just after the rows block below, so `swapped` is true exactly once
    // per session: at the first refresh the panel runs after a swap — the
    // swap's own refresh with the panel open, or the reopen's synchronous
    // refresh (`on_iptc_toggle` → `reveal_cursor` → `refresh`) with it
    // closed. It is read HERE, before that stamp, so the chip block below
    // the stamp still sees the swap through this local. On that refresh
    // the rows and the chips are replaced WHATEVER the comparison says: a
    // cache reset to empty can alias the new session's content — a batch
    // with no keywords yet compares equal to the reset chip cache — and the
    // old folder's chips stayed on screen with the old folder's counts
    // until the new folder's first keyword landed. Within a session content
    // alone decides, as before. The rows rebuild no more often than they
    // did: the first visible refresh of a session always found eleven rows
    // against an empty cache. The template names are not part of this
    // rule: their cache is never reset (`IptcPanelState::begin_session`),
    // so it cannot alias, and the comparison alone decides at the swap too.
    let swapped = st.iptc_panel.cache.seen_gen != session_gen;
    if swapped || st.iptc_panel.cache.rows != rows {
        // THE ITEM-TREE MUTATION (issues #63/#64): replacing the model
        // destroys all eleven `LineEdit`s and builds eleven new ones, so
        // whichever of them held the keyboard stops existing. Slint's
        // window keeps a WEAK reference to its focus item and nothing
        // reassigns it, so the keyboard is stranded until someone claims
        // it. This mark is the "when" of that; the `focus:` marks around
        // it say who was holding it.
        crate::trace::trace_mark_with(|| format!("iptc rows rebuilt (gen {session_gen})"));
        // Bumped BEFORE the replacement (issue #63 F2): an editor stamps
        // this on focus gain, and a blur that finds the stamp stale knows
        // its row was destroyed under it and DISCARDS the in-flight text
        // — the recorded 2026-08-03 rule, now enforced by a compare
        // instead of by whether Slint happened to deliver a FocusOut
        // before dropping the item.
        win.set_iptc_rebuild_gen(win.get_iptc_rebuild_gen().wrapping_add(1));
        win.set_iptc_fields(slint::ModelRc::new(VecModel::from(
            rows.iter()
                .map(|(label, value, mixed)| IptcFieldRow {
                    label: label.clone().into(),
                    value: value.clone().into(),
                    mixed: *mixed,
                })
                .collect::<Vec<_>>(),
        )));
        // …and the rescue, in the SAME pass, so the window in which
        // nobody owns the keyboard is zero-length. The row ids are
        // `1..=IptcField::ALL.len()` (the editor writes `i + 1` on focus
        // gain); the keyword field's id is one past them and is
        // deliberately NOT in this range — it survives this replacement.
        //
        // WHERE the keyboard goes is the whole question. Within one
        // session it goes back to the same row: the user is captioning,
        // and "focus stays where clicked" (iptc-templates.md) is a
        // shipped rule — sending it to the grid instead makes the next
        // caption character a cull command. Across a session SWAP the
        // field's meaning went with the folder, so it goes to the grid,
        // which is the #41 D3 contract.
        crate::focus::reclaim_destroyed_editors(
            win,
            1..=crate::focus::FIELD_ROWS,
            if swapped {
                crate::focus::Reclaim::TopmostScope
            } else {
                crate::focus::Reclaim::SameRow
            },
        );
        st.iptc_panel.cache.rows = rows;
    }
    st.iptc_panel.cache.seen_gen = session_gen;
    // No reclaim after these two replacements — the chips' forced
    // replacement at a swap included — deliberately (#63/#64): the chip
    // repeater holds Text and TouchArea items and the template model feeds a
    // ComboBox — neither owns an editor, and the keyword LineEdit is a
    // SIBLING of both, not a child. A reclaim here would fire while the
    // editor it "rescued" was still alive and focused, taking the keyboard
    // away from a user mid-word every time another image's sidecar landed
    // a keyword. The rows model is the only one whose replacement destroys
    // a focus holder.
    if swapped || st.iptc_panel.cache.chips != chips {
        win.set_iptc_keywords(slint::ModelRc::new(VecModel::from(
            chips
                .iter()
                .map(|(text, cov)| KeywordChip {
                    text: text.clone().into(),
                    coverage: cov.clone().into(),
                })
                .collect::<Vec<_>>(),
        )));
        st.iptc_panel.cache.chips = chips;
    }
    // The template names: replaced exactly when the re-read names differ
    // from what the window holds — at a swap, a reopen or an Apply alike —
    // and the dropdown FOLLOWS THE NAME (iptc-templates.md "The panel's
    // models"; brief 015 R5): the name the window's index points at in the
    // OLD list keeps its selection at its position in the NEW one, the first
    // entry when it is gone. Model first, index second, which reads as cause
    // then effect; the order is immaterial, because the ComboBox answers a
    // new model with `reset-current()`, a CLAMP, run deferred after this
    // callback returns, and a clamp leaves an in-range index where it was
    // put (ui-grid.md, *Slint facts*; Cargo.toml's seventh canary). Within
    // an unchanged list nothing is written: the selection is the user's.
    if st.iptc_panel.cache.names != names {
        // The twin of `iptc rows rebuilt (gen G)` (test-harness.md,
        // "Template names"): a COUNT of this mark is the change-only rule's
        // witness — a failed read, an unchanged file and every refresh in
        // between emit none (QE 2026-10-16, finding 4). A closure, so
        // nothing is formatted when nobody listens.
        crate::trace::trace_mark_with(|| format!("iptc names replaced (gen {session_gen})"));
        let selected = st
            .iptc_panel
            .cache
            .names
            .get(win.get_iptc_template_index().max(0) as usize)
            .map(String::as_str);
        let index = fastcull_core::iptc::selection_after_replacement(selected, &names);
        win.set_iptc_templates(slint::ModelRc::new(VecModel::from(
            names
                .iter()
                .map(|n| slint::SharedString::from(n.as_str()))
                .collect::<Vec<_>>(),
        )));
        win.set_iptc_template_index(index as i32);
        st.iptc_panel.cache.names = names;
    }
    win.set_iptc_revert_enabled(!st.iptc_panel.revert_ids.is_empty());
    win.set_iptc_revert_label(st.iptc_panel.revert_label.clone().into());
}

/// Apply, after the click (iptc-templates.md, "Apply re-reads first and
/// applies by name"; brief 015 R1–R3, R6): re-read templates.toml, find the
/// template called `name` in what was read, and stamp it on the batch.
/// Returns the outcome the `template apply` mark names. Every way to apply
/// NOTHING leaves the revert slot untouched and returns early, in this
/// order: the re-read failed as a whole (R4 — the strip already carries the
/// error and the last-read note); the name is not in the file any more (R3
/// — the strip is told, and no other template is stamped in its place); no
/// image to stamp (D12 — the strip unchanged); the expansion failed (R6 —
/// the error APPENDED to the read's warnings, never put in their place).
fn apply_by_name(st: &mut AppState, name: &str) -> String {
    // R1: the third read moment, BEFORE anything is applied — a saved edit
    // applies at once.
    if !reload_templates(st, true) {
        return "templates.toml could not be re-read — nothing applied".to_string();
    }
    let tpl = match fastcull_core::iptc::template_by_name(&st.templates.list, name) {
        Ok(tpl) => tpl.clone(),
        Err(e) => {
            st.templates.warnings.push(e.to_string());
            return "not in templates.toml — nothing applied".to_string();
        }
    };
    let batch = st.grid.selection.batch(&st.grid.view, st.grid.cursor);
    if batch.is_empty() {
        return "no image — nothing applied".to_string();
    }
    // Contexts in batch (= view) order: the {seq} contract.
    let ctxs: Vec<_> = batch
        .iter()
        .map(|id| {
            let name = st.session.labels.get(*id).cloned().unwrap_or_default();
            let mtime = st
                .session
                .paths
                .get(*id)
                .and_then(|p| std::fs::metadata(p).ok())
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::UNIX_EPOCH);
            fastcull_core::iptc::ExpandContext::from_sort_key(
                st.session.capture_keys.get(*id).and_then(|k| k.as_deref()),
                mtime,
                &name,
                st.session.camera_models.get(*id).and_then(|c| c.as_deref()),
            )
        })
        .collect();
    let mut images: Vec<_> = batch
        .iter()
        .filter_map(|id| st.session.iptc.get(*id).cloned())
        .collect();
    match fastcull_core::iptc::apply_template(&tpl, &mut images, &ctxs) {
        Ok(snaps) => {
            for (id, data) in batch.iter().zip(images) {
                if let Some(slot) = st.session.iptc.get_mut(*id) {
                    *slot = data;
                }
            }
            let label = format!("apply '{}' to {} image(s)", tpl.name, batch.len());
            commit_batch_mutation(st, &batch, snaps, &label);
            format!("applied to {} image(s)", batch.len())
        }
        Err(e) => {
            // All-or-nothing: nothing changed; surface it.
            let error = e.to_string();
            st.templates.warnings.push(error.clone());
            format!("{error} — nothing applied")
        }
    }
}

/// Persist + arm the shared revert slot after a batch mutation (template
/// Apply, manual field commit, keyword add/chip removal — every one, per
/// the user decision). `snapshots` are pre-mutation states parallel to
/// `ids`; writes go through the serialized writer thread.
fn commit_batch_mutation(
    st: &mut AppState,
    ids: &[usize],
    snapshots: Vec<fastcull_core::iptc::IptcData>,
    label: &str,
) {
    st.iptc_panel.revert.store(snapshots);
    st.iptc_panel.revert_ids = ids.to_vec();
    st.iptc_panel.revert_label = format!("Revert: {label}");
    st.session.touched_iptc.extend(ids.iter().copied());
    if let Some(writer) = &st.session.writer {
        for id in ids {
            if let (Some(path), Some(data)) = (st.session.paths.get(*id), st.session.iptc.get(*id))
            {
                writer.iptc(path.clone(), data.clone());
            }
        }
    }
}
