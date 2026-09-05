//! THE POPUP AND THE SPEC AGREE — a parity test for ui-grid.md's
//! acceptance line "the shortcuts popup lists every binding in this spec".
//!
//! That line was aspirational for as long as the popup was a 1,200-character
//! string literal: `drag` sat in the spec's keyboard map and not on the card
//! for three milestones, and nothing could say so. Two lists exist here (the
//! map and the card) and this is what checks they still describe the same
//! app.
//!
//! It reads the two FILES rather than the running program on purpose. The
//! card paraphrases and REGROUPS the map — one spec row becomes four card
//! rows, two spec rows share one card row — so no automatic derivation is
//! possible and none is wanted; what is wanted is that adding a binding to
//! the spec cannot be finished without deciding where it goes on the card.
//! The table below is that decision, written down. Both directions are
//! checked, so a map row with no home fails, and a card row the spec never
//! mentions fails too.
//!
//! Note what a test keyed on the KEY HANDLER instead would miss: `drag`,
//! `wheel`, `click` and `double-click` are not keys, and half the reason
//! the card was wrong is that they were filed as if they were.

use std::path::{Path, PathBuf};

/// Every row of ui-grid.md's keyboard map, and the card cells that carry
/// it. The left string is the map's first column, verbatim; the right is
/// the `KeyRow`s (`k:` cells) in `main.slint`'s popup that must exist for
/// that row to count as listed.
///
/// An EMPTY list means the binding is on the card but not as a key row —
/// today that is only `?`/F1, which is named in the title-row hint (the
/// key that opens the card has no natural section, and the reader holding
/// it does not need a row to find it). It is asserted separately, and
/// `HINTED_INSTEAD_OF_ROWED` below is the whole permitted list: an empty
/// cell list asserts NOTHING about the card, so left open to anyone it
/// would let the next binding be written `("Ctrl+P", &[])` and satisfy
/// this test in both directions while never appearing on the card — which
/// is the `drag` failure this file exists to prevent, wearing this file's
/// own uniform.
const CARD_ROWS_FOR: &[(&str, &[&str])] = &[
    (
        "Arrows / PgUp / PgDn / Home / End",
        &["← / →", "↑ / ↓", "PgUp / PgDn", "Home / End"],
    ),
    ("`Y`, `P` or `Space`", &["Y / P / Space"]),
    ("`N` or `X`", &["N / X"]),
    ("`U`", &["U"]),
    ("`+` / `-`", &["+ / -"]),
    ("`Z`", &["Z"]),
    ("wheel", &["Wheel"]),
    ("click (loupe)", &["Click"]),
    ("double-click (grid)", &["Double-click"]),
    ("double-click (loupe)", &["Double-click"]),
    ("drag", &["Drag"]),
    ("`G`", &["G"]),
    ("`Esc`", &["Esc"]),
    ("`I`", &["I"]),
    ("`K`", &["K"]),
    ("Shift+arrows", &["Shift+arrows"]),
    ("`Ctrl+A`", &["Ctrl+A"]),
    ("`[` / `]`", &["[ / ]"]),
    (
        "Shift+`[` / Shift+`]` (also `{` / `}`, the shifted characters a US keyboard sends)",
        &["Shift+[ / ]"],
    ),
    ("`Ctrl+Shift+B`", &["Ctrl+Shift+B"]),
    ("`Ctrl+O`", &["Ctrl+O"]),
    ("`Ctrl+Q`", &["Ctrl+Q"]),
    ("`Ctrl+E` (menu: Copy picks…)", &["Ctrl+E"]),
    (
        "`Ctrl+Shift+E` (menu: Export Frames as Video…)",
        &["Ctrl+Shift+E"],
    ),
    ("`?` / `F1`", &[]),
    ("`1`–`5`, `0`", &["1–5, 0"]),
];

/// The only spec rows allowed an empty cell list in `CARD_ROWS_FOR`, and
/// the string each is named by on the card instead of in a `KeyRow`.
/// Adding to this list is a decision about the CARD — that a binding is
/// better taught where the reader already is than filed under a heading —
/// so it is made here, once, and not by leaving a `&[]` in a table of 27
/// rows where nobody would see it.
const HINTED_INSTEAD_OF_ROWED: &[(&str, &str)] = &[("`?` / `F1`", "? or F1 to open")];

fn repo_root() -> PathBuf {
    // crates/fastcull-app -> crates -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root above crates/fastcull-app")
        .to_path_buf()
}

/// The first column of every row of the `## Keyboard map` table, in order.
fn spec_map_keys(spec: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut inside = false;
    for line in spec.lines() {
        if line.starts_with("## Keyboard map") {
            inside = true;
            continue;
        }
        if inside {
            if line.starts_with("## ") {
                break;
            }
            let Some(row) = line.strip_prefix('|') else {
                // The table ends at the first line that is not a row; the
                // section continues in prose below it.
                if !keys.is_empty() {
                    break;
                }
                continue;
            };
            let cell = row.split('|').next().unwrap_or("").trim();
            // The header and its `|---|---|` separator.
            if cell == "Key" || cell.chars().all(|c| c == '-') {
                continue;
            }
            keys.push(cell.to_string());
        }
    }
    keys
}

/// The popup's block of `main.slint`, from its `ModalScrim` to the About
/// dialog that follows it — everything the card can be said to "list".
fn popup_block(ui: &str) -> &str {
    let start = ui
        .find("if root.shortcuts-visible: ModalScrim {")
        .expect("the shortcuts popup's ModalScrim in main.slint");
    let rest = &ui[start..];
    let end = rest
        .find("if root.about-visible: ModalScrim {")
        .expect("the About dialog after the shortcuts popup in main.slint");
    &rest[..end]
}

/// Every `k: "…";` in the popup block — the card's key cells, in order.
fn card_key_cells(block: &str) -> Vec<String> {
    let mut cells = Vec::new();
    for (_, after) in block
        .match_indices("k: \"")
        .map(|(i, m)| (i, &block[i + m.len()..]))
    {
        if let Some(end) = after.find('"') {
            cells.push(after[..end].to_string());
        }
    }
    cells
}

#[test]
fn the_shortcuts_card_lists_every_binding_in_the_spec() {
    let root = repo_root();
    let spec = std::fs::read_to_string(root.join("specs/modules/ui-grid.md"))
        .expect("specs/modules/ui-grid.md");
    let ui = std::fs::read_to_string(root.join("crates/fastcull-app/ui/main.slint"))
        .expect("crates/fastcull-app/ui/main.slint");

    let map = spec_map_keys(&spec);
    assert!(
        map.len() > 20,
        "the keyboard map parsed as only {} rows — the table moved or its \
         shape changed, and this test is measuring nothing: {map:?}",
        map.len()
    );

    // --- the table below is complete, in both directions -------------
    for key in &map {
        assert!(
            CARD_ROWS_FOR.iter().any(|(k, _)| k == key),
            "ui-grid.md's keyboard map has a row `{key}` that no entry of \
             CARD_ROWS_FOR claims. A new binding is not finished until the \
             shortcuts card lists it (ui-grid.md: \"the shortcuts popup \
             lists every binding in this spec\") — add the row to the card \
             and the pairing here."
        );
    }
    for (key, _) in CARD_ROWS_FOR {
        assert!(
            map.iter().any(|k| k == key),
            "CARD_ROWS_FOR pairs a spec row `{key}` that ui-grid.md's \
             keyboard map no longer has — the map is the source of truth, \
             so either the row moved (fix the string) or the binding is \
             gone (drop it from the card too)."
        );
    }

    // --- every mapped binding really is a cell on the card -----------
    let cells = card_key_cells(popup_block(&ui));
    assert!(
        cells.len() > 20,
        "the popup block parsed as only {} key cells: {cells:?}",
        cells.len()
    );
    for (key, wanted) in CARD_ROWS_FOR {
        for w in *wanted {
            assert!(
                cells.iter().any(|c| c == w),
                "ui-grid.md's `{key}` should be on the card as the key cell \
                 `{w}`, and no `KeyRow` carries it. The card has: {cells:?}"
            );
        }
    }

    // --- and nothing on the card is undocumented ---------------------
    for cell in &cells {
        assert!(
            CARD_ROWS_FOR
                .iter()
                .any(|(_, w)| w.contains(&cell.as_str())),
            "the card has a key cell `{cell}` that no row of ui-grid.md's \
             keyboard map accounts for — the map is the source of truth for \
             this list, so add the binding there first."
        );
    }

    // --- the escape hatch is exactly as wide as it is documented -----
    //
    // Without this, `&[]` is a way to add a binding to the spec, satisfy
    // both directions of the parity above, and ship a card that does not
    // list it — the exact hole `drag` fell through, reopened inside the
    // test that closed it.
    for (key, wanted) in CARD_ROWS_FOR {
        assert!(
            !wanted.is_empty() || HINTED_INSTEAD_OF_ROWED.iter().any(|(k, _)| k == key),
            "CARD_ROWS_FOR gives the spec row `{key}` an EMPTY cell list, \
             which asserts nothing at all about the card. Only a binding \
             the card teaches in prose instead of in a row may have one, \
             and every such binding is named in HINTED_INSTEAD_OF_ROWED \
             with the words that carry it. Give `{key}` a `KeyRow`, or add \
             it there and say where it is written."
        );
    }

    // --- and those bindings really are named on the card -------------
    let block = popup_block(&ui);
    for (key, hint) in HINTED_INSTEAD_OF_ROWED {
        assert!(
            CARD_ROWS_FOR.iter().any(|(k, w)| k == key && w.is_empty()),
            "HINTED_INSTEAD_OF_ROWED claims `{key}` is taught in prose, but \
             CARD_ROWS_FOR does not pair it with an empty cell list — the \
             two tables disagree about which binding this is."
        );
        assert!(
            block.contains(hint),
            "the card no longer says \"{hint}\", which is the only place \
             `{key}` is listed (CARD_ROWS_FOR gives it no key row on \
             purpose)."
        );
    }
}
