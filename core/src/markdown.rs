//! Markdown, parsed into something a front-end can draw without interpreting.
//!
//! A `.md` file is text, so the text viewer already shows it. What it is not
//! is *readable* that way: a README read as a wall of `##` and `*` is a file
//! you decode rather than read.
//!
//! What crosses is the **parse**, never the drawing - the same answer
//! [`crate::termview`] gives, for the same reason. What counts as a heading,
//! when a blank line ends a paragraph, how a lazy continuation joins a quote,
//! what a reference link resolves to: that is CommonMark, a specification
//! with a thousand edge cases, and two front-ends that each parsed it would
//! disagree within a week. What a heading *looks like* is a front-end's
//! business, and the two draw differently on purpose.
//!
//! The shape is a flat list of [`Block`]s, each holding [`Run`]s. Flat and
//! not a tree: a tree would make the front-end walk it, which is the
//! front-end doing structure again, and a depth to indent by is all either of
//! them actually does with nesting.

use serde::Serialize;

/// What a stretch of inline text is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    Plain,
    Emphasis,
    Strong,
    /// Both at once, which markdown spells `***like this***`.
    StrongEmphasis,
    /// Backticks: drawn in a fixed-width font, never re-parsed.
    Code,
    Strike,
}

/// A stretch of inline text sharing one style.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Run {
    pub text: String,
    pub style: Style,
    /// Where a link points, if this run is one.
    ///
    /// The front-end draws it as a link and opens it only when clicked.
    /// Nothing about rendering a document should reach the network on its
    /// own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    /// Where an image points, if this run is one. `text` is its alt text.
    ///
    /// Never fetched here, and the front-end must not fetch a remote one
    /// either - see [`Image::remote`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<Image>,
}

/// An image reference, and whether showing it would touch the network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Image {
    pub src: String,
    /// True for anything that is not a plain relative or absolute path.
    ///
    /// A preview that loaded `![](https://example.com/pixel.png)` would tell
    /// whoever hosts it the moment somebody opened the file - which is what a
    /// tracking pixel in an email is, and a file manager must not be one. So
    /// the front-end draws a placeholder saying where it points, and the
    /// reader decides.
    pub remote: bool,
}

impl Image {
    fn of(src: &str) -> Image {
        let lowered = src.to_ascii_lowercase();
        let remote = lowered.starts_with("http://")
            || lowered.starts_with("https://")
            || lowered.starts_with("//")
            // data: is not the network, but it is arbitrary bytes decoded by
            // an image loader on the strength of a text file saying so.
            || lowered.starts_with("data:");
        Image {
            src: src.to_string(),
            remote,
        }
    }
}

/// What kind of block this is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Kind {
    Heading {
        level: u8,
    },
    Paragraph,
    /// One item. `number` is `None` for a bullet.
    ListItem {
        ordered: bool,
        number: Option<u64>,
    },
    /// A fenced or indented block. The text is kept exactly as written.
    Code {
        language: Option<String>,
    },
    Quote,
    Rule,
    /// One row of a table. The front-end lays the cells out in whatever grid
    /// it has; how many columns there are is how many cells arrived.
    TableRow {
        header: bool,
    },
    /// Raw HTML, which is not rendered.
    ///
    /// Rendering it would make this a browser. The block carries the source
    /// so a front-end can say what it skipped rather than silently dropping
    /// part of the document.
    Html,
}

/// One block of a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Block {
    #[serde(flatten)]
    pub kind: Kind,
    /// How deeply nested, for indenting. Zero at the top level.
    pub depth: u8,
    /// The inline runs. For a table row, the cells in order, one run each
    /// when they are plain - a cell with styling contributes several.
    pub runs: Vec<Run>,
    /// Where each cell of a table row starts in `runs`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cells: Vec<usize>,
}

impl Block {
    /// The block's text with the styling thrown away, for a search or a test.
    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }
}

/// Parse a document into blocks.
pub fn parse(source: &str) -> Vec<Block> {
    use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TASKLISTS);

    let mut blocks: Vec<Block> = Vec::new();
    let mut runs: Vec<Run> = Vec::new();
    let mut cells: Vec<usize> = Vec::new();

    // The styles currently open, as a stack: markdown nests them and the
    // innermost is not always the whole answer - `**bold *and italic***`.
    let mut emphasis = 0u8;
    let mut strong = 0u8;
    let mut strike = 0u8;
    let mut code = false;
    let mut href: Option<String> = None;
    let mut image: Option<Image> = None;

    // How deep we are in lists and quotes, and what the current list is.
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let mut quote_depth = 0u8;
    let mut in_header_row = false;
    let mut pending: Option<Kind> = None;

    // Set when the next run must start a new one whatever its style.
    //
    // A table cell is the case: two plain cells side by side would otherwise
    // be joined into one run - "Kind" and "Crosses as" came out as
    // "KindCrosses as" - and the offsets saying where each cell starts would
    // then point into a vector with one element in it.
    //
    // Two slashes, not three: a doc comment on a `let` documents nothing, and
    // the compiler drops it - so the reason this exists was invisible in the
    // rendered docs and a warning in every build.
    let mut fresh = false;

    fn style_now(emphasis: u8, strong: u8, strike: u8, code: bool) -> Style {
        if code {
            return Style::Code;
        }
        if strike > 0 {
            return Style::Strike;
        }
        match (strong > 0, emphasis > 0) {
            (true, true) => Style::StrongEmphasis,
            (true, false) => Style::Strong,
            (false, true) => Style::Emphasis,
            (false, false) => Style::Plain,
        }
    }

    let depth_of = |list_stack: &Vec<Option<u64>>, quote_depth: u8| -> u8 {
        (list_stack.len() as u8).saturating_add(quote_depth)
    };

    for event in Parser::new_ext(source, options) {
        // Asked before the match, which takes the event apart. Both are
        // about what to do *after* handling it, and borrowing a value the
        // match has already moved out of is not allowed.
        let ends_item = matches!(event, Event::End(TagEnd::Item));
        let ends_header = matches!(event, Event::End(TagEnd::TableHead));
        let opens_code = matches!(event, Event::Start(Tag::CodeBlock(_)));
        let closes_code = matches!(event, Event::End(TagEnd::CodeBlock));

        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                pending = Some(Kind::Heading {
                    level: match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        HeadingLevel::H3 => 3,
                        HeadingLevel::H4 => 4,
                        HeadingLevel::H5 => 5,
                        HeadingLevel::H6 => 6,
                    },
                });
            }
            Event::Start(Tag::Paragraph) => {
                pending = Some(match quote_depth > 0 {
                    true => Kind::Quote,
                    false => Kind::Paragraph,
                });
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let language = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(name) if !name.is_empty() => {
                        Some(name.to_string())
                    }
                    _ => None,
                };
                pending = Some(Kind::Code { language });
            }
            Event::Start(Tag::List(first)) => {
                // The parent item's own text is flushed before the nested
                // list opens. Without this "two" and "inner" ended up in one
                // block, labelled as the inner bullet: the item's text
                // arrives before its child list, and the child's flush would
                // take it.
                if !runs.is_empty() {
                    let kind = pending.take().unwrap_or(Kind::Paragraph);
                    blocks.push(Block {
                        kind,
                        depth: depth_of(&list_stack, quote_depth),
                        runs: std::mem::take(&mut runs),
                        cells: std::mem::take(&mut cells),
                    });
                }
                list_stack.push(first);
            }
            Event::End(TagEnd::List(_)) => {
                list_stack.pop();
            }
            Event::Start(Tag::Item) => {
                let number = list_stack.last().copied().flatten();
                pending = Some(Kind::ListItem {
                    ordered: number.is_some(),
                    number,
                });
            }
            Event::Start(Tag::BlockQuote(_)) => quote_depth = quote_depth.saturating_add(1),
            Event::End(TagEnd::BlockQuote(_)) => quote_depth = quote_depth.saturating_sub(1),
            Event::Start(Tag::TableHead) => {
                in_header_row = true;
                // The header arrives as TableHead and not as a TableRow, so
                // without this it never became a block at all.
                pending = Some(Kind::TableRow { header: true });
            }
            Event::Start(Tag::TableRow) => {
                pending = Some(Kind::TableRow { header: false });
            }
            Event::Start(Tag::TableCell) => {
                cells.push(runs.len());
                fresh = true;
            }
            Event::Start(Tag::Emphasis) => emphasis += 1,
            Event::End(TagEnd::Emphasis) => emphasis = emphasis.saturating_sub(1),
            Event::Start(Tag::Strong) => strong += 1,
            Event::End(TagEnd::Strong) => strong = strong.saturating_sub(1),
            Event::Start(Tag::Strikethrough) => strike += 1,
            Event::End(TagEnd::Strikethrough) => strike = strike.saturating_sub(1),
            Event::Start(Tag::Link { dest_url, .. }) => href = Some(dest_url.to_string()),
            Event::End(TagEnd::Link) => href = None,
            Event::Start(Tag::Image { dest_url, .. }) => image = Some(Image::of(&dest_url)),
            Event::End(TagEnd::Image) => {
                // An image with no alt text still has to be drawn, or a
                // document of screenshots comes out empty.
                if let Some(picture) = image.take() {
                    if runs.last().is_none_or(|run| run.image.is_none()) {
                        runs.push(Run {
                            text: String::new(),
                            style: Style::Plain,
                            href: None,
                            image: Some(picture),
                        });
                    }
                }
            }

            Event::Text(text) | Event::InlineMath(text) | Event::DisplayMath(text) => {
                push_run(
                    &mut runs,
                    &text,
                    style_now(emphasis, strong, strike, code),
                    &href,
                    &image,
                    std::mem::take(&mut fresh),
                );
            }
            Event::Code(text) => {
                push_run(&mut runs, &text, Style::Code, &href, &None, std::mem::take(&mut fresh));
            }
            Event::SoftBreak => push_run(&mut runs, " ", Style::Plain, &href, &None, false),
            Event::HardBreak => push_run(&mut runs, "\n", Style::Plain, &href, &None, false),

            Event::Html(text) | Event::InlineHtml(text) => {
                // Kept, not rendered: dropping it silently would lose part of
                // the document with nothing said about it.
                if pending.is_none() && runs.is_empty() {
                    pending = Some(Kind::Html);
                }
                push_run(&mut runs, &text, Style::Code, &None, &None, std::mem::take(&mut fresh));
            }

            Event::Rule => blocks.push(Block {
                kind: Kind::Rule,
                depth: depth_of(&list_stack, quote_depth),
                runs: Vec::new(),
                cells: Vec::new(),
            }),

            Event::TaskListMarker(done) => {
                push_run(
                    &mut runs,
                    if done { "[x] " } else { "[ ] " },
                    Style::Plain,
                    &None,
                    &None,
                    std::mem::take(&mut fresh),
                );
            }

            Event::End(TagEnd::Heading(_))
            | Event::End(TagEnd::Paragraph)
            | Event::End(TagEnd::CodeBlock)
            | Event::End(TagEnd::TableRow)
            | Event::End(TagEnd::TableHead)
            // A tight list has no paragraph inside its items - the text sits
            // straight between Start(Item) and End(Item) - so the item is
            // where it has to be flushed.
            | Event::End(TagEnd::Item)
            | Event::End(TagEnd::HtmlBlock) => {
                let kind = pending.take().unwrap_or(Kind::Paragraph);
                let kind = match (kind, in_header_row) {
                    (Kind::TableRow { .. }, header) => Kind::TableRow { header },
                    (other, _) => other,
                };
                if !runs.is_empty() {
                    blocks.push(Block {
                        kind,
                        depth: depth_of(&list_stack, quote_depth),
                        runs: std::mem::take(&mut runs),
                        cells: std::mem::take(&mut cells),
                    });
                } else {
                    runs.clear();
                    cells.clear();
                }
            }
            // An item's own text ends at its End(Item), which arrives after
            // any paragraph inside it - so a plain item is flushed here.
            Event::Start(Tag::HtmlBlock) => pending = Some(Kind::Html),
            _ => {}
        }

        // A list item whose body was a paragraph has already been flushed as
        // one; relabel it, because a bullet is not a paragraph.
        // Every row after the head is a body row.
        if ends_header {
            in_header_row = false;
        }

        if ends_item {
            // The next item in an ordered list counts on from this one.
            if let Some(Some(number)) = list_stack.last_mut() {
                *number += 1;
            }
            if let Some(last) = blocks.last_mut() {
                if matches!(last.kind, Kind::Paragraph) {
                    let number = list_stack
                        .last()
                        .copied()
                        .flatten()
                        .map(|n| n.saturating_sub(1));
                    last.kind = Kind::ListItem {
                        ordered: number.is_some(),
                        number,
                    };
                }
            }
        }

        if opens_code {
            code = true;
        }
        if closes_code {
            code = false;
        }
    }

    blocks
}

/// Add text to the runs, joining it to the last one when nothing changed.
fn push_run(
    runs: &mut Vec<Run>,
    text: &str,
    style: Style,
    href: &Option<String>,
    image: &Option<Image>,
    fresh: bool,
) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut().filter(|_| !fresh) {
        if last.style == style && &last.href == href && last.image.is_none() && image.is_none() {
            last.text.push_str(text);
            return;
        }
    }
    runs.push(Run {
        text: text.to_string(),
        style,
        href: href.clone(),
        image: image.clone(),
    });
}

/// Whether a name is one the markdown viewer should render.
pub fn looks_like_markdown(name: &str) -> bool {
    matches!(
        name.rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase())
            .unwrap_or_default()
            .as_str(),
        "md" | "markdown" | "mdown" | "mkd" | "mdx"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(blocks: &[Block]) -> Vec<&Kind> {
        blocks.iter().map(|b| &b.kind).collect()
    }

    #[test]
    fn a_heading_carries_its_level() {
        let blocks = parse("# One\n\n### Three\n");
        assert_eq!(
            kinds(&blocks),
            vec![&Kind::Heading { level: 1 }, &Kind::Heading { level: 3 }]
        );
        assert_eq!(blocks[0].text(), "One");
    }

    #[test]
    fn a_setext_heading_is_a_heading_too() {
        // The underlined form, which is the one a hand-written README uses
        // and the one a naive line-by-line reader misses entirely.
        let blocks = parse("Title\n=====\n\nBody\n");
        assert_eq!(blocks[0].kind, Kind::Heading { level: 1 });
        assert_eq!(blocks[0].text(), "Title");
    }

    #[test]
    fn emphasis_inside_strong_is_both() {
        let blocks = parse("**bold *and italic***\n");
        let styles: Vec<Style> = blocks[0].runs.iter().map(|r| r.style).collect();
        assert!(styles.contains(&Style::Strong), "{styles:?}");
        assert!(
            styles.contains(&Style::StrongEmphasis),
            "the inner run is both, not just the innermost one: {styles:?}"
        );
    }

    #[test]
    fn a_fence_keeps_its_language_and_its_text_exactly() {
        let blocks = parse("```rust\nlet x = 1;  // two spaces\n```\n");
        assert_eq!(
            blocks[0].kind,
            Kind::Code {
                language: Some("rust".to_string())
            }
        );
        // Exactly as written - a code block that lost its spacing would be a
        // code block nobody could copy out.
        assert_eq!(blocks[0].text(), "let x = 1;  // two spaces\n");
    }

    #[test]
    fn code_spans_are_not_parsed_again() {
        let blocks = parse("Use `**not bold**` here.\n");
        let code: Vec<&Run> = blocks[0]
            .runs
            .iter()
            .filter(|r| r.style == Style::Code)
            .collect();
        assert_eq!(code.len(), 1);
        assert_eq!(code[0].text, "**not bold**");
    }

    #[test]
    fn a_list_counts_and_nests() {
        let blocks = parse("1. one\n2. two\n   - inner\n");
        let items: Vec<&Block> = blocks
            .iter()
            .filter(|b| matches!(b.kind, Kind::ListItem { .. }))
            .collect();
        assert_eq!(items.len(), 3, "{:?}", kinds(&blocks));
        assert_eq!(
            items[0].kind,
            Kind::ListItem {
                ordered: true,
                number: Some(1)
            }
        );
        assert_eq!(
            items[1].kind,
            Kind::ListItem {
                ordered: true,
                number: Some(2)
            }
        );
        // The nested bullet is deeper, which is all a front-end needs to
        // indent it correctly.
        assert!(items[2].depth > items[0].depth, "{:?}", items[2]);
        assert_eq!(
            items[2].kind,
            Kind::ListItem {
                ordered: false,
                number: None
            }
        );
    }

    #[test]
    fn a_quote_is_a_quote_at_the_depth_it_sits() {
        let blocks = parse("> said\n>\n> > deeper\n");
        let quotes: Vec<&Block> = blocks.iter().filter(|b| b.kind == Kind::Quote).collect();
        assert_eq!(quotes.len(), 2, "{:?}", kinds(&blocks));
        assert!(quotes[1].depth > quotes[0].depth);
    }

    #[test]
    fn a_link_carries_where_it_points_and_nothing_is_fetched() {
        let blocks = parse("See [the docs](https://example.com/x) for more.\n");
        let link = blocks[0]
            .runs
            .iter()
            .find(|r| r.href.is_some())
            .expect("a link");
        assert_eq!(link.text, "the docs");
        assert_eq!(link.href.as_deref(), Some("https://example.com/x"));
    }

    #[test]
    fn a_remote_image_is_marked_so_it_is_never_fetched() {
        // The one that matters. A preview that loaded this would tell whoever
        // hosts it that somebody opened the file, which is a tracking pixel.
        let blocks = parse("![](https://example.com/pixel.png)\n");
        let picture = blocks[0].runs[0].image.as_ref().expect("an image");
        assert!(picture.remote, "a https image must be marked remote");

        let blocks = parse("![shot](./shot.png)\n");
        let picture = blocks[0].runs[0].image.as_ref().expect("an image");
        assert!(!picture.remote, "a relative path is not the network");
        assert_eq!(blocks[0].runs[0].text, "shot");

        // data: is not the network, but it is arbitrary bytes handed to an
        // image loader because a text file said so.
        let blocks = parse("![](data:image/png;base64,AAAA)\n");
        assert!(blocks[0].runs[0].image.as_ref().unwrap().remote);
    }

    #[test]
    fn an_image_with_no_alt_text_still_arrives() {
        let blocks = parse("![](./shot.png)\n");
        assert_eq!(blocks.len(), 1, "{:?}", kinds(&blocks));
        assert!(blocks[0].runs[0].image.is_some());
    }

    #[test]
    fn a_rule_is_its_own_block() {
        let blocks = parse("one\n\n---\n\ntwo\n");
        assert!(
            kinds(&blocks).contains(&&Kind::Rule),
            "{:?}",
            kinds(&blocks)
        );
    }

    #[test]
    fn a_table_says_which_row_is_the_header_and_where_its_cells_start() {
        let blocks = parse("| a | b |\n|---|---|\n| 1 | 2 |\n");
        let rows: Vec<&Block> = blocks
            .iter()
            .filter(|b| matches!(b.kind, Kind::TableRow { .. }))
            .collect();
        assert_eq!(rows.len(), 2, "{:?}", kinds(&blocks));
        assert_eq!(rows[0].kind, Kind::TableRow { header: true });
        assert_eq!(rows[1].kind, Kind::TableRow { header: false });
        // Two cells each, so a front-end knows where one ends.
        assert_eq!(rows[0].cells.len(), 2);
        assert_eq!(rows[1].cells.len(), 2);
    }

    #[test]
    fn two_plain_cells_do_not_run_into_one() {
        // The bug the app caught: runs are joined when nothing about their
        // style changed, and two plain cells side by side qualified - so
        // "Kind" and "Crosses as" became "KindCrosses as", and the offsets
        // saying where each cell starts pointed into a vector holding one.
        let blocks = parse(
            "| Kind | Crosses as |
|---|---|
| a | b |
",
        );
        let header = blocks
            .iter()
            .find(|b| b.kind == Kind::TableRow { header: true })
            .expect("a header row");

        assert_eq!(header.cells, vec![0, 1], "{:?}", header.runs);
        assert_eq!(header.runs.len(), 2, "{:?}", header.runs);
        assert_eq!(header.runs[0].text, "Kind");
        assert_eq!(header.runs[1].text, "Crosses as");
    }

    #[test]
    fn html_is_kept_but_not_rendered() {
        let blocks = parse("<div class=\"x\">raw</div>\n");
        assert!(
            blocks.iter().any(|b| b.kind == Kind::Html),
            "{:?}",
            kinds(&blocks)
        );
        // The source is there, so a front-end can say what it skipped rather
        // than dropping part of the document in silence.
        assert!(blocks.iter().any(|b| b.text().contains("div")));
    }

    #[test]
    fn a_soft_break_is_a_space_and_a_hard_break_is_a_line() {
        let blocks = parse("one\ntwo\n");
        assert_eq!(blocks[0].text(), "one two");

        let blocks = parse("one  \ntwo\n");
        assert!(blocks[0].text().contains('\n'), "{:?}", blocks[0].text());
    }

    #[test]
    fn the_extensions_that_count_as_markdown() {
        assert!(looks_like_markdown("README.md"));
        assert!(looks_like_markdown("NOTES.MARKDOWN"));
        assert!(!looks_like_markdown("README"));
        assert!(!looks_like_markdown("script.rs"));
    }
}
