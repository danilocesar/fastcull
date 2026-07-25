//! SQLite preview cache: 320 px thumbs + EXIF summaries keyed by
//! (path, size, mtime), so reopening a folder paints without touching a
//! single RAW file (`specs/modules/catalog-cache.md`).
//!
//! The cache is *only* a cache: a corrupt or schema-incompatible database is
//! deleted and recreated silently (logged once to stderr). Losing it costs a
//! re-extraction pass, never data.
//!
//! Recorded decision (validator risk note on the catalog step): records with
//! no readable mtime are simply not cached — `store` is a no-op and `lookup`
//! always misses. A cache key that cannot detect staleness is worse than no
//! cache.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension};

use crate::exif::ExifSummary;

/// Bump when the schema or the meaning of stored data changes (e.g. when
/// SequenceNumber joins ExifSummary in M7). v2: thumbs are stored
/// post-orientation — v1 thumbs of portrait images were sideways.
const SCHEMA_VERSION: i32 = 2;

/// Default size cap for stored thumbnails (spec: 2 GiB).
pub const DEFAULT_CAP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cache database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("cache path error: {0}")]
    Io(#[from] std::io::Error),
    #[error("cache schema version {found} unsupported (expected {SCHEMA_VERSION})")]
    SchemaMismatch { found: i32 },
    #[error("cannot serialize EXIF for cache: {0}")]
    Serialize(#[from] serde_json::Error),
}

impl CacheError {
    /// True only for errors that mean the FILE itself is unusable (corrupt or
    /// incompatible) — the cases where deleting and recreating is correct.
    /// Lock contention (`SQLITE_BUSY`/`SQLITE_LOCKED`) is emphatically not in
    /// this set: deleting a healthy, merely-locked DB under another live
    /// connection loses data and can SIGBUS the other process (QE finding).
    fn file_is_unusable(&self) -> bool {
        match self {
            CacheError::SchemaMismatch { .. } => true,
            CacheError::Db(rusqlite::Error::SqliteFailure(f, _)) => matches!(
                f.code,
                rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt
            ),
            _ => false,
        }
    }
}

/// One cache hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedPreview {
    pub exif: ExifSummary,
    pub thumb_jpeg: Vec<u8>,
}

pub struct PreviewCache {
    conn: Connection,
}

impl PreviewCache {
    /// Open (or create) the cache at `db_path`. A corrupt or
    /// schema-incompatible FILE is deleted and recreated — it is only a
    /// cache. Any other failure (locking, permissions, I/O) is returned as an
    /// error and never triggers deletion.
    pub fn open(db_path: &Path) -> Result<Self, CacheError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match Self::try_open(db_path) {
            Ok(cache) => Ok(cache),
            Err(err) if err.file_is_unusable() => {
                eprintln!(
                    "fastcull: preview cache at {} unusable ({err}); recreating",
                    db_path.display()
                );
                // Remove the WAL/SHM sidecars too: a stale hot WAL next to a
                // fresh DB would replay old frames into it (validator finding).
                for suffix in ["", "-wal", "-shm"] {
                    let mut os = db_path.as_os_str().to_owned();
                    os.push(suffix);
                    std::fs::remove_file(PathBuf::from(os)).ok();
                }
                Self::try_open(db_path)
            }
            Err(err) => Err(err),
        }
    }

    fn try_open(db_path: &Path) -> Result<Self, CacheError> {
        let conn = Connection::open(db_path)?;
        // The pipeline has background writers while the UI reads; without a
        // busy timeout every overlap surfaces as SQLITE_BUSY immediately.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // WAL-safe and MUCH faster commits (the cache is expendable by
        // design): synchronous=FULL held write locks long enough on Windows
        // CI (Defender scanning the -wal) to blow past the busy timeout.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != 0 && version != SCHEMA_VERSION {
            return Err(CacheError::SchemaMismatch { found: version });
        }
        // Run the schema batch only on uninitialized files: WAL journal mode
        // is persistent, so initialized DBs need no writes at open — this
        // also keeps concurrent opens of an existing DB write-free. Callers
        // that open one DB from several threads at once should open it once
        // first to serialize creation (Pipeline::start does).
        if version == 0 {
            conn.execute_batch(&format!(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE IF NOT EXISTS previews (
                     path       TEXT PRIMARY KEY,
                     size       INTEGER NOT NULL,
                     mtime_ns   INTEGER NOT NULL,
                     exif_json  TEXT NOT NULL,
                     thumb_jpeg BLOB NOT NULL,
                     last_used  INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS previews_last_used ON previews(last_used);
                 PRAGMA user_version = {SCHEMA_VERSION};"
            ))?;
        }
        // Exercise the table so a corrupt file fails here, inside try_open.
        conn.query_row("SELECT COUNT(*) FROM previews", [], |r| r.get::<_, i64>(0))?;
        Ok(Self { conn })
    }

    /// Look up a preview; hit only when path, size, AND mtime all match.
    /// Hits bump `last_used` for LRU accounting.
    pub fn lookup(
        &mut self,
        path: &Path,
        size: u64,
        mtime: Option<SystemTime>,
    ) -> Result<Option<CachedPreview>, CacheError> {
        let Some(mtime_ns) = mtime_nanos(mtime) else {
            return Ok(None);
        };
        let row = self
            .conn
            .query_row(
                "SELECT exif_json, thumb_jpeg FROM previews
                 WHERE path = ?1 AND size = ?2 AND mtime_ns = ?3",
                rusqlite::params![path_key(path), size as i64, mtime_ns],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        let Some((exif_json, thumb_jpeg)) = row else {
            return Ok(None);
        };
        // Unparseable EXIF JSON (e.g. hand-edited DB) is a miss, not an error.
        let Ok(exif) = serde_json::from_str(&exif_json) else {
            return Ok(None);
        };
        // LRU bookkeeping is best-effort: a busy cache must degrade to a
        // slightly stale last_used, never to a failed READ (gate finding).
        with_busy_retry(|| {
            self.conn.execute(
                "UPDATE previews SET last_used = ?2 WHERE path = ?1",
                rusqlite::params![path_key(path), now_secs()],
            )
        })
        .ok();
        Ok(Some(CachedPreview { exif, thumb_jpeg }))
    }

    /// Store (or replace) the preview row for `path`. No-op when mtime is
    /// unavailable (see module docs).
    pub fn store(
        &mut self,
        path: &Path,
        size: u64,
        mtime: Option<SystemTime>,
        exif: &ExifSummary,
        thumb_jpeg: &[u8],
    ) -> Result<(), CacheError> {
        let Some(mtime_ns) = mtime_nanos(mtime) else {
            return Ok(());
        };
        let exif_json = serde_json::to_string(exif)?;
        with_busy_retry(|| {
            self.conn.execute(
                "INSERT OR REPLACE INTO previews (path, size, mtime_ns, exif_json, thumb_jpeg, last_used)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    path_key(path),
                    size as i64,
                    mtime_ns,
                    &exif_json,
                    thumb_jpeg,
                    now_secs()
                ],
            )
        })?;
        Ok(())
    }

    /// Evict least-recently-used rows until stored thumbnails total at most
    /// `cap_bytes`.
    pub fn enforce_cap(&mut self, cap_bytes: u64) -> Result<(), CacheError> {
        loop {
            let total: i64 = self.conn.query_row(
                "SELECT COALESCE(SUM(LENGTH(thumb_jpeg)), 0) FROM previews",
                [],
                |r| r.get(0),
            )?;
            if total as u64 <= cap_bytes {
                return Ok(());
            }
            // Evict the oldest 64 rows per round to amortize the SUM.
            let evicted = with_busy_retry(|| {
                self.conn.execute(
                    "DELETE FROM previews WHERE path IN
                     (SELECT path FROM previews ORDER BY last_used ASC, path ASC LIMIT 64)",
                    [],
                )
            })?;
            if evicted == 0 {
                return Ok(());
            }
        }
    }

    /// Number of stored rows (test/diagnostic helper).
    pub fn len(&self) -> Result<u64, CacheError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM previews", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    pub fn is_empty(&self) -> Result<bool, CacheError> {
        Ok(self.len()? == 0)
    }
}

/// The per-user default cache DB location (spec: one DB per user) — product
/// policy lives here in core so the CLI and the app can never drift onto
/// different caches (validator finding). Opens the DB once, which creates it
/// and opportunistically enforces the default size cap; explicit caller-
/// provided cache paths are uncapped in v1 (recorded in catalog-cache.md).
pub fn default_cache_path() -> Option<std::path::PathBuf> {
    let dirs = directories::ProjectDirs::from("org", "fastcull", "fastcull")?;
    let path = dirs.cache_dir().join("previews.db");
    if let Ok(mut cache) = PreviewCache::open(&path) {
        cache.enforce_cap(DEFAULT_CAP_BYTES).ok();
    }
    Some(path)
}

/// Paths are keyed by their lossy UTF-8 form: stable, and collisions would
/// require two distinct non-UTF8 paths with identical lossy forms AND
/// identical size+mtime — acceptable for a cache.
fn path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Bounded retry for lock contention beyond the busy timeout (seen on
/// Windows CI: antivirus + slow fsync can exceed even a 5 s timeout). A
/// cache must degrade to waiting, not to errors.
fn with_busy_retry<T>(
    mut op: impl FnMut() -> Result<T, rusqlite::Error>,
) -> Result<T, rusqlite::Error> {
    let mut attempt = 0u32;
    loop {
        match op() {
            Err(rusqlite::Error::SqliteFailure(f, _))
                if attempt < 5
                    && matches!(
                        f.code,
                        rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                    ) =>
            {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(50 * u64::from(attempt)));
            }
            other => return other,
        }
    }
}

fn mtime_nanos(mtime: Option<SystemTime>) -> Option<i64> {
    let t = mtime?;
    let ns = t.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_nanos();
    i64::try_from(ns).ok()
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fastcull-cache-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn exif(model: &str) -> ExifSummary {
        ExifSummary {
            camera_model: Some(model.into()),
            ..Default::default()
        }
    }

    fn t(secs: u64) -> Option<SystemTime> {
        Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs))
    }

    #[test]
    fn store_lookup_roundtrip_and_mtime_invalidation() {
        let dir = tmp();
        let mut cache = PreviewCache::open(&dir.join("cache.db")).unwrap();
        let p = Path::new("/photos/DSC00001.ARW");
        cache
            .store(p, 100, t(1000), &exif("ILCE-1"), b"thumbbytes")
            .unwrap();

        let hit = cache.lookup(p, 100, t(1000)).unwrap().unwrap();
        assert_eq!(hit.thumb_jpeg, b"thumbbytes");
        assert_eq!(hit.exif.camera_model.as_deref(), Some("ILCE-1"));

        assert!(cache.lookup(p, 100, t(2000)).unwrap().is_none()); // mtime changed
        assert!(cache.lookup(p, 999, t(1000)).unwrap().is_none()); // size changed
        assert!(cache
            .lookup(Path::new("/other.ARW"), 100, t(1000))
            .unwrap()
            .is_none());

        // Replacement: new mtime row supersedes the old one.
        cache
            .store(p, 100, t(2000), &exif("ILCE-1"), b"newer")
            .unwrap();
        assert!(cache.lookup(p, 100, t(1000)).unwrap().is_none());
        assert_eq!(
            cache.lookup(p, 100, t(2000)).unwrap().unwrap().thumb_jpeg,
            b"newer"
        );
        assert_eq!(cache.len().unwrap(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_mtime_is_never_cached() {
        let dir = tmp();
        let mut cache = PreviewCache::open(&dir.join("cache.db")).unwrap();
        let p = Path::new("/photos/DSC00002.ARW");
        cache.store(p, 100, None, &exif("X"), b"data").unwrap();
        assert!(cache.is_empty().unwrap());
        assert!(cache.lookup(p, 100, None).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn eviction_respects_cap_and_lru_order() {
        let dir = tmp();
        let mut cache = PreviewCache::open(&dir.join("cache.db")).unwrap();
        // 200 rows x 1000 bytes; pin distinct last_used by direct update.
        for i in 0..200u64 {
            let p = PathBuf::from(format!("/photos/DSC{i:05}.ARW"));
            cache.store(&p, i, t(1), &exif("X"), &[0u8; 1000]).unwrap();
            cache
                .conn
                .execute(
                    "UPDATE previews SET last_used = ?2 WHERE path = ?1",
                    rusqlite::params![p.to_string_lossy(), i as i64],
                )
                .unwrap();
        }
        cache.enforce_cap(100_000).unwrap(); // keep at most 100 rows
        assert!(cache.len().unwrap() <= 100);
        // Newest rows survive.
        assert!(cache
            .lookup(Path::new("/photos/DSC00199.ARW"), 199, t(1))
            .unwrap()
            .is_some());
        assert!(cache
            .lookup(Path::new("/photos/DSC00000.ARW"), 0, t(1))
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Regression (validator finding): the entire point of the cache is that
    /// stores survive process restarts — close, reopen, and hit.
    #[test]
    fn persists_across_reopen() {
        let dir = tmp();
        let db = dir.join("cache.db");
        let p = Path::new("/photos/persist me — 苍鹭.ARW");
        {
            let mut cache = PreviewCache::open(&db).unwrap();
            cache.store(p, 7, t(42), &exif("ILCE-1"), b"bytes").unwrap();
        }
        let mut cache = PreviewCache::open(&db).unwrap();
        let hit = cache.lookup(p, 7, t(42)).unwrap().unwrap();
        assert_eq!(hit.thumb_jpeg, b"bytes");
        assert_eq!(hit.exif.camera_model.as_deref(), Some("ILCE-1"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Regression (QE major defect): two handles on one DB must contend via
    /// busy-timeout, never misdiagnose the lock as corruption and delete a
    /// healthy database. The sentinel row proves the file survived.
    #[test]
    fn concurrent_handles_never_destroy_data() {
        let dir = tmp();
        let db = dir.join("cache.db");
        let sentinel = Path::new("/sentinel.ARW");
        {
            let mut cache = PreviewCache::open(&db).unwrap();
            cache.store(sentinel, 1, t(1), &exif("S"), b"s").unwrap();
        }
        let workers: Vec<_> = (0..2)
            .map(|w| {
                let db = db.clone();
                std::thread::spawn(move || {
                    let mut cache = PreviewCache::open(&db).unwrap();
                    for i in 0..200u64 {
                        let p = PathBuf::from(format!("/w{w}/img{i}.ARW"));
                        cache.store(&p, i, t(1), &exif("X"), b"d").unwrap();
                        assert!(cache.lookup(&p, i, t(1)).unwrap().is_some());
                    }
                })
            })
            .collect();
        for h in workers {
            h.join().unwrap();
        }
        let mut cache = PreviewCache::open(&db).unwrap();
        assert!(
            cache.lookup(sentinel, 1, t(1)).unwrap().is_some(),
            "sentinel lost: a healthy DB was deleted under contention"
        );
        assert_eq!(cache.len().unwrap(), 401);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_db_self_heals() {
        let dir = tmp();
        let db = dir.join("cache.db");
        std::fs::write(&db, b"this is definitely not sqlite").unwrap();
        let mut cache = PreviewCache::open(&db).unwrap();
        let p = Path::new("/photos/a.ARW");
        cache.store(p, 1, t(1), &exif("X"), b"d").unwrap();
        assert!(cache.lookup(p, 1, t(1)).unwrap().is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn future_schema_version_recreates() {
        let dir = tmp();
        let db = dir.join("cache.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch("PRAGMA user_version = 99;").unwrap();
        }
        let mut cache = PreviewCache::open(&db).unwrap();
        cache
            .store(Path::new("/p.ARW"), 1, t(1), &exif("X"), b"d")
            .unwrap();
        let version: i32 = cache
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn busy_retry_retries_then_succeeds() {
        let mut calls = 0;
        let result = with_busy_retry(|| {
            calls += 1;
            if calls < 3 {
                Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                    Some("database is locked".into()),
                ))
            } else {
                Ok(42)
            }
        });
        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls, 3);
    }

    #[test]
    fn tampered_exif_json_is_a_miss_not_an_error() {
        let dir = tmp();
        let mut cache = PreviewCache::open(&dir.join("cache.db")).unwrap();
        let p = Path::new("/photos/a.ARW");
        cache.store(p, 1, t(1), &exif("X"), b"d").unwrap();
        cache
            .conn
            .execute("UPDATE previews SET exif_json = 'garbage{'", [])
            .unwrap();
        assert!(cache.lookup(p, 1, t(1)).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
