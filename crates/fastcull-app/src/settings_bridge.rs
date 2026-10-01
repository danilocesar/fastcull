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
                    }
                }
                trace_read(&st.settings);
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
                    // A refused value changes nothing; `present` below puts
                    // the value in force back in the field.
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
            // OFF the UI thread (settings.md: the VACUUM rewrites the file,
            // seconds on a big cache). Its own connection, through which the
            // clear runs — never an unlink (catalog-cache.md's lock rule).
            // The window is woken through a callback, the `kitchen-ready`
            // pattern: the state is not `Send`, the receiver in it is the
            // way the outcome gets back.
            let weak = win.as_weak();
            std::thread::spawn(move || {
                let before = fastcull_core::cache::size_on_disk(&db);
                let error = fastcull_core::cache::PreviewCache::open(&db)
                    .and_then(|mut cache| cache.clear())
                    .err()
                    .map(|e| e.to_string());
                let after = fastcull_core::cache::size_on_disk(&db);
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
    let broken = st.loaded.error.is_some();
    match settings::write(&path, &st.loaded.settings, broken) {
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
            st.write_error = Some(e.to_string());
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
    if let Some(e) = &st.write_error {
        return format!("Could not write {file}: {e}");
    }
    if let Some(aside) = &st.moved_aside {
        return format!(
            "{file} rewritten — the file that would not read is {}",
            file_name(aside)
        );
    }
    if let Some(e) = &st.loaded.error {
        return format!(
            "{file} could not be read (defaults in force): {}",
            whole_error(e)
        );
    }
    String::new()
}

/// The status line's piece of the same story (ui-grid.md's status bar;
/// settings.md, "Reading" and "Writing"): a read failure until the file
/// reads again or is moved aside, then where it went, for the session.
pub(crate) fn status_note(st: &SettingsState) -> String {
    let file = settings::FILE_NAME;
    if let Some(aside) = &st.moved_aside {
        format!(
            " — {file} rewritten — the file that would not read is {}",
            file_name(aside)
        )
    } else if st.loaded.error.is_some() {
        format!(" — ⚠ {file} could not be read (defaults in force)")
    } else {
        String::new()
    }
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
