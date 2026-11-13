//! Received HTML made safe to quote in a reply: the body's content, without anything active,
//! global or hidden-by-trick.

use ego_tree::NodeRef;
use scraper::{Html, Node};

use crate::dom::is_safe_href;
use crate::escape;

/// Elements dropped with their content: active, embedded, or styling the whole document.
const DROPPED: &[&str] = &[
    "head", "script", "style", "title", "template", "noscript", "iframe", "frame", "frameset",
    "object", "embed", "applet", "form", "input", "button", "select", "textarea", "meta", "link",
    "base", "audio", "video", "canvas", "dialog",
];

/// Elements with no closing tag.
const VOID: &[&str] = &["area", "br", "col", "hr", "img", "source", "track", "wbr"];

/// The content of `html`'s body, as HTML to put inside a quote.
///
/// Scripts, forms, embedded content, style sheets, comments and event handlers are dropped, and
/// links and image sources keep only web, mail and phone addresses (and `cid:` images, which are
/// sent along with the message).
#[must_use]
pub fn quotable(html: &str) -> String {
    let document = Html::parse_document(html);
    let mut out = String::new();
    let body = document
        .tree
        .root()
        .descendants()
        .find(|n| n.value().as_element().is_some_and(|e| e.name() == "body"));
    if let Some(body) = body {
        children(body, &mut out);
    }
    out.trim().to_owned()
}

fn children(node: NodeRef<'_, Node>, out: &mut String) {
    for child in node.children() {
        match child.value() {
            Node::Text(text) => out.push_str(&escape_text(text)),
            Node::Element(element) => {
                let name = element.name();
                if DROPPED.contains(&name) {
                    continue;
                }
                out.push('<');
                out.push_str(name);
                for (attribute, value) in element.attrs() {
                    if !keeps(name, attribute, value) {
                        continue;
                    }
                    out.extend([" ", attribute, "=\"", &escape(value), "\""]);
                }
                out.push('>');
                if VOID.contains(&name) {
                    continue;
                }
                children(child, out);
                out.extend(["</", name, ">"]);
            }
            _ => {}
        }
    }
}

/// Text escaped for an element's content: quotes may stay.
fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn keeps(element: &str, attribute: &str, value: &str) -> bool {
    let attribute = attribute.to_ascii_lowercase();
    if attribute.starts_with("on") || attribute == "srcdoc" || attribute == "formaction" {
        return false;
    }
    match attribute.as_str() {
        "href" => is_safe_href(value) || value.trim_start().starts_with('#'),
        "src" => {
            is_safe_href(value)
                || (element == "img"
                    && value
                        .trim_start()
                        .get(..4)
                        .is_some_and(|s| s.eq_ignore_ascii_case("cid:")))
        }
        // A url() in a style can fetch, and an expression() run, in some apps.
        "style" => {
            let lower = value.to_ascii_lowercase();
            !lower.contains("expression(") && !lower.contains("javascript:")
        }
        "background" | "action" | "lowsrc" | "dynsrc" | "srcset" => is_safe_href(value),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_and_global_content_is_dropped() {
        #[expect(
            clippy::literal_string_with_formatting_args,
            reason = "CSS, not a format string"
        )]
        let html = quotable(
            "<html><head><style>body{color:red}</style></head><body>\
             <p onclick='x()' style='color:blue'>Hi &amp; <b>bye</b></p><script>alert(1)</script>\
             <!-- hidden --><a href='javascript:alert(1)'>a</a><a href='https://x.test?a=1&b=2'>b</a>\
             <img src='cid:logo' alt='Logo'><form><input></form></body></html>",
        );
        assert_eq!(
            html,
            "<p style=\"color:blue\">Hi &amp; <b>bye</b></p><a>a</a>\
             <a href=\"https://x.test?a=1&amp;b=2\">b</a><img src=\"cid:logo\" alt=\"Logo\">"
        );
    }

    #[test]
    fn a_fragment_is_quoted_whole() {
        assert_eq!(quotable("Hello <i>you</i>"), "Hello <i>you</i>");
    }
}
