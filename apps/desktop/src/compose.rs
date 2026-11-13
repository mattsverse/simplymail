//! The message being written: its fields, its text ([`RichEditor`]), the signature below it, the
//! quote a reply carries, and how well mail apps support its markup.

use std::collections::{BTreeSet, HashMap};

use rune::{Image, ImageCache, TextEdit};
use simplymail_app::{Draft, Quote, signature};
use simplymail_core::account::Signature;
use simplymail_html::compat::{self, Report};

pub(crate) use crate::rich::Mode;
use crate::rich::RichEditor;

/// The compose form's collapsible sections that are open.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Expanded {
    /// The quote of the message replied to, in full.
    pub(crate) quote: bool,
    /// What each mail app doesn't support.
    pub(crate) compat: bool,
}

/// A signature as it's shown: as Markdown, with its images.
#[derive(Default)]
pub(crate) struct Preview {
    pub(crate) markdown: String,
    /// The pictures of its images, by `cid:` source.
    pub(crate) pictures: HashMap<String, Image>,
    /// The width each image is sent at, by `cid:` source, when the signature sets it.
    pub(crate) widths: HashMap<String, f32>,
}

impl Preview {
    /// How `signature` looks, its pictures decoded by `images`, if there's a cache.
    pub(crate) fn of(signature: &Signature, images: Option<&ImageCache>) -> Self {
        let source = |cid: &str| format!("cid:{cid}");
        Self {
            markdown: simplymail_html::to_markdown_with_cid_images(&signature.html),
            pictures: images.map_or_else(HashMap::new, |cache| {
                signature
                    .images
                    .iter()
                    .map(|i| (source(&i.cid), cache.load(std::sync::Arc::clone(&i.data))))
                    .collect()
            }),
            widths: signature::image_widths(&signature.html)
                .into_iter()
                .filter_map(|(cid, w)| Some((source(&cid), f32::from(u16::try_from(w).ok()?))))
                .collect(),
        }
    }
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
    /// What's written.
    pub(crate) body: RichEditor,
    /// The account's signature, below what's written.
    pub(crate) signature: Option<Signature>,
    /// How the signature looks.
    pub(crate) signature_preview: Preview,
    /// The features the signature's markup uses, found once.
    signature_features: BTreeSet<String>,
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
    /// top of a reply's text, above the quote. The signature's pictures are decoded by `images`.
    pub(crate) fn new(draft: Draft, images: Option<&ImageCache>) -> Self {
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
            body: RichEditor::visual(draft.html.as_deref(), &draft.text),
            signature: None,
            signature_preview: Preview::default(),
            signature_features: BTreeSet::new(),
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
            form.body.text.set_caret(0);
            form.body.text.focused = true;
        } else {
            form.to.focused = true;
        }
        form.set_signature(draft.signature, images);
        form
    }

    /// Whether it's open and being sent.
    pub(crate) const fn is_sending(&self) -> bool {
        self.open && self.sending
    }

    pub(crate) const fn is_reply(&self) -> bool {
        self.in_reply_to.is_some()
    }

    /// Scores the markup again, after an edit.
    pub(crate) fn refresh(&mut self) {
        let mut used = if self.body.is_formatted() {
            compat::features(&self.body.html())
        } else {
            BTreeSet::new()
        };
        used.extend(self.signature_features.iter().cloned());
        used.extend(self.quote_features.iter().cloned());
        self.compat = compat::report(&used);
    }

    /// The fields as a draft. Unformatted text goes as plain text.
    pub(crate) fn draft(&self) -> Draft {
        let (text, html) = if self.body.is_formatted() {
            let html = self.body.html();
            (simplymail_html::to_text(&html), Some(html))
        } else {
            (self.body.text.text.clone(), None)
        };
        Draft {
            account: self.account.clone(),
            to: self.to.text.clone(),
            cc: self.cc.text.clone(),
            bcc: self.bcc.text.clone(),
            subject: self.subject.text.clone(),
            text,
            html,
            signature: self.signature.clone(),
            quote: self.quote.clone(),
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
        }
    }

    /// Puts `signature` below what's written, in place of any other, as when the message is sent
    /// from another account.
    pub(crate) fn set_signature(
        &mut self,
        signature: Option<Signature>,
        images: Option<&ImageCache>,
    ) {
        self.signature_preview = signature
            .as_ref()
            .map(|s| Preview::of(s, images))
            .unwrap_or_default();
        self.signature_features = signature
            .as_ref()
            .filter(|s| signature::is_formatted(s))
            .map(|s| compat::features(&s.html))
            .unwrap_or_default();
        self.signature = signature;
        self.refresh();
    }

    /// Switches between the visual editor and the HTML source.
    pub(crate) fn set_mode(&mut self, mode: Mode) {
        if self.body.set_mode(mode) {
            self.refresh();
        }
    }

    /// Drops the quote of the message replied to.
    pub(crate) fn remove_quote(&mut self) {
        self.quote = None;
        self.quote_markdown = None;
        self.quote_features.clear();
        self.refresh();
    }
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
            signature: Some(Signature {
                html: "<p>Me</p>".into(),
                source: false,
                images: Vec::new(),
            }),
            quote: Some(Quote {
                header: "Ada wrote:".into(),
                text: "Noon?".into(),
                html: None,
            }),
            in_reply_to: Some("parent@example.com".into()),
            references: vec!["parent@example.com".into()],
            ..Draft::new("me@example.com")
        };
        let form = ComposeForm::new(draft.clone(), None);
        assert!(form.is_reply());
        assert!(form.body.text.focused, "a reply starts in its text");
        assert_eq!(form.draft(), draft, "plain text stays plain");
        assert_eq!(form.signature_preview.markdown, "Me");
        assert!(
            ComposeForm::new(Draft::new("me@example.com"), None)
                .to
                .focused,
            "a new message starts at its recipients"
        );
    }

    #[test]
    fn formatting_is_sent_as_html_with_a_text_version() {
        let mut form = ComposeForm::new(Draft::new("me@example.com"), None);
        form.body.text.replace_text("Hello there");
        form.body.text.set_selection(6, 11);
        form.body.toggle_mark(rune::Mark::Bold);
        form.refresh();
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
        let mut form = ComposeForm::new(Draft::new("me@example.com"), None);
        form.body.text.replace_text("Hi");
        form.set_mode(Mode::Source);
        assert_eq!(form.body.mode, Mode::Source);
        #[expect(
            clippy::literal_string_with_formatting_args,
            reason = "CSS, not a format string"
        )]
        form.body
            .source
            .replace_text("<style>p{display:flex}</style><p style='display:grid'>Hi</p>");
        form.refresh();
        assert!(form.compat.percent() < 100, "{:?}", form.compat);
        assert!(form.draft().html.unwrap().contains("display:grid"));

        form.set_mode(Mode::Visual);
        assert_eq!(form.body.mode, Mode::Source, "the first press warns");
        form.set_mode(Mode::Visual);
        assert_eq!(form.body.mode, Mode::Visual, "the second goes ahead");
        assert_eq!(form.body.text.text.trim(), "Hi");
    }

    #[test]
    fn the_quote_and_signature_count_toward_the_score() {
        let mut draft = Draft::new("me@example.com");
        draft.quote = Some(Quote {
            header: "Ada wrote:".into(),
            text: "x".into(),
            html: Some("<div style=\"display:flex\">x</div>".into()),
        });
        let mut form = ComposeForm::new(draft, None);
        assert!(form.compat.percent() < 100, "{:?}", form.compat);
        form.remove_quote();
        assert_eq!(form.compat.percent(), 100);

        let flex = Signature {
            html: "<div style=\"display:flex\">Me</div>".into(),
            source: true,
            images: Vec::new(),
        };
        form.set_signature(Some(flex), None);
        assert!(form.compat.percent() < 100, "{:?}", form.compat);
        form.set_signature(None, None);
        assert_eq!(form.compat.percent(), 100);
        assert_eq!(form.draft().signature, None, "removed");
    }
}
