pub mod edit;
pub mod parse;

pub use parse::parse_markdown;

use crate::highlight::HighlightSpan;
pub use crate::outline::OutlineItem;

/// Top-level block (corresponds roughly to a CommonMark block-level element).
#[derive(Debug, Clone)]
pub struct Block {
    pub kind: BlockKind,
    /// Rope line range (start..end) covered by this block.
    /// Monotonically non-decreasing — the scroll-sync invariant.
    pub source_lines: std::ops::Range<usize>,
}

#[derive(Debug, Clone)]
pub enum BlockKind {
    Heading {
        level: u8,
        inlines: Vec<InlineRun>,
    },
    Paragraph {
        inlines: Vec<InlineRun>,
    },
    CodeBlock {
        lang: Option<String>,
        text: String,
        /// Per-line highlight spans (same type as the editor's highlight cache).
        highlights: Vec<Vec<HighlightSpan>>,
    },
    Blockquote {
        children: Vec<Block>,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<ListItem>,
    },
    Table {
        head: Vec<Vec<InlineRun>>,
        rows: Vec<Vec<Vec<InlineRun>>>,
    },
    Rule,
    HtmlBlock {
        text: String,
    },
}

#[derive(Debug, Clone)]
pub struct ListItem {
    pub task: Option<bool>,
    /// Rope line of the `- [ ]` / `- [x]` marker — for checkbox toggle.
    pub task_source_line: Option<usize>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Default)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
}

#[derive(Debug, Clone)]
pub enum InlineRun {
    Text {
        text: String,
        style: InlineStyle,
        link: Option<String>,
    },
    Image {
        alt: String,
        dest: String,
    },
    SoftBreak,
    HardBreak,
}

/// A parsed markdown document ready for the preview view.
#[derive(Debug, Clone)]
pub struct MarkdownDoc {
    pub blocks: Vec<Block>,
    pub outline: Vec<OutlineItem>,
}
