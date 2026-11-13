//! HTML mail: reading it as Markdown or plain text, cleaning it up to quote in a reply, and
//! scoring how well the major mail apps support a message's markup ([`compat`]).
//!
//! Nothing here fetches anything: images become their alt text and remote content is dropped.

pub mod compat;
mod dom;
mod render;
mod sanitize;

pub use sanitize::quotable;

/// `html` as `CommonMark` with GFM tables and strikethrough, to show. Styling is dropped; text is
/// escaped so nothing in it reads as Markdown. Only `http`, `https`, `mailto` and `tel` links
/// are kept.
#[must_use]
pub fn to_markdown(html: &str) -> String {
    render::markdown(&dom::parse(html))
}

/// `html` as plain text: what's sent alongside it for apps that don't show HTML.
#[must_use]
pub fn to_text(html: &str) -> String {
    render::text(&dom::parse(html))
}

/// `text` with `&`, `<`, `>` and quotes escaped, to put in HTML.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Plain `text` as HTML: a paragraph per blank-line-separated block, `<br>` within one.
#[must_use]
pub fn from_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .fold(String::new(), |mut html, p| {
            html.extend([
                "<p>",
                &escape(p.trim_matches('\n')).replace('\n', "<br>"),
                "</p>",
            ]);
            html
        })
}

/// `body`, an HTML fragment, as a whole document to send. A `body` that's already a whole
/// document is sent as written.
#[must_use]
pub fn document(body: &str) -> String {
    let start = body
        .trim_start()
        .get(..15)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if start.starts_with("<!doctype") || start.starts_with("<html") {
        return body.to_owned();
    }
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n</head>\n<body>\n{body}\n</body>\n</html>\n"
    )
}

/// The elements a visual editor shows as they are: text formatting, links, lists, quotes and
/// code. Anything else in a message would be lost by editing it visually.
const EDITABLE: &[&str] = &[
    "html",
    "head",
    "body",
    "p",
    "div",
    "br",
    "b",
    "strong",
    "i",
    "em",
    "u",
    "ins",
    "s",
    "del",
    "strike",
    "a",
    "code",
    "pre",
    "ul",
    "ol",
    "li",
    "blockquote",
    "span",
];

/// Whether `html` uses only what a visual editor keeps (see [`EDITABLE`]), with no attributes
/// but links' `href`. Converting anything else to the editor drops formatting.
#[must_use]
pub fn fits_editor(html: &str) -> bool {
    let document = scraper::Html::parse_document(html);
    document
        .tree
        .root()
        .descendants()
        .all(|node| match node.value() {
            scraper::Node::Element(e) => {
                EDITABLE.contains(&e.name())
                    && e.attrs().all(|(name, _)| e.name() == "a" && name == "href")
            }
            scraper::Node::Comment(_) | scraper::Node::ProcessingInstruction(_) => false,
            _ => true,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_becomes_escaped_paragraphs() {
        assert_eq!(
            from_text("Hi <you>,\nthere\n\n\nBye & thanks"),
            "<p>Hi &lt;you&gt;,<br>there</p><p>Bye &amp; thanks</p>"
        );
    }

    #[test]
    fn fragments_are_wrapped_once() {
        let doc = document("<p>x</p>");
        assert!(doc.starts_with("<!DOCTYPE html>"), "{doc}");
        assert!(doc.contains("<body>\n<p>x</p>\n</body>"), "{doc}");
        assert_eq!(document(&doc), doc);
    }

    #[test]
    fn simple_formatting_fits_the_editor() {
        assert!(fits_editor(
            "<p><b>Hi</b> <a href='https://x.test'>x</a></p><ul><li>a</li></ul>"
        ));
        assert!(!fits_editor("<p style='color:red'>x</p>"), "style");
        assert!(!fits_editor("<table><tr><td>x</td></tr></table>"), "table");
        assert!(!fits_editor("<!-- x --><p>x</p>"), "comment");
    }
}
