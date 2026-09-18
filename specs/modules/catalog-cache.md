# Module spec: catalog & cache (`catalog.rs`, `cache.rs`)

## Purpose

Turn a folder into a session — one `ImageRecord` per image, instantly — and
keep the thumbnail and EXIF of every file the pipeline has read in a
per-user SQLite cache, so a folder opened a second time paints without
reading a RAW.

## Behaviour

### The catalog

- `Session::open(folder)` scans one directory (non-recursive: the user's
  ingest produces one flat folder per job) and returns at once with
  placeholder records — no per-file I/O at scan time. It admits the RAW
  extensions rawler knows (`.ARW` first-class) plus `.jpg`/`.jpeg`,
  case-insensitive. Non-image files are ignored silently, never broken
  cells; HEIC, PNG and TIFF are out of scope.
- **The JPEG import rule** (issue #8, persona-designed 2026-07-26): a JPEG
  with a same-stem RAW sibling (`DSC01234.ARW` + `DSC01234.JPG`, any case)
  stays hidden — the RAW represents the moment, and darktable exports
  dropped back into a shoot folder stay out of the grid. A JPEG with no RAW
  sibling is a first-class image, so JPEG-only folders (phone cards, a
  second body on JPEG) work end to end. Only a real file hides: a directory
  or a broken symlink named `DSC001.ARW` does not swallow `DSC001.JPG`.
  Non-UTF-8 stems import both sides — a name that cannot be compared never
  hides anything (cosmetic; cameras emit ASCII). No include/ignore setting
  in v1: the rule is folder-content-driven, and the "show paired JPEGs too"
  setting arrives with the Settings dialog (issue #15) with this rule as its
  default. RAW+JPEG pairing as one entry that travels through Copy Picks is
  deferred; two-entry import is rejected outright.
- `ImageRecord`: path, size, mtime, load state (Placeholder → Loaded →
  Failed), EXIF summary (capture time, camera make/model/serial, subseconds,
  the Sony sequence number), pick state, IPTC data, burst id, copied flag.
- The Sony `SequenceNumber` comes from the in-tree maker-note reader
  (`raw/sony.rs`) — rawler exposes no maker notes. The field is
  `#[serde(default)]` and the cache schema is at v3, so rows written before
  it re-read instead of silently lacking it.
- Pre-existing sidecars are read during load, so picks and IPTC from a
  previous session, or from another app, appear in the UI
  (xmp-sidecars.md).
- **No folder watching in v1** (user decision 2026-09-17: "not important"):
  a session is a snapshot of the folder at open; files added or removed
  while it is open appear, or disappear, on the next open.

### The cache

- One SQLite database per user in the config dir, resolved by
  `cache::default_cache_path()` at CLI and app startup; tests pass an
  explicit path. Table `previews(path TEXT PRIMARY KEY, size, mtime_ns,
  exif_json, thumb_jpeg BLOB, last_used)`, schema version in `PRAGMA
  user_version`. A hit needs path, size and `mtime_ns` all to match; a store
  replaces the path's row.
- It stores the 320 px thumb (the JPEG-q80 bytes the pipeline hands it —
  the re-encode is the pipeline's job) and the EXIF summary. Fit and
  full-res assets are never cached: cheap to re-extract.
- **No-mtime rule**: a file whose mtime cannot be read, or predates the
  epoch, is never cached — store is a no-op, lookup always misses. A key
  that cannot detect staleness is worse than no cache.
- **EXIF-failure rule**: an image whose thumb extracts but whose EXIF read
  fails is cached with an all-None summary — zero RAW reads on reopen
  outranks metadata completeness. On a hit it reports empty metadata rather
  than re-reading the RAW each session.
- Reopening a folder paints entirely from cache with zero RAW reads; the
  pipeline test asserts it (the reads belong to the pipeline module).
- **Size cap**, default 2 GiB, enforced by LRU eviction on `last_used` (1 s
  resolution, path tie-break) when the default database is resolved at
  startup. Caller-provided paths are uncapped in v1, and one long session
  may exceed the cap until the next start. The cap bounds thumb bytes; the
  file itself plateaus at its high-water mark — no VACUUM, pages are reused.
- **Concurrency**: WAL mode; `synchronous=NORMAL` (FULL's fsync-heavy
  commits held write locks past the busy timeout on Windows CI; the worst
  case on power loss is losing recent rows, which cost a re-extract); a 5 s
  busy timeout; writes retried five times with backoff; the `last_used` bump
  best-effort, so contention can never fail a read. Only a provably unusable
  FILE — `SQLITE_NOTADB`, corrupt, a schema-version mismatch — is deleted
  and recreated with its `-wal`/`-shm` files, logged once. Lock contention
  never triggers deletion: deleting a merely-locked database under a live
  connection loses data and can SIGBUS the peer process.

## Contracts

- `Session::open` reads no file contents: one `read_dir` plus two `stat`s
  per RAW-extension entry, one per unpaired JPEG, none for anything else —
  linear in the entry count. The unit test asserts the shape clock-free; the
  folder-scan perf budget holds the clock (01-architecture.md).
- The cache key is (path, size, mtime_ns); `exif_json` is `ExifSummary`, so
  a field added to it takes `#[serde(default)]` and a schema bump.
- `SessionEvent::Sidecar` carries a sidecar's pick state and full
  `IptcData` at load (xmp-sidecars.md).
- `FASTCULL_NO_CACHE` (app) and `--no-cache` (CLI) run without the
  database; the screenshot suite sets the former (test-harness.md).

## Acceptance criteria

- [x] A 1,000-entry folder yields 1,000 placeholders and reads no file
      contents — `thousand_entry_scan_yields_placeholders_without_reading_them`
      (its no-read half is `cfg(unix)`); the wall clock (< 50 ms, release,
      idle seat) is `perf_budgets::budget_folder_scan_1000_entries_under_50ms`,
      advisory on CI like the rest of the table.
- [x] Store → lookup returns identical thumb bytes and EXIF; touching mtime
      invalidates; a file with no mtime is never cached —
      `store_lookup_roundtrip_and_mtime_invalidation`,
      `missing_mtime_is_never_cached`.
- [x] Reopening a folder reads no RAW at all —
      `tests/pipeline.rs::second_run_serves_from_cache_without_touching_raws`.
- [x] Eviction respects the cap in LRU order; a corrupt file self-heals; a
      future schema version recreates; concurrent handles never destroy
      data; a busy write retries; a tampered row is a miss, not an error;
      rows persist across reopen — `eviction_respects_cap_and_lru_order`,
      `corrupt_db_self_heals`, `future_schema_version_recreates`,
      `concurrent_handles_never_destroy_data`,
      `busy_retry_retries_then_succeeds`,
      `tampered_exif_json_is_a_miss_not_an_error`, `persists_across_reopen`.
- [x] Sidecar-at-open: existing `.ARW.xmp` files yield Sidecar events with
      their pick state — `tests/pipeline.rs::existing_sidecars_are_reported_at_load`;
      keywords and IPTC fields ride the same event since M5.

## History

- 2026-09-17 — Rewritten (brief 007); folder watching dropped from v1 (the
  user). The pre-rewrite text is `specs/history/catalog-cache.md`.
- 2026-08-30 — The folder-scan clock moved to the perf budgets (issue #59);
  the structural claim stays clock-free.
- 2026-07-27 — EXIF through the in-tree TIFF walker; `RawSource` only on the
  non-TIFF fallback and full-res paths (raw-pipeline.md).
- 2026-07-26 — The JPEG import rule (issue #8); the Sony sequence number
  with M7.
- 2026-07-24 — M1: the catalog and the cache with their recorded decisions
  (no-mtime, EXIF-failure, `synchronous=NORMAL`, no VACUUM, lock contention
  never deletes).
