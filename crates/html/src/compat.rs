//! How well the major mail apps support a message's markup, from caniemail.com's data.
//!
//! [`check`] finds the HTML and CSS features a message uses and, for each mail app (each
//! provider on each platform caniemail tests), whether it supports all of them. The
//! [`Report::score`] is the share of apps that show everything, averaged so each provider weighs
//! the same however many platforms it has. An app that supports a feature only partly still
//! counts, and the report lists it as a caveat; one with no data for a feature isn't counted for it.

mod css;
mod data;

use std::collections::BTreeSet;

use ego_tree::NodeRef;
use scraper::{Html, Node};

/// A mail provider whose apps are scored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Provider {
    Gmail,
    Outlook,
    AppleMail,
    Yahoo,
    Thunderbird,
}

impl Provider {
    /// Every provider scored, in the order a report lists them.
    pub const ALL: [Self; 5] = [
        Self::Gmail,
        Self::Outlook,
        Self::AppleMail,
        Self::Yahoo,
        Self::Thunderbird,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Gmail => "Gmail",
            Self::Outlook => "Outlook",
            Self::AppleMail => "Apple Mail",
            Self::Yahoo => "Yahoo",
            Self::Thunderbird => "Thunderbird",
        }
    }
}

/// One provider's app on one platform, such as Gmail on Android.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Client {
    pub provider: Provider,
    /// The platform as caniemail names it: "iOS", "webmail", "Outlook.com".
    pub platform: &'static str,
}

impl Client {
    /// "Gmail (Android)".
    #[must_use]
    pub fn name(&self) -> String {
        format!("{} ({})", self.provider.name(), self.platform)
    }
}

/// How well an app supports a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Support {
    No,
    Partial,
    Yes,
}

/// An HTML or CSS feature caniemail has data for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Feature {
    /// caniemail's name for it, as in `https://www.caniemail.com/features/{slug}/`.
    pub slug: &'static str,
    /// What it is, as `<style> element` or `display:flex`.
    pub title: &'static str,
    /// One letter per client in `data::CLIENTS`: y, a (partly), n, or u (untested).
    support: &'static str,
}

impl Feature {
    /// How `client` (an index into [`clients`]) supports this, or `None` if it wasn't tested.
    fn support(&self, client: usize) -> Option<Support> {
        match self.support.as_bytes().get(client)? {
            b'y' => Some(Support::Yes),
            b'a' => Some(Support::Partial),
            b'n' => Some(Support::No),
            _ => None,
        }
    }

    /// Its page on caniemail.com.
    #[must_use]
    pub fn url(&self) -> String {
        format!("https://www.caniemail.com/features/{}/", self.slug)
    }
}

/// Every client scored.
#[must_use]
pub fn clients() -> &'static [Client] {
    data::CLIENTS
}

/// When caniemail last updated the data, as `2026-09-16`.
#[must_use]
pub const fn data_date() -> &'static str {
    data::UPDATED
}

fn feature(slug: &str) -> Option<&'static Feature> {
    data::FEATURES
        .binary_search_by(|f| f.slug.cmp(slug))
        .ok()
        .and_then(|i| data::FEATURES.get(i))
}

/// A feature some apps don't fully support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub feature: &'static Feature,
    /// The apps that don't show it at all.
    pub unsupported: Vec<Client>,
    /// The apps that support it only partly.
    pub partial: Vec<Client>,
}

/// What [`check`] found.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Report {
    /// The features used that caniemail has data for, by slug.
    pub features: Vec<&'static Feature>,
    /// From 0 to 1: see the [module docs](self).
    pub score: f32,
    /// Each provider's score, for those with data for something used.
    pub providers: Vec<(Provider, f32)>,
    /// The features not every app fully supports, the most widely unsupported first.
    pub issues: Vec<Issue>,
}

impl Report {
    /// The score as a whole percentage, rounded down so that only full support shows 100%.
    #[must_use]
    pub const fn percent(&self) -> u8 {
        percent(self.score)
    }

    /// Whether anything used isn't shown, or is shown only partly, somewhere.
    #[must_use]
    pub const fn has_issues(&self) -> bool {
        !self.issues.is_empty()
    }
}

/// `score`, from 0 to 1, as a whole percentage rounded down.
#[must_use]
pub const fn percent(score: f32) -> u8 {
    let percent = score.clamp(0.0, 1.0).mul_add(100.0, 1e-3).floor();
    // In 0..=100 after the clamp, so the cast can't truncate.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::as_conversions,
        reason = "clamped to 0..=100 above"
    )]
    let percent = percent as u8;
    percent
}

/// The features `html` uses, by caniemail slug. `html` may be a whole document or a fragment.
#[must_use]
pub fn features(html: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let lower = html.to_ascii_lowercase();
    if lower.trim_start().starts_with("<!doctype") {
        found.insert("html-doctype".to_owned());
    }
    if lower.contains("<body") {
        found.insert("html-body".to_owned());
    }
    let document = Html::parse_document(html);
    walk(document.tree.root(), &mut found);
    found
}

/// Scores the features in `used` (as [`features`] finds them).
#[must_use]
pub fn report<'a>(used: impl IntoIterator<Item = &'a String>) -> Report {
    let features: Vec<&'static Feature> = used.into_iter().filter_map(|s| feature(s)).collect();
    let clients = clients();
    // Each client's worst support across the features used, if it has data for any.
    let worst: Vec<Option<Support>> = (0..clients.len())
        .map(|c| features.iter().filter_map(|f| f.support(c)).min())
        .collect();
    let mut providers = Vec::new();
    for provider in Provider::ALL {
        let scored: Vec<Support> = clients
            .iter()
            .zip(&worst)
            .filter(|(client, _)| client.provider == provider)
            .filter_map(|(_, worst)| *worst)
            .collect();
        if scored.is_empty() {
            continue;
        }
        let shown = scored.iter().filter(|s| **s != Support::No).count();
        providers.push((provider, ratio(shown, scored.len())));
    }
    let score = if providers.is_empty() {
        1.0
    } else {
        providers.iter().map(|(_, s)| s).sum::<f32>() / count(providers.len())
    };
    let mut issues: Vec<Issue> = features
        .iter()
        .map(|feature| {
            let with = |support| {
                clients
                    .iter()
                    .enumerate()
                    .filter(|(c, _)| feature.support(*c) == Some(support))
                    .map(|(_, client)| *client)
                    .collect::<Vec<_>>()
            };
            Issue {
                feature,
                unsupported: with(Support::No),
                partial: with(Support::Partial),
            }
        })
        .filter(|issue| !issue.unsupported.is_empty() || !issue.partial.is_empty())
        .collect();
    issues.sort_by_key(|i| {
        (
            std::cmp::Reverse(i.unsupported.len()),
            std::cmp::Reverse(i.partial.len()),
            i.feature.slug,
        )
    });
    Report {
        features,
        score,
        providers,
        issues,
    }
}

/// [`report`] on the features `html` uses.
#[must_use]
pub fn check(html: &str) -> Report {
    report(&features(html))
}

fn count(n: usize) -> f32 {
    // Client counts are tiny; u16 holds them exactly and f32 holds any u16.
    f32::from(u16::try_from(n).unwrap_or(u16::MAX))
}

fn ratio(part: usize, whole: usize) -> f32 {
    if whole == 0 {
        1.0
    } else {
        count(part) / count(whole)
    }
}

/// Elements caniemail scores as `html-{name}`.
const OWN_FEATURE: &[&str] = &[
    "abbr",
    "acronym",
    "address",
    "audio",
    "base",
    "bdi",
    "blockquote",
    "code",
    "del",
    "dfn",
    "dialog",
    "div",
    "form",
    "hr",
    "img",
    "link",
    "marquee",
    "meter",
    "object",
    "p",
    "picture",
    "pre",
    "progress",
    "select",
    "small",
    "span",
    "strong",
    "style",
    "textarea",
    "video",
    "wbr",
    "svg",
    "ruby",
    "rt",
    "rp",
    "table",
];

const SEMANTICS: &[&str] = &[
    "article",
    "aside",
    "details",
    "figcaption",
    "figure",
    "footer",
    "header",
    "main",
    "mark",
    "nav",
    "section",
    "summary",
    "time",
];

/// Attributes caniemail scores as `html-{name}`.
const ATTRIBUTES: &[&str] = &[
    "align",
    "valign",
    "width",
    "height",
    "background",
    "cellpadding",
    "cellspacing",
    "dir",
    "lang",
    "role",
    "hidden",
    "required",
    "target",
    "srcset",
    "popover",
    "aria-describedby",
    "aria-hidden",
    "aria-label",
    "aria-labelledby",
    "aria-live",
];

fn walk(node: NodeRef<'_, Node>, found: &mut BTreeSet<String>) {
    for child in node.children() {
        match child.value() {
            Node::Comment(_) => {
                found.insert("html-comments".to_owned());
            }
            Node::Element(element) => {
                element_features(element, found);
                if element.name() == "style" {
                    let mut sheet = String::new();
                    for text in child.children().filter_map(|c| c.value().as_text()) {
                        sheet.push_str(text);
                    }
                    css::sheet(&sheet, found);
                }
                walk(child, found);
            }
            _ => walk(child, found),
        }
    }
}

fn element_features(element: &scraper::node::Element, found: &mut BTreeSet<String>) {
    let mut styles = Vec::new();
    let mut add = |slug: String| {
        found.insert(slug);
    };
    let name = element.name();
    if OWN_FEATURE.contains(&name) {
        add(format!("html-{name}"));
    }
    if SEMANTICS.contains(&name) {
        add("html-semantics".to_owned());
    }
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => add("html-h1-h6".to_owned()),
        "ul" | "ol" | "li" | "dl" | "dt" | "dd" => add("html-lists".to_owned()),
        "s" | "strike" => add("html-strike".to_owned()),
        "thead" | "tbody" | "tfoot" | "tr" | "td" | "th" => add("html-table".to_owned()),
        "map" | "area" => add("html-image-maps".to_owned()),
        "input" => {
            let kind = element.attr("type").unwrap_or("text").to_ascii_lowercase();
            add(format!("html-input-{kind}"));
        }
        "button" => {
            let kind = element
                .attr("type")
                .unwrap_or("submit")
                .to_ascii_lowercase();
            add(format!("html-button-{kind}"));
        }
        "meta" => {
            if element
                .attr("name")
                .is_some_and(|n| n.eq_ignore_ascii_case("color-scheme"))
            {
                add("html-meta-color-scheme".to_owned());
            }
        }
        "a" => {
            let href = element.attr("href").unwrap_or_default().trim();
            if href
                .get(..7)
                .is_some_and(|s| s.eq_ignore_ascii_case("mailto:"))
            {
                add("html-mailto-links".to_owned());
            } else if href.starts_with('#') && href.len() > 1 {
                add("html-anchor-links".to_owned());
            }
        }
        "img" | "source" => {
            if let Some(src) = element.attr("src") {
                if let Some(format) = image_format(src) {
                    add(format!("image-{format}"));
                }
                if src
                    .trim_start()
                    .get(..5)
                    .is_some_and(|s| s.eq_ignore_ascii_case("data:"))
                {
                    add("image-base64".to_owned());
                }
            }
        }
        _ => {}
    }
    for (attribute, value) in element.attrs() {
        let attribute = attribute.to_ascii_lowercase();
        if ATTRIBUTES.contains(&attribute.as_str()) {
            add(format!("html-{attribute}"));
        }
        match attribute.as_str() {
            "loading" => add("html-loading-attribute".to_owned()),
            "inert" => add("css-inert-attribute".to_owned()),
            "command" | "commandfor" => add("html-command-attribute".to_owned()),
            "popovertarget" => add("html-popover".to_owned()),
            "usemap" => add("html-image-maps".to_owned()),
            "style" => styles.push(value),
            _ => {}
        }
    }
    for style in styles {
        css::declarations(style, found);
    }
}

/// The image format a `src` names, by its extension or `data:` type.
fn image_format(src: &str) -> Option<&'static str> {
    let src = src.trim().to_ascii_lowercase();
    let kind = if let Some(data) = src.strip_prefix("data:image/") {
        data.split([';', ',']).next().unwrap_or_default().to_owned()
    } else {
        let path = src.split(['?', '#']).next().unwrap_or_default();
        path.rsplit_once('.').map(|(_, ext)| ext.to_owned())?
    };
    Some(match kind.as_str() {
        "png" => "png",
        "jpg" | "jpeg" => "jpg",
        "gif" => "gif",
        "webp" => "webp",
        "svg" | "svg+xml" => "svg",
        "avif" => "avif",
        "bmp" => "bmp",
        "ico" | "x-icon" | "vnd.microsoft.icon" => "ico",
        "tif" | "tiff" => "tiff",
        "heic" | "heif" => "heif",
        "apng" => "apng",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_is_sorted_and_complete() {
        assert!(
            data::FEATURES.windows(2).all(|w| w[0].slug < w[1].slug),
            "binary search needs sorted slugs"
        );
        for f in data::FEATURES {
            assert_eq!(
                f.support.len(),
                clients().len(),
                "{} has a letter per client",
                f.slug
            );
        }
    }

    #[test]
    fn markup_names_its_features() {
        #[expect(
            clippy::literal_string_with_formatting_args,
            reason = "CSS, not a format string"
        )]
        let found = features(
            "<!DOCTYPE html><html><body><style>.x{color:red}</style>\
             <!-- note --><h2 align=center>Hi</h2><ul><li>a</li></ul>\
             <a href='mailto:a@x.test'>mail</a><img src='https://x.test/a.webp?v=1' width=10>\
             <section style='display:grid'></section></body></html>",
        );
        for slug in [
            "html-doctype",
            "html-body",
            "html-style",
            "html-comments",
            "html-h1-h6",
            "html-align",
            "html-lists",
            "html-mailto-links",
            "html-img",
            "image-webp",
            "html-width",
            "html-semantics",
            "css-display-grid",
            "css-selector-class",
        ] {
            assert!(found.contains(slug), "{slug} missing from {found:?}");
        }
    }

    #[test]
    fn plain_markup_is_fully_supported() {
        let report = check("<p>Hello <strong>there</strong></p><div><br></div>");
        assert_eq!(report.percent(), 100, "{report:?}");
        assert!(!report.has_issues(), "{report:?}");
    }

    #[test]
    fn nothing_used_is_fully_supported() {
        assert_eq!(check("").percent(), 100);
    }

    #[test]
    fn unsupported_features_lower_the_score_and_are_listed_first() {
        let report = check("<p style='display:flex'>x</p><ul><li>y</li></ul>");
        assert!(report.percent() < 100, "{report:?}");
        let first = report.issues.first().expect("an issue");
        assert!(!first.unsupported.is_empty(), "{first:?}");
        let lists = report
            .issues
            .iter()
            .find(|i| i.feature.slug == "html-lists")
            .expect("lists are partly supported");
        assert!(
            lists.unsupported.is_empty() && !lists.partial.is_empty(),
            "{lists:?}"
        );
        let providers: Vec<Provider> = report.providers.iter().map(|(p, _)| *p).collect();
        assert_eq!(providers, Provider::ALL.to_vec(), "every provider has data");
    }

    #[test]
    fn partial_support_counts_as_shown() {
        let report = check("<ul><li>y</li></ul>");
        assert_eq!(report.percent(), 100, "{report:?}");
        assert!(report.has_issues(), "but it's a caveat");
    }
}
