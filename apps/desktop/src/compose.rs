//! The message being written: its fields, the formatted editor and the HTML source it can switch
//! to, the quote a reply carries, and how well mail apps support its markup.

use std::collections::BTreeSet;

use rune::{Block, Mark, TextEdit};
use simplymail_app::{Draft, Quote};
use simplymail_html::compat::{self, Report};

/// How the message's text is being edited.
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
fn editor(html: Option<&str>, text: &str) -> TextEdit {
    let edit = html.map_or_else(|| TextEdit::new(text), TextEdit::from_html);
    edit.rich().allow_marks(marks()).allow_blocks(BLOCKS)
}

/// The compose form's collapsible sections that are open.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Expanded {
    /// The quote of the message replied to, in full.
    pub(crate) quote: bool,
    /// What each mail app doesn't support.
    pub(crate) compat: bool,
}

/// A message being written, while `open`.
#[derive(Default)]
pub(crate) struct ComposeForm {
    pub(crate) open: bool,
    /// The account it's sent from.
    pub(crate) account: String,
    pub(crate) to: TextEdit,
    pub(crate) cc: TextEdit,
    pub(crate) bcc: TextEdit,
    pub(crate) subject: TextEdit,
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
    /// The message replied to, quoted below.
    pub(crate) quote: Option<Quote>,
    /// The features the quote's markup uses, found once.
    quote_features: BTreeSet<String>,
    /// The quote's HTML as Markdown, to show, converted once.
    pub(crate) quote_markdown: Option<String>,

    /// How well mail apps support the message's markup, as of the last edit.
    pub(crate) compat: Report,
    /// Which of the form's collapsible sections are open.
    pub(crate) expanded: Expanded,
    in_reply_to: Option<String>,
    references: Vec<String>,
    /// The mailroom is sending it.
    pub(crate) sending: bool,
    /// Why it couldn't be sent.
    pub(crate) error: Option<String>,
}

impl ComposeForm {
    /// The form for `draft`, focused where typing starts: the recipients of a new message, or the
    /// top of a reply's text, above the quote.
    pub(crate) fn new(draft: Draft) -> Self {
        let quote_features = draft
            .quote
            .as_ref()
            .map(|q| compat::features(&q.as_html()))
            .unwrap_or_default();
        let mut form = Self {
            open: true,
            account: draft.account,
            to: TextEdit::new(draft.to),
            cc: TextEdit::new(draft.cc),
            bcc: TextEdit::new(draft.bcc),
            subject: TextEdit::new(draft.subject),
            text: editor(draft.html.as_deref(), &draft.text),
            source: TextEdit::new(""),
            mode: Mode::Visual,
            confirm_visual: false,
            link: None,
            quote_markdown: draft
                .quote
                .as_ref()
                .and_then(|q| q.html.as_deref())
                .map(simplymail_html::to_markdown),
            quote: draft.quote,
            quote_features,
            compat: Report::default(),
            expanded: Expanded::default(),
            in_reply_to: draft.in_reply_to,
            references: draft.references,
            sending: false,
            error: None,
        };
        if form.in_reply_to.is_some() {
            form.text.set_caret(0);
            form.text.focused = true;
        } else {
            form.to.focused = true;
        }
        form.refresh();
        form
    }

    /// Whether it's open and being sent.
    pub(crate) const fn is_sending(&self) -> bool {
        self.open && self.sending
    }

    pub(crate) const fn is_reply(&self) -> bool {
        self.in_reply_to.is_some()
    }

    /// Whether what's written has any formatting, so it goes as HTML.
    fn is_formatted(&self) -> bool {
        match self.mode {
            Mode::Source => true,
            Mode::Visual => {
                !self.text.marks().is_empty()
                    || self.text.blocks().iter().any(|b| *b != Block::Paragraph)
            }
        }
    }

    /// What's written, as an HTML fragment (or document, if the source is one).
    pub(crate) fn body_html(&self) -> String {
        match self.mode {
            Mode::Visual => self.text.to_html(),
            Mode::Source => self.source.text.clone(),
        }
    }

    /// Scores the markup again, after an edit.
    pub(crate) fn refresh(&mut self) {
        let mut used = if self.is_formatted() {
            compat::features(&self.body_html())
        } else {
            BTreeSet::new()
        };
        used.extend(self.quote_features.iter().cloned());
        self.compat = compat::report(&used);
    }

    /// The fields as a draft. Unformatted text goes as plain text.
    pub(crate) fn draft(&self) -> Draft {
        let (text, html) = if self.is_formatted() {
            let html = self.body_html();
            (simplymail_html::to_text(&html), Some(html))
        } else {
            (self.text.text.clone(), None)
        };
        Draft {
            account: self.account.clone(),
            to: self.to.text.clone(),
            cc: self.cc.text.clone(),
            bcc: self.bcc.text.clone(),
            subject: self.subject.text.clone(),
            text,
            html,
            quote: self.quote.clone(),
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
        }
    }

    /// Switches between the visual editor and the HTML source. Leaving source that uses markup
    /// the editor can't show takes a second press, since that markup is dropped.
    pub(crate) fn set_mode(&mut self, mode: Mode) {
        if mode == self.mode {
            self.confirm_visual = false;
            return;
        }
        match mode {
            Mode::Source => {
                self.source = TextEdit::new(readable(&self.text.to_html()));
                self.source.focused = true;
                self.link = None;
            }
            Mode::Visual => {
                if !self.confirm_visual && !simplymail_html::fits_editor(&self.source.text) {
                    self.confirm_visual = true;
                    return;
                }
                self.text = editor(Some(&self.source.text), "");
                self.text.focused = true;
            }
        }
        self.confirm_visual = false;
        self.mode = mode;
        self.refresh();
    }

    /// Turns `mark` on or off for the selection, or for what's typed next.
    pub(crate) fn toggle_mark(&mut self, mark: Mark) {
        self.text.toggle_mark(mark);
        self.text.focused = true;
        self.refresh();
    }

    /// Turns the selected paragraphs into `block`s, or back into paragraphs.
    pub(crate) fn toggle_block(&mut self, block: Block) {
        self.text.toggle_block(block);
        self.text.focused = true;
        self.refresh();
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
        self.refresh();
    }

    pub(crate) fn remove_link(&mut self) {
        self.link = None;
        self.text.set_link(None);
        self.text.focused = true;
        self.refresh();
    }

    /// Drops the quote of the message replied to.
    pub(crate) fn remove_quote(&mut self) {
        self.quote = None;
        self.quote_markdown = None;
        self.quote_features.clear();
        self.refresh();
    }
}

/// The editor's HTML with a line break after each block, to read and edit as source.
fn readable(html: &str) -> String {
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
    use simplymail_app::Quote;

    use super::*;

    #[test]
    fn a_compose_form_gives_back_its_draft() {
        let draft = Draft {
            to: "ada@example.com".into(),
            subject: "Re: Lunch".into(),
            text: "Noon works.".into(),
            quote: Some(Quote {
                header: "Ada wrote:".into(),
                text: "Noon?".into(),
                html: None,
            }),
            in_reply_to: Some("parent@example.com".into()),
            references: vec!["parent@example.com".into()],
            ..Draft::new("me@example.com")
        };
        let form = ComposeForm::new(draft.clone());
        assert!(form.is_reply());
        assert!(form.text.focused, "a reply starts in its text");
        assert_eq!(form.draft(), draft, "plain text stays plain");
        assert!(
            ComposeForm::new(Draft::new("me@example.com")).to.focused,
            "a new message starts at its recipients"
        );
    }

    #[test]
    fn formatting_is_sent_as_html_with_a_text_version() {
        let mut form = ComposeForm::new(Draft::new("me@example.com"));
        form.text.replace_text("Hello there");
        form.text.set_selection(6, 11);
        form.toggle_mark(Mark::Bold);
        let draft = form.draft();
        assert_eq!(draft.text, "Hello there");
        let html = draft.html.unwrap();
        assert!(
            html.contains("<b>there</b>") || html.contains("<strong>there</strong>"),
            "{html}"
        );
        assert_eq!(form.compat.percent(), 100, "{:?}", form.compat);
    }

    #[test]
    fn source_mode_scores_its_markup() {
        let mut form = ComposeForm::new(Draft::new("me@example.com"));
        form.text.replace_text("Hi");
        form.set_mode(Mode::Source);
        assert_eq!(form.mode, Mode::Source);
        #[expect(
            clippy::literal_string_with_formatting_args,
            reason = "CSS, not a format string"
        )]
        form.source
            .replace_text("<style>p{display:flex}</style><p style='display:grid'>Hi</p>");
        form.refresh();
        assert!(form.compat.percent() < 100, "{:?}", form.compat);
        assert!(form.draft().html.unwrap().contains("display:grid"));

        form.set_mode(Mode::Visual);
        assert_eq!(form.mode, Mode::Source, "the first press warns");
        assert!(form.confirm_visual);
        form.set_mode(Mode::Visual);
        assert_eq!(form.mode, Mode::Visual, "the second goes ahead");
        assert_eq!(form.text.text.trim(), "Hi");
    }

    #[test]
    fn simple_source_goes_back_to_visual_at_once() {
        let mut form = ComposeForm::new(Draft::new("me@example.com"));
        form.set_mode(Mode::Source);
        form.source.replace_text("<p>Hi <i>you</i></p>");
        form.set_mode(Mode::Visual);
        assert_eq!(form.mode, Mode::Visual);
        assert!(form.text.marks().iter().any(|m| m.mark == Mark::Italic));
    }

    #[test]
    fn typed_link_addresses_get_a_scheme() {
        let mut form = ComposeForm::new(Draft::new("me@example.com"));
        form.text.replace_text("site");
        form.text.set_selection(0, 4);
        form.toggle_link();
        form.link.as_mut().unwrap().replace_text("example.com");
        form.apply_link();
        assert!(
            form.body_html().contains("https://example.com"),
            "{}",
            form.body_html()
        );
    }

    #[test]
    fn the_quote_counts_toward_the_score() {
        let mut draft = Draft::new("me@example.com");
        draft.quote = Some(Quote {
            header: "Ada wrote:".into(),
            text: "x".into(),
            html: Some("<div style=\"display:flex\">x</div>".into()),
        });
        let mut form = ComposeForm::new(draft);
        assert!(form.compat.percent() < 100, "{:?}", form.compat);
        form.remove_quote();
        assert_eq!(form.compat.percent(), 100);
    }

    #[test]
    fn source_is_shown_a_block_per_line() {
        assert_eq!(
            readable("<p>a</p><ul><li>b</li></ul>"),
            "<p>a</p>\n<ul><li>b</li>\n</ul>"
        );
    }
}
