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
            {
                let mut st = state.borrow_mut();
                st.iptc_panel.visible = !st.iptc_panel.visible;
                if st.iptc_panel.visible {
                    reload_templates(&mut st); // read-on-open live-reload
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
            // decision). Close only: on OPEN the K path may just have
            // landed focus in the keyword field, which must keep it.
            if !win.get_iptc_visible() {
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
            {
                let mut st = state.borrow_mut();
                let batch = st.grid.selection.batch(&st.grid.view, st.grid.cursor);
                let Some(tpl) = st.session.templates.get(tpl_index as usize).cloned() else {
                    return;
                };
                if !batch.is_empty() {
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
                            // Camera model is not plumbed yet: {camera}
                            // expands empty (recorded in the spec ledger).
                            fastcull_core::iptc::ExpandContext::from_sort_key(
                                st.session.capture_keys.get(*id).and_then(|k| k.as_deref()),
                                mtime,
                                &name,
                                None,
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
                            commit_batch_mutation(&mut st, &batch, snaps, &label);
                        }
                        Err(e) => {
                            // All-or-nothing: nothing changed; surface it.
                            st.session.template_warnings = vec![e.to_string()];
                        }
                    }
                }
            }
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
/// coverage counts on multi-selections (persona: an un-revertible
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
    win.set_iptc_warning(st.session.template_warnings.join("\n").into());
    // Build plain-data snapshots first; the Slint models are rebuilt ONLY
    // when content changed (gate finding: unconditional rebuilds tore the
    // field editors down mid-typing on every 33 ms engine tick).
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
    let names: Vec<String> = st
        .session
        .templates
        .iter()
        .map(|t| t.name.clone())
        .collect();

    if st.iptc_panel.cache.rows != rows {
        win.set_iptc_fields(slint::ModelRc::new(VecModel::from(
            rows.iter()
                .map(|(label, value, mixed)| IptcFieldRow {
                    label: label.clone().into(),
                    value: value.clone().into(),
                    mixed: *mixed,
                })
                .collect::<Vec<_>>(),
        )));
        st.iptc_panel.cache.rows = rows;
    }
    if st.iptc_panel.cache.chips != chips {
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
    if st.iptc_panel.cache.names != names {
        win.set_iptc_templates(slint::ModelRc::new(VecModel::from(
            names
                .iter()
                .map(|n| slint::SharedString::from(n.as_str()))
                .collect::<Vec<_>>(),
        )));
        st.iptc_panel.cache.names = names;
    }
    win.set_iptc_revert_enabled(!st.iptc_panel.revert_ids.is_empty());
    win.set_iptc_revert_label(st.iptc_panel.revert_label.clone().into());
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
