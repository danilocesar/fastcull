//! The Settings dialog bridge (settings.md, brief 008): opening and closing
//! it, a value committed or a tab reset, Clear cache — and `present`, the
//! one function that writes every one of the dialog's properties from the
//! settings in force.
//!
//! Every rule is core's (`fastcull_core::settings`: the parse, the clamps,
//! the precedence, the file, the notes); this module calls them and words
//! the result for the screen. Its only sentences of its own are
//! presentation: the loupe memory hint, the cache row, the notice line.

use std::cell::RefCell;
use std::rc::Rc;

use fastcull_core::settings::{self, Key, MemorySource, Readers, Settings};
use slint::ComponentHandle;

use crate::copy_bridge::human_bytes;
use crate::focus::refocus_topmost_deferred;
use crate::presenter::refresh;
use crate::state::{clamp_wash_opacity, AppState, CacheCleared, SettingsState};
use crate::trace::trace_mark;
use crate::MainWindow;

/// The pseudo-key the Read workers checkbox commits under: not a file key
/// (the file has one integer, `max_readers`), so it is routed to
/// `Settings::set_readers_adaptive` rather than parsed.
const READERS_ADAPTIVE: &str = "readers_adaptive";

/// Wire the dialog's callbacks, and give it what never changes: the tab
/// titles and the notes, both core's.
pub(crate) fn wire(window: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let titles: Vec<slint::SharedString> =
        settings::TABS.iter().map(|t| t.title().into()).collect();
    window.set_settings_tabs(slint::ModelRc::new(slint::VecModel::from(titles)));
    window.set_settings_note_auto_advance(Key::AutoAdvance.note().into());
    window.set_settings_note_wash(Key::SelectionWash.note().into());
    window.set_settings_note_loupe_memory(Key::LoupeMemory.note().into());
    window.set_settings_note_cache_cap(Key::CacheCap.note().into());
    window.set_settings_note_readers(Key::MaxReaders.note().into());
    window.set_settings_note_clear_cache(settings::CLEAR_CACHE_NOTE.into());
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_settings_open(move || {
            let Some(win) = win.upgrade() else { return };
            if win.get_settings_visible() {
                return;
            }
            {
                let mut st = state.borrow_mut();
                // The file is read again at every open, and what it says is
                // applied now (settings.md, "Reading") — unless the last
                // write failed, when the commits since live only in memory
                // and a re-read would silently take them back.
                if st.settings.write_error.is_none() {
                    if let Some(path) = st.settings.loaded.path.clone() {
                        st.settings.loaded = settings::load(&path);
                        // A re-read that fails says so on stderr like the
                        // startup read, in core's one wording (settings.md,
                        // "Reading"; QE 2026-10-01, D31: this read used to
                        // print nothing, only the trace mark below).
                        st.settings.loaded.report_on_stderr();
                    }
                    trace_read(&st.settings);
                } else {
                    // No read happened, and the mark says so rather than
                    // naming a file it did not read (senior-developer
                    // review F5 of brief 008).
                    trace_mark(
                        "settings: not re-read (a write failed and none has succeeded since)",
                    );
                }
                apply_instant(&win, st.current_settings());
                if st.settings.clear_rx.is_none() {
                    st.settings.cache_readout = cache_readout(None);
                }
                present(&win, &st);
                win.set_settings_tab(st.settings.last_tab);
            }
            win.set_settings_visible(true);
            // The dialog owns the keyboard from here (focus continuity, the
            // `-1` token): now, and again once the menu's own focus restore
            // has unwound (issue #41 D2) — a field it covers commits like a
            // click-away as the keyboard leaves it.
            win.invoke_dbg_focus_claim("settings-dialog".into());
            win.invoke_focus_keys();
            refocus_topmost_deferred(&win);
            trace_mark("settings opened");
            // The re-read can change the status line's settings note.
            refresh(&win, &state);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_settings_close(move || {
            let Some(win) = win.upgrade() else { return };
            state.borrow_mut().settings.last_tab = win.get_settings_tab();
            // Visibility FIRST, the keyboard second: the field the keyboard
            // leaves reads `settings-visible` in its deferred blur, and false
            // there is what makes Esc DISCARD its half-typed text
            // (settings.md, "Apply on commit").
            win.set_settings_visible(false);
            win.invoke_dbg_focus_claim("settings-close".into());
            win.invoke_focus_keys();
            trace_mark("settings closed");
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_settings_commit(move |name, text| {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                let before = st.current_settings().clone();
                let accepted = if name.as_str() == READERS_ADAPTIVE {
                    st.settings
                        .loaded
                        .settings
                        .set_readers_adaptive(text.as_str() == "true");
                    Some(Key::MaxReaders)
                } else if let Some(key) = Key::from_name(name.as_str()) {
                    // A refused value changes nothing in the model. The field
                    // is re-shown by its own `accepted` or blur (main.slint),
                    // never by `present` below: the user's first keystroke
                    // broke the field's binding to its property, and the value
                    // in force did not change (QE 2026-10-02, round 5).
                    st.settings
                        .loaded
                        .settings
                        .set_from_text(key, text.as_str())
                        .ok()
                        .map(|()| key)
                } else {
                    None
                };
                if let Some(key) = accepted {
                    trace_mark(&format!(
                        "settings committed {}.{} = {}",
                        key.table(),
                        key.name(),
                        st.current_settings().text_of(key)
                    ));
                    apply_instant(&win, st.current_settings());
                    let changed = *st.current_settings() != before;
                    save(&mut st.settings, changed);
                }
                present(&win, &st);
            }
            refresh(&win, &state);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_settings_reset(move |index| {
            let Some(win) = win.upgrade() else { return };
            let Some(tab) = usize::try_from(index)
                .ok()
                .and_then(settings::Tab::from_index)
            else {
                return;
            };
            {
                let mut st = state.borrow_mut();
                let before = st.current_settings().clone();
                st.settings.loaded.settings.reset_tab(tab);
                trace_mark(&format!("settings reset {}", tab.table()));
                apply_instant(&win, st.current_settings());
                let changed = *st.current_settings() != before;
                save(&mut st.settings, changed);
                present(&win, &st);
            }
            refresh(&win, &state);
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_settings_clear_cache(move || {
            let Some(win) = win.upgrade() else { return };
            let mut st = state.borrow_mut();
            if st.settings.clear_rx.is_some() || cache_off() {
                return;
            }
            let Some(db) = fastcull_core::cache::default_cache_file() else {
                return;
            };
            let (tx, rx) = std::sync::mpsc::channel();
            st.settings.clear_rx = Some(rx);
            st.settings.cache_readout = "Clearing…".to_string();
            present(&win, &st);
            // The clear has begun (test-harness.md; QE 2026-10-01, D24): the
            // click-away matrix orders a field's commit before it. What the
            // row SHOWS meanwhile, and whether Clear is offered, are the
            // row's and the button's own marks (main.slint; QE 2026-10-02,
            // round 5) — this mark's order alone proved neither.
            trace_mark("settings cache clearing");
            // OFF the UI thread (settings.md: the VACUUM rewrites the file,
            // seconds on a big cache). Its own connection, through which the
            // clear runs — never an unlink (catalog-cache.md's lock rule).
            // The window is woken through a callback, the `kitchen-ready`
            // pattern: the state is not `Send`, the receiver in it is the
            // way the outcome gets back.
            //
            // A NAMED thread, and the worker says on the trace where it ran
            // (`settings cache clear ran on settings-clear`): a clear moved
            // back onto the UI thread reads `… ran on main`, which a driven
            // run sees (QE 2026-10-02, round 5). The name proves the worker,
            // not that the UI thread never waits for it — a `join()` right
            // after the spawn would block the UI and still report this name
            // — so "never blocks" stays review-verified (settings.md AC12).
            let weak = win.as_weak();
            let spawned = std::thread::Builder::new()
                .name("settings-clear".into())
                .spawn(move || {
                    let before = fastcull_core::cache::size_on_disk(&db);
                    let error = fastcull_core::cache::PreviewCache::open(&db)
                        .and_then(|mut cache| cache.clear())
                        .err()
                        .map(|e| e.to_string());
                    let after = fastcull_core::cache::size_on_disk(&db);
                    // From the worker itself, before the outcome is sent: so
                    // it lands between `settings cache clearing` and
                    // `settings cache cleared`. `trace_mark` is safe off the
                    // UI thread — one `eprintln!`, a whole line, and a mutex.
                    trace_mark(&format!(
                        "settings cache clear ran on {}",
                        std::thread::current().name().unwrap_or("<unnamed>")
                    ));
                    tx.send(CacheCleared {
                        before,
                        after,
                        error,
                    })
                    .ok();
                    slint::invoke_from_event_loop(move || {
                        if let Some(win) = weak.upgrade() {
                            win.invoke_settings_cache_cleared();
                        }
                    })
                    .ok();
                });
            if let Err(e) = spawned {
                // No worker, so no clear: the row says so as a failed clear
                // does, and Clear is offered again (settings.md: a clear
                // that fails says so in the row).
                st.settings.clear_rx = None;
                st.settings.cache_readout = cache_readout(Some(&e.to_string()));
                present(&win, &st);
            }
        });
    }
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_settings_cache_cleared(move || {
            let Some(win) = win.upgrade() else { return };
            let mut st = state.borrow_mut();
            let Some(outcome) = st
                .settings
                .clear_rx
                .as_ref()
                .and_then(|rx| rx.try_recv().ok())
            else {
                return;
            };
            st.settings.clear_rx = None;
            trace_mark(&format!(
                "settings cache cleared {} -> {}",
                outcome.before, outcome.after
            ));
            // Re-measured from disk, never a claimed "0 B" (settings.md).
            st.settings.cache_readout = cache_readout(outcome.error.as_deref());
            present(&win, &st);
        });
    }
}

impl AppState {
    /// The settings in force — shorthand for the dialog's group.
    fn current_settings(&self) -> &Settings {
        self.settings.current()
    }
}

/// The settings that apply AT ONCE (settings.md): the wash, through the
/// app's one clamped write site. Auto-advance needs no push — the mark
/// handler reads it at every mark — and the Performance three wait for the
/// next folder open, which reads them from the state.
pub(crate) fn apply_instant(win: &MainWindow, s: &Settings) {
    win.set_selection_wash_opacity(clamp_wash_opacity(s.selection_wash as f32 / 100.0));
}

/// The read's trace mark (test-harness.md), at startup and at every open.
pub(crate) fn trace_read(st: &SettingsState) {
    match (&st.loaded.path, st.loaded.error_first_line()) {
        (Some(path), Some(error)) => trace_mark(&format!(
            "settings: {} could not be read: {error}",
            path.display()
        )),
        (Some(path), None) if path.is_file() => {
            trace_mark(&format!("settings loaded from {}", path.display()))
        }
        _ => trace_mark("settings: no file (defaults in force)"),
    }
}

/// Write the settings in force — on every commit and Reset, and never
/// otherwise; a missing file stays missing until something CHANGES
/// (settings.md, "Writing"). On the UI thread, by ADR 0005: ~1 KB on an
/// explicit user action inside a modal.
fn save(st: &mut SettingsState, changed: bool) {
    let Some(path) = st.loaded.path.clone() else {
        trace_mark("settings not written: FASTCULL_NO_CONFIG is set");
        return;
    };
    if !changed && !path.exists() {
        return;
    }
    // Core decides from the file as it is now — moved aside only if it does
    // not parse at this moment, whatever the last read said (settings.md,
    // "Writing").
    let outcome = settings::write(&path, &st.loaded.settings);
    record_write(st, &path, outcome);
}

/// What a write's outcome leaves in the dialog's state: the notice, the
/// status line and the next open read it.
fn record_write(
    st: &mut SettingsState,
    path: &std::path::Path,
    outcome: Result<Option<std::path::PathBuf>, settings::WriteError>,
) {
    match outcome {
        Ok(aside) => {
            st.write_error = None;
            if let Some(aside) = aside {
                trace_mark(&format!("settings moved aside {}", aside.display()));
                st.moved_aside = Some(aside);
            }
            // What is on disk now is what is in memory: the read error is
            // answered (the file it named was moved aside).
            st.loaded.error = None;
            trace_mark(&format!("settings written {}", path.display()));
        }
        Err(e) => {
            // The commit stays in force in memory, and says so (settings.md).
            eprintln!("fastcull: could not write {}: {e}", path.display());
            trace_mark(&format!("settings not written: {e}"));
            // The broken file may already be gone from its name — moved
            // aside before the write failed — and the session must still
            // say where it went; the read error is answered by that move,
            // so the next write starts a fresh file instead of looking for
            // one to move (senior-developer review F4 of brief 008).
            if let Some(aside) = e.moved_aside() {
                trace_mark(&format!("settings moved aside {}", aside.display()));
                st.moved_aside = Some(aside.to_path_buf());
                st.loaded.error = None;
            }
            // Kept whole: its KIND decides how the lines name an earlier
            // aside (`write_error_aside`), its Display is the notice's words.
            st.write_error = Some(e);
        }
    }
}

/// Is the thumbnail cache switched off for this run?
fn cache_off() -> bool {
    std::env::var_os("FASTCULL_NO_CACHE").is_some()
}

/// The Thumbnail cache row (settings.md): its size as `du` would show it —
/// the database plus `-wal` and `-shm` — and its real path; `off` under
/// FASTCULL_NO_CACHE; a failed clear says so.
fn cache_readout(clear_error: Option<&str>) -> String {
    if cache_off() {
        return "Thumbnail cache: off (FASTCULL_NO_CACHE is set)".to_string();
    }
    let Some(db) = fastcull_core::cache::default_cache_file() else {
        return "Thumbnail cache: no cache directory on this system".to_string();
    };
    let size = human_bytes(fastcull_core::cache::size_on_disk(&db));
    let place = home_relative(&db);
    match clear_error {
        Some(e) => format!("Thumbnail cache: could not be cleared ({e}) — {size} in {place}"),
        None => format!("Thumbnail cache: {size} in {place}"),
    }
}

/// A path under the home directory as `~/…`, the way the spec's readout
/// writes it; anything else, and every path on Windows, as it is.
fn home_relative(path: &std::path::Path) -> String {
    #[cfg(unix)]
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

/// The loupe memory hint (settings.md, "Performance › Loupe memory"): the
/// bytes in force and the machine's RAM through the byte formatter, how
/// many decoded A1 frames that is — and, when the figure was clamped or
/// unknowable, which way.
pub(crate) fn loupe_hint(s: &Settings, total_ram: Option<u64>) -> String {
    let (bytes, source) = s.loupe_memory_bytes(total_ram);
    let n = settings::a1_frames(bytes);
    let frames = format!("≈ {n} A1 frame{}", if n == 1 { "" } else { "s" });
    let in_force = human_bytes(bytes);
    let of_total = total_ram
        .map(|t| format!(" of {}", human_bytes(t)))
        .unwrap_or_default();
    match source {
        MemorySource::AsGiven => format!("= {in_force}{of_total} {frames}"),
        MemorySource::FlooredAt200MB => format!("= {in_force} (the floor){of_total} {frames}"),
        MemorySource::CappedAtTotal => {
            format!("= {in_force} (all of this machine's RAM) {frames}")
        }
        MemorySource::PercentUnknownRamDefault => {
            format!("= {in_force} (total RAM unknown — the default) {frames}")
        }
    }
}

/// The notice line (settings.md, "The card"): empty unless the file is not
/// saved, could not be written, was moved aside, or could not be read.
fn notice(st: &SettingsState) -> String {
    let file = settings::FILE_NAME;
    if st.loaded.path.is_none() {
        return if std::env::var_os(settings::NO_CONFIG_VAR).is_some() {
            format!("Not saved: {} is set", settings::NO_CONFIG_VAR)
        } else {
            "Not saved: this system has no config directory".to_string()
        };
    }
    // The write error first: it is always the NEWEST event, because no open
    // re-reads the file while one stands (settings.md, "Reading").
    if let Some(e) = &st.write_error {
        return format!("Could not write {file}: {e}{}", write_error_aside(st));
    }
    // A read error BEFORE `rewritten`: a read error standing after a move is
    // newer than the move — the open re-read a fresh file that a hand edit
    // broke since — and it is the only thing that says why every setting
    // just went back to its default; the move it follows is still named
    // (settings.md, "Writing"; QE 2026-10-01, D26 — the old order kept
    // `rewritten` on screen while the defaults silently took over).
    if let Some(e) = &st.loaded.error {
        return format!(
            "{file} could not be read (defaults in force): {}{}",
            whole_error(e),
            earlier_aside(st)
        );
    }
    if let Some(aside) = &st.moved_aside {
        return format!(
            "{file} rewritten — the file that would not read is {}",
            file_name(aside)
        );
    }
    String::new()
}

/// The status line's piece of the same story (ui-grid.md's status bar;
/// settings.md, "Reading" and "Writing"), in [`notice`]'s order, newest
/// event first: a standing write error — no open re-reads the file while
/// one stands — naming where a broken file went when one was moved aside;
/// then a read failure, which says "defaults in force" only while it IS the
/// newest event, the move it follows still named (a read failure AFTER a
/// move wins over `rewritten`); then where the file went, for the session.
pub(crate) fn status_note(st: &SettingsState) -> String {
    let file = settings::FILE_NAME;
    if st.write_error.is_some() {
        // A commit is in force in memory and could not be saved: after a
        // failed read too, where "(defaults in force)" would be false (brief
        // 008 D41; QE 2026-10-02, D43). "Rewritten" never, while it is not
        // true (senior-developer review F4 of brief 008).
        format!(" — ⚠ {file} could not be written{}", write_error_aside(st))
    } else if st.loaded.error.is_some() {
        format!(
            " — ⚠ {file} could not be read (defaults in force){}",
            earlier_aside(st)
        )
    } else if let Some(aside) = &st.moved_aside {
        format!(
            " — {file} rewritten — the file that would not read is {}",
            file_name(aside)
        )
    } else {
        String::new()
    }
}

/// What follows a standing write error, on both lines alike: where a
/// broken file was moved aside, named for the rest of the session, a failed
/// write after the move included (settings.md, "Writing"; senior-developer
/// review F4 of brief 008) — unless settings.toml ITSELF is now the file
/// that would not read, which is so in two states: a read error NEWER than
/// that move still stands (this write moved nothing — a move that happens
/// answers the read error, [`record_write`]; QE 2026-10-02, round 5, D46),
/// or this write failed AT THE MOVE-ASIDE with no read error standing — the
/// fresh file broken by hand while the dialog was open, so no open re-read
/// it, and the dir unwritable (brief 010 R6; the developer's finding of
/// 2026-10-02 in issue #100). Then the aside is named as the earlier one,
/// as the read error's own lines name it: in both states the notice says,
/// right before, that "the file that would not read could not be moved
/// aside", and both lines used to call the earlier aside that very file.
/// The second state is read off the error's KIND, `WriteError::MoveAside`,
/// never off its words.
fn write_error_aside(st: &SettingsState) -> String {
    let failed_at_the_move = matches!(st.write_error, Some(settings::WriteError::MoveAside(_)));
    if st.loaded.error.is_some() || failed_at_the_move {
        return earlier_aside(st);
    }
    st.moved_aside
        .as_deref()
        .map(|aside| format!(" — the file that would not read is {}", file_name(aside)))
        .unwrap_or_default()
}

/// ` — the earlier one is settings.toml.broken` when a file was moved aside
/// before the read error that is now on screen, else nothing.
fn earlier_aside(st: &SettingsState) -> String {
    st.moved_aside
        .as_deref()
        .map(|aside| format!(" — the earlier one is {}", file_name(aside)))
        .unwrap_or_default()
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// A parse error, whole, on the one notice line: TOML's message is a header,
/// a quoted snippet of the file with a caret under the column, and the
/// explanation. The snippet's lines (`  |`, `1 | …`) only line up in a
/// monospaced terminal; every other line is kept, joined with dashes.
fn whole_error(error: &str) -> String {
    error
        .lines()
        .map(str::trim)
        .filter(|l| {
            let snippet = l.starts_with('|')
                || l.split_once(" |")
                    .is_some_and(|(n, _)| n.bytes().all(|b| b.is_ascii_digit()));
            !l.is_empty() && !snippet
        })
        .collect::<Vec<_>>()
        .join(" — ")
}

/// Write every dialog property from the settings in force, the
/// environment and the machine — ONE place, so no field can show a value
/// the model does not hold.
pub(crate) fn present(win: &MainWindow, st: &AppState) {
    let set = &st.settings;
    let s = set.current();
    win.set_settings_auto_advance(s.auto_advance);
    win.set_settings_wash(s.text_of(Key::SelectionWash).into());
    win.set_settings_loupe_memory(s.text_of(Key::LoupeMemory).into());
    win.set_settings_loupe_hint(loupe_hint(s, set.total_ram).into());
    win.set_settings_cache_cap(s.text_of(Key::CacheCap).into());
    // The read workers: the environment wins and shows read-only with its
    // own value and note (settings.md, "Environment precedence").
    let (adaptive, limit, locked) = match settings::resolve_max_readers_from_env(s.max_readers) {
        Readers::Adaptive => (true, String::new(), false),
        Readers::Limit(n) => (false, n.to_string(), false),
        Readers::Environment(n) => (false, n.to_string(), true),
    };
    win.set_settings_readers_adaptive(adaptive);
    win.set_settings_readers_limit(limit.into());
    win.set_settings_readers_locked(locked);
    win.set_settings_readers_env_note(
        if locked {
            settings::environment_note(settings::MAX_READERS_VAR)
        } else {
            String::new()
        }
        .into(),
    );
    win.set_settings_cache_readout(set.cache_readout.clone().into());
    win.set_settings_cache_clear_enabled(set.clear_rx.is_none() && !cache_off());
    win.set_settings_notice(notice(set).into());
}

/// The settings block of a `dump.` line (test-harness.md's field order),
/// appended by the harness after `focusowner=`.
pub(crate) fn dump_fields(win: &MainWindow, st: &AppState) -> String {
    let set = &st.settings;
    let s = set.current();
    let readers = match settings::resolve_max_readers_from_env(s.max_readers) {
        Readers::Adaptive => "adaptive".to_string(),
        Readers::Limit(n) => format!("limit:{n}"),
        Readers::Environment(n) => format!("env:{n}"),
    };
    let readers_env = std::env::var_os(settings::MAX_READERS_VAR)
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file = set
        .loaded
        .path
        .as_ref()
        .map_or_else(|| "none".to_string(), |p| p.display().to_string());
    format!(
        "settings={} settingstab={} settingsfile={:?} settingsnote={:?} autoadvance={} \
         wash={} washprop={:.3} loupemem={} loupehint={:?} cachecap={} readers={} \
         readersenv={:?} cachereadout={:?}",
        win.get_settings_visible(),
        win.get_settings_tab(),
        file,
        win.get_settings_notice().as_str(),
        s.auto_advance,
        s.selection_wash,
        win.get_selection_wash_opacity(),
        s.loupe_memory_bytes(set.total_ram).0,
        win.get_settings_loupe_hint().as_str(),
        s.cache_cap_bytes(),
        readers,
        readers_env,
        win.get_settings_cache_readout().as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastcull_core::settings::MemorySpec;

    const GIB: u64 = 1 << 30;

    /// The hint reads the way settings.md writes it, in each of its four
    /// shapes.
    #[test]
    fn the_loupe_hint_reads_as_the_spec_writes_it() {
        let with = |spec: MemorySpec| Settings {
            loupe_memory: spec,
            ..Settings::default()
        };
        let total = Some(31 * GIB + GIB / 10);
        assert_eq!(
            loupe_hint(&with(MemorySpec::Percent(40)), total),
            "= 12.4 GB of 31.1 GB ≈ 89 A1 frames"
        );
        assert_eq!(
            loupe_hint(&with(MemorySpec::Gb(0.01)), total),
            "= 200.0 MB (the floor) of 31.1 GB ≈ 1 A1 frame"
        );
        assert_eq!(
            loupe_hint(&with(MemorySpec::Gb(64.0)), total),
            "= 31.1 GB (all of this machine's RAM) ≈ 223 A1 frames"
        );
        assert_eq!(
            loupe_hint(&with(MemorySpec::Percent(40)), None),
            "= 2.0 GB (total RAM unknown — the default) ≈ 14 A1 frames"
        );
        assert_eq!(
            loupe_hint(&with(MemorySpec::Gb(0.5)), total),
            "= 512.0 MB of 31.1 GB ≈ 3 A1 frames"
        );
    }

    /// A write that moved the broken file aside and THEN failed (a full
    /// disk, a permission flipped mid-session) still leaves the session
    /// naming where the user's file went — on the status line and the
    /// notice, neither claiming a rewrite — and answers the read error, so
    /// the next write starts a fresh file; the write that then succeeds
    /// says "rewritten". The core half — `write` attaching the path to its
    /// error — is review-verified: a rename that succeeds followed by a
    /// write that fails in the same directory cannot be provoked
    /// deterministically (senior-developer review F4 of brief 008).
    ///
    /// Mutant (2026-10-01): the `e.moved_aside()` arm taken out of
    /// `record_write` → `moved_aside` stays `None` and this goes red.
    #[test]
    fn a_failed_write_after_the_move_aside_still_names_where_the_file_went() {
        let path = std::path::PathBuf::from("/nowhere/fastcull/settings.toml");
        let aside = path.with_file_name("settings.toml.broken");
        let mut st = SettingsState::new(
            settings::Loaded {
                settings: Settings::default(),
                error: Some("TOML parse error at line 1, column 9".to_string()),
                path: Some(path.clone()),
            },
            None,
        );
        record_write(
            &mut st,
            &path,
            Err(settings::WriteError::Io {
                source: std::io::Error::other("No space left on device"),
                moved_aside: Some(aside.clone()),
            }),
        );
        assert_eq!(st.moved_aside.as_deref(), Some(aside.as_path()));
        assert_eq!(st.loaded.error, None, "the move answered the read error");
        // The error is kept whole since brief 010 (its kind decides the
        // wording); its words are its Display.
        assert_eq!(
            st.write_error.as_ref().map(ToString::to_string).as_deref(),
            Some("No space left on device")
        );
        assert_eq!(
            status_note(&st),
            " — ⚠ settings.toml could not be written — the file that would not read is \
             settings.toml.broken"
        );
        assert_eq!(
            notice(&st),
            "Could not write settings.toml: No space left on device — the file that would \
             not read is settings.toml.broken"
        );
        // The next write succeeds: now it is a rewrite, and still named.
        record_write(&mut st, &path, Ok(None));
        assert!(st.write_error.is_none());
        assert_eq!(
            status_note(&st),
            " — settings.toml rewritten — the file that would not read is settings.toml.broken"
        );
        assert_eq!(
            notice(&st),
            "settings.toml rewritten — the file that would not read is settings.toml.broken"
        );
    }

    /// A read error NEWER than the move-aside wins over `rewritten`
    /// (settings.md, "Writing"; QE 2026-10-01, D26). The first write after a
    /// failed read moved the file aside and wrote a fresh one; a hand edit
    /// then broke the fresh file, and the open's re-read failed. Both lines
    /// must say THAT — every setting just went back to its default, and
    /// this is the only place that says why — while still naming the file
    /// moved aside earlier; the next write moves the new one aside too and
    /// both lines say `rewritten` again, naming it.
    ///
    /// RED on 6f20679, the head before the fix: `notice` and `status_note`
    /// checked `moved_aside` before `loaded.error`, so both kept reading
    /// `settings.toml rewritten — the file that would not read is
    /// settings.toml.broken` while the defaults silently took over. When
    /// this fails that way it is that defect; do not quiet it.
    ///
    /// Mutant (2026-10-01): the old arm order restored in either function →
    /// red at step 3.
    #[test]
    fn a_read_error_after_the_move_aside_is_shown_not_masked_by_rewritten() {
        let path = std::path::PathBuf::from("/nowhere/fastcull/settings.toml");
        // 1. The first write after a failed read: the broken file moved
        //    aside, a fresh one written.
        let mut st = SettingsState::new(
            settings::Loaded {
                settings: Settings::default(),
                error: None,
                path: Some(path.clone()),
            },
            None,
        );
        record_write(
            &mut st,
            &path,
            Ok(Some(path.with_file_name("settings.toml.broken"))),
        );
        // 2. A hand edit broke the fresh file, and the open re-read it.
        st.loaded = settings::Loaded {
            settings: Settings::default(),
            error: Some(
                "TOML parse error at line 2, column 16\n  |\n2 | auto_advance = maybe\n  \
                 |                ^\ninvalid string\nexpected `\"`, `'`"
                    .to_string(),
            ),
            path: Some(path.clone()),
        };
        // 3. The newer read error is what both lines say, the earlier
        //    move still named.
        assert_eq!(
            status_note(&st),
            " — ⚠ settings.toml could not be read (defaults in force) — the earlier one is \
             settings.toml.broken"
        );
        assert_eq!(
            notice(&st),
            "settings.toml could not be read (defaults in force): TOML parse error at line 2, \
             column 16 — invalid string — expected `\"`, `'` — the earlier one is \
             settings.toml.broken"
        );
        // 4. The next write moves the new broken file aside too: a rewrite
        //    again, naming where THIS one went.
        record_write(
            &mut st,
            &path,
            Ok(Some(path.with_file_name("settings.toml.broken.1"))),
        );
        assert_eq!(st.loaded.error, None, "the move answered the read error");
        assert_eq!(
            status_note(&st),
            " — settings.toml rewritten — the file that would not read is \
             settings.toml.broken.1"
        );
        assert_eq!(
            notice(&st),
            "settings.toml rewritten — the file that would not read is settings.toml.broken.1"
        );
    }

    /// A standing write error is the NEWEST event — no open re-reads the file
    /// while one stands — so it wins over a read error on the status line,
    /// as it already did on the notice (settings.md, "Writing"; brief 008
    /// D41). The file would not read, a commit is in force, and its write
    /// failed at the move-aside (a read-only config dir): the status line
    /// says the file could not be written, never "(defaults in force)" — the
    /// user's value IS in force; the notice still names the failed move;
    /// the write that then succeeds moves the file aside and both lines say
    /// `rewritten`.
    ///
    /// RED on f80e2f0, the head before the fix (QE 2026-10-02, D43):
    /// `status_note` asked about the read error first, so the status line
    /// read `⚠ settings.toml could not be read (defaults in force)` beside a
    /// committed wash of 20 in force. When this fails that way it is that
    /// defect; do not quiet it.
    ///
    /// Mutant (2026-10-02): the old order — the write-error arm yielding to
    /// a standing read error (`&& st.loaded.error.is_none()`) → the status
    /// line reads `(defaults in force)` again — red.
    #[test]
    fn a_write_error_wins_over_a_read_error_on_the_status_line() {
        let path = std::path::PathBuf::from("/nowhere/fastcull/settings.toml");
        let mut st = SettingsState::new(
            settings::Loaded {
                settings: Settings::default(),
                error: Some("TOML parse error at line 1, column 4".to_string()),
                path: Some(path.clone()),
            },
            None,
        );
        // The read failed: the defaults are in force, and the line says so.
        assert_eq!(
            status_note(&st),
            " — ⚠ settings.toml could not be read (defaults in force)"
        );
        // A commit, written to a dir the broken file cannot leave.
        st.loaded.settings.selection_wash = 20;
        record_write(
            &mut st,
            &path,
            Err(settings::WriteError::MoveAside(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied,
            ))),
        );
        assert!(
            st.loaded.error.is_some(),
            "nothing was moved, so the read error still stands"
        );
        assert_eq!(st.moved_aside, None);
        let status = status_note(&st);
        assert_eq!(
            status, " — ⚠ settings.toml could not be written",
            "the status line does not say the newest thing — the write failed"
        );
        assert!(
            !status.contains("defaults in force"),
            "the status line claims the defaults are in force beside a commit in force"
        );
        assert!(
            notice(&st).starts_with(
                "Could not write settings.toml: the file that would not read could not be \
                 moved aside: "
            ),
            "the notice no longer names the failed move: {}",
            notice(&st)
        );
        // The dir is writable again: the next write moves the file aside.
        record_write(
            &mut st,
            &path,
            Ok(Some(path.with_file_name("settings.toml.broken"))),
        );
        assert_eq!(
            status_note(&st),
            " — settings.toml rewritten — the file that would not read is settings.toml.broken"
        );
    }

    /// A write that fails after a read error NEWER than the move-aside names
    /// the aside as the EARLIER one (settings.md, "Writing"; QE 2026-10-02,
    /// round 5, D46). The launch file would not read and the first write
    /// moved it aside; a hand edit broke the fresh file and the open re-read
    /// it; then the config dir turned read-only and a commit's write could
    /// not move the new broken file aside. settings.toml itself is now the
    /// file that would not read — the notice says exactly that file could
    /// not be moved — so both lines name `settings.toml.broken` as the
    /// earlier one, as the read error's own lines do; the write that then
    /// succeeds moves the newer file aside and both say `rewritten` again,
    /// naming it.
    ///
    /// RED on 7559db0, the head before the fix: both lines called the
    /// earlier aside `the file that would not read`, the notice beside its
    /// own `the file that would not read could not be moved aside` — a line
    /// that contradicted itself. When this fails that way it is that
    /// defect; do not quiet it.
    ///
    /// Mutant (2026-10-02): the write error's suffix without its newer-read-
    /// error arm — 7559db0's wording — → red at step 3.
    #[test]
    fn a_failed_write_after_a_newer_read_error_names_the_earlier_aside_as_the_earlier_one() {
        let path = std::path::PathBuf::from("/nowhere/fastcull/settings.toml");
        // 1. The launch read failed; the first write moved the file aside
        //    and wrote a fresh one.
        let mut st = SettingsState::new(
            settings::Loaded {
                settings: Settings::default(),
                error: Some("TOML parse error at line 1, column 4".to_string()),
                path: Some(path.clone()),
            },
            None,
        );
        record_write(
            &mut st,
            &path,
            Ok(Some(path.with_file_name("settings.toml.broken"))),
        );
        // 2. A hand edit broke the fresh file, and the open re-read it.
        st.loaded = settings::Loaded {
            settings: Settings::default(),
            error: Some("TOML parse error at line 2, column 16".to_string()),
            path: Some(path.clone()),
        };
        // 3. A commit, written to a config dir that turned read-only: the
        //    newer broken file cannot be moved aside.
        st.loaded.settings.selection_wash = 15;
        record_write(
            &mut st,
            &path,
            Err(settings::WriteError::MoveAside(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied,
            ))),
        );
        assert!(
            st.loaded.error.is_some(),
            "the premise: this write moved nothing, so the newer read error still stands"
        );
        assert_eq!(
            status_note(&st),
            " — ⚠ settings.toml could not be written — the earlier one is settings.toml.broken",
            "the status line calls the earlier aside the file that would not read, while \
             settings.toml itself is that file now"
        );
        let line = notice(&st);
        assert!(
            line.starts_with(
                "Could not write settings.toml: the file that would not read could not be \
                 moved aside: "
            ) && line.ends_with(" — the earlier one is settings.toml.broken"),
            "the notice does not name the failed move and then the earlier aside as the \
             earlier one: {line}"
        );
        // 4. The dir is writable again: the next write moves the newer file
        //    aside too, and both lines say `rewritten`, naming it.
        record_write(
            &mut st,
            &path,
            Ok(Some(path.with_file_name("settings.toml.broken.1"))),
        );
        assert_eq!(
            status_note(&st),
            " — settings.toml rewritten — the file that would not read is \
             settings.toml.broken.1"
        );
        assert_eq!(
            notice(&st),
            "settings.toml rewritten — the file that would not read is settings.toml.broken.1"
        );
    }

    /// A write that fails AT THE MOVE-ASIDE with NO read error standing
    /// names the earlier aside as the earlier one too (settings.md,
    /// "Writing"; brief 010 R6 — the developer's 2026-10-02 finding in issue
    /// #100). The launch file would not read and the first write moved it
    /// aside; a hand edit then broke the fresh file while the dialog was
    /// OPEN, so no open re-read it and no read error stands; the config dir
    /// turned read-only and a commit's write could not move the newly broken
    /// file aside. settings.toml is the file that would not read here too —
    /// the notice says that file could not be moved — so both lines call
    /// `settings.toml.broken` the earlier one. The bridge decides by the
    /// write error's KIND, `WriteError::MoveAside`, never by its text.
    ///
    /// RED on f771f6f, the bridge before the fix: both lines read `— the
    /// file that would not read is settings.toml.broken` beside the notice's
    /// own `the file that would not read could not be moved aside`. When
    /// this fails that way it is that defect; do not quiet it.
    ///
    /// Mutant (2026-10-03): the suffix decided by `moved_aside` alone — the
    /// `MoveAside` term taken out of `write_error_aside`, the old wording →
    /// red at step 3.
    #[test]
    fn a_failed_move_aside_with_no_read_error_standing_names_the_earlier_aside_as_the_earlier_one()
    {
        let path = std::path::PathBuf::from("/nowhere/fastcull/settings.toml");
        // 1. The launch read failed; the first write moved the file aside
        //    and wrote a fresh one, answering the read error.
        let mut st = SettingsState::new(
            settings::Loaded {
                settings: Settings::default(),
                error: Some("TOML parse error at line 1, column 4".to_string()),
                path: Some(path.clone()),
            },
            None,
        );
        record_write(
            &mut st,
            &path,
            Ok(Some(path.with_file_name("settings.toml.broken"))),
        );
        // 2. A hand edit broke the fresh file while the dialog stayed open:
        //    nothing re-read it, so no read error stands.
        assert_eq!(st.loaded.error, None, "the premise: no read error stands");
        // 3. A commit, written to a config dir that turned read-only: the
        //    newly broken file cannot be moved aside.
        st.loaded.settings.selection_wash = 15;
        record_write(
            &mut st,
            &path,
            Err(settings::WriteError::MoveAside(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied,
            ))),
        );
        assert_eq!(
            st.loaded.error, None,
            "the premise: still no read error after the failed write"
        );
        assert_eq!(
            status_note(&st),
            " — ⚠ settings.toml could not be written — the earlier one is settings.toml.broken",
            "the status line calls the earlier aside the file that would not read, while \
             settings.toml itself is that file now"
        );
        let line = notice(&st);
        assert!(
            line.starts_with(
                "Could not write settings.toml: the file that would not read could not be \
                 moved aside: "
            ) && line.ends_with(" — the earlier one is settings.toml.broken"),
            "the notice does not name the failed move and then the earlier aside as the \
             earlier one: {line}"
        );
        // 4. The dir is writable again: the next write moves the newer file
        //    aside, and both lines say `rewritten`, naming it.
        record_write(
            &mut st,
            &path,
            Ok(Some(path.with_file_name("settings.toml.broken.1"))),
        );
        assert_eq!(
            status_note(&st),
            " — settings.toml rewritten — the file that would not read is \
             settings.toml.broken.1"
        );
        assert_eq!(
            notice(&st),
            "settings.toml rewritten — the file that would not read is settings.toml.broken.1"
        );
    }

    /// A PLAIN failed write — no read error standing, no file moved aside,
    /// a full disk or a read-only file — says so on the status line, and
    /// says nothing else: ` — ⚠ settings.toml could not be written`, never
    /// `rewritten`, never `(defaults in force)` (settings.md, "Writing";
    /// brief 010 R2, issue #100's first NOW guard). The notice carries the
    /// error's own words. The driven half is
    /// `a_failed_settings_write_keeps_the_commit_and_the_next_open_does_not_reread`,
    /// whose dumps read the same line off the window.
    ///
    /// Mutant (2026-10-03): the write-error arm of `status_note` taken out
    /// (`if false && …`) → the status line is empty — red.
    #[test]
    fn a_plain_failed_write_says_so_on_the_status_line() {
        let path = std::path::PathBuf::from("/nowhere/fastcull/settings.toml");
        let mut st = SettingsState::new(
            settings::Loaded {
                settings: Settings::default(),
                error: None,
                path: Some(path.clone()),
            },
            None,
        );
        st.loaded.settings.selection_wash = 15;
        record_write(
            &mut st,
            &path,
            Err(settings::WriteError::Io {
                source: std::io::Error::other("Read-only file system"),
                moved_aside: None,
            }),
        );
        assert_eq!(st.moved_aside, None, "the premise: nothing was moved aside");
        let status = status_note(&st);
        assert_eq!(
            status, " — ⚠ settings.toml could not be written",
            "a plain failed write does not say so on the status line"
        );
        assert!(
            !status.contains("rewritten") && !status.contains("defaults in force"),
            "the status line claims a rewrite or the defaults beside a commit in force: \
             {status}"
        );
        assert_eq!(
            notice(&st),
            "Could not write settings.toml: Read-only file system"
        );
    }

    /// A TOML parse error keeps every line of its words and drops only the
    /// snippet drawn for a monospaced terminal.
    #[test]
    fn a_parse_error_reads_whole_on_one_line() {
        let error = "TOML parse error at line 1, column 9\n  |\n1 | [general\n  |         ^\n\
                     invalid table header\nexpected `.`, `]`";
        assert_eq!(
            whole_error(error),
            "TOML parse error at line 1, column 9 — invalid table header — expected `.`, `]`"
        );
    }
}
