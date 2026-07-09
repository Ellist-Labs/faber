//! Quick probe: run the engine on a real project and print how many files were indexed.
//!
//! Usage: cargo run -p faber-index --example probe -- <project-root>

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use faber_index::{
    engine::{FilesModule, IndexEngine},
    module::ModuleState,
    trigger::IndexTrigger,
};
use faber_lang::LanguageRegistry;

fn main() -> anyhow::Result<()> {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("usage: probe <project-root>"))?;

    eprintln!("probing: {}", root.display());

    let registry = Arc::new(LanguageRegistry::with_defaults());
    let mut engine = IndexEngine::new(root, registry)?;
    let files = engine.register(FilesModule);
    let engine = Arc::new(engine);
    engine.clone().start();
    engine.request(IndexTrigger::FolderOpened);

    eprintln!("engine started; waiting for files module...");

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match files.state() {
            ModuleState::Ready { .. } => break,
            ModuleState::Building { done, total } => {
                eprintln!("  building: {done}/{total}");
            }
            ModuleState::Cold => {
                eprintln!("  cold...");
            }
        }
        if Instant::now() > deadline {
            eprintln!("TIMEOUT: module never became Ready");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let snap = files.load().expect("snapshot published");
    eprintln!("files indexed: {}", snap.entries.len());
    for e in snap.entries.iter().take(10) {
        eprintln!("  {}", e.rel_path);
    }
    if snap.entries.len() > 10 {
        eprintln!("  ... ({} total)", snap.entries.len());
    }
    Ok(())
}
