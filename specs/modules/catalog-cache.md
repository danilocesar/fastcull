# Module spec: catalog & cache (`catalog.rs`, `cache.rs`)

## Catalog

- `Session::open(folder)` scans one directory (non-recursive, v1 — the user's
  ingest produces one flat folder per job) for supported RAW extensions (from
  rawler's known set; `.ARW` first-class) and returns immediately with
  placeholder `ImageRecord`s — no per-file I/O at scan time.
- **JPEG import rule (issue #8, persona-designed, 2026-07-26)**: the scan
  keeps RAW extensions plus `.jpg`/`.jpeg` (case-insensitive). A JPEG
  with a same-stem RAW sibling (`DSC01234.ARW` + `DSC01234.JPG`, any
  case) stays HIDDEN — the RAW represents the moment, and darktable
  exports dropped back into a shoot folder stay out of the grid. A JPEG
  with no RAW sibling is a first-class image (JPEG-only folders — phone
  cards, second bodies on JPEG — work end to end). The rule is
  deterministic and folder-content-driven: NO include/ignore setting in
  v1 (persona IN-MY-WAY on invisible toggle state via env/CLI; the
  user's requested setting arrives as "show paired JPEGs too" WITH the
  Settings dialog, and this rule stays its default — tracked as issue
  #15, postponed). Only a REAL FILE
  counts as a hiding RAW: a directory or broken symlink named
  `DSC001.ARW` does not swallow `DSC001.JPG` (the hiding rationale
  presumes a shown RAW). Non-UTF8 stems import both sides — a name
  that cannot be compared never hides anything (recorded, cosmetic:
  cameras emit ASCII). Deferred with it:
  RAW+JPEG pairing (one entry, both files travel through copy-picks) —
  two-entry import is recorded as rejected outright. Non-image files
  (video etc.) are silently ignored, never broken cells. HEIC/PNG/TIFF
  are explicitly out of scope.
- `ImageRecord`: path, size, mtime, load state (Placeholder → Loaded → Failed),
  EXIF summary (capture time, camera model/serial, sequence number), pick state,
  IPTC data, burst id, copied flag.
- **Deferral (recorded)**: the Sony maker-note SequenceNumber is NOT in
  `ExifSummary` until the burst milestone (M7) — rawler 0.7 exposes no maker
  notes (upstream has the parsing commented out), so M7 needs an in-tree Sony
  maker-note reader. When the field is added it must be `#[serde(default)]`
  and the cache `schema_ver` must be bumped, or pre-M7 cached rows silently
  lack sequence numbers.
- Pre-existing sidecars are read during load (pipeline metadata pass) so picks and
  IPTC from a previous session (or Photo Mechanic) appear in the UI.
- Folder watching (`notify` crate): files added/removed while a session is open are
  reflected; removal of a file with unsaved state logs and drops it.

## Cache (SQLite via rusqlite, bundled)

- One DB per user (config-dir; location wiring lands with the CLI/app — until
  then the DB path is caller-provided). Table:
  `previews(path TEXT PRIMARY KEY, size, mtime_ns, exif_json, thumb_jpeg BLOB,
  last_used)`, schema version via `PRAGMA user_version`. Lookup hit requires
  path + size + mtime_ns to all match; a store replaces the path's row.
- **No-mtime rule (recorded decision)**: entries whose mtime cannot be read
  (or predates the epoch) are never cached — store is a no-op, lookup always
  misses. A cache key that cannot detect staleness is worse than no cache.
- Stores the 320 px thumb + EXIF summary; the JPEG-q80 re-encode is the
  pipeline's job (the cache stores the bytes it is given). Fit and FullRes
  assets are never cached (cheap to re-extract).
- **EXIF-failure rule (recorded decision)**: an image whose thumb extracts but
  whose EXIF read fails is cached with an all-None EXIF summary — the
  zero-RAW-reads-on-reopen guarantee outranks metadata completeness. On a
  cache hit such an image reports empty metadata rather than re-reading the
  RAW each session.
- Reopening a folder must paint entirely from cache with zero RAW reads (event
  stream assertable — verified in the pipeline module, which owns the reads).
- Size cap (default 2 GiB) enforced by LRU eviction on `last_used`
  (1 s resolution, path tie-break). The cap bounds logical thumb bytes; the
  DB file itself plateaus at its high-water mark (no VACUUM — recorded
  decision, pages are reused). Enforcement point (recorded decision): the cap
  is applied when the default per-user DB is resolved via
  `cache::default_cache_path()` (CLI/app startup); explicitly caller-provided
  cache paths are uncapped in v1, and a single long session may exceed the
  cap until the next startup.
- Concurrency: 5 s busy-timeout; WAL mode; `synchronous=NORMAL` (recorded
  decision: WAL-safe, and FULL's fsync-heavy commits held write locks past
  the busy timeout on Windows CI — worst case on power loss is losing recent
  cache rows, which only cost a re-extract). Writes additionally use a
  bounded busy-retry (5 attempts, backoff); the LRU `last_used` bump is
  best-effort so contention can never fail a read. Only a provably unusable FILE
  (SQLITE_NOTADB / corrupt / schema-version mismatch) is deleted and recreated
  (with its -wal/-shm sidecars, logged once). Lock contention must NEVER
  trigger deletion — deleting a merely-locked DB under a live connection
  loses data and can SIGBUS the peer process.

## Acceptance criteria (tests)

- [x] Scan of a 1,000-entry folder yields 1,000 placeholders and reads no file
      contents (`thousand_entry_scan_yields_placeholders_without_reading_them`);
      its wall-clock budget (< 50 ms, release, idle dev machine) is a perf
      budget in `perf_budgets.rs`, advisory on CI like the rest of the table.
- [ ] Cache hit round-trip: store → lookup returns identical thumb bytes + EXIF;
      touching mtime invalidates.
- [ ] Reopen-from-cache produces no RAW-file reads at all (permission-based
      pipeline test; historically "no `RawSource` opens" — the EXIF pass moved
      to the in-tree walker 2026-07-27, so RawSource only appears in the
      non-TIFF fallback and full-res decode paths).
- [ ] Eviction respects the cap; corrupt DB file self-heals.
- [x] Sidecar-at-open: existing `.ARW.xmp` files yield Sidecar events with
      pick state (pipeline test). Keywords populate in M5 with the IPTC panel
      (recorded scope split, see xmp-sidecars.md).
