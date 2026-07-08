use std::path::Path;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser as CmarkParser, Tag, TagEnd};
use ropey::Rope;

use faber_lang::LanguageRegistry;

use super::{Block, BlockKind, InlineRun, InlineStyle, ListItem, MarkdownDoc};
use crate::highlight::{HighlightCache, HighlightSpan};
use crate::outline::OutlineItem;

/// Parse a markdown string into a `MarkdownDoc`.
///
/// `rope` is used to convert byte offsets to line numbers for scroll sync.
/// `registry` resolves fenced-code-block language tags for syntax highlighting.
pub fn parse_markdown(source: &str, rope: &Rope, registry: &LanguageRegistry) -> MarkdownDoc {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;

    let parser = CmarkParser::new_ext(source, options).into_offset_iter();
    let mut ctx = ParseCtx {
        source,
        rope,
        registry,
        outline: Vec::new(),
        block_ix_counter: 0,
    };

    let blocks = collect_blocks(parser.collect::<Vec<_>>().as_slice(), &mut ctx);
    MarkdownDoc {
        blocks,
        outline: ctx.outline,
    }
}

// ── internal helpers ─────────────────────────────────────────────────────────

struct ParseCtx<'a> {
    source: &'a str,
    rope: &'a Rope,
    registry: &'a LanguageRegistry,
    outline: Vec<OutlineItem>,
    block_ix_counter: usize,
}

impl<'a> ParseCtx<'a> {
    fn byte_to_line(&self, byte: usize) -> usize {
        self.rope.byte_to_line(byte.min(self.source.len()))
    }

    fn source_lines(&self, range: &std::ops::Range<usize>) -> std::ops::Range<usize> {
        let start = self.byte_to_line(range.start);
        let end = self.byte_to_line(range.end.saturating_sub(1)).max(start);
        start..end + 1
    }
}

/// Collect a flat sequence of (Event, Range) pairs into a Vec<Block>.
/// Consumes pairs starting at `pos` up to (but not including) a closing tag
/// that matches the given nesting depth for lists/blockquotes.
fn collect_blocks(
    events: &[(Event<'_>, std::ops::Range<usize>)],
    ctx: &mut ParseCtx<'_>,
) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut i = 0;

    while i < events.len() {
        let (event, range) = &events[i];
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let source_lines = ctx.source_lines(range);
                // Collect inline events up to the matching End.
                let (inlines, skip) =
                    collect_inlines_until(&events[i + 1..], TagEnd::Heading(*level));
                let level_u8 = heading_level_u8(*level);
                let text = inline_text(&inlines);
                let block_ix = ctx.block_ix_counter;
                ctx.block_ix_counter += 1;
                // depth = level - 1: H1→0, H2→1, …
                // context = "#" repeated level times, matching the markdown visual style.
                // end_line = source_line: heading is a single line; section extent is
                // computed at overlay-hover time using the full outline.
                ctx.outline.push(OutlineItem {
                    depth: (level_u8 - 1) as u32,
                    name: text.clone(),
                    context: Some("#".repeat(level_u8 as usize)),
                    source_line: source_lines.start,
                    end_line: source_lines.start,
                    byte_range: range.clone(),
                    block_ix: Some(block_ix),
                });
                blocks.push(Block {
                    kind: BlockKind::Heading {
                        level: level_u8,
                        inlines,
                    },
                    source_lines,
                });
                i += 1 + skip + 1; // start + inlines + end tag
            }

            Event::Start(Tag::Paragraph) => {
                let source_lines = ctx.source_lines(range);
                let (inlines, skip) = collect_inlines_until(&events[i + 1..], TagEnd::Paragraph);
                blocks.push(Block {
                    kind: BlockKind::Paragraph { inlines },
                    source_lines,
                });
                ctx.block_ix_counter += 1;
                i += 1 + skip + 1;
            }

            Event::Start(Tag::CodeBlock(kind)) => {
                let source_lines = ctx.source_lines(range);
                let lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(s) if !s.is_empty() => {
                        Some(s.to_string())
                    }
                    _ => None,
                };
                // Consume Text events until End.
                let (text, skip) = collect_code_text(&events[i + 1..]);
                let highlights = highlight_code(&text, lang.as_deref(), ctx.registry);
                blocks.push(Block {
                    kind: BlockKind::CodeBlock {
                        lang,
                        text,
                        highlights,
                    },
                    source_lines,
                });
                ctx.block_ix_counter += 1;
                i += 1 + skip + 1;
            }

            Event::Start(Tag::BlockQuote(_)) => {
                let source_lines = ctx.source_lines(range);
                // Find the matching BlockQuote end.
                let (inner, skip) = extract_nested(&events[i + 1..], TagEnd::BlockQuote(None));
                let children = collect_blocks(inner, ctx);
                blocks.push(Block {
                    kind: BlockKind::Blockquote { children },
                    source_lines,
                });
                ctx.block_ix_counter += 1;
                i += 1 + skip + 1;
            }

            Event::Start(Tag::List(start_num)) => {
                let source_lines = ctx.source_lines(range);
                let ordered = start_num.is_some();
                let start = start_num.unwrap_or(1);
                let (inner, skip) = extract_nested(&events[i + 1..], TagEnd::List(ordered));
                let items = collect_list_items(inner, ctx);
                blocks.push(Block {
                    kind: BlockKind::List {
                        ordered,
                        start,
                        items,
                    },
                    source_lines,
                });
                ctx.block_ix_counter += 1;
                i += 1 + skip + 1;
            }

            Event::Start(Tag::Table(alignments)) => {
                let source_lines = ctx.source_lines(range);
                let (inner, skip) = extract_nested(&events[i + 1..], TagEnd::Table);
                let (head, rows) = parse_table(inner, alignments.len());
                blocks.push(Block {
                    kind: BlockKind::Table { head, rows },
                    source_lines,
                });
                ctx.block_ix_counter += 1;
                i += 1 + skip + 1;
            }

            Event::Rule => {
                let source_lines = ctx.source_lines(range);
                blocks.push(Block {
                    kind: BlockKind::Rule,
                    source_lines,
                });
                ctx.block_ix_counter += 1;
                i += 1;
            }

            Event::Html(text) => {
                let source_lines = ctx.source_lines(range);
                blocks.push(Block {
                    kind: BlockKind::HtmlBlock {
                        text: text.to_string(),
                    },
                    source_lines,
                });
                ctx.block_ix_counter += 1;
                i += 1;
            }

            // Bare inline events (tight-list items: pulldown-cmark suppresses
            // the wrapping Paragraph for tight lists, emitting raw inlines).
            // Collect a maximal inline run and synthesize an implicit Paragraph.
            Event::Text(_)
            | Event::Code(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::Start(
                Tag::Emphasis
                | Tag::Strong
                | Tag::Strikethrough
                | Tag::Link { .. }
                | Tag::Image { .. },
            )
            | Event::TaskListMarker(_) => {
                let source_lines = ctx.source_lines(range);
                let (inlines, skip) = collect_inlines(&events[i..], is_block_boundary);
                if !inlines.is_empty() {
                    blocks.push(Block {
                        kind: BlockKind::Paragraph { inlines },
                        source_lines,
                    });
                    ctx.block_ix_counter += 1;
                }
                i += skip.max(1);
            }

            // Skip End tags and anything else at block level.
            _ => {
                i += 1;
            }
        }
    }

    blocks
}

fn heading_level_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Walk inline events until `stop` returns true; return inlines + skip count.
/// `stop` is called on each event *before* it is processed; returning true
/// terminates WITHOUT consuming that event (it stays at index `i`).
fn collect_inlines(
    events: &[(Event<'_>, std::ops::Range<usize>)],
    stop: impl Fn(&Event<'_>) -> bool,
) -> (Vec<InlineRun>, usize) {
    let mut inlines = Vec::new();
    let mut style_stack: Vec<InlineStyle> = vec![InlineStyle::default()];
    let mut link_stack: Vec<String> = Vec::new();
    let mut i = 0;

    while i < events.len() {
        let (event, _) = &events[i];
        if stop(event) {
            return (inlines, i);
        }
        match event {
            Event::Start(Tag::Strong) => {
                let mut s = style_stack.last().cloned().unwrap_or_default();
                s.bold = true;
                style_stack.push(s);
            }
            Event::End(TagEnd::Strong) => {
                style_stack.pop();
            }

            Event::Start(Tag::Emphasis) => {
                let mut s = style_stack.last().cloned().unwrap_or_default();
                s.italic = true;
                style_stack.push(s);
            }
            Event::End(TagEnd::Emphasis) => {
                style_stack.pop();
            }

            Event::Start(Tag::Strikethrough) => {
                let mut s = style_stack.last().cloned().unwrap_or_default();
                s.strike = true;
                style_stack.push(s);
            }
            Event::End(TagEnd::Strikethrough) => {
                style_stack.pop();
            }

            Event::Start(Tag::Link { dest_url, .. }) => {
                link_stack.push(dest_url.to_string());
            }
            Event::End(TagEnd::Link) => {
                link_stack.pop();
            }

            Event::Start(Tag::Image { dest_url, .. }) => {
                let mut alt = String::new();
                i += 1;
                while i < events.len() {
                    match &events[i].0 {
                        Event::End(TagEnd::Image) => break,
                        Event::Text(t) => alt.push_str(t),
                        _ => {}
                    }
                    i += 1;
                }
                inlines.push(InlineRun::Image {
                    alt,
                    dest: dest_url.to_string(),
                });
                i += 1;
                continue;
            }

            Event::Code(text) => {
                let mut s = style_stack.last().cloned().unwrap_or_default();
                s.code = true;
                inlines.push(InlineRun::Text {
                    text: text.to_string(),
                    style: s,
                    link: link_stack.last().cloned(),
                });
            }
            Event::Text(text) => {
                inlines.push(InlineRun::Text {
                    text: text.to_string(),
                    style: style_stack.last().cloned().unwrap_or_default(),
                    link: link_stack.last().cloned(),
                });
            }
            Event::SoftBreak => inlines.push(InlineRun::SoftBreak),
            Event::HardBreak => inlines.push(InlineRun::HardBreak),
            _ => {}
        }
        i += 1;
    }
    (inlines, i)
}

/// Walk inline events until we hit a matching TagEnd; return inlines + skip count.
/// The TagEnd itself is consumed (returned skip points past it).
fn collect_inlines_until(
    events: &[(Event<'_>, std::ops::Range<usize>)],
    end: TagEnd,
) -> (Vec<InlineRun>, usize) {
    let (inlines, stop_ix) = collect_inlines(events, |e| matches!(e, Event::End(t) if *t == end));
    (inlines, stop_ix)
}

/// Returns true for events that mark a block-level boundary.
/// Used as the `stop` predicate when harvesting bare inlines from tight-list items.
fn is_block_boundary(event: &Event<'_>) -> bool {
    matches!(
        event,
        Event::Start(
            Tag::Heading { .. }
                | Tag::Paragraph
                | Tag::CodeBlock(_)
                | Tag::BlockQuote(_)
                | Tag::List(_)
                | Tag::Item
                | Tag::Table(_)
                | Tag::TableHead
                | Tag::TableRow
                | Tag::TableCell
                | Tag::FootnoteDefinition(_)
        ) | Event::End(
            TagEnd::Heading(_)
                | TagEnd::Paragraph
                | TagEnd::CodeBlock
                | TagEnd::BlockQuote(_)
                | TagEnd::List(_)
                | TagEnd::Item
                | TagEnd::Table
                | TagEnd::TableHead
                | TagEnd::TableRow
                | TagEnd::TableCell
                | TagEnd::FootnoteDefinition
        ) | Event::Rule
            | Event::Html(_)
    )
}

/// Collect Text content up to End::CodeBlock.
fn collect_code_text(events: &[(Event<'_>, std::ops::Range<usize>)]) -> (String, usize) {
    let mut text = String::new();
    for (i, (event, _)) in events.iter().enumerate() {
        match event {
            Event::Text(t) => text.push_str(t),
            Event::End(TagEnd::CodeBlock) => return (text, i),
            _ => {}
        }
    }
    (text, events.len())
}

/// Extract a nested slice of events up to the matching TagEnd, respecting nesting.
fn extract_nested<'e, 'a>(
    events: &'e [(Event<'a>, std::ops::Range<usize>)],
    end: TagEnd,
) -> (&'e [(Event<'a>, std::ops::Range<usize>)], usize) {
    let mut depth = 0i32;
    for (i, (event, _)) in events.iter().enumerate() {
        match event {
            // At depth 0, hitting the target end means we found our boundary.
            Event::End(t) if depth == 0 && *t == end => return (&events[..i], i),
            // All other starts increment; all other ends decrement — keeping the
            // counter balanced so non-target ends don't inflate depth.
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            _ => {}
        }
    }
    (events, events.len())
}

fn collect_list_items(
    events: &[(Event<'_>, std::ops::Range<usize>)],
    ctx: &mut ParseCtx<'_>,
) -> Vec<ListItem> {
    let mut items = Vec::new();
    let mut i = 0;
    while i < events.len() {
        let (event, range) = &events[i];
        if let Event::Start(Tag::Item) = event {
            let (inner, skip) = extract_nested(&events[i + 1..], TagEnd::Item);
            let task_source_line = Some(ctx.byte_to_line(range.start));
            // Detect task checkbox from the first event inside.
            let task = inner.first().and_then(|(e, _)| {
                if let Event::TaskListMarker(checked) = e {
                    Some(*checked)
                } else {
                    None
                }
                // `checked` is already a bool value, no deref needed above
            });
            // Skip the TaskListMarker event itself when collecting sub-blocks.
            let inner_for_blocks = if task.is_some() && !inner.is_empty() {
                &inner[1..]
            } else {
                inner
            };
            let blocks = collect_blocks(inner_for_blocks, ctx);
            items.push(ListItem {
                task,
                task_source_line: if task.is_some() {
                    task_source_line
                } else {
                    None
                },
                blocks,
            });
            i += 1 + skip + 1;
        } else {
            i += 1;
        }
    }
    items
}

fn parse_table(
    events: &[(Event<'_>, std::ops::Range<usize>)],
    _col_count: usize,
) -> (Vec<Vec<InlineRun>>, Vec<Vec<Vec<InlineRun>>>) {
    let mut head: Vec<Vec<InlineRun>> = Vec::new();
    let mut rows: Vec<Vec<Vec<InlineRun>>> = Vec::new();
    let mut i = 0;

    while i < events.len() {
        match &events[i].0 {
            Event::Start(Tag::TableHead) => {
                let (inner, skip) = extract_nested(&events[i + 1..], TagEnd::TableHead);
                head = parse_table_row(inner);
                i += 1 + skip + 1;
            }
            Event::Start(Tag::TableRow) => {
                let (inner, skip) = extract_nested(&events[i + 1..], TagEnd::TableRow);
                rows.push(parse_table_row(inner));
                i += 1 + skip + 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    (head, rows)
}

fn parse_table_row(events: &[(Event<'_>, std::ops::Range<usize>)]) -> Vec<Vec<InlineRun>> {
    let mut cells = Vec::new();
    let mut i = 0;
    while i < events.len() {
        if let Event::Start(Tag::TableCell) = &events[i].0 {
            let (inlines, skip) = collect_inlines_until(&events[i + 1..], TagEnd::TableCell);
            cells.push(inlines);
            i += 1 + skip + 1;
        } else {
            i += 1;
        }
    }
    cells
}

fn highlight_code(
    text: &str,
    lang_tag: Option<&str>,
    registry: &LanguageRegistry,
) -> Vec<Vec<HighlightSpan>> {
    let Some(tag) = lang_tag else {
        return empty_line_vecs(text);
    };

    // Map common tags to file extensions.
    let ext = match tag {
        "rust" | "rs" => "rs",
        "python" | "py" => "py",
        "javascript" | "js" => "js",
        "typescript" | "ts" => "ts",
        "markdown" | "md" => "md",
        other => other,
    };

    let Some(lang) = registry.language_for_path(Path::new(&format!("_.{ext}"))) else {
        return empty_line_vecs(text);
    };

    let mut parser = lang.make_parser();
    let tree = match parser.parse(text, None) {
        Some(t) => t,
        None => return empty_line_vecs(text),
    };

    let grammar = std::sync::Arc::new(lang.build_grammar());
    let mut cache = HighlightCache::default();
    cache.setup(Some(&grammar));
    cache.compute(&tree, text);

    if cache.lines.is_empty() {
        empty_line_vecs(text)
    } else {
        cache.lines
    }
}

fn empty_line_vecs(text: &str) -> Vec<Vec<HighlightSpan>> {
    let line_count = text.lines().count().max(1);
    vec![Vec::new(); line_count]
}

fn inline_text(inlines: &[InlineRun]) -> String {
    inlines
        .iter()
        .filter_map(|r| match r {
            InlineRun::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use faber_lang::LanguageRegistry;
    use ropey::Rope;

    fn parse(src: &str) -> MarkdownDoc {
        let rope = Rope::from_str(src);
        let registry = LanguageRegistry::default();
        parse_markdown(src, &rope, &registry)
    }

    fn item_text(item: &ListItem) -> String {
        item.blocks
            .iter()
            .flat_map(|b| match &b.kind {
                BlockKind::Paragraph { inlines } => inlines.iter().collect::<Vec<_>>(),
                _ => vec![],
            })
            .filter_map(|r| match r {
                InlineRun::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn list_does_not_swallow_following_heading() {
        let doc = parse("- a\n\n# H");
        assert_eq!(
            doc.blocks.len(),
            2,
            "expected List + Heading, got {:?} blocks",
            doc.blocks.len()
        );
        assert!(matches!(doc.blocks[0].kind, BlockKind::List { .. }));
        assert!(matches!(
            doc.blocks[1].kind,
            BlockKind::Heading { level: 1, .. }
        ));
    }

    #[test]
    fn tight_list_items_contain_text() {
        let doc = parse("- foo\n- bar");
        assert_eq!(doc.blocks.len(), 1);
        let BlockKind::List { items, .. } = &doc.blocks[0].kind else {
            panic!("expected List")
        };
        assert_eq!(items.len(), 2);
        assert_eq!(item_text(&items[0]), "foo");
        assert_eq!(item_text(&items[1]), "bar");
    }

    #[test]
    fn ordered_list_items_contain_text() {
        let doc = parse("1. first\n2. second");
        let BlockKind::List { ordered, items, .. } = &doc.blocks[0].kind else {
            panic!()
        };
        assert!(ordered);
        assert_eq!(items.len(), 2);
        assert_eq!(item_text(&items[0]), "first");
        assert_eq!(item_text(&items[1]), "second");
    }

    #[test]
    fn nested_list_structure() {
        let doc = parse("- a\n  - b");
        let BlockKind::List { items, .. } = &doc.blocks[0].kind else {
            panic!()
        };
        assert_eq!(items.len(), 1);
        // item 0 should have a nested list
        let has_nested = items[0]
            .blocks
            .iter()
            .any(|b| matches!(b.kind, BlockKind::List { .. }));
        assert!(has_nested, "expected nested list inside item 0");
    }

    #[test]
    fn blockquote_does_not_swallow_following_heading() {
        let doc = parse("> q\n\n# H");
        assert_eq!(doc.blocks.len(), 2);
        assert!(matches!(doc.blocks[0].kind, BlockKind::Blockquote { .. }));
        assert!(matches!(
            doc.blocks[1].kind,
            BlockKind::Heading { level: 1, .. }
        ));
    }

    #[test]
    fn task_list_items_contain_text() {
        let doc = parse("- [ ] todo\n- [x] done");
        let BlockKind::List { items, .. } = &doc.blocks[0].kind else {
            panic!()
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].task, Some(false));
        assert_eq!(items[1].task, Some(true));
        assert_eq!(item_text(&items[0]), "todo");
        assert_eq!(item_text(&items[1]), "done");
    }
}
