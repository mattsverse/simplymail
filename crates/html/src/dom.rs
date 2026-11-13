//! HTML read as a tree of blocks: what a reader can show of it without its styling.
//!
//! Layout tables (most of a newsletter) become the blocks in their cells, one after another; a
//! table with header cells is kept as a table. Hidden elements, scripts, styles and embedded media
//! are dropped, and images become their alt text, so nothing is ever fetched.

use ego_tree::NodeRef;
use scraper::{Html, Node};

/// A paragraph-level piece of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Block {
    Para(Vec<Inline>),
    /// A heading of level 1 to 6.
    Heading(u8, Vec<Inline>),
    Quote(Vec<Self>),
    List {
        ordered: bool,
        /// The first item's number, for an ordered list.
        start: u64,
        items: Vec<Vec<Self>>,
    },
    /// Preformatted text, kept as is.
    Code(String),
    Rule,
    /// A table with header cells. The first row is the header when all its cells are.
    Table {
        head: Option<Vec<Vec<Inline>>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
}

/// A run of text within a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Inline {
    Text(String),
    /// A line break within the block.
    Break,
    Styled(Style, Vec<Self>),
    Code(String),
    /// A link to a URL with a safe scheme.
    Link(String, Vec<Self>),
    /// An image sent with the message (`src="cid:…"`): its source and alt text. Only read when
    /// asked for ([`parse_keeping_cid_images`]); otherwise images are their alt text.
    Image {
        src: String,
        alt: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Style {
    Bold,
    Italic,
    Strike,
}

/// Reads `html`, a whole document or a fragment.
pub(crate) fn parse(html: &str) -> Vec<Block> {
    read(html, false)
}

/// [`parse`], keeping images sent with the message (`src="cid:…"`) as [`Inline::Image`].
pub(crate) fn parse_keeping_cid_images(html: &str) -> Vec<Block> {
    read(html, true)
}

fn read(html: &str, cid_images: bool) -> Vec<Block> {
    let document = Html::parse_document(html);
    let mut builder = Builder::new(cid_images);
    builder.children(document.tree.root());
    builder.finish()
}

/// Whether `src` names an image sent with the message.
pub(crate) fn is_cid(src: &str) -> bool {
    src.trim_start()
        .get(..4)
        .is_some_and(|s| s.eq_ignore_ascii_case("cid:"))
}

/// Elements whose content is never shown.
const SKIPPED: &[&str] = &[
    "head", "script", "style", "title", "template", "noscript", "svg", "math", "object", "embed",
    "iframe", "frame", "frameset", "video", "audio", "canvas", "map", "select", "input", "button",
    "meta", "link", "base",
];

/// Elements that start and end a paragraph, and otherwise just hold their content.
const PARAGRAPHS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "nav",
    "aside",
    "address",
    "center",
    "figure",
    "figcaption",
    "details",
    "summary",
    "dl",
    "dt",
    "dd",
    "fieldset",
    "form",
    "hgroup",
    "caption",
    "legend",
    "li",
    "tr",
    "td",
    "th",
    "thead",
    "tbody",
    "tfoot",
];

/// Whether a URL is safe to follow: web, mail and phone links only.
pub(crate) fn is_safe_href(url: &str) -> bool {
    let cleaned = url
        .chars()
        .filter(|c| !c.is_control() && !c.is_whitespace())
        .take(16)
        .collect::<String>()
        .to_ascii_lowercase();
    ["http:", "https:", "mailto:", "tel:"]
        .iter()
        .any(|scheme| cleaned.starts_with(scheme))
}

/// Whether an element's inline style or attributes hide it.
pub(crate) fn is_hidden(element: &scraper::node::Element) -> bool {
    if element.attr("hidden").is_some() {
        return true;
    }
    let Some(style) = element.attr("style") else {
        return false;
    };
    let style = style
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    ["display:none", "visibility:hidden", "mso-hide:all"]
        .iter()
        .any(|rule| style.contains(rule))
}

/// The inline style an element's `style` attribute gives, as `<span style="font-weight:bold">`.
fn style_of(element: &scraper::node::Element) -> Vec<Style> {
    let Some(style) = element.attr("style") else {
        return Vec::new();
    };
    let style = style
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    let mut styles = Vec::new();
    if [
        "font-weight:bold",
        "font-weight:bolder",
        "font-weight:600",
        "font-weight:700",
        "font-weight:800",
        "font-weight:900",
    ]
    .iter()
    .any(|rule| style.contains(rule))
    {
        styles.push(Style::Bold);
    }
    if style.contains("font-style:italic") {
        styles.push(Style::Italic);
    }
    if style.contains("line-through") {
        styles.push(Style::Strike);
    }
    styles
}

/// An inline element open around the text being read.
#[derive(Debug)]
enum Wrap {
    Styled(Style),
    Link(String),
}

/// Reads a run of nodes into blocks, gathering text into the paragraph being read.
#[derive(Debug)]
struct Builder {
    blocks: Vec<Block>,
    /// The paragraph being read: its own inlines first, then those of each open inline element.
    open: Vec<(Option<Wrap>, Vec<Inline>)>,
    /// Images sent with the message are kept as [`Inline::Image`].
    cid_images: bool,
}

impl Builder {
    fn new(cid_images: bool) -> Self {
        Self {
            blocks: Vec::new(),
            open: vec![(None, Vec::new())],
            cid_images,
        }
    }

    /// The blocks inside `node`, read the same way.
    fn blocks_of(&self, node: NodeRef<'_, Node>) -> Vec<Block> {
        let mut builder = Self::new(self.cid_images);
        builder.children(node);
        builder.finish()
    }

    fn finish(mut self) -> Vec<Block> {
        self.flush();
        self.blocks
    }

    /// Ends the paragraph being read, leaving inline elements open for what follows the block.
    fn flush(&mut self) {
        let mut content = Vec::new();
        for (wrap, inlines) in self.open.iter_mut().rev() {
            inlines.append(&mut content);
            content = match wrap {
                Some(Wrap::Styled(style)) => vec![Inline::Styled(*style, std::mem::take(inlines))],
                Some(Wrap::Link(href)) => vec![Inline::Link(href.clone(), std::mem::take(inlines))],
                None => std::mem::take(inlines),
            };
        }
        let content = tidy(content);
        if !content.is_empty() {
            self.blocks.push(Block::Para(content));
        }
    }

    fn block(&mut self, block: Block) {
        self.flush();
        self.blocks.push(block);
    }

    fn push(&mut self, inline: Inline) {
        if let Some((_, inlines)) = self.open.last_mut() {
            inlines.push(inline);
        }
    }

    fn children(&mut self, node: NodeRef<'_, Node>) {
        for child in node.children() {
            self.node(child);
        }
    }

    fn node(&mut self, node: NodeRef<'_, Node>) {
        match node.value() {
            Node::Text(text) => self.push(Inline::Text(collapse(text))),
            Node::Element(element) => self.element(node, element),
            Node::Document | Node::Fragment => self.children(node),
            Node::Doctype(_) | Node::Comment(_) | Node::ProcessingInstruction(_) => {}
        }
    }

    /// Reads the children inside `wrap`, closing it after them.
    fn wrapped(&mut self, node: NodeRef<'_, Node>, wrap: Wrap) {
        self.open.push((Some(wrap), Vec::new()));
        self.children(node);
        if let Some((Some(wrap), inlines)) = self.open.pop() {
            self.push(match wrap {
                Wrap::Styled(style) => Inline::Styled(style, inlines),
                Wrap::Link(href) => Inline::Link(href, inlines),
            });
        }
    }

    fn element(&mut self, node: NodeRef<'_, Node>, element: &scraper::node::Element) {
        let name = element.name();
        if SKIPPED.contains(&name) || is_hidden(element) {
            return;
        }
        match name {
            "br" => self.push(Inline::Break),
            "hr" => self.block(Block::Rule),
            "img" if self.cid_images && element.attr("src").is_some_and(is_cid) => {
                self.push(Inline::Image {
                    src: element.attr("src").unwrap_or_default().trim().to_owned(),
                    alt: collapse(element.attr("alt").unwrap_or_default().trim()),
                });
            }
            "img" => {
                if let Some(alt) = element.attr("alt").map(str::trim).filter(|a| !a.is_empty()) {
                    self.push(Inline::Text(format!("[{}]", collapse(alt))));
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = name.get(1..).and_then(|l| l.parse().ok()).unwrap_or(1);
                let content = inline_of(&self.blocks_of(node));
                if !content.is_empty() {
                    self.block(Block::Heading(level, content));
                }
            }
            "pre" => {
                let mut code = String::new();
                raw_text(node, &mut code);
                let code = code.trim_end_matches(['\n', '\r']).to_owned();
                if !code.trim().is_empty() {
                    self.block(Block::Code(code));
                }
            }
            "blockquote" => {
                let blocks = self.blocks_of(node);
                if !blocks.is_empty() {
                    self.block(Block::Quote(blocks));
                }
            }
            "ul" | "ol" => self.list(node, name == "ol", element.attr("start")),
            "table" => self.table(node),
            "code" | "kbd" | "samp" | "tt" => {
                let mut code = String::new();
                raw_text(node, &mut code);
                let code = collapse(&code);
                if !code.trim().is_empty() {
                    self.push(Inline::Code(code));
                }
            }
            "a" => match element
                .attr("href")
                .map(str::trim)
                .filter(|h| is_safe_href(h))
            {
                Some(href) => self.wrapped(node, Wrap::Link(href.to_owned())),
                None => self.children(node),
            },
            "b" | "strong" => self.wrapped(node, Wrap::Styled(Style::Bold)),
            "i" | "em" | "cite" | "var" | "dfn" => self.wrapped(node, Wrap::Styled(Style::Italic)),
            "s" | "strike" | "del" => self.wrapped(node, Wrap::Styled(Style::Strike)),
            _ => {
                let paragraph = PARAGRAPHS.contains(&name);
                if paragraph {
                    self.flush();
                }
                let styles = style_of(element);
                for style in &styles {
                    self.open.push((Some(Wrap::Styled(*style)), Vec::new()));
                }
                self.children(node);
                for _ in &styles {
                    if let Some((Some(wrap), inlines)) = self.open.pop() {
                        self.push(match wrap {
                            Wrap::Styled(style) => Inline::Styled(style, inlines),
                            Wrap::Link(href) => Inline::Link(href, inlines),
                        });
                    }
                }
                if paragraph {
                    self.flush();
                }
            }
        }
    }

    fn list(&mut self, node: NodeRef<'_, Node>, ordered: bool, start: Option<&str>) {
        let mut items: Vec<Vec<Block>> = Vec::new();
        let mut loose = Self::new(self.cid_images);
        for child in node.children() {
            let item = child
                .value()
                .as_element()
                .filter(|e| e.name() == "li" && !is_hidden(e));
            if item.is_some() {
                let stray = std::mem::replace(&mut loose, Self::new(self.cid_images)).finish();
                if !stray.is_empty() {
                    items.push(stray);
                }
                items.push(self.blocks_of(child));
            } else {
                loose.node(child);
            }
        }
        let stray = loose.finish();
        if !stray.is_empty() {
            items.push(stray);
        }
        if items.is_empty() {
            return;
        }
        self.block(Block::List {
            ordered,
            start: start.and_then(|s| s.trim().parse().ok()).unwrap_or(1),
            items,
        });
    }

    fn table(&mut self, node: NodeRef<'_, Node>) {
        let mut rows = Vec::new();
        let mut captions = Vec::new();
        rows_of(node, &mut rows, &mut captions);
        let has_headers = rows
            .iter()
            .flatten()
            .any(|cell| cell.value().as_element().is_some_and(|e| e.name() == "th"));
        for caption in captions {
            self.flush();
            self.children(caption);
            self.flush();
        }
        if !has_headers {
            // A layout table: its cells' content, in reading order.
            for cell in rows.into_iter().flatten() {
                self.flush();
                self.children(cell);
                self.flush();
            }
            return;
        }
        let is_header = |row: &[NodeRef<'_, Node>]| {
            !row.is_empty()
                && row
                    .iter()
                    .all(|c| c.value().as_element().is_some_and(|e| e.name() == "th"))
        };
        let mut rows = rows.into_iter().peekable();
        let head = rows.next_if(|row| is_header(row)).map(|row| {
            row.into_iter()
                .map(|c| inline_of(&self.blocks_of(c)))
                .collect()
        });
        let rows: Vec<Vec<Vec<Inline>>> = rows
            .map(|row| {
                row.into_iter()
                    .map(|c| inline_of(&self.blocks_of(c)))
                    .collect()
            })
            .filter(|row: &Vec<Vec<Inline>>| row.iter().any(|c| !c.is_empty()))
            .collect();
        self.block(Block::Table { head, rows });
    }
}

/// The rows of a table, not counting those of tables nested in its cells, and its captions.
fn rows_of<'a>(
    node: NodeRef<'a, Node>,
    rows: &mut Vec<Vec<NodeRef<'a, Node>>>,
    captions: &mut Vec<NodeRef<'a, Node>>,
) {
    for child in node.children() {
        let Some(element) = child.value().as_element() else {
            continue;
        };
        if is_hidden(element) {
            continue;
        }
        match element.name() {
            "thead" | "tbody" | "tfoot" => rows_of(child, rows, captions),
            "caption" => captions.push(child),
            "tr" => rows.push(
                child
                    .children()
                    .filter(|c| {
                        c.value()
                            .as_element()
                            .is_some_and(|e| matches!(e.name(), "td" | "th") && !is_hidden(e))
                    })
                    .collect(),
            ),
            _ => {}
        }
    }
}

/// `blocks` as one run of inlines, a line break between blocks: for a heading or a table cell.
pub(crate) fn inline_of(blocks: &[Block]) -> Vec<Inline> {
    let mut out = Vec::new();
    for block in blocks {
        let more = match block {
            Block::Para(inlines) | Block::Heading(_, inlines) => inlines.clone(),
            Block::Quote(blocks) => inline_of(blocks),
            Block::List { items, .. } => {
                let mut list = Vec::new();
                for item in items {
                    if !list.is_empty() {
                        list.push(Inline::Break);
                    }
                    list.extend(inline_of(item));
                }
                list
            }
            Block::Code(code) => vec![Inline::Code(collapse(code))],
            Block::Rule => Vec::new(),
            Block::Table { head, rows } => {
                let mut table = Vec::new();
                for row in head.iter().chain(rows) {
                    if !table.is_empty() {
                        table.push(Inline::Break);
                    }
                    for (i, cell) in row.iter().enumerate() {
                        if i > 0 {
                            table.push(Inline::Text(" ".to_owned()));
                        }
                        table.extend(cell.iter().cloned());
                    }
                }
                table
            }
        };
        if more.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(Inline::Break);
        }
        out.extend(more);
    }
    out
}

/// The text inside `node` exactly as written, with `<br>` as a newline: for `<pre>` and `<code>`.
fn raw_text(node: NodeRef<'_, Node>, out: &mut String) {
    for child in node.children() {
        match child.value() {
            Node::Text(text) => out.push_str(text),
            Node::Element(e) if e.name() == "br" => out.push('\n'),
            Node::Element(e) if SKIPPED.contains(&e.name()) || is_hidden(e) => {}
            Node::Element(_) => raw_text(child, out),
            _ => {}
        }
    }
}

/// `text` with every run of whitespace as one space, as HTML shows it. No-break spaces stay.
pub(crate) fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_ascii_whitespace()
            || matches!(
                c,
                '\u{200b}' | '\u{200c}' | '\u{feff}' | '\u{034f}' | '\u{00ad}'
            )
        {
            // Zero-width and soft-hyphen characters pad preheaders; they show as nothing.
            if c.is_ascii_whitespace() {
                space = true;
            }
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    out
}

/// Trims the spaces at the start and end of a paragraph and around its line breaks, merges
/// neighbouring text, and drops what's left empty. Returns nothing for a paragraph with no text.
fn tidy(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut inlines = merge(inlines);
    let mut at_start = true;
    trim_leading(&mut inlines, &mut at_start);
    let mut at_end = true;
    trim_trailing(&mut inlines, &mut at_end);
    let mut inlines = merge(inlines);
    // Line breaks at the very start or end show nothing.
    while inlines.first() == Some(&Inline::Break) {
        inlines.remove(0);
    }
    while inlines.last() == Some(&Inline::Break) {
        inlines.pop();
    }
    if has_text(&inlines) {
        inlines
    } else {
        Vec::new()
    }
}

fn has_text(inlines: &[Inline]) -> bool {
    inlines.iter().any(|i| match i {
        Inline::Text(t) => !t.trim().is_empty(),
        Inline::Code(_) | Inline::Image { .. } => true,
        Inline::Break => false,
        Inline::Styled(_, inner) | Inline::Link(_, inner) => has_text(inner),
    })
}

/// Joins neighbouring text and drops empty runs.
fn merge(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::with_capacity(inlines.len());
    for inline in inlines {
        let inline = match inline {
            Inline::Styled(style, inner) => Inline::Styled(style, merge(inner)),
            Inline::Link(href, inner) => Inline::Link(href, merge(inner)),
            other => other,
        };
        match (&inline, out.last_mut()) {
            (Inline::Text(t), _) if t.is_empty() => {}
            (Inline::Styled(_, inner), _) if inner.is_empty() => {}
            (Inline::Link(_, inner), _) if inner.is_empty() => {}
            (Inline::Text(t), Some(Inline::Text(last))) => {
                if last.ends_with(' ') && t.starts_with(' ') {
                    last.push_str(&t[1..]);
                } else {
                    last.push_str(t);
                }
            }
            _ => out.push(inline),
        }
    }
    out
}

fn trim_leading(inlines: &mut [Inline], at_start: &mut bool) {
    for inline in inlines {
        match inline {
            Inline::Text(t) => {
                if *at_start && t.starts_with(' ') {
                    t.remove(0);
                }
                if !t.is_empty() {
                    *at_start = t.ends_with(' ');
                }
            }
            Inline::Break => *at_start = true,
            Inline::Code(_) | Inline::Image { .. } => *at_start = false,
            Inline::Styled(_, inner) | Inline::Link(_, inner) => trim_leading(inner, at_start),
        }
    }
}

fn trim_trailing(inlines: &mut [Inline], at_end: &mut bool) {
    for inline in inlines.iter_mut().rev() {
        match inline {
            Inline::Text(t) => {
                if *at_end && t.ends_with(' ') {
                    t.pop();
                }
                if !t.is_empty() {
                    *at_end = false;
                }
            }
            Inline::Break => *at_end = true,
            Inline::Code(_) | Inline::Image { .. } => *at_end = false,
            Inline::Styled(_, inner) | Inline::Link(_, inner) => trim_trailing(inner, at_end),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Inline {
        Inline::Text(s.to_owned())
    }

    #[test]
    fn paragraphs_and_inline_styles_are_read() {
        assert_eq!(
            parse(
                "<p>Hello <b>there</b>,\n  <a href='https://x.test'>you</a></p><p> </p><div>Bye</div>"
            ),
            vec![
                Block::Para(vec![
                    text("Hello "),
                    Inline::Styled(Style::Bold, vec![text("there")]),
                    text(", "),
                    Inline::Link("https://x.test".into(), vec![text("you")]),
                ]),
                Block::Para(vec![text("Bye")]),
            ]
        );
    }

    #[test]
    fn hidden_and_unsafe_content_is_dropped() {
        let blocks = parse(
            "<head><style>p{}</style><title>T</title></head>\
             <div style='display: none'>preheader</div><script>alert(1)</script>\
             <p><a href='javascript:alert(1)'>click</a> <img src='https://track.test/p.gif'>\
             <img src='logo.png' alt='Logo'></p>",
        );
        assert_eq!(blocks, vec![Block::Para(vec![text("click [Logo]")])]);
    }

    #[test]
    fn layout_tables_are_flattened_and_data_tables_kept() {
        let layout = parse("<table><tr><td>One</td><td><p>Two</p></td></tr></table>");
        assert_eq!(
            layout,
            vec![
                Block::Para(vec![text("One")]),
                Block::Para(vec![text("Two")])
            ]
        );
        let data = parse(
            "<table><tr><th>Item</th><th>Price</th></tr><tr><td>Tea</td><td>3</td></tr></table>",
        );
        assert_eq!(
            data,
            vec![Block::Table {
                head: Some(vec![vec![text("Item")], vec![text("Price")]]),
                rows: vec![vec![vec![text("Tea")], vec![text("3")]]],
            }]
        );
    }

    #[test]
    fn lists_quotes_code_and_breaks_are_kept() {
        let blocks = parse(
            "<ol start='3'><li>a</li><li>b<br>c</li></ol><blockquote>q</blockquote>\
             <pre>  x\n  y</pre><hr><h2>Title</h2>",
        );
        assert_eq!(
            blocks,
            vec![
                Block::List {
                    ordered: true,
                    start: 3,
                    items: vec![
                        vec![Block::Para(vec![text("a")])],
                        vec![Block::Para(vec![text("b"), Inline::Break, text("c")])],
                    ],
                },
                Block::Quote(vec![Block::Para(vec![text("q")])]),
                Block::Code("  x\n  y".into()),
                Block::Rule,
                Block::Heading(2, vec![text("Title")]),
            ]
        );
    }

    #[test]
    fn a_block_inside_bold_text_splits_the_paragraph_but_keeps_the_style() {
        assert_eq!(
            parse("<b>one<div>two</div>three</b>"),
            vec![
                Block::Para(vec![Inline::Styled(Style::Bold, vec![text("one")])]),
                Block::Para(vec![Inline::Styled(Style::Bold, vec![text("two")])]),
                Block::Para(vec![Inline::Styled(Style::Bold, vec![text("three")])]),
            ]
        );
    }
}
