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
            win.invoke_focus_keys();
            refocus_topmost_deferred(&win);
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
pub(crate) fn refocus_topmost_deferred(win: &MainWindow) {
    let win = win.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        if let Some(win) = win.upgrade() {
            win.invoke_focus_keys();
        }
    });
}
