//! Symbol outline types.
//!
//! The tree-sitter outline engine (`OutlineItem`, `Outline`, `OutlineCache`)
//! lives in `faber-lang`; it is re-exported here so existing editor consumers
//! (and the markdown outline path, which uses `MarkdownDoc` from this crate)
//! keep a single import surface.
pub use faber_lang::{Outline, OutlineCache, OutlineItem};

#[cfg(test)]
mod tests {
    use crate::buffer::Document;
    use faber_lang::LanguageRegistry;
    use std::path::Path;

    fn rust_doc(src: &str) -> Document {
        let reg = LanguageRegistry::with_defaults();
        let lang = reg.language_for_path(Path::new("foo.rs")).unwrap();
        Document::from_str(src, Some(&lang))
    }

    #[test]
    fn basic_fn_depth_zero() {
        let doc = rust_doc("fn hello() {}");
        let items = &doc.outline.items;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "hello");
        assert_eq!(items[0].depth, 0);
        assert_eq!(items[0].context.as_deref(), Some("fn"));
    }

    #[test]
    fn impl_with_methods() {
        let src = "struct Foo; impl Foo { fn a() {} fn b() {} }";
        let doc = rust_doc(src);
        let items = &doc.outline.items;
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"Foo"), "struct expected");
        assert!(names.contains(&"a"), "method a expected");
        assert!(names.contains(&"b"), "method b expected");
        let impl_depth = items
            .iter()
            .find(|i| i.name == "Foo" && i.context.as_deref() == Some("impl"))
            .map(|i| i.depth)
            .unwrap_or(99);
        let a_depth = items
            .iter()
            .find(|i| i.name == "a")
            .map(|i| i.depth)
            .unwrap_or(99);
        assert!(a_depth > impl_depth, "method should be deeper than impl");
    }

    #[test]
    fn mod_containing_struct_and_fn() {
        let src = "mod app { struct S; impl S { fn bar() {} } fn free() {} }";
        let doc = rust_doc(src);
        let items = &doc.outline.items;
        let depth_of = |name: &str| items.iter().find(|i| i.name == name).map(|i| i.depth);
        assert_eq!(depth_of("app"), Some(0), "mod at depth 0");
        assert!(depth_of("S").unwrap_or(0) > 0, "struct inside mod");
        assert!(
            depth_of("bar").unwrap_or(0) > depth_of("S").unwrap_or(0),
            "fn deeper than struct"
        );
        assert_eq!(
            depth_of("free"),
            depth_of("S"),
            "free fn and struct at same depth"
        );
    }

    #[test]
    fn nested_fns() {
        let src = "fn outer() { fn inner() {} }";
        let doc = rust_doc(src);
        let items = &doc.outline.items;
        let outer = items.iter().find(|i| i.name == "outer").map(|i| i.depth);
        let inner = items.iter().find(|i| i.name == "inner").map(|i| i.depth);
        assert!(
            inner.unwrap_or(0) > outer.unwrap_or(99),
            "inner fn deeper than outer"
        );
    }
}
