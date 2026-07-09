//! Language support: grammar loading, `Language`, `LanguageRegistry`.
//!
//! Adding a new language = call `LanguageRegistry::register`; no changes to
//! core or editor required (Strategy + Registry pattern).

pub mod language;
pub mod registry;

pub use language::{
    Grammar, HighlightConfig, Language, LanguageId, Outline, OutlineCache, OutlineConfig,
    OutlineItem, SyntaxToken, capture_name_to_token,
};
pub use registry::LanguageRegistry;
