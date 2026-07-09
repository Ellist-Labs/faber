//! Dump a project's persisted index for inspection.
//!
//! Usage: `cargo run -p faber-index --example dump -- <project-root>`
//!
//! Prints, per module, every stamp and every data-entry key (not values).

use std::path::PathBuf;

use faber_index::store::IndexStore;

fn main() -> anyhow::Result<()> {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("usage: dump <project-root>"))?;

    let store = IndexStore::open(&root)?;
    println!("index for {}", root.display());

    // The store's DB names aren't publicly enumerable, so probe the known v1
    // module names plus any the caller passes via FABER_DUMP_MODULES (comma-sep).
    let mut modules = vec!["files".to_string(), "symbols".to_string()];
    if let Ok(extra) = std::env::var("FABER_DUMP_MODULES") {
        modules.extend(
            extra
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
    }
    modules.sort();
    modules.dedup();

    for module in &modules {
        let stamps: Vec<_> = store.iter_stamps(module)?.collect::<Result<_, _>>()?;
        let data: Vec<_> = store.iter_data(module)?.collect::<Result<_, _>>()?;
        if stamps.is_empty() && data.is_empty() {
            continue;
        }
        println!("\n== module: {module} ==");
        println!("  version: {:?}", store.module_version(module)?);

        println!("  stamps: {}", stamps.len());
        for (rel_path, stamp) in &stamps {
            println!(
                "    {}  size={} mtime={:?} hash={}",
                display_key(rel_path),
                stamp.size,
                stamp.mtime,
                stamp.hash.map(|_| "yes").unwrap_or("no"),
            );
        }

        println!("  data entries: {}", data.len());
        for (key, value) in &data {
            println!("    key={}  ({} bytes)", display_key(key), value.len());
        }
    }

    Ok(())
}

/// Render a possibly-non-UTF8 key with the `\0` separator made visible.
fn display_key(key: &[u8]) -> String {
    key.iter()
        .map(|&b| match b {
            0 => "\\0".to_string(),
            0x20..=0x7e => (b as char).to_string(),
            other => format!("\\x{other:02x}"),
        })
        .collect()
}
