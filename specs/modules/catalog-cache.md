# Module spec: catalog & cache (`catalog.rs`, `cache.rs`)

## Catalog

- `Session::open(folder)` scans one directory (non-recursive, v1) for supported RAW
  extensions (from rawler's known set; `.ARW` first-class) and returns immediately
  with placeholder `ImageRecord`s — no per-file I/O at scan time.
- `ImageRecord`: path, size, mtime, load state (Placeholder → Loaded → Failed),
  EXIF summary (capture time, camera model/serial, sequence number), pick state,
  IPTC data, burst id, copied flag.
- Pre-existing sidecars are read during load (pipeline metadata pass) so picks and
  IPTC from a previous session (or Photo Mechanic) appear in the UI.
- Folder watching (`notify` crate): files added/removed while a session is open are
  reflected; removal of a file with unsaved state logs and drops it.

## Cache (SQLite via rusqlite, bundled)

- One DB per user (config-dir), table `previews(path, size, mtime, exif_json,
  thumb_jpeg BLOB, schema_ver)`. Key = (path, size, mtime): any mismatch is a miss;
  stale rows for a path are replaced on write.
- Stores the 320 px thumb (re-encoded JPEG q80, ~30–60 KB) + EXIF summary. Fit and
  FullRes assets are never cached (cheap to re-extract).
- Reopening a folder must paint entirely from cache with zero RAW reads (event
  stream assertable).
- Size cap (default 2 GiB) enforced by LRU eviction on `last_used`.
- Corrupt/old-schema DB: delete and recreate silently (it is only a cache), log once.

## Acceptance criteria (tests)

- [ ] Scan of a 1,000-entry tempdir returns in < 50 ms with all placeholders.
- [ ] Cache hit round-trip: store → lookup returns identical thumb bytes + EXIF;
      touching mtime invalidates.
- [ ] Reopen-from-cache produces no `RawSource` opens (counting instrumentation).
- [ ] Eviction respects the cap; corrupt DB file self-heals.
- [ ] Sidecar-at-open: fixture folder with existing `.ARW.xmp` yields records with
      pick state and keywords populated.
