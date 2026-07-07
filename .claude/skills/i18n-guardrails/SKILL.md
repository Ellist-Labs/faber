# i18n Guardrails for Faber

## Rule
Every user-facing string in `crates/faber-app/` must go through `t!("namespace.key")`.
Adding a string = adding a key to `crates/faber-app/locales/en.toml` first.

## What NOT to translate
- App name literals: `"Faber"` (proper noun)
- GPUI element IDs (`.id("some-id")`)
- Log/stderr messages (`eprintln!`, `println!`)
- Serde keys / config values (e.g. `"afterDelay"`, `"en"`)
- Symbolic UI chips: `"Aa"`, `"W"`, `".*"`
- Numeric format strings: `"{}/{}"` in match counters

## Checker

Run `check.sh` from the repo root to flag suspect literals in `faber-app/src/`:

```sh
bash .claude/skills/i18n-guardrails/check.sh
```

### Automated tests (always run before committing)

```sh
cargo test -p faber-settings          # Language detection + settings roundtrip
cargo test -p faber --test i18n_parity  # Key parity across all locale files
```

## Adding a new locale

1. Copy `crates/faber-app/locales/en.toml` → e.g. `pt-BR.toml`.
2. Translate every value (keep all keys identical to `en.toml`).
3. Run `cargo test -p faber --test i18n_parity` — must pass.
4. Add `Language::PtBr` to `faber_settings::Language` (+ `code`, `autonym`, `key`, `from_locale`).
5. Add to `Language::SUPPORTED`. The settings picker auto-populates.

## Live locale switching

`Settings.language` → `apply_change` → `crate::i18n::apply(cx)` →
`rust_i18n::set_locale` + `register_menus` + `apply_settings` → `refresh_windows`.

All GPUI views read `t!()` during their `Render::render` call, so switching locale
triggers a full repaint with the new strings automatically.
