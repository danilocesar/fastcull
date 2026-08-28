# ADR 0004: Derived outputs

**Status**: accepted (2026-08-27, user decision) · **Extends**: ADR 0003

## Context

ADR 0003 made FastCull write nothing but `<name>.<ext>.xmp` sidecars, and
the README promised "everything FastCull writes goes into sidecars". Copy
Picks (M6) already bent that sentence — it writes copies of RAWs and
sidecars into a folder the user chose — and "export frames as video"
(video-export.md) writes a file that is neither a sidecar nor a copy. The
rule needs to say what it always meant.

## Decision

FastCull writes exactly three kinds of file, and nothing else:

1. **Sidecars** — the user's culling state, next to the RAW, ADR 0003
   unchanged.
2. **Copies** — byte-identical RAWs and sidecars, Copy Picks.
3. **Derived outputs** — new files built from what the RAW already contains
   (today: a Motion JPEG `.mov` of the embedded full-res JPEGs).

Rules for kinds 2 and 3, the "derived-output contract":

- Only into a folder the user chose in that dialog. Never the RAW folder
  by default; allowed when chosen.
- A RAW file is never opened for writing. A sidecar is never modified by an
  export. (The ADR 0003 tests cover every module that writes.)
- Nothing at the destination is replaced without the user's explicit
  Overwrite answer to the clash question (fileops.md); nothing at the
  destination is ever deleted.
- Written through a unique temp name and a no-clobber commit; never a
  partial file under the final name; a hard quit leaves at most a hidden
  `.fastcull-partial-*` file, documented.
- Verified: every byte the output was built from is hashed on the way in
  and re-read from the finished file; the "verified" line in the report is
  earned, never assumed.
- Derived outputs are built from bytes the RAW already contains — never
  from a re-encode, a crop or an edit of them (video-export.md, "never an
  editor").

## Consequences

- README wording changes from "everything FastCull writes goes into
  sidecars" to: the RAW is never modified; the user's state lives in
  sidecars; copies and exports go only where the user points them.
- Each new derived output is a module spec with the contract's acceptance
  tests, not a menu item.
