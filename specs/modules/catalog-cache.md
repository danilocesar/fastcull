# Module spec: catalog & cache (`catalog.rs`, `cache.rs`)

## Catalog

- `Session::open(folder)` scans one directory (non-recursive, v1 — the user's
  ingest produces one flat folder per job) for supported RAW extensions (from
  rawler's known set; `.ARW` first-class) and returns immediately with
  placeholder `ImageRecord`s — no per-file I/O at scan time.
- Same-stem JPEG siblings (`DSC01234.ARW` + `DSC01234.JPG`) are ignored in v1:
  not shown, not copied. Recorded user decision — he shoots RAW+JPEG but does
  not care yet; showing/copying paired JPEGs is a v2 candidate, so the scan
  code should keep the sibling check cheap to add.
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

- [ ] Scan of a 1,000-entry tempdir returns in < 50 ms with all placeholders
      (< 400 ms on Windows CI — Defender scans fresh tempdir files; recorded).
- [ ] Cache hit round-trip: store → lookup returns identical thumb bytes + EXIF;
      touching mtime invalidates.
- [ ] Reopen-from-cache produces no `RawSource` opens (counting instrumentation).
- [ ] Eviction respects the cap; corrupt DB file self-heals.
- [ ] Sidecar-at-open: fixture folder with existing `.ARW.xmp` yields records with
      pick state and keywords populated.
