//! LMDB-backed persistence for the indexing engine.
//!
//! One `heed::Env` per project at `~/.cache/faber/index/<blake3(root)>/`.
//! GPUI-free, storage-only: it knows about stamps, per-module data entries, and
//! meta bookkeeping, and nothing about the engine's scheduling or module logic.

use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use heed::types::Bytes;
use heed::{Database, Env, EnvFlags, EnvOpenOptions, MdbError};

/// Engine schema version. Bump when the on-disk store *format* changes in a way
/// that invalidates every module (the `meta` layout, key composition, etc.).
pub const STORE_VERSION: u32 = 1;

/// Initial LMDB map size: 1 GiB virtual reservation (grown on map-full).
const INITIAL_MAP_SIZE: usize = 1 << 30;

/// Upper bound on named databases: `meta` + `stamps:{m}` + `data:{m}` per module.
/// Generous; LMDB reserves a slot per name, not memory.
const MAX_DBS: u32 = 128;

/// Delete cached index dirs untouched for longer than this on startup.
const GC_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Meta keys (serde-agnostic byte keys inside the `meta` DB).
const META_SCHEMA_VERSION: &[u8] = b"schema_version";
const META_LAST_OPENED: &[u8] = b"last_opened";
const META_LAST_REBUILD_REASON: &[u8] = b"last_rebuild_reason";
const META_MODULE_VERSION_PREFIX: &[u8] = b"modver:";

/// Separator between the path prefix and a module's key suffix, per the TDD
/// key-layout convention: `{rel_path}\0{suffix}`.
const KEY_SEP: u8 = 0;

/// One data entry a module wrote for a file: `(key_suffix, value)`. The engine
/// composes the stored key as `{rel_path}\0{key_suffix}`.
pub type DataEntry = (Vec<u8>, Vec<u8>);

/// One file in a [`IndexStore::write_batch`] call: its rel_path, staleness
/// [`Stamp`], and the module's data entries for it.
pub type FileBatchEntry = (Vec<u8>, Stamp, Vec<DataEntry>);

/// Per-file staleness stamp. META-only modules leave `hash` as `None`; content
/// modules store the blake3 of the file body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    pub mtime: SystemTime,
    pub size: u64,
    pub hash: Option<[u8; 32]>,
}

impl Stamp {
    /// Encode as: mtime_secs(u64 LE) | mtime_nanos(u32 LE) | size(u64 LE) |
    /// has_hash(u8) | [hash(32) if present]. Hand-rolled and fixed-width so it
    /// never depends on bincode's config drift for the hot per-file path.
    fn encode(&self) -> Vec<u8> {
        let (secs, nanos) = system_time_parts(self.mtime);
        let mut out = Vec::with_capacity(8 + 4 + 8 + 1 + 32);
        out.extend_from_slice(&secs.to_le_bytes());
        out.extend_from_slice(&nanos.to_le_bytes());
        out.extend_from_slice(&self.size.to_le_bytes());
        match self.hash {
            Some(h) => {
                out.push(1);
                out.extend_from_slice(&h);
            }
            None => out.push(0),
        }
        out
    }

    fn decode(bytes: &[u8]) -> Result<Stamp> {
        anyhow::ensure!(bytes.len() >= 21, "stamp too short");
        let secs = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let nanos = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let size = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
        let hash = match bytes[20] {
            0 => None,
            1 => {
                anyhow::ensure!(bytes.len() >= 53, "stamp hash truncated");
                let mut h = [0u8; 32];
                h.copy_from_slice(&bytes[21..53]);
                Some(h)
            }
            other => anyhow::bail!("invalid stamp hash tag: {other}"),
        };
        Ok(Stamp {
            mtime: UNIX_EPOCH + Duration::new(secs, nanos),
            size,
            hash,
        })
    }
}

fn system_time_parts(t: SystemTime) -> (u64, u32) {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    (d.as_secs(), d.subsec_nanos())
}

/// `$HOME/.cache/faber` — mirrors faber-settings' HOME approach; no `dirs` crate.
fn cache_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".cache/faber/index")
}

/// Deterministic per-project subdir: `blake3(root_abs_path)` hex.
fn project_dir_name(root: &Path) -> String {
    let abs = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    blake3::hash(abs.as_os_str().as_encoded_bytes())
        .to_hex()
        .to_string()
}

/// Open an env at `dir` with the given map size.
///
/// The env is always opened `NO_SYNC` so commits never fsync implicitly — cold
/// runs batch thousands of files behind a single durable [`IndexStore::sync`] at
/// run end, and incremental [`IndexStore::write_batch`] calls issue their own
/// `sync()` per batch. A crash loses at most the uncommitted tail, which the next
/// merge-join reindexes (the store is a cache, not a source of truth). The flag
/// must be identical across every open of a path, since heed caches the `Env`
/// process-globally keyed by path+options and rejects a mismatched reopen.
fn open_env(dir: &Path, map_size: usize) -> Result<Env> {
    std::fs::create_dir_all(dir).with_context(|| format!("create index dir {dir:?}"))?;
    let mut opts = EnvOpenOptions::new();
    opts.map_size(map_size).max_dbs(MAX_DBS);
    // SAFETY: NO_SYNC only weakens durability of the uncommitted tail; no aliasing
    // or soundness concern. Durability is restored by explicit `sync()`.
    unsafe {
        opts.flags(EnvFlags::NO_SYNC);
    }
    // SAFETY: standard single-process open; heed adds NO_TLS itself.
    let env = unsafe { opts.open(dir) }.with_context(|| format!("open lmdb env {dir:?}"))?;
    Ok(env)
}

/// True for resource-exhaustion failures that a short backoff can resolve —
/// matched on the rendered chain because heed/LMDB wrap the underlying errno.
fn is_transient_open_error(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    msg.contains("Resource temporarily unavailable") // EAGAIN (os error 35)
        || msg.contains("Too many open files") // EMFILE (os error 24)
        || msg.contains("os error 35")
        || msg.contains("os error 24")
}

/// The storage handle; the engine holds one per project. `heed::Env` is itself
/// `Send + Sync` and its read/write/resize methods take `&self`, so no lock wraps
/// it. `map_size` is behind a lock only to serialize concurrent map-full grows.
pub struct IndexStore {
    env: Env,
    map_size: RwLock<usize>,
}

impl IndexStore {
    /// Open (or create) the store for `root`. Runs 30-day GC across the cache
    /// dir, then self-heals this project's env: a schema-version mismatch or an
    /// unrecoverable open failure wipes and rebuilds. Never panics.
    pub fn open(root: &Path) -> Result<Self> {
        static GC_DONE: OnceLock<()> = OnceLock::new();
        if GC_DONE.set(()).is_ok() {
            gc_stale_dirs();
        }

        let dir = cache_root().join(project_dir_name(root));

        // Transient resource errors (EAGAIN from exhausted SysV semaphores on
        // macOS, EMFILE) are NOT corruption: wiping on them destroys a healthy
        // index and the reopen fails identically. Retry with backoff instead;
        // wipe only for genuinely unrecoverable (corrupt) envs.
        let mut store = None;
        let mut last_transient: Option<anyhow::Error> = None;
        for attempt in 0u32..4 {
            match Self::open_at(&dir, INITIAL_MAP_SIZE) {
                Ok(s) => {
                    store = Some(s);
                    break;
                }
                Err(err) if is_transient_open_error(&err) => {
                    log::warn!("index env open transient failure (attempt {attempt}): {err:#}");
                    last_transient = Some(err);
                    std::thread::sleep(std::time::Duration::from_millis(50u64 << attempt));
                }
                Err(err) => {
                    log::warn!("index env at {dir:?} unrecoverable ({err:#}); rebuilding fresh");
                    let _ = std::fs::remove_dir_all(&dir);
                    let s = Self::open_at(&dir, INITIAL_MAP_SIZE)
                        .context("reopen fresh index env after wipe")?;
                    s.record_rebuild_reason("corrupt env: reset")?;
                    store = Some(s);
                    break;
                }
            }
        }
        let store = match store {
            Some(s) => s,
            None => {
                return Err(last_transient
                    .expect("retry loop exits with either a store or an error")
                    .context("open index env: transient failure persisted"));
            }
        };

        store.reconcile_schema_version()?;
        Ok(store)
    }

    fn open_at(dir: &Path, map_size: usize) -> Result<Self> {
        let env = open_env(dir, map_size)?;
        // Ensure the meta DB exists so first-run reads don't error.
        {
            let mut wtxn = env.write_txn()?;
            let _meta: Database<Bytes, Bytes> = env.create_database(&mut wtxn, Some("meta"))?;
            wtxn.commit()?;
        }
        Ok(Self {
            env,
            map_size: RwLock::new(map_size),
        })
    }

    /// Test-only: open with a caller-chosen initial map size and skip GC /
    /// self-heal, so the map-full resize path can be provoked deterministically.
    #[cfg(test)]
    fn open_with_map(dir: &Path, map_size: usize) -> Result<Self> {
        let store = Self::open_at(dir, map_size)?;
        store.set_meta_u32(META_SCHEMA_VERSION, STORE_VERSION)?;
        Ok(store)
    }

    /// If the recorded engine schema version differs from [`STORE_VERSION`],
    /// the whole store is stale: wipe it and stamp the new version. This is the
    /// coarse global gate; per-module version bumps are handled by the engine
    /// via [`Self::module_version`] / [`Self::drop_module`].
    fn reconcile_schema_version(&self) -> Result<()> {
        let recorded = self.get_meta_u32(META_SCHEMA_VERSION)?;
        if recorded == Some(STORE_VERSION) {
            return Ok(());
        }
        if recorded.is_some() {
            log::info!("index schema {recorded:?} != {STORE_VERSION}; resetting store");
            self.reset_all_data()?;
            self.record_rebuild_reason("schema version bump")?;
        }
        self.set_meta_u32(META_SCHEMA_VERSION, STORE_VERSION)?;
        Ok(())
    }

    /// Drop every named DB except `meta`, then clear stale module-version keys.
    fn reset_all_data(&self) -> Result<()> {
        let env = &self.env;
        let names = named_dbs(env)?;
        let mut wtxn = env.write_txn()?;
        for name in &names {
            if name == "meta" {
                continue;
            }
            if let Some(db) = env.open_database::<Bytes, Bytes>(&wtxn, Some(name))? {
                db.clear(&mut wtxn)?;
            }
        }
        // Wipe recorded per-module versions so everything rebuilds.
        let meta: Database<Bytes, Bytes> = env
            .open_database(&wtxn, Some("meta"))?
            .context("meta db missing")?;
        let stale: Vec<Vec<u8>> = meta
            .iter(&wtxn)?
            .filter_map(|kv| kv.ok())
            .filter(|(k, _)| k.starts_with(META_MODULE_VERSION_PREFIX))
            .map(|(k, _)| k.to_vec())
            .collect();
        for k in stale {
            meta.delete(&mut wtxn, &k)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    // ---- module version bookkeeping -------------------------------------

    /// Recorded version for a module (`None` = never built).
    pub fn module_version(&self, module_name: &str) -> Result<Option<u32>> {
        self.get_meta_u32(&module_version_key(module_name))
    }

    /// Record the version a module was last built at.
    pub fn set_module_version(&self, module_name: &str, version: u32) -> Result<()> {
        self.set_meta_u32(&module_version_key(module_name), version)
    }

    // ---- reads ----------------------------------------------------------

    /// Read a stamp for `rel_path` in `module_name`.
    pub fn get_stamp(&self, module_name: &str, rel_path: &[u8]) -> Result<Option<Stamp>> {
        let env = &self.env;
        let rtxn = env.read_txn()?;
        let db = match env.open_database::<Bytes, Bytes>(&rtxn, Some(&stamps_db(module_name)))? {
            Some(db) => db,
            None => return Ok(None),
        };
        match db.get(&rtxn, rel_path)? {
            Some(bytes) => Ok(Some(Stamp::decode(bytes)?)),
            None => Ok(None),
        }
    }

    /// Read a single data entry by full key in `module_name`.
    pub fn get_data(&self, module_name: &str, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let env = &self.env;
        let rtxn = env.read_txn()?;
        let db = match env.open_database::<Bytes, Bytes>(&rtxn, Some(&data_db(module_name)))? {
            Some(db) => db,
            None => return Ok(None),
        };
        Ok(db.get(&rtxn, key)?.map(|b| b.to_vec()))
    }

    /// Iterate all stamps for a module (inspection / `examples/dump.rs`).
    /// Materialized to owned pairs like [`Self::iter_data`].
    pub fn iter_stamps<'txn>(
        &'txn self,
        module_name: &str,
    ) -> Result<impl Iterator<Item = Result<(Vec<u8>, Stamp)>> + 'txn> {
        let env = &self.env;
        let rtxn = env.read_txn()?;
        let db = env.open_database::<Bytes, Bytes>(&rtxn, Some(&stamps_db(module_name)))?;
        let mut out: Vec<Result<(Vec<u8>, Stamp)>> = Vec::new();
        if let Some(db) = db {
            for item in db.iter(&rtxn)? {
                match item {
                    Ok((k, v)) => out.push(Stamp::decode(v).map(|s| (k.to_vec(), s))),
                    Err(e) => out.push(Err(anyhow::Error::new(e))),
                }
            }
        }
        Ok(out.into_iter())
    }

    /// Iterate all data entries for a module (warm-start publish). Owns its own
    /// read transaction; entries are materialized to owned `Vec`s so the borrow
    /// of the txn can't escape into the caller.
    pub fn iter_data<'txn>(
        &'txn self,
        module_name: &str,
    ) -> Result<impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'txn> {
        let env = &self.env;
        let rtxn = env.read_txn()?;
        let db = env.open_database::<Bytes, Bytes>(&rtxn, Some(&data_db(module_name)))?;
        // Collect eagerly under the read guard; the DB may be absent (never built).
        let mut out: Vec<Result<(Vec<u8>, Vec<u8>)>> = Vec::new();
        if let Some(db) = db {
            for item in db.iter(&rtxn)? {
                match item {
                    Ok((k, v)) => out.push(Ok((k.to_vec(), v.to_vec()))),
                    Err(e) => out.push(Err(anyhow::Error::new(e))),
                }
            }
        }
        Ok(out.into_iter())
    }

    // ---- writes ---------------------------------------------------------

    /// Write a batch of files for `module_name`. Each file's stamp and data
    /// entries share ONE write transaction; the whole batch commits together.
    /// On map-full the env is reopened with a doubled map size and the batch is
    /// retried once against the larger env.
    ///
    /// `cold_run` toggles durability: cold runs rely on the caller issuing a
    /// single [`Self::sync`] at run end (env opened `NO_SYNC`); incremental runs
    /// sync per commit. Either way the batch is atomic.
    pub fn write_batch(
        &self,
        module_name: &str,
        entries: &[FileBatchEntry],
        cold_run: bool,
    ) -> Result<()> {
        loop {
            match self.try_write_batch(module_name, entries) {
                Ok(()) => {
                    if !cold_run {
                        self.sync()?;
                    }
                    return Ok(());
                }
                Err(err) if is_map_full(&err) => {
                    self.grow_map()?;
                    // retry against the enlarged env on the next loop iteration.
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn try_write_batch(&self, module_name: &str, entries: &[FileBatchEntry]) -> Result<()> {
        let env = &self.env;
        let mut wtxn = env.write_txn()?;
        let stamps: Database<Bytes, Bytes> =
            env.create_database(&mut wtxn, Some(&stamps_db(module_name)))?;
        let data: Database<Bytes, Bytes> =
            env.create_database(&mut wtxn, Some(&data_db(module_name)))?;

        for (rel_path, stamp, kvs) in entries {
            stamps.put(&mut wtxn, rel_path, &stamp.encode())?;
            for (suffix, value) in kvs {
                let key = compose_key(rel_path, suffix);
                data.put(&mut wtxn, &key, value)?;
            }
        }
        wtxn.commit().map_err(anyhow::Error::new)
    }

    /// Delete every stamp and data entry for a file (delete/rename). Iterates the
    /// data DB by the file's `{rel_path}\0` prefix, matching the key layout.
    pub fn delete_file(&self, module_name: &str, rel_path: &[u8]) -> Result<()> {
        let env = &self.env;
        let mut wtxn = env.write_txn()?;

        if let Some(stamps) =
            env.open_database::<Bytes, Bytes>(&wtxn, Some(&stamps_db(module_name)))?
        {
            stamps.delete(&mut wtxn, rel_path)?;
        }

        if let Some(data) = env.open_database::<Bytes, Bytes>(&wtxn, Some(&data_db(module_name)))? {
            let mut prefix = rel_path.to_vec();
            prefix.push(KEY_SEP);
            let victims: Vec<Vec<u8>> = data
                .prefix_iter(&wtxn, &prefix)?
                .filter_map(|kv| kv.ok())
                .map(|(k, _)| k.to_vec())
                .collect();
            for k in victims {
                data.delete(&mut wtxn, &k)?;
            }
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Durable flush — one call at the end of a cold run.
    pub fn sync(&self) -> Result<()> {
        let env = &self.env;
        env.force_sync().map_err(anyhow::Error::new)
    }

    /// Drop a module's stamps + data DBs and its recorded version. Used by the
    /// engine when a module's `version()` bumps (rebuild that module only).
    pub fn drop_module(&self, module_name: &str) -> Result<()> {
        let env = &self.env;
        let mut wtxn = env.write_txn()?;
        if let Some(db) = env.open_database::<Bytes, Bytes>(&wtxn, Some(&stamps_db(module_name)))? {
            db.clear(&mut wtxn)?;
        }
        if let Some(db) = env.open_database::<Bytes, Bytes>(&wtxn, Some(&data_db(module_name)))? {
            db.clear(&mut wtxn)?;
        }
        let meta: Database<Bytes, Bytes> = env
            .open_database(&wtxn, Some("meta"))?
            .context("meta db missing")?;
        meta.delete(&mut wtxn, &module_version_key(module_name))?;
        wtxn.commit()?;
        Ok(())
    }

    /// Stamp `last_opened = now` (called on engine start; feeds the 30-day GC).
    pub fn touch_last_opened(&self) -> Result<()> {
        let (secs, _) = system_time_parts(SystemTime::now());
        self.set_meta_u64(META_LAST_OPENED, secs)
    }

    // ---- internals ------------------------------------------------------

    fn record_rebuild_reason(&self, reason: &str) -> Result<()> {
        let env = &self.env;
        let mut wtxn = env.write_txn()?;
        let meta: Database<Bytes, Bytes> = env.create_database(&mut wtxn, Some("meta"))?;
        meta.put(&mut wtxn, META_LAST_REBUILD_REASON, reason.as_bytes())?;
        wtxn.commit()?;
        Ok(())
    }

    /// Double the map size in place. Called after a batch aborts with `MapFull`;
    /// the aborting write txn is already rolled back, so no txn is active — the
    /// precondition for `mdb_env_set_mapsize`. Resizing beats reopening: heed
    /// caches the `Env` by path+options, so a reopen with a new map size would
    /// hit `BadOpenOptions`.
    fn grow_map(&self) -> Result<()> {
        let mut size = self.map_size.write().unwrap();
        let new_size = size.saturating_mul(2);
        log::info!("index map full; growing {} -> {} bytes", *size, new_size);
        // SAFETY: no active txn (the failed write txn was aborted before we got
        // here) and we hold the write lock on `map_size`, serializing resizes.
        unsafe { self.env.resize(new_size) }?;
        *size = new_size;
        Ok(())
    }

    fn get_meta_u32(&self, key: &[u8]) -> Result<Option<u32>> {
        let env = &self.env;
        let rtxn = env.read_txn()?;
        let meta: Database<Bytes, Bytes> = match env.open_database(&rtxn, Some("meta"))? {
            Some(db) => db,
            None => return Ok(None),
        };
        match meta.get(&rtxn, key)? {
            Some(b) if b.len() == 4 => Ok(Some(u32::from_le_bytes(b.try_into().unwrap()))),
            Some(_) => anyhow::bail!("meta u32 corrupt for key {key:?}"),
            None => Ok(None),
        }
    }

    fn set_meta_u32(&self, key: &[u8], value: u32) -> Result<()> {
        self.put_meta(key, &value.to_le_bytes())
    }

    fn set_meta_u64(&self, key: &[u8], value: u64) -> Result<()> {
        self.put_meta(key, &value.to_le_bytes())
    }

    fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<()> {
        let env = &self.env;
        let mut wtxn = env.write_txn()?;
        let meta: Database<Bytes, Bytes> = env.create_database(&mut wtxn, Some("meta"))?;
        meta.put(&mut wtxn, key, value)?;
        wtxn.commit()?;
        Ok(())
    }
}

// ---- free helpers -------------------------------------------------------

fn stamps_db(module_name: &str) -> String {
    format!("stamps:{module_name}")
}

fn data_db(module_name: &str) -> String {
    format!("data:{module_name}")
}

fn module_version_key(module_name: &str) -> Vec<u8> {
    let mut k = META_MODULE_VERSION_PREFIX.to_vec();
    k.extend_from_slice(module_name.as_bytes());
    k
}

/// Compose the stored data key: `{rel_path}\0{suffix}`.
fn compose_key(rel_path: &[u8], suffix: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(rel_path.len() + 1 + suffix.len());
    key.extend_from_slice(rel_path);
    key.push(KEY_SEP);
    key.extend_from_slice(suffix);
    key
}

fn is_map_full(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(heed::Error::Mdb(MdbError::MapFull)) = cause.downcast_ref::<heed::Error>() {
            return true;
        }
    }
    false
}

/// Names of all named databases, read from the unnamed root DB.
fn named_dbs(env: &Env) -> Result<Vec<String>> {
    let rtxn = env.read_txn()?;
    let root: Database<Bytes, Bytes> = match env.open_database(&rtxn, None)? {
        Some(db) => db,
        None => return Ok(Vec::new()),
    };
    let mut names = Vec::new();
    for kv in root.iter(&rtxn)? {
        let (k, _) = kv?;
        if let Ok(name) = std::str::from_utf8(k) {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

/// Delete cache subdirs whose `meta.last_opened` is older than [`GC_MAX_AGE`].
/// A dir we can't read `last_opened` from is left alone (conservative). Never
/// panics; individual failures are logged and skipped.
pub(crate) fn gc_stale_dirs() {
    let root = cache_root();
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(_) => return, // no cache yet
    };
    let now_secs = system_time_parts(SystemTime::now()).0;
    let max_age = GC_MAX_AGE.as_secs();

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        match read_last_opened(&path) {
            Some(last) if now_secs.saturating_sub(last) > max_age => {
                log::info!("gc: removing stale index dir {path:?}");
                if let Err(e) = std::fs::remove_dir_all(&path) {
                    log::warn!("gc: failed to remove {path:?}: {e}");
                }
            }
            _ => {}
        }
    }
}

/// Best-effort read of `meta.last_opened` (seconds since epoch) for GC. Any
/// open/read failure returns `None` so a broken dir is skipped, not deleted.
fn read_last_opened(dir: &Path) -> Option<u64> {
    let env = open_env(dir, INITIAL_MAP_SIZE).ok()?;
    let rtxn = env.read_txn().ok()?;
    let meta: Database<Bytes, Bytes> = env.open_database(&rtxn, Some("meta")).ok()??;
    let bytes = meta.get(&rtxn, META_LAST_OPENED).ok()??;
    let arr: [u8; 8] = bytes.try_into().ok()?;
    Some(u64::from_le_bytes(arr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    /// Point HOME at a tempdir so the cache lands in an isolated place, then run
    /// `f`. HOME is process-global; tests here run serially via a mutex.
    fn with_home<R>(home: &Path, f: impl FnOnce() -> R) -> R {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap();
        let prev = std::env::var_os("HOME");
        // SAFETY: guarded by LOCK; no other thread reads HOME concurrently here.
        unsafe { std::env::set_var("HOME", home) };
        let out = f();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        out
    }

    fn stamp(mtime_secs: u64, size: u64, hash: Option<[u8; 32]>) -> Stamp {
        Stamp {
            mtime: UNIX_EPOCH + Duration::from_secs(mtime_secs),
            size,
            hash,
        }
    }

    #[test]
    fn stamp_roundtrips_with_and_without_hash() {
        let a = stamp(1_700_000_000, 4096, Some([7u8; 32]));
        let b = stamp(123, 0, None);
        assert_eq!(Stamp::decode(&a.encode()).unwrap(), a);
        assert_eq!(Stamp::decode(&b.encode()).unwrap(), b);
    }

    #[test]
    fn open_creates_dir_and_survives_reopen() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        with_home(home.path(), || {
            let store = IndexStore::open(project.path()).unwrap();
            drop(store);
            // Reopen the same project: no panic, schema version already set.
            let store2 = IndexStore::open(project.path()).unwrap();
            assert_eq!(
                store2.get_meta_u32(META_SCHEMA_VERSION).unwrap(),
                Some(STORE_VERSION)
            );
            // The per-project cache dir exists under HOME/.cache/faber/index.
            let dir = cache_root().join(project_dir_name(project.path()));
            assert!(dir.exists());
        });
    }

    #[test]
    fn write_and_read_back_a_stamp() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        with_home(home.path(), || {
            let store = IndexStore::open(project.path()).unwrap();
            let s = stamp(42, 10, None);
            store
                .write_batch("files", &[(b"src/a.rs".to_vec(), s.clone(), vec![])], false)
                .unwrap();
            let got = store.get_stamp("files", b"src/a.rs").unwrap();
            assert_eq!(got, Some(s));
            assert_eq!(store.get_stamp("files", b"missing").unwrap(), None);
        });
    }

    #[test]
    fn write_batch_then_iterate_data() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        with_home(home.path(), || {
            let store = IndexStore::open(project.path()).unwrap();
            let entries = vec![
                (
                    b"a.rs".to_vec(),
                    stamp(1, 1, Some([1u8; 32])),
                    vec![(b"sym0".to_vec(), b"fn main".to_vec())],
                ),
                (
                    b"b.rs".to_vec(),
                    stamp(2, 2, Some([2u8; 32])),
                    vec![
                        (b"sym0".to_vec(), b"struct X".to_vec()),
                        (b"sym1".to_vec(), b"impl X".to_vec()),
                    ],
                ),
            ];
            store.write_batch("symbols", &entries, true).unwrap();
            store.sync().unwrap();

            // get_data reads a composed key.
            let key = compose_key(b"b.rs", b"sym1");
            assert_eq!(
                store.get_data("symbols", &key).unwrap(),
                Some(b"impl X".to_vec())
            );

            let mut all: Vec<(Vec<u8>, Vec<u8>)> = store
                .iter_data("symbols")
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            all.sort();
            assert_eq!(all.len(), 3);
            // Bytewise-sorted keys: a.rs\0sym0, b.rs\0sym0, b.rs\0sym1.
            assert_eq!(all[0].0, compose_key(b"a.rs", b"sym0"));
            assert_eq!(all[2].0, compose_key(b"b.rs", b"sym1"));
        });
    }

    #[test]
    fn delete_file_removes_stamp_and_all_data() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        with_home(home.path(), || {
            let store = IndexStore::open(project.path()).unwrap();
            let entries = vec![(
                b"a.rs".to_vec(),
                stamp(1, 1, None),
                vec![
                    (b"k0".to_vec(), b"v0".to_vec()),
                    (b"k1".to_vec(), b"v1".to_vec()),
                ],
            )];
            store.write_batch("m", &entries, false).unwrap();
            store.delete_file("m", b"a.rs").unwrap();

            assert_eq!(store.get_stamp("m", b"a.rs").unwrap(), None);
            let left: Vec<_> = store.iter_data("m").unwrap().collect();
            assert!(left.is_empty());
        });
    }

    #[test]
    fn module_version_set_get_and_drop() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        with_home(home.path(), || {
            let store = IndexStore::open(project.path()).unwrap();
            assert_eq!(store.module_version("symbols").unwrap(), None);
            store.set_module_version("symbols", 3).unwrap();
            assert_eq!(store.module_version("symbols").unwrap(), Some(3));

            store
                .write_batch(
                    "symbols",
                    &[(
                        b"a.rs".to_vec(),
                        stamp(1, 1, None),
                        vec![(b"k".to_vec(), b"v".to_vec())],
                    )],
                    false,
                )
                .unwrap();

            store.drop_module("symbols").unwrap();
            assert_eq!(store.module_version("symbols").unwrap(), None);
            assert!(store.iter_data("symbols").unwrap().next().is_none());
            assert_eq!(store.get_stamp("symbols", b"a.rs").unwrap(), None);
        });
    }

    #[test]
    fn version_mismatch_triggers_clean_rebuild() {
        // Simulate an engine-driven per-module version bump: the engine records
        // v1, writes data, then discovers the module now reports v2 and drops it.
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        with_home(home.path(), || {
            let store = IndexStore::open(project.path()).unwrap();
            store.set_module_version("files", 1).unwrap();
            store
                .write_batch(
                    "files",
                    &[(b"x".to_vec(), stamp(1, 1, None), vec![])],
                    false,
                )
                .unwrap();
            assert!(store.get_stamp("files", b"x").unwrap().is_some());

            // Engine sees recorded(1) != current(2) -> drop + rebuild path.
            if store.module_version("files").unwrap() != Some(2) {
                store.drop_module("files").unwrap();
            }
            assert_eq!(store.module_version("files").unwrap(), None);
            assert!(store.get_stamp("files", b"x").unwrap().is_none());

            // Clean rebuild at the new version.
            store.set_module_version("files", 2).unwrap();
            store
                .write_batch(
                    "files",
                    &[(b"x".to_vec(), stamp(9, 9, None), vec![])],
                    false,
                )
                .unwrap();
            assert_eq!(store.module_version("files").unwrap(), Some(2));
            assert_eq!(
                store.get_stamp("files", b"x").unwrap().unwrap().mtime,
                stamp(9, 9, None).mtime
            );
        });
    }

    #[test]
    fn schema_version_bump_resets_store() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        with_home(home.path(), || {
            let store = IndexStore::open(project.path()).unwrap();
            store
                .write_batch(
                    "files",
                    &[(b"x".to_vec(), stamp(1, 1, None), vec![])],
                    false,
                )
                .unwrap();
            store.set_module_version("files", 1).unwrap();
            // Force an older recorded schema; reopen must wipe module data.
            store
                .set_meta_u32(META_SCHEMA_VERSION, STORE_VERSION - 1)
                .unwrap();
            drop(store);

            let store2 = IndexStore::open(project.path()).unwrap();
            assert_eq!(
                store2.get_meta_u32(META_SCHEMA_VERSION).unwrap(),
                Some(STORE_VERSION)
            );
            assert_eq!(store2.get_stamp("files", b"x").unwrap(), None);
            assert_eq!(store2.module_version("files").unwrap(), None);
        });
    }

    #[test]
    fn corrupt_env_recovers_to_fresh_store() {
        // Real-world corruption is left behind by a *previous* process/crash: the
        // current process opens the bad file cold. We reproduce that by writing a
        // corrupt `data.mdb` directly, so `IndexStore::open` is the first heed open
        // of the path (heed caches open envs process-globally by canonical path, so
        // corrupting a file it has already mapped this run would be an unrepresentable
        // scenario that LMDB aborts on rather than surfacing as a Result).
        //
        // A zeroed, valid-length `data.mdb` fails `mdb_env_read_header` with
        // MDB_INVALID at open — a catchable error, no mmap fault. Our recovery must
        // wipe the dir and return a fresh, writable env.
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        with_home(home.path(), || {
            let dir = cache_root().join(project_dir_name(project.path()));
            std::fs::create_dir_all(&dir).unwrap();
            // 32 KiB of zeros: LMDB reads page 0 as non-meta -> MDB_INVALID at open.
            std::fs::write(dir.join("data.mdb"), vec![0u8; 32 * 1024]).unwrap();

            // Must not panic; wipes and returns a fresh empty, writable env.
            let store = IndexStore::open(project.path()).unwrap();
            assert_eq!(store.get_stamp("files", b"x").unwrap(), None);
            store
                .write_batch(
                    "files",
                    &[(b"y".to_vec(), stamp(2, 2, None), vec![])],
                    false,
                )
                .unwrap();
            assert!(store.get_stamp("files", b"y").unwrap().is_some());
            // The rebuild reason is recorded for diagnosability.
            let env = &store.env;
            let rtxn = env.read_txn().unwrap();
            let meta: Database<Bytes, Bytes> =
                env.open_database(&rtxn, Some("meta")).unwrap().unwrap();
            assert_eq!(
                meta.get(&rtxn, META_LAST_REBUILD_REASON).unwrap(),
                Some(b"corrupt env: reset".as_slice())
            );
        });
    }

    #[test]
    fn gc_deletes_old_dirs_keeps_recent() {
        use filetime::{FileTime, set_file_mtime};

        let home = TempDir::new().unwrap();
        let recent = TempDir::new().unwrap();
        let old = TempDir::new().unwrap();

        with_home(home.path(), || {
            // Create two project stores; touch_last_opened stamps "now".
            let s_recent = IndexStore::open(recent.path()).unwrap();
            s_recent.touch_last_opened().unwrap();
            drop(s_recent);

            let s_old = IndexStore::open(old.path()).unwrap();
            // Backdate last_opened well beyond the GC window (40 days).
            let old_secs = system_time_parts(SystemTime::now()).0 - 40 * 24 * 60 * 60;
            s_old.set_meta_u64(META_LAST_OPENED, old_secs).unwrap();
            s_old.sync().unwrap();
            drop(s_old);

            let recent_dir = cache_root().join(project_dir_name(recent.path()));
            let old_dir = cache_root().join(project_dir_name(old.path()));
            // Also age the dir's filesystem mtime for good measure (not used by GC
            // logic, but keeps the fixture honest).
            let old_ft = FileTime::from_unix_time(old_secs as i64, 0);
            let _ = set_file_mtime(&old_dir, old_ft);

            assert!(recent_dir.exists());
            assert!(old_dir.exists());

            // Trigger GC directly (bypasses the process-level OnceLock in open()).
            gc_stale_dirs();

            assert!(recent_dir.exists(), "recent dir must survive GC");
            assert!(!old_dir.exists(), "old dir must be GC'd");
        });
    }

    #[test]
    fn map_full_helper_matches_only_map_full() {
        let full = anyhow::Error::new(heed::Error::Mdb(MdbError::MapFull));
        assert!(is_map_full(&full));
        let other = anyhow::Error::new(heed::Error::Mdb(MdbError::NotFound));
        assert!(!is_map_full(&other));
        let plain = anyhow::anyhow!("unrelated");
        assert!(!is_map_full(&plain));
    }

    #[test]
    fn write_batch_grows_map_when_full_and_succeeds() {
        // Open with a deliberately tiny map (a few OS pages), then write a batch
        // whose values dwarf it. The first commit hits MDB_MAPFULL; `write_batch`
        // doubles the map and retries until it fits, then the data reads back.
        let dir = TempDir::new().unwrap();
        let start = 512 * 1024; // 512 KiB: enough for the empty env, too small for the batch.
        let store = IndexStore::open_with_map(dir.path(), start).unwrap();

        // ~3 MiB of values across 6 files — well past 512 KiB, forcing several grows.
        let big = vec![0xABu8; 512 * 1024];
        let entries: Vec<FileBatchEntry> = (0..6u8)
            .map(|i| {
                (
                    vec![b'f', i],
                    stamp(i as u64, big.len() as u64, None),
                    vec![(b"blob".to_vec(), big.clone())],
                )
            })
            .collect();

        store.write_batch("m", &entries, false).unwrap();
        assert!(
            *store.map_size.read().unwrap() > start,
            "map must have grown"
        );

        // All entries survive the resize+retry.
        let key = compose_key(&[b'f', 3], b"blob");
        assert_eq!(store.get_data("m", &key).unwrap(), Some(big));
        assert_eq!(store.iter_data("m").unwrap().count(), 6);
    }
}
