//! An account's settings page: what it's called, the name its mail is sent under, and its
//! signature, written in the visual editor or as HTML, with images.

use rune::{ImageCache, TextEdit};
use simplymail_app::signature;
use simplymail_core::account::{Account, Signature};
use simplymail_core::message::InlineImage;

use crate::compose::Preview;
use crate::rich::{Mode, RichEditor, readable};

/// The file types a signature's images can be picked from.
pub(crate) const IMAGE_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "gif"];

/// What the page saves, to tell whether anything changed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Saved {
    label: Option<String>,
    name: Option<String>,
    signature: Option<Signature>,
}

/// An account's settings, as they're edited, while `open`.
#[derive(Default)]
pub(crate) struct AccountSettings {
    pub(crate) open: bool,
    /// The account's id.
    pub(crate) account: String,
    /// What the UI calls it, if not its name or address.
    pub(crate) label: TextEdit,
    /// The name its mail is sent under.
    pub(crate) name: TextEdit,
    /// The signature's text: in the visual editor, its images follow it; as source, they go
    /// wherever it shows them.
    pub(crate) signature: RichEditor,
    /// The signature's images, in order.
    pub(crate) images: Vec<InlineImage>,
    /// How the signature looks, as of the last edit.
    pub(crate) preview: Preview,
    /// Files are being dragged over the images.
    pub(crate) dropping: bool,
    /// Why saving, or adding an image, failed.
    pub(crate) error: Option<String>,
    saved: Saved,
}

/// Text typed in a field, or `None` when it's blank.
fn optional(edit: &TextEdit) -> Option<String> {
    Some(edit.text.trim().to_owned()).filter(|t| !t.is_empty())
}

impl AccountSettings {
    /// The page for `account`, whose signature is `signature`. A signature written in the
    /// visual editor opens there again, unless its HTML has since become more than it shows.
    pub(crate) fn new(
        account: &Account,
        signature: Option<&Signature>,
        images: Option<&ImageCache>,
    ) -> Self {
        let editor = match signature {
            Some(s) if s.source => RichEditor::source(&s.html),
            Some(s) => match signature::to_editor(&s.html, &s.images) {
                (html, true) => RichEditor::visual(Some(&html), ""),
                (_, false) => RichEditor::source(&s.html),
            },
            None => RichEditor::visual(None, ""),
        };
        let mut page = Self {
            open: true,
            account: account.id.clone(),
            label: TextEdit::new(account.label.clone().unwrap_or_default()),
            name: TextEdit::new(account.display_name.clone().unwrap_or_default()),
            signature: editor,
            images: signature.map(|s| s.images.clone()).unwrap_or_default(),
            preview: Preview::default(),
            dropping: false,
            error: None,
            saved: Saved::default(),
        };
        page.saved = page.current();
        page.refresh(images);
        page
    }

    /// The label to save, or `None` to show the name or address.
    pub(crate) fn label(&self) -> Option<String> {
        optional(&self.label)
    }

    /// The name to send mail under, or `None` for the address alone.
    pub(crate) fn name(&self) -> Option<String> {
        optional(&self.name)
    }

    /// The signature's HTML as it stands, its images' pills turned back into `<img>` tags.
    fn html(&self) -> String {
        match self.signature.mode {
            Mode::Visual => signature::from_editor(&self.signature.text.to_html(), &self.images),
            Mode::Source => self.signature.source.text.clone(),
        }
    }

    fn signature(&self) -> Signature {
        Signature {
            html: self.html(),
            source: self.signature.mode == Mode::Source,
            images: self.images.clone(),
        }
    }

    /// The signature as written, or `None` when it's blank.
    pub(crate) fn built(&self) -> Option<Signature> {
        Some(self.signature()).filter(|s| !signature::is_blank(s))
    }

    /// The `Content-ID`s of the images the signature shows.
    pub(crate) fn shown(&self) -> Vec<String> {
        signature::shown_images(&self.signature())
            .into_iter()
            .map(|i| i.cid)
            .collect()
    }

    fn current(&self) -> Saved {
        Saved {
            label: self.label(),
            name: self.name(),
            signature: self.built(),
        }
    }

    /// Whether it's open with something that differs from what's saved.
    pub(crate) fn is_changed(&self) -> bool {
        if !self.open {
            return false;
        }
        // An editor's HTML can differ from what it was given without the text differing, so a
        // visual signature compares by what it shows, images in place.
        let shown = |s: &Option<Signature>| {
            s.as_ref().map(|s| {
                (
                    signature::text(s),
                    signature::image_widths(&s.html),
                    s.images.clone(),
                    s.source,
                )
            })
        };
        let current = self.current();
        current.label != self.saved.label
            || current.name != self.saved.name
            || match (&current.signature, &self.saved.signature) {
                (Some(a), Some(b)) if a.source || b.source => a != b,
                _ => shown(&current.signature) != shown(&self.saved.signature),
            }
    }

    /// Draws the preview again, after an edit.
    pub(crate) fn refresh(&mut self, images: Option<&ImageCache>) {
        self.preview = Preview::of(&self.signature(), images);
    }

    /// Switches between the visual editor and the HTML source. Images stay where they stand:
    /// pills in the editor, `<img>` tags in the source.
    pub(crate) fn set_mode(&mut self, mode: Mode) {
        match mode {
            _ if mode == self.signature.mode => {
                self.signature.set_mode(mode);
            }
            Mode::Source => {
                let html = readable(&self.html());
                self.signature.show_source(html);
            }
            Mode::Visual => {
                let (html, fits) = signature::to_editor(&self.signature.source.text, &self.images);
                self.signature.show_visual(&html, fits);
            }
        }
    }

    /// Adds `image` to the signature where the caret is.
    pub(crate) fn add_image(&mut self, image: InlineImage) {
        let cid = image.cid.clone();
        self.images.push(image);
        self.insert_image(&cid);
        self.error = None;
    }

    /// Shows one of the signature's images where the caret is, again or elsewhere.
    pub(crate) fn insert_image(&mut self, cid: &str) {
        let Some(image) = self.images.iter().find(|i| i.cid == cid) else {
            return;
        };
        match self.signature.mode {
            Mode::Visual => {
                let (id, label) = (signature::pill_id(image), image.name.clone());
                self.signature.text.insert_pill(id, &label);
                self.signature.text.focused = true;
            }
            Mode::Source => {
                self.signature.source.insert(&signature::image_tag(image));
                self.signature.source.focused = true;
            }
        }
    }

    /// Takes an image out of the signature, wherever it shows it.
    pub(crate) fn remove_image(&mut self, cid: &str) {
        let html = signature::remove_image_tags(&self.html(), cid);
        self.images.retain(|i| i.cid != cid);
        match self.signature.mode {
            Mode::Visual => {
                let (html, _) = signature::to_editor(&html, &self.images);
                self.signature.text = RichEditor::visual(Some(&html), "").text;
            }
            Mode::Source => self.signature.source.replace_text(html),
        }
    }
}

#[cfg(test)]
mod tests {
    use simplymail_core::account::{Auth, Security, Server};

    use super::*;

    fn account() -> Account {
        let server = Server {
            host: "localhost".into(),
            port: 1,
            security: Security::None,
            username: "me".into(),
        };
        Account {
            id: "me@example.com".into(),
            display_name: Some("Ada".into()),
            label: None,
            imap: server.clone(),
            smtp: server,
            auth: Auth::Password,
        }
    }

    fn logo(cid: &str) -> InlineImage {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(64_u32.to_be_bytes());
        png.extend(32_u32.to_be_bytes());
        InlineImage {
            cid: cid.into(),
            name: format!("{cid}.png"),
            mime: "image/png".into(),
            data: png.into(),
        }
    }

    #[test]
    fn a_new_page_has_nothing_to_save() {
        assert!(!AccountSettings::default().is_changed(), "closed");
        let page = AccountSettings::new(&account(), None, None);
        assert!(!page.is_changed());
        assert_eq!(page.built(), None);
        assert_eq!(page.name().as_deref(), Some("Ada"));
        assert_eq!(page.label(), None);

        let signature = Signature {
            html: "<p><b>Ada</b> Lovelace</p>".into(),
            source: false,
            images: vec![logo("logo")],
        };
        let page = AccountSettings::new(&account(), Some(&signature), None);
        assert!(
            !page.is_changed(),
            "the editor's HTML may differ, but not what it shows"
        );
    }

    #[test]
    fn edits_are_noticed_and_blank_fields_left_out() {
        let mut page = AccountSettings::new(&account(), None, None);
        page.label.replace_text("  Work ");
        assert!(page.is_changed());
        assert_eq!(page.label().as_deref(), Some("Work"));
        page.label.replace_text(" ");
        assert!(!page.is_changed(), "blank is no label, as before");
        page.name.replace_text(" ");
        assert_eq!(page.name(), None);
        page.signature.text.replace_text("Ada");
        let built = page.built().unwrap();
        assert!(!built.source);
        assert_eq!(signature::text(&built), "Ada");
    }

    #[test]
    fn images_go_where_the_caret_is_and_stay_there() {
        let mut page = AccountSettings::new(&account(), None, None);
        page.signature.text.replace_text("John Doe\n\n1 Main St");
        page.signature.text.set_caret("John Doe\n".len());
        page.add_image(logo("logo"));
        let built = page.built().unwrap();
        assert!(!built.source);
        assert_eq!(signature::text(&built), "John Doe\n\n[logo]\n\n1 Main St");
        assert_eq!(page.shown(), ["logo"]);
        assert!(page.is_changed());

        page.set_mode(Mode::Source);
        let source = page.signature.source.text.clone();
        let at = |s: &str| source.find(s).unwrap();
        assert!(
            at("John") < at("cid:logo") && at("cid:logo") < at("Main"),
            "{source}"
        );
        assert!(source.contains("width=\"64\" height=\"32\""), "{source}");

        page.set_mode(Mode::Visual);
        assert_eq!(
            page.signature.mode,
            Mode::Visual,
            "an image is no reason to stay"
        );
        assert_eq!(page.signature.text.pills().count(), 1);
        assert_eq!(
            signature::text(&page.built().unwrap()),
            signature::text(&built)
        );

        page.add_image(logo("badge"));
        page.remove_image("logo");
        assert_eq!(page.shown(), ["badge"]);
        assert_eq!(page.images.len(), 1);
        let text = signature::text(&page.built().unwrap());
        assert!(
            text.contains("John Doe") && !text.contains("[logo]"),
            "{text}"
        );
    }

    #[test]
    fn a_signature_opens_as_it_was_written() {
        let visual = Signature {
            html: "<p>John Doe</p><p><img src=\"cid:logo\" alt=\"logo\" width=\"64\" height=\"32\"></p>"
                .into(),
            source: false,
            images: vec![logo("logo")],
        };
        let page = AccountSettings::new(&account(), Some(&visual), None);
        assert_eq!(page.signature.mode, Mode::Visual);
        assert_eq!(
            page.built().unwrap().html,
            visual.html,
            "saved again as it was"
        );

        let styled = Signature {
            html: "<p style=\"color:red\">John</p>".into(),
            ..visual
        };
        let page = AccountSettings::new(&account(), Some(&styled), None);
        assert_eq!(
            page.signature.mode,
            Mode::Source,
            "rather than drop the style"
        );
    }
}
