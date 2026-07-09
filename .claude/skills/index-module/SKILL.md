# Adding a new IndexModule

## When to use this

Any project-wide computation (symbols, git status, diagnostics, dependency graph…) must be implemented as an `IndexModule` in `faber-index`, not as a bespoke scanner. This keeps scanning, caching, and threading in one place.

## Trait contract

```rust
pub trait IndexModule: Send + Sync + 'static {
    type Snapshot: Send + Sync + 'static;
    fn name(&self) -> &'static str;   // stable; names the LMDB sub-DBs — never rename
    fn version(&self) -> u32;         // bump → rebuild this module only on next start
    fn needs(&self) -> InputNeeds;    // META | TEXT | SYNTAX — only request what you use
    fn accepts(&self, meta: &FileMeta) -> bool;
    fn index(&self, input: &FileInput) -> anyhow::Result<Vec<(KeySuffix, Vec<u8>)>>;
    fn publish(&self, entries: &mut dyn Iterator<Item = (&[u8], &[u8])>)
        -> anyhow::Result<Self::Snapshot>;
}
```

## Purity rule

`index()` must be a pure function of `FileInput`. No filesystem reads, no globals, no state mutation. This keeps modules independently rebuildable and table-testable.

## Key suffixes

Modules return key *suffixes*. The engine composes stored keys as `{rel_path_bytes}\0{suffix}`. Use short, stable suffixes (e.g. `b"outline"`, `b"meta"`). Multiple entries per file are fine (different suffixes).

## InputNeeds

- `META` only: the module runs synchronously right after the tree walk, before any file reads. The file list is available seconds after folder open (not minutes). Use for anything derivable from path, size, mtime, language.
- `TEXT | SYNTAX`: runs in the content pipeline. `FileInput.text` and `.syntax` are populated. Binary files (first 8KB NUL sniff) skip the content phase entirely.

## Snapshot design

- META-only modules: materialize a full snapshot (it's cheap — no I/O, no parsing cost).
- Content modules: **do NOT materialize all entries into memory**. Use a thin snapshot (generation counter) and query LMDB at read time via `IndexStore::get_data` / `IndexStore::iter_data`. Materializing 200K files of symbols costs hundreds of MB and negates the zero-copy benefit.

## Registration

```rust
// In faber-app, before engine.start():
let my_handle: SnapshotHandle<MySnapshot> = engine.register(MyModule::new(registry.clone()));
// Store the handle somewhere the query code can read it.
```

## Version bumping

Increment `version()` whenever the stored format or indexing logic changes. The engine drops only this module's `stamps:name` and `data:name` DBs and rebuilds. Other modules are unaffected.

## Testing pattern

Modules are pure — test `index()` directly with synthetic `FileInput`:

```rust
#[test]
fn my_module_indexes_correctly() {
    let meta = FileMeta { rel_path: b"src/lib.rs".into(), size: 100, mtime: SystemTime::now(),
                          is_ignored: false, language: Some(LanguageId::new("rust")) };
    let module = MyModule::new(/* deps */);
    let input = FileInput { meta: &meta, text: Some("fn foo() {}"), syntax: None };
    let kvs = module.index(&input).unwrap();
    assert!(!kvs.is_empty());

    // Test publish by piping index() output through the iterator:
    let mut iter = kvs.iter().map(|(k, v)| (k.as_slice(), v.as_slice()));
    let snap = module.publish(&mut iter).unwrap();
    // assert on snap fields
}
```

For store round-trips, use `tempfile::TempDir` and `IndexStore::open`:

```rust
let dir = tempfile::tempdir().unwrap();
let store = IndexStore::open(dir.path()).unwrap();
// write_batch → symbols_for / get_data assertions
```

## Checklist

- [ ] `name()` is a stable `&'static str` (if you rename it, the old DB names linger)
- [ ] `version()` bumped from 1 only when format changes
- [ ] `needs()` returns the minimum required flags
- [ ] `index()` has no side effects
- [ ] Key suffixes are unique within the module
- [ ] Content modules use a thin `Snapshot` (no bulk materialization)
- [ ] At least one test on `index()` + `publish()` without a store
- [ ] Module registered in `faber-app` before `engine.start()`
- [ ] If TEXT or SYNTAX: `accepts()` guards against unsupported languages
