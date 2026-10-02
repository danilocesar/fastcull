//! The environment never reaches the settings file (settings.md, "Writing";
//! the user, 2026-10-02, brief 008 D42: "make sure that environment
//! variables don't rewrite settings").
//!
//! A test binary of its own, holding this one test, because it sets
//! FASTCULL_MAX_READERS in its OWN process: the precedence is read from the
//! process's environment (`settings::resolve_max_readers_from_env`, the call
//! both binaries make), so a regression that let the variable into the
//! model — at the read — or into the writer would read it there, and only
//! there. Every other core test injects the environment instead and never
//! touches the process's own, which the threads of one test binary share.

use fastcull_core::settings::{self, Key, Readers};

/// `FASTCULL_MAX_READERS=3` over a file that says `max_readers = 7`: the
/// variable is what is in force, the file keeps its 7 — read, written back
/// after a commit of ANOTHER key, and read again — and the variable is
/// still what is in force afterwards. Green on 2506ef6, before any line of
/// the commit that adds it — a guard for the user's condition, not a bug
/// fix.
///
/// Mutants (2026-10-02): the writer emitting the value IN FORCE for
/// `max_readers` (`resolve_max_readers_from_env(..)` in `toml_value`) → the
/// file reads `max_readers = 3` — red; the read applying the variable to
/// the model (in `from_document`) → the model holds 3, not the file's 7 —
/// red.
#[test]
fn the_environment_never_reaches_the_settings_file() {
    // This binary's only test, so no other thread reads the environment
    // while it changes.
    std::env::set_var(settings::MAX_READERS_VAR, "3");
    let dir = std::env::temp_dir().join(format!("fastcull-settings-env-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(settings::FILE_NAME);
    std::fs::write(&path, "[performance]\nmax_readers = 7\n").unwrap();

    let loaded = settings::load(&path);
    assert_eq!(loaded.error, None);
    assert_eq!(
        loaded.settings.max_readers, 7,
        "the read put something other than the file's value in the model"
    );
    assert_eq!(
        settings::resolve_max_readers_from_env(loaded.settings.max_readers),
        Readers::Environment(3),
        "the premise: the variable governs what is in force"
    );

    // A commit of another key, saved as the dialog saves it.
    let mut s = loaded.settings;
    s.set_from_text(Key::SelectionWash, "15").unwrap();
    settings::write(&path, &s).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.lines().any(|line| line.trim() == "max_readers = 7"),
        "the save did not write the file's own `max_readers = 7` back — the \
         environment reached the file: {text:?}"
    );
    assert!(
        !text.lines().any(|line| line.trim() == "max_readers = 3"),
        "the environment's value was written to the file: {text:?}"
    );

    let reread = settings::load(&path);
    assert_eq!(reread.settings.max_readers, 7);
    assert_eq!(
        reread.settings.selection_wash, 15,
        "the commit itself was saved"
    );
    assert_eq!(
        settings::resolve_max_readers_from_env(reread.settings.max_readers),
        Readers::Environment(3),
        "the variable no longer governs after the save"
    );
    std::env::remove_var(settings::MAX_READERS_VAR);
    std::fs::remove_dir_all(&dir).ok();
}
