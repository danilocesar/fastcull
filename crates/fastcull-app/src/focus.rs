//! Focus continuity (issue #41): the one rule that keyboard focus returns
//! to the topmost surface whenever the focused editor is destroyed or
//! covered, and the modal callback that claims it.

use slint::ComponentHandle;

use crate::MainWindow;

/// Wire the "a modal opened" claim (Help > About / Keyboard Shortcuts and
/// every other cover): steal the keyboard now, and again after the menu's
/// own focus restore has unwound.
pub(crate) fn wire(window: &MainWindow) {
    {
        // Help > About / Keyboard Shortcuts: steal the keyboard from any
        // focused editor now COVERED by the modal (issue #41 D2). The
        // immediate claim handles the non-menu callers; the deferred one
        // survives the MenuBar's post-activation focus restore, which
        // otherwise hands the keys back to the field hidden behind the
        // scrim — an un-dismissable modal, with every keystroke landing
        // invisibly in the field and committable as metadata.
        let win = window.as_weak();
        window.on_modal_opened(move || {
            let Some(win) = win.upgrade() else { return };
            win.invoke_dbg_focus_claim("modal".into());
            win.invoke_focus_keys();
            refocus_topmost_deferred(&win);
        });
    }
    {
        // Every menu item, whatever it does (issue #63, QE finding):
        // see `reassert_owner_deferred`.
        let win = window.as_weak();
        window.on_menu_activated(move || {
            let Some(win) = win.upgrade() else { return };
            reassert_owner_deferred(&win, "menu");
        });
    }
}

/// THE focus-continuity rule (issue #41): whenever the focused editor is
/// destroyed or covered — IPTC panel close, session swap, a modal opening
/// over a focused field — keyboard focus must deterministically return to
/// the topmost surface's key scope, or the keyboard dies (no element has
/// focus and nothing reclaims it; at 1:1 there is no discoverable
/// recovery) or, worse, keystrokes land invisibly in a field hidden
/// behind a modal's scrim and get committed as metadata.
///
/// Deferred via a zero-length timer ON PURPOSE: Slint's MenuBar restores
/// focus to the previously-focused element AFTER the item activation
/// callback runs, inside the same event dispatch — QE proved a
/// synchronous `focus-keys()` inside an activation is overridden by that
/// restore. A timer scheduled during the dispatch cannot fire until the
/// dispatch (activation + menu close + focus restore) has fully
/// unwound, so the queued claim always lands last. The Slint side adds a
/// synchronous belt-and-braces bounce on the editors themselves (a focus
/// gain that arrives behind a modal is handed straight to the modal's
/// scope) so the dangerous surfaces never hold the keyboard even
/// mid-dispatch.
///
/// This is the MENU half of the rule. The half that answers a destroyed
/// editor is `reclaim_destroyed_editors` below, and it is synchronous —
/// the deferral is a cost paid only where the MenuBar makes it necessary.
pub(crate) fn refocus_topmost_deferred(win: &MainWindow) {
    let win = win.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        if let Some(win) = win.upgrade() {
            // The claim that actually lands, one event-loop iteration
            // after the caller queued it (issues #63/#64): the reason
            // above says who asked, this says when it arrived.
            win.invoke_dbg_focus_claim("deferred".into());
            win.invoke_focus_keys();
        }
    });
}

/// Put the keyboard back where the owner token says it belongs, one
/// event-loop iteration from now (issue #63, QE finding 2026-08-30).
///
/// The hole this closes: activating ANY menu item blurs a focused panel
/// field, that blur COMMITS (G7), the commit rebuilds the field rows and
/// destroys the editor — and then the MenuBar restores focus to the
/// destroyed item, after the activation has returned. No synchronous
/// claim can survive that restore, which is why the deferral is the whole
/// mechanism here (see `refocus_topmost_deferred`), and the strand was
/// measured 5 times in 5 through View > Filter Bar.
///
/// It re-asserts the TOKEN rather than grabbing `keys`, because after a
/// menu action the keyboard belongs wherever it belonged before: for a
/// panel field that is the field ("focus stays where clicked"), and
/// blanket-claiming the grid would make the user's next caption character
/// a cull command — the same defect as reclaiming a rebuild to the grid.
/// A covering surface wins over both: `focus-keys()` routes to it.
pub(crate) fn reassert_owner_deferred(win: &MainWindow, why: &'static str) {
    // The session this claim was queued FOR (issue #63 FAIL-4). A swap
    // that lands before the timer fires makes the token meaningless — it
    // names a field of a folder that is gone — and re-asserting it would
    // put the keyboard into an editor holding the new session's values
    // instead of the grid, which is the #41 D3 contract. It is reachable
    // without any rebuild: two folders whose rows are identical (both
    // un-captioned) replace no model at all.
    let queued_at = win.get_session_gen();
    let win = win.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        let Some(win) = win.upgrade() else { return };
        let owner = win.get_focus_owner();
        let covered = win.get_about_visible()
            || win.get_shortcuts_visible()
            || win.get_copy_visible()
            || win.get_clip_visible();
        let panel_editor = win.get_iptc_visible() && !covered && win.get_session_gen() == queued_at;
        if panel_editor && (1..=FIELD_ROWS).contains(&owner) {
            win.invoke_dbg_focus_claim(format!("{why} -> row {}", owner - 1).into());
            arm_row_refocus(&win, owner - 1);
        } else if panel_editor && owner == FIELD_ROWS + 1 {
            // The keyword field, through the flag it already watches. It
            // is NOT inside the rows model, so a menu action does not
            // usually destroy it and the MenuBar's own restore lands
            // correctly — but claiming `keys` here instead would take the
            // keyboard off a live editor mid-word, which is the shipped
            // RUN17 behaviour ("the menu restore put the keyboard back in
            // the still-alive field") and a red test when broken.
            win.invoke_dbg_focus_claim(format!("{why} -> keywords").into());
            win.set_iptc_focus_keywords(true);
        } else {
            win.invoke_dbg_focus_claim(format!("{why} -> keys").into());
            win.invoke_focus_keys();
        }
    });
}

/// How many editors the IPTC panel's field-rows model holds. The owner
/// token numbers them `1..=FIELD_ROWS`; the keyword field is one past.
pub(crate) const FIELD_ROWS: i32 = fastcull_core::iptc::IptcField::ALL.len() as i32;

/// Ask row `row` to take the keyboard, stamped with the item-tree
/// generation it must have been BORN for (issue #63 FAIL-1).
///
/// The stamp is the whole mechanism. A repeater does not tear its children
/// down when the model is replaced — they die at its next update — so the
/// doomed instance is still alive and still watching this flag, and its
/// `changed want-refocus` runs first. Armed with the index alone it
/// consumed the flag in the rebuild's own millisecond, focused itself,
/// cleared the flag and then died: the recreated row saw nothing and the
/// keyboard sat on a destroyed item, measured dead 10 times in 10.
fn arm_row_refocus(win: &MainWindow, row: i32) {
    win.set_iptc_refocus_gen(win.get_iptc_rebuild_gen());
    win.set_iptc_refocus_row(row);
}

/// Arm it one event-loop iteration from now (issue #63 FAIL-1, QE
/// 2026-08-30). Two reasons it is deferred rather than written at once:
///
///  * `self.focus()` called from a repeater row's `init` DOES NOT TAKE
///    EFFECT in Slint 1.17 — proven by QE, 10 runs in 10 dead, and by
///    the trace of the first cut here: the recreated row's `init` claimed
///    at [3359] and the keyboard was still on nothing until an unrelated
///    deferred claim re-armed the flag at [3491]. A row can only take
///    focus from an event-loop callback that runs once it is alive to
///    the window, so a flag that arrives AFTER the repeater has rebuilt
///    is the one the recreated row can answer at once, from `changed
///    want-refocus`.
///  * the still-alive DOOMED instance watches the same flag and its
///    `changed want-refocus` runs first, so an immediate write is
///    consumed by the row that is about to die (validator FAIL-1).
///
/// Deferring makes that LATE ordering the likely one; it does not
/// guarantee it (CI on the v0.13.0 commit, 2026-09-01: on a 2-core
/// headless runner this timer fired inside the model swap's own
/// millisecond and the rows were recreated 16 ms later). The ordering it
/// cannot promise is covered by two belts in main.slint, one per hazard:
/// the generation stamp in `arm_row_refocus` against the doomed instance
/// consuming an early flag, and the row's own `Timer` against a live
/// instance born with the flag already true and therefore never seeing
/// a `changed` edge. The cost of deferring is the ownerless gap this
/// step exists to close, so it is measured and reported rather than
/// assumed: one event-loop iteration, not the ~200 ms the DEFERRED CLAIM
/// used to take, because nothing is racing it — but not zero either.
fn arm_row_refocus_deferred(win: &MainWindow, row: i32) {
    let win = win.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        if let Some(win) = win.upgrade() {
            arm_row_refocus(&win, row);
        }
    });
}

/// Where the keyboard goes when the app destroys the editor holding it.
pub(crate) enum Reclaim {
    /// Back to the SAME field row, once the repeater has recreated it.
    /// The panel is a captioning surface and iptc-templates.md's "focus
    /// stays where clicked" is a shipped rule; a rebuild that lands the
    /// keys on the grid instead turns the next character of a caption
    /// into a cull command (`x` rejects the photo and writes a sidecar).
    SameRow,
    /// To the topmost key scope. Only for a transition that takes the
    /// field's MEANING away — a session swap (the folder is gone, #41 D3)
    /// or the panel closing (the editors are not coming back).
    TopmostScope,
}

/// The SYNCHRONOUS half of the rule (issues #63/#64): answer, in the same
/// pass, an item-tree mutation that destroyed the editor named by
/// `focus-owner`. `destroyed` is the id range the caller just tore down.
///
/// Why it is needed at all: Slint's window holds a WEAK reference to its
/// focus item (`WindowInner::focus_item`). An editor destroyed by a model
/// replacement delivers NO `FocusOut`, and nothing reassigns focus — the
/// reference simply dangles and every key event afterwards dies on an
/// `upgrade()` that returns `None`. Measured on 20 instrumented session
/// swaps mid-edit, the keyboard was ownerless for 178-269 ms (median 215)
/// until the deferred claim's zero-length timer happened to run, and a
/// `key:y` sent inside that window is provably lost.
///
/// Why SYNCHRONOUS here, where `refocus_topmost_deferred` is not: the
/// deferral exists solely for the MenuBar, which restores focus to the
/// previously-focused element AFTER an item activation, inside the same
/// dispatch. A model rebuild runs in app code (a `refresh` pass), so no
/// restore follows it and a claim made now is the last word — which is the
/// point, since a deferred claim IS the ownerless window.
///
/// `destroyed` is a RANGE, not "any editor": for a field-rows replacement
/// the keyword field is NOT in it, because that editor is a sibling of the
/// rows model and survives — reclaiming on its token would yank the
/// keyboard out of a live editor mid-word, which the `K` flow parks there
/// deliberately.
pub(crate) fn reclaim_destroyed_editors(
    win: &MainWindow,
    destroyed: std::ops::RangeInclusive<i32>,
    to: Reclaim,
) {
    let owner = win.get_focus_owner();
    if !destroyed.contains(&owner) {
        return;
    }
    match to {
        Reclaim::SameRow => {
            // Arm the flag the RECREATED row watches, stamped with the
            // generation it will be born for so the still-alive doomed
            // instance cannot consume it (see `arm_row_refocus`). Nothing
            // is focused between here and that row's `init`, which runs
            // inside the repeater's own update — not on a timer — so this
            // is not the deferral the ownerless window was. The token is
            // left alone: it still names a field row, and the row that
            // claims will rewrite it.
            win.invoke_dbg_focus_claim(format!("rebuild -> row {}", owner - 1).into());
            arm_row_refocus_deferred(win, owner - 1);
            // …and a deferred re-assert behind it (issue #63 FAIL-3).
            // A menu opened over a focused field and then DISMISSED
            // without activating anything — a click elsewhere, Esc, a
            // miss — fires no `activated`, and Slint 1.17 offers no
            // open/dismiss callback to hang one on. The blur-commit
            // rebuild reclaims correctly, and then the MenuBar's restore
            // hands focus to the item that rebuild destroyed. This
            // re-reads the token when it fires, so it also routes to a
            // dialog that took over in between, and is a no-op when the
            // row already has the keyboard.
            reassert_owner_deferred(win, "restore");
        }
        Reclaim::TopmostScope => {
            win.invoke_dbg_focus_claim("rebuild -> keys".into());
            win.invoke_focus_keys();
            // Cleared HERE rather than left to `keys`'s own `changed
            // has-focus`, which Slint runs on the next event-loop
            // iteration: without this a second replacement in the same
            // refresh pass would read the dead editor's id and claim all
            // over again. Zero is honest either way — whatever
            // `focus-keys()` routed to, the element this token named no
            // longer exists.
            win.set_focus_owner(0);
        }
    }
}
