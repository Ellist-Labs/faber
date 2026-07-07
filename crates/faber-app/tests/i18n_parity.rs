/// Guardrail: every locale file must have exactly the same keys as en.toml.
/// Vacuously passes with one locale; enforces parity the moment a second is added.
use std::collections::BTreeSet;
use std::path::Path;

fn flatten_keys(table: &toml::Value, prefix: &str, out: &mut BTreeSet<String>) {
    if let toml::Value::Table(t) = table {
        for (k, v) in t {
            let full = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            if matches!(v, toml::Value::Table(_)) {
                flatten_keys(v, &full, out);
            } else {
                out.insert(full);
            }
        }
    }
}

fn load_keys(path: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("can't read {}: {e}", path.display()));
    let val: toml::Value = toml::from_str(&text)
        .unwrap_or_else(|e| panic!("invalid TOML in {}: {e}", path.display()));
    let mut keys = BTreeSet::new();
    flatten_keys(&val, "", &mut keys);
    keys
}

#[test]
fn all_locales_match_english_keys() {
    let locales_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
    let en_keys = load_keys(&locales_dir.join("en.toml"));

    let extra: Vec<_> = std::fs::read_dir(&locales_dir)
        .expect("locales/ must exist")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().map_or(false, |e| e == "toml")
                && p.file_stem().map_or(true, |s| s != "en")
        })
        .collect();

    for locale_path in extra {
        let locale_keys = load_keys(&locale_path);
        let missing: Vec<_> = en_keys.difference(&locale_keys).collect();
        let extra_keys: Vec<_> = locale_keys.difference(&en_keys).collect();
        assert!(
            missing.is_empty() && extra_keys.is_empty(),
            "{} has key mismatches vs en.toml.\n  Missing: {missing:?}\n  Extra: {extra_keys:?}",
            locale_path.display()
        );
    }
}

#[test]
fn english_keys_non_empty() {
    let locales_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
    let en_keys = load_keys(&locales_dir.join("en.toml"));
    assert!(!en_keys.is_empty(), "en.toml must contain translation keys");
}
