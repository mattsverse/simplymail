//! An account's [`Signature`]: the HTML and text it adds to a message, the images it sends
//! along, and reading those images from files.

use std::hash::{BuildHasher as _, Hasher as _};
use std::path::Path;

use simplymail_core::account::Signature;
use simplymail_core::message::InlineImage;
use simplymail_store::Store;

use crate::{Error, Result};

/// The largest image a signature takes. Every message carries its signature's images, so they
/// should stay small; a logo is a few kilobytes.
pub const MAX_IMAGE_BYTES: usize = 1024 * 1024;

/// The widest a signature's image is shown when first added, in CSS pixels. Wider ones are
/// scaled down; the HTML source can size them otherwise.
const MAX_WIDTH: u32 = 300;

/// The image formats every major mail app shows.
const FORMATS: [(&str, &[u8]); 3] = [
    ("image/png", b"\x89PNG\r\n\x1a\n"),
    ("image/jpeg", b"\xff\xd8\xff"),
    ("image/gif", b"GIF8"),
];

/// Starts the id of a pill that stands for an image in the visual editor:
/// `img:CID|WIDTH|HEIGHT`, each size empty when the image has none.
const PILL: &str = "img:";

/// The signature as plain text, for apps that don't show HTML. Images are their alt text.
#[must_use]
pub fn text(signature: &Signature) -> String {
    simplymail_html::to_text(&signature.html)
}

/// Whether the signature needs HTML: it has images or formatting.
#[must_use]
pub fn is_formatted(signature: &Signature) -> bool {
    signature.source || !simplymail_html::is_plain(&signature.html)
}

/// Whether the signature adds nothing: no text and no images.
#[must_use]
pub fn is_blank(signature: &Signature) -> bool {
    text(signature).trim().is_empty() && img_tags(&signature.html).is_empty()
}

/// The images the signature shows, to send with it. Those it no longer shows are left out.
#[must_use]
pub fn shown_images(signature: &Signature) -> Vec<InlineImage> {
    let shown: Vec<String> = img_tags(&signature.html)
        .iter()
        .filter_map(|t| t.cid().map(str::to_owned))
        .collect();
    signature
        .images
        .iter()
        .filter(|image| shown.contains(&image.cid))
        .cloned()
        .collect()
}

/// The width each image is shown at, by `Content-ID`, where the HTML sets one.
#[must_use]
pub fn image_widths(html: &str) -> Vec<(String, u32)> {
    img_tags(html)
        .iter()
        .filter_map(|t| Some((t.cid()?.to_owned(), t.attr("width")?.trim().parse().ok()?)))
        .collect()
}

/// The width and height an image is first shown at: its own, scaled down to fit
/// [`MAX_WIDTH`]. `None` when its size can't be read.
#[must_use]
pub fn shown_size(image: &InlineImage) -> Option<(u32, u32)> {
    let (w, h) = size(&image.data)?;
    let width = w.min(MAX_WIDTH);
    let height = u64::from(h) * u64::from(width) / u64::from(w.max(1));
    Some((width, u32::try_from(height).unwrap_or(h).max(1)))
}

/// An `<img>` showing `image` at the size it's first shown at.
#[must_use]
pub fn image_tag(image: &InlineImage) -> String {
    sized_tag(image, shown_size(image))
}

/// An `<img>` showing `image`, `size` pixels wide and tall if given.
fn sized_tag(image: &InlineImage, size: Option<(u32, u32)>) -> String {
    let alt = Path::new(&image.name)
        .file_stem()
        .map_or_else(|| image.name.clone(), |s| s.to_string_lossy().into_owned());
    let size = size.map_or_else(String::new, |(width, height)| {
        format!(" width=\"{width}\" height=\"{height}\"")
    });
    format!(
        "<img src=\"cid:{}\" alt=\"{}\"{size}>",
        image.cid,
        simplymail_html::escape(&alt)
    )
}

/// The id of the pill that stands for `image` in the visual editor, at the size it's first
/// shown at. Its label is the image's name.
#[must_use]
pub fn pill_id(image: &InlineImage) -> String {
    pill(&image.cid, shown_size(image))
}

fn pill(cid: &str, size: Option<(u32, u32)>) -> String {
    let (w, h) = size.map_or_else(Default::default, |(w, h)| (w.to_string(), h.to_string()));
    format!("{PILL}{cid}|{w}|{h}")
}

/// The image a pill's id stands for, and the size it's shown at.
#[must_use]
pub fn pill_image(id: &str) -> Option<(&str, Option<(u32, u32)>)> {
    let mut parts = id.strip_prefix(PILL)?.split('|');
    let cid = parts.next().filter(|c| !c.is_empty())?;
    let w = parts.next().and_then(|w| w.parse().ok());
    let h = parts.next().and_then(|h| h.parse().ok());
    Some((cid, w.zip(h)))
}

/// A signature's HTML for the visual editor, each image a pill where it stands in the text.
///
/// The pill is `<span data-pill="img:…">logo.png</span>`. Also returns whether the editor
/// keeps everything; when it doesn't (styles, tables, an image sized other than by `width` and
/// `height`), opening it there drops what it can't show.
#[must_use]
pub fn to_editor(html: &str, images: &[InlineImage]) -> (String, bool) {
    let mut out = String::with_capacity(html.len());
    let mut without_images = String::with_capacity(html.len());
    let mut fits = true;
    let mut at = 0;
    for tag in img_tags(html) {
        let before = &html[at..tag.range.start];
        out.push_str(before);
        without_images.push_str(before);
        at = tag.range.end;
        let simple = tag
            .attrs
            .iter()
            .all(|(name, _)| matches!(name.as_str(), "src" | "alt" | "width" | "height"));
        let image = tag
            .cid()
            .and_then(|cid| images.iter().find(|i| i.cid == cid));
        match image {
            Some(image) if simple => {
                let size = tag
                    .attr("width")
                    .and_then(|w| w.trim().parse().ok())
                    .zip(tag.attr("height").and_then(|h| h.trim().parse().ok()));
                fits &= tag.attr("width").is_some() == size.is_some()
                    && tag.attr("height").is_some() == size.is_some();
                out.extend([
                    "<span data-pill=\"",
                    &pill(&image.cid, size),
                    "\">",
                    &simplymail_html::escape(&image.name),
                    "</span>",
                ]);
            }
            _ => fits = false,
        }
    }
    out.push_str(&html[at..]);
    without_images.push_str(&html[at..]);
    (out, fits && simplymail_html::fits_editor(&without_images))
}

/// The visual editor's HTML as a signature's: each image's pill back to an `<img>`. Pills for
/// images that aren't in `images` are dropped.
#[must_use]
pub fn from_editor(html: &str, images: &[InlineImage]) -> String {
    const OPEN: &str = "<span data-pill=\"";
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let (Some(quote), Some(end)) = (after.find('"'), closing_span(after)) else {
            break;
        };
        out.push_str(&rest[..start]);
        if let Some((cid, size)) = pill_image(&after[..quote])
            && let Some(image) = images.iter().find(|i| i.cid == cid)
        {
            out.push_str(&sized_tag(image, size));
        } else if pill_image(&after[..quote]).is_none() {
            // Some other pill: keep it as it was.
            out.push_str(&rest[start..start + OPEN.len() + end]);
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// Where the `<span>` whose content `html` starts inside ends, past its `</span>`.
fn closing_span(html: &str) -> Option<usize> {
    let mut depth = 1_usize;
    let mut at = 0;
    while depth > 0 {
        let next = html[at..].find("<span").map(|n| n + at);
        let close = html[at..].find("</span>")? + at;
        match next {
            Some(open) if open < close => {
                depth += 1;
                at = open + 5;
            }
            _ => {
                depth -= 1;
                at = close + 7;
            }
        }
    }
    Some(at)
}

/// `html` without the `<img>` tags that show `cid`, and without the paragraphs that leaves
/// empty.
#[must_use]
pub fn remove_image_tags(html: &str, cid: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut at = 0;
    for tag in img_tags(html).into_iter().filter(|t| t.cid() == Some(cid)) {
        out.push_str(&html[at..tag.range.start]);
        at = tag.range.end;
    }
    out.push_str(&html[at..]);
    drop_empty_paragraphs(&out)
}

/// `html` without `<p>` elements holding nothing but spaces.
fn drop_empty_paragraphs(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find("<p>") {
        let after = &rest[at + 3..];
        let blank = after.len() - after.trim_start().len();
        if after[blank..].starts_with("</p>") {
            out.push_str(&rest[..at]);
            rest = &after[blank + 4..];
        } else {
            out.push_str(&rest[..at + 3]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// An `<img>` tag in some HTML: where it is, and its attributes, names lowercased.
#[derive(Debug)]
struct ImgTag {
    range: std::ops::Range<usize>,
    attrs: Vec<(String, String)>,
}

impl ImgTag {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The `Content-ID` of the image it shows, when its source is `cid:…`.
    fn cid(&self) -> Option<&str> {
        let src = self.attr("src")?.trim();
        src.get(..4)
            .filter(|scheme| scheme.eq_ignore_ascii_case("cid:"))
            .and_then(|_| src.get(4..))
    }
}

/// Every `<img>` tag in `html`, in order. Attribute values are taken as written; a signature's
/// sources and sizes have no entities in them.
fn img_tags(html: &str) -> Vec<ImgTag> {
    let lower = html.to_ascii_lowercase();
    let mut tags = Vec::new();
    let mut at = 0;
    while let Some(found) = lower[at..].find("<img") {
        let start = at + found;
        let mut i = start + 4;
        if !lower[i..].starts_with(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/') {
            at = i;
            continue;
        }
        let mut attrs = Vec::new();
        let bytes = html.as_bytes();
        loop {
            while bytes
                .get(i)
                .is_some_and(|b| b.is_ascii_whitespace() || *b == b'/')
            {
                i += 1;
            }
            match bytes.get(i) {
                None => return tags,
                Some(b'>') => break,
                Some(_) => {}
            }
            let name_end = html[i..]
                .find(|c: char| c.is_ascii_whitespace() || matches!(c, '=' | '>' | '/'))
                .map_or(html.len(), |n| i + n);
            let name = lower[i..name_end].to_owned();
            i = name_end;
            while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
                i += 1;
            }
            let mut value = String::new();
            if bytes.get(i) == Some(&b'=') {
                i += 1;
                while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
                    i += 1;
                }
                let end = match bytes.get(i) {
                    Some(&quote @ (b'"' | b'\'')) => {
                        i += 1;
                        html[i..]
                            .find(char::from(quote))
                            .map_or(html.len(), |n| i + n)
                    }
                    _ => html[i..]
                        .find(|c: char| c.is_ascii_whitespace() || c == '>')
                        .map_or(html.len(), |n| i + n),
                };
                html[i..end].clone_into(&mut value);
                i = (end + 1).min(html.len());
                if !matches!(bytes.get(end), Some(b'"' | b'\'')) {
                    i = end;
                }
            }
            if !name.is_empty() {
                attrs.push((name, value));
            }
        }
        tags.push(ImgTag {
            range: start..i + 1,
            attrs,
        });
        at = i + 1;
    }
    tags
}

/// Reads an image to add to a signature from `path`.
///
/// # Errors
///
/// Returns [`Error::Invalid`] when the file isn't a PNG, JPEG or GIF image or is larger than
/// [`MAX_IMAGE_BYTES`], and [`Error::Io`] when it can't be read.
pub fn read_image(path: &Path) -> Result<InlineImage> {
    let name = path
        .file_name()
        .map_or_else(|| "image".to_owned(), |n| n.to_string_lossy().into_owned());
    let size = std::fs::metadata(path)?.len();
    if usize::try_from(size).map_or(true, |size| size > MAX_IMAGE_BYTES) {
        return Err(Error::Invalid(format!(
            "{name} is {} KB; a signature's images can be up to {} KB",
            size / 1024,
            MAX_IMAGE_BYTES / 1024
        )));
    }
    image(name, std::fs::read(path)?)
}

/// `data`, named `name`, as an image to add to a signature, under a new `Content-ID`.
///
/// # Errors
///
/// Returns [`Error::Invalid`] when it isn't a PNG, JPEG or GIF image or is larger than
/// [`MAX_IMAGE_BYTES`].
pub fn image(name: String, data: Vec<u8>) -> Result<InlineImage> {
    if data.len() > MAX_IMAGE_BYTES {
        return Err(Error::Invalid(format!(
            "{name} is {} KB; a signature's images can be up to {} KB",
            data.len() / 1024,
            MAX_IMAGE_BYTES / 1024
        )));
    }
    let Some((mime, _)) = FORMATS.iter().find(|(_, magic)| data.starts_with(magic)) else {
        return Err(Error::Invalid(format!(
            "{name} isn't a PNG, JPEG or GIF image, the formats every mail app shows"
        )));
    };
    Ok(InlineImage {
        cid: new_cid(&name),
        mime: (*mime).to_owned(),
        name,
        data: data.into(),
    })
}

/// A `Content-ID` no other image in a message has. It's random, so it says nothing about the file
/// or when it was added.
fn new_cid(name: &str) -> String {
    let mut hasher = std::hash::RandomState::new().build_hasher();
    hasher.write(name.as_bytes());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    hasher.write_u128(nanos);
    format!("{:016x}@simplymail", hasher.finish())
}

/// An image's width and height in pixels, read from a PNG, GIF or JPEG header.
#[must_use]
pub fn size(data: &[u8]) -> Option<(u32, u32)> {
    let be16 = |at: usize| {
        Some(u32::from(u16::from_be_bytes(
            data.get(at..at + 2)?.try_into().ok()?,
        )))
    };
    let le16 = |at: usize| {
        Some(u32::from(u16::from_le_bytes(
            data.get(at..at + 2)?.try_into().ok()?,
        )))
    };
    let be32 = |at: usize| Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?));
    if data.starts_with(b"\x89PNG") {
        return Some((be32(16)?, be32(20)?)).filter(|&(w, h)| w > 0 && h > 0);
    }
    if data.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?)).filter(|&(w, h)| w > 0 && h > 0);
    }
    if !data.starts_with(b"\xff\xd8") {
        return None;
    }
    // A JPEG's segments, until the start of frame that holds its size.
    let mut at = 2;
    loop {
        if *data.get(at)? != 0xff {
            return None;
        }
        let marker = *data.get(at + 1)?;
        let length = usize::try_from(be16(at + 2)?).ok()?;
        // SOF0 to SOF15, except DHT (C4), JPG (C8) and DAC (CC).
        if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            return Some((be16(at + 7)?, be16(at + 5)?)).filter(|&(w, h)| w > 0 && h > 0);
        }
        at += 2 + length;
    }
}

/// Saves the account's signature, or removes it when it's `None` or blank.
///
/// # Errors
///
/// Returns an error when the store can't be written, such as for an account that doesn't exist.
pub fn save_signature(
    store: &mut Store,
    account: &str,
    signature: Option<&Signature>,
) -> Result<()> {
    let signature = signature.filter(|s| !is_blank(s));
    store.save_signature(account, signature)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1×1 PNG, as some file says it is: `w` by `h`.
    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut data = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        data.extend(w.to_be_bytes());
        data.extend(h.to_be_bytes());
        data.extend([8, 6, 0, 0, 0]);
        data
    }

    #[test]
    fn image_sizes_are_read_from_their_headers() {
        assert_eq!(size(&png(640, 200)), Some((640, 200)));
        let gif = b"GIF89a\x20\x00\x10\x00".to_vec();
        assert_eq!(size(&gif), Some((32, 16)));
        // SOI, an APP0 segment of 4 bytes, then SOF0: precision 8, height 0x0102, width 0x0304.
        let jpeg = b"\xff\xd8\xff\xe0\x00\x04ab\xff\xc0\x00\x11\x08\x01\x02\x03\x04".to_vec();
        assert_eq!(size(&jpeg), Some((0x0304, 0x0102)));
        assert_eq!(size(b"nope"), None);
        assert_eq!(size(b"\xff\xd8\xff\xe0\x00\x10"), None, "cut short");
    }

    #[test]
    fn only_small_images_mail_apps_show_are_taken() {
        let logo = image("logo.png".into(), png(1, 1)).unwrap();
        assert_eq!(logo.mime, "image/png");
        assert!(logo.cid.ends_with("@simplymail"), "{}", logo.cid);
        assert_ne!(
            logo.cid,
            image("logo.png".into(), png(1, 1)).unwrap().cid,
            "each gets its own id"
        );
        let svg = image("logo.svg".into(), b"<svg/>".to_vec()).unwrap_err();
        assert!(svg.to_string().contains("PNG, JPEG or GIF"), "{svg}");
        let mut big = png(1, 1);
        big.resize(MAX_IMAGE_BYTES + 1, 0);
        let big = image("big.png".into(), big).unwrap_err();
        assert!(big.to_string().contains("up to 1024 KB"), "{big}");
    }

    fn signature(html: &str, source: bool, images: Vec<InlineImage>) -> Signature {
        Signature {
            html: html.into(),
            source,
            images,
        }
    }

    fn logo(cid: &str, w: u32, h: u32) -> InlineImage {
        let mut logo = image(format!("{cid}.png"), png(w, h)).unwrap();
        logo.cid = format!("{cid}@simplymail");
        logo
    }

    #[test]
    fn images_stand_among_the_lines_and_only_those_shown_are_sent() {
        let (shown, unused) = (logo("logo", 600, 200), logo("unused", 1, 1));
        assert_eq!(
            image_tag(&shown),
            "<img src=\"cid:logo@simplymail\" alt=\"logo\" width=\"300\" height=\"100\">",
            "scaled down to fit"
        );
        let s = signature(
            &format!(
                "<p>John Doe</p><p>{}</p><p>1 Main St</p>",
                image_tag(&shown)
            ),
            false,
            vec![shown.clone(), unused],
        );
        assert_eq!(text(&s), "John Doe\n\n[logo]\n\n1 Main St");
        assert!(is_formatted(&s));
        assert_eq!(shown_images(&s), vec![shown.clone()]);
        assert_eq!(
            image_widths(&s.html),
            vec![("logo@simplymail".to_owned(), 300)]
        );

        let plain = signature("<p>Ada</p>", false, Vec::new());
        assert!(!is_formatted(&plain), "plain text can go as plain text");
        assert!(!is_blank(&plain));
        assert!(
            is_blank(&signature("<p> </p>", false, vec![shown])),
            "unshown images"
        );
        assert!(!is_blank(&signature(
            "<p><IMG SRC='cid:x'></p>",
            true,
            Vec::new()
        )));
    }

    #[test]
    fn the_visual_editor_shows_images_as_pills_where_they_stand() {
        let images = [logo("logo", 600, 200)];
        let html = "<p>John Doe</p><p><img alt=\"Logo\" src='cid:logo@simplymail' width=120 \
                    height=\"40\"></p><p><b>1 Main St</b></p>";
        let (editor, fits) = to_editor(html, &images);
        assert!(fits);
        assert_eq!(
            editor,
            "<p>John Doe</p><p><span data-pill=\"img:logo@simplymail|120|40\">logo.png</span>\
             </p><p><b>1 Main St</b></p>"
        );
        assert_eq!(
            from_editor(&editor, &images),
            "<p>John Doe</p><p><img src=\"cid:logo@simplymail\" alt=\"logo\" width=\"120\" \
             height=\"40\"></p><p><b>1 Main St</b></p>",
            "its size survives"
        );
        assert_eq!(
            pill_id(&images[0]),
            "img:logo@simplymail|300|100",
            "a new one starts scaled to fit"
        );
        assert_eq!(
            from_editor(
                "<p><span data-pill=\"img:gone@simplymail||\"><b>gone.png</b></span>x</p>",
                &images
            ),
            "<p>x</p>",
            "a pill for an image that's gone goes too"
        );

        for (html, why) in [
            (
                "<p><img src='cid:logo@simplymail' style='border:0'></p>",
                "styled",
            ),
            ("<p><img src='cid:other@simplymail'></p>", "unknown image"),
            ("<table><tr><td>x</td></tr></table>", "table"),
            (
                "<p><img src='cid:logo@simplymail' width='50%'></p>",
                "relative width",
            ),
        ] {
            assert!(!to_editor(html, &images).1, "{why}");
        }
    }

    #[test]
    fn removing_an_image_drops_its_tags() {
        let html = "<p>Ada</p>\n<p><img src=\"cid:a@x\" alt=\"a\"> <IMG alt='b' src='cid:b@x'></p>\
                    <p>see cid:b@x</p>";
        assert_eq!(
            remove_image_tags(html, "b@x"),
            "<p>Ada</p>\n<p><img src=\"cid:a@x\" alt=\"a\"> </p><p>see cid:b@x</p>",
            "only image tags go"
        );
        assert_eq!(
            remove_image_tags(&remove_image_tags(html, "a@x"), "b@x"),
            "<p>Ada</p>\n<p>see cid:b@x</p>",
            "and the paragraph they leave empty"
        );
    }

    #[test]
    fn blank_signatures_are_removed() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .save_account(&crate::google_account("me@gmail.com", None))
            .unwrap();
        let s = signature("<p>Ada</p>", false, Vec::new());
        save_signature(&mut store, "me@gmail.com", Some(&s)).unwrap();
        assert_eq!(store.signature("me@gmail.com").unwrap(), Some(s));
        save_signature(
            &mut store,
            "me@gmail.com",
            Some(&signature("", false, Vec::new())),
        )
        .unwrap();
        assert_eq!(store.signature("me@gmail.com").unwrap(), None);
    }
}
