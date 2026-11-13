//! Formatted text, as a message or a signature is written: a visual editor, or the HTML source
//! it can switch to.

use rune::{Block, Mark, TextEdit};

/// How formatted text is being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Mode {
    /// Formatted, as it will look.
    #[default]
    Visual,
    /// As HTML source.
    Source,
}

/// The inline formatting the visual editor offers, all of which every major mail app shows.
const fn marks() -> [Mark; 6] {
    [
        Mark::Bold,
        Mark::Italic,
        Mark::Underline,
        Mark::Strike,
        Mark::Code,
        Mark::Link(String::new()),
    ]
}

const BLOCKS: [Block; 4] = [
    Block::Bullet(0),
    Block::Numbered(0),
    Block::Quote,
    Block::Code,
];

/// A visual editor holding `html`, or plain `text` when there's no HTML.
fn visual(html: Option<&str>, text: &str) -> TextEdit {
    let edit = html.map_or_else(|| TextEdit::new(text), TextEdit::from_html);
    edit.rich().allow_marks(marks()).allow_blocks(BLOCKS)
}

/// Formatted text, edited visually or as HTML source.
#[derive(Default)]
pub(crate) struct RichEditor {
    /// The text, formatted, while in [`Mode::Visual`].
    pub(crate) text: TextEdit,
    /// The text as HTML, while in [`Mode::Source`].
    pub(crate) source: TextEdit,
    pub(crate) mode: Mode,
    /// Switching from source to visual would drop markup the editor can't show; a second press
    /// does it anyway.
    pub(crate) confirm_visual: bool,
    /// The address of the link being added to the selection, while it's typed.
    pub(crate) link: Option<TextEdit>,
}

impl RichEditor {
    /// A visual editor holding `html`, or plain `text` when there's no HTML.
    pub(crate) fn visual(html: Option<&str>, text: &str) -> Self {
        Self {
            text: visual(html, text),
            ..Self::default()
        }
    }

    /// An editor showing `html` as source.
    pub(crate) fn source(html: &str) -> Self {
        Self {
            text: visual(None, ""),
            source: TextEdit::new(html),
            mode: Mode::Source,
            ..Self::default()
        }
    }

    /// Whether what's written has any formatting, so it goes as HTML.
    pub(crate) fn is_formatted(&self) -> bool {
        match self.mode {
            Mode::Source => true,
            Mode::Visual => {
                !self.text.marks().is_empty()
                    || self.text.blocks().iter().any(|b| *b != Block::Paragraph)
            }
        }
    }

    /// What's written, as an HTML fragment (or document, if the source is one).
    pub(crate) fn html(&self) -> String {
        match self.mode {
            Mode::Visual => self.text.to_html(),
            Mode::Source => self.source.text.clone(),
        }
    }

    /// Switches between the visual editor and the HTML source. Leaving source that uses markup
    /// the editor can't show takes a second press, since that markup is dropped. Returns whether
    /// it switched.
    pub(crate) fn set_mode(&mut self, mode: Mode) -> bool {
        match mode {
            _ if mode == self.mode => {
                self.confirm_visual = false;
                false
            }
            Mode::Source => {
                self.show_source(readable(&self.text.to_html()));
                true
            }
            Mode::Visual => {
                let html = self.source.text.clone();
                let fits = simplymail_html::fits_editor(&html);
                self.show_visual(&html, fits)
            }
        }
    }

    /// Edits `html` as source.
    pub(crate) fn show_source(&mut self, html: String) {
        self.source = TextEdit::new(html);
        self.source.focused = true;
        self.link = None;
        self.confirm_visual = false;
        self.mode = Mode::Source;
    }

    /// Edits `html` visually, unless it doesn't `fit` (it has markup the editor can't show) and
    /// this is the first try. Returns whether it switched.
    pub(crate) fn show_visual(&mut self, html: &str, fits: bool) -> bool {
        if !self.confirm_visual && !fits {
            self.confirm_visual = true;
            return false;
        }
        self.text = visual(Some(html), "");
        self.text.focused = true;
        self.confirm_visual = false;
        self.mode = Mode::Visual;
        true
    }

    /// Turns `mark` on or off for the selection, or for what's typed next.
    pub(crate) fn toggle_mark(&mut self, mark: Mark) {
        self.text.toggle_mark(mark);
        self.text.focused = true;
    }

    /// Turns the selected paragraphs into `block`s, or back into paragraphs.
    pub(crate) fn toggle_block(&mut self, block: Block) {
        self.text.toggle_block(block);
        self.text.focused = true;
    }

    /// Starts typing a link's address for the selection, or stops.
    pub(crate) fn toggle_link(&mut self) {
        if self.link.take().is_some() {
            self.text.focused = true;
            return;
        }
        let mut link = TextEdit::new(self.text.active_link().unwrap_or_default());
        link.focused = true;
        self.link = Some(link);
    }

    /// Links the selection to the address typed, or unlinks it when the address is blank.
    pub(crate) fn apply_link(&mut self) {
        let Some(link) = self.link.take() else { return };
        let url = link.text.trim();
        let url = if url.is_empty() || url.contains(':') || url.starts_with('#') {
            url.to_owned()
        } else if url.contains('@') && !url.contains('/') {
            format!("mailto:{url}")
        } else {
            format!("https://{url}")
        };
        self.text.set_link(Some(&url));
        self.text.focused = true;
    }

    pub(crate) fn remove_link(&mut self) {
        self.link = None;
        self.text.set_link(None);
        self.text.focused = true;
    }

    /// Breaks the line where the caret is, as Return does in any document.
    pub(crate) fn new_line(&mut self) {
        match self.mode {
            Mode::Visual => self.text.insert("\n"),
            Mode::Source => self.source.insert("\n"),
        }
    }
}

/// The editor's HTML with a line break after each block, to read and edit as source.
pub(crate) fn readable(html: &str) -> String {
    let mut out = String::with_capacity(html.len() + html.len() / 8);
    let mut rest = html;
    while let Some(i) = rest.find("</") {
        let close = rest.get(i..).and_then(|r| r.find('>')).map(|j| i + j + 1);
        let Some(close) = close else { break };
        let tag = &rest[i + 2..close - 1];
        out.push_str(&rest[..close]);
        if matches!(tag, "p" | "ul" | "ol" | "li" | "blockquote" | "pre" | "div") {
            out.push('\n');
        }
        rest = &rest[close..];
    }
    out.push_str(rest);
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_link_addresses_get_a_scheme() {
        let mut e = RichEditor::visual(None, "site");
        e.text.set_selection(0, 4);
        e.toggle_link();
        e.link.as_mut().unwrap().replace_text("example.com");
        e.apply_link();
        assert!(e.html().contains("https://example.com"), "{}", e.html());
    }

    #[test]
    fn simple_source_goes_back_to_visual_at_once() {
        let mut e = RichEditor::visual(None, "");
        assert!(e.set_mode(Mode::Source));
        e.source.replace_text("<p>Hi <i>you</i></p>");
        assert!(e.set_mode(Mode::Visual));
        assert!(e.text.marks().iter().any(|m| m.mark == Mark::Italic));
    }

    #[test]
    fn leaving_rich_source_takes_a_second_press() {
        let mut e = RichEditor::source("<table><tr><td>Hi</td></tr></table>");
        assert!(e.is_formatted(), "source is always HTML");
        assert!(!e.set_mode(Mode::Visual), "the first press warns");
        assert!(e.confirm_visual);
        assert!(e.set_mode(Mode::Visual), "the second goes ahead");
        assert_eq!(e.text.text.trim(), "Hi");
    }

    #[test]
    fn source_is_shown_a_block_per_line() {
        assert_eq!(
            readable("<p>a</p><ul><li>b</li></ul>"),
            "<p>a</p>\n<ul><li>b</li>\n</ul>"
        );
    }
}
