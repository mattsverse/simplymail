//! Finding the CSS features a style sheet or `style` attribute uses, named as caniemail names them
//! (`css-display-flex`, `css-at-media`, `css-pseudo-class-hover`). Names caniemail has no data
//! for are reported too; scoring ignores them.

use std::collections::BTreeSet;

/// Adds the features `sheet`, the content of a `<style>` element, uses to `found`.
pub(crate) fn sheet(sheet: &str, found: &mut BTreeSet<String>) {
    let sheet = strip_comments(sheet, found);
    rules(&sheet, found);
}

/// Adds the features `style`, a `style` attribute, uses to `found`.
pub(crate) fn declarations(style: &str, found: &mut BTreeSet<String>) {
    let style = strip_comments(style, found);
    for declaration in split_top_level(&style, ';') {
        declaration_features(declaration, found);
    }
}

fn add(found: &mut BTreeSet<String>, slug: impl Into<String>) {
    found.insert(slug.into());
}

/// `css` without its comments, noting that it had some.
fn strip_comments(css: &str, found: &mut BTreeSet<String>) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        add(found, "css-comments");
        out.push_str(&rest[..start]);
        out.push(' ');
        rest = rest
            .get(start + 2..)
            .and_then(|r| r.find("*/").map(|end| &r[end + 2..]))
            .unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// Reads a run of rules: `selector { declarations }` and at-rules.
fn rules(css: &str, found: &mut BTreeSet<String>) {
    let mut rest = css;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return;
        }
        let Some(end) = find_top_level(rest, &['{', ';']) else {
            return;
        };
        let prelude = rest[..end].trim();
        if rest[end..].starts_with(';') {
            // A statement at-rule: `@import url(…);`, `@charset "…";`.
            at_rule(prelude, found);
            rest = &rest[end + 1..];
            continue;
        }
        let body_start = end + 1;
        let body_end = matching_brace(rest, end).unwrap_or(rest.len());
        let body = rest.get(body_start..body_end).unwrap_or_default();
        rest = rest.get(body_end + 1..).unwrap_or_default();
        if prelude.starts_with('@') {
            let name = at_rule(prelude, found);
            match name.as_str() {
                "media" | "supports" | "document" | "layer" | "container" => rules(body, found),
                // Descriptors and keyframe selectors, not rules of their own.
                _ => {}
            }
        } else {
            selectors(prelude, found);
            block(body, found);
        }
    }
}

/// A rule's block: declarations, and nested rules (CSS nesting).
fn block(body: &str, found: &mut BTreeSet<String>) {
    if find_top_level(body, &['{']).is_some() {
        add(found, "css-nesting");
        // Declarations before the first nested rule are still declarations.
        let mut rest = body;
        while let Some(open) = find_top_level(rest, &['{']) {
            let before = &rest[..open];
            let (decls, selector) = before.rsplit_once(';').unwrap_or(("", before));
            for d in split_top_level(decls, ';') {
                declaration_features(d, found);
            }
            selectors(selector.trim().trim_start_matches('&'), found);
            let close = matching_brace(rest, open).unwrap_or(rest.len());
            block(rest.get(open + 1..close).unwrap_or_default(), found);
            rest = rest.get(close + 1..).unwrap_or_default();
        }
        for d in split_top_level(rest, ';') {
            declaration_features(d, found);
        }
    } else {
        for d in split_top_level(body, ';') {
            declaration_features(d, found);
        }
    }
}

/// Notes an at-rule's features, and returns its name.
fn at_rule(prelude: &str, found: &mut BTreeSet<String>) -> String {
    let name = prelude
        .trim_start_matches('@')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>()
        .to_ascii_lowercase();
    let name = name
        .strip_prefix("-webkit-")
        .or_else(|| name.strip_prefix("-moz-"))
        .unwrap_or(&name)
        .to_owned();
    let lower = prelude.to_ascii_lowercase();
    match name.as_str() {
        "media" => {
            add(found, "css-at-media");
            for (needle, slug) in [
                ("prefers-color-scheme", "css-at-media-prefers-color-scheme"),
                (
                    "prefers-reduced-motion",
                    "css-at-media-prefers-reduced-motion",
                ),
                ("orientation", "css-at-media-orientation"),
                ("device-pixel-ratio", "css-at-media-device-pixel-ratio"),
                ("resolution", "css-at-media-device-pixel-ratio"),
                ("hover", "css-at-media-hover"),
            ] {
                if lower.contains(needle) {
                    add(found, slug);
                }
            }
        }
        "font-face" => add(found, "css-at-font-face"),
        "import" => add(found, "css-at-import"),
        "keyframes" => add(found, "css-at-keyframes"),
        "supports" => add(found, "css-at-supports"),
        _ => {}
    }
    name
}

/// Notes the kinds of selector in a selector list.
fn selectors(list: &str, found: &mut BTreeSet<String>) {
    let parts = split_top_level(list, ',');
    if parts.iter().filter(|p| !p.trim().is_empty()).count() > 1 {
        add(found, "css-selector-grouping");
    }
    for selector in parts {
        complex_selector(selector.trim(), found);
    }
}

fn complex_selector(selector: &str, found: &mut BTreeSet<String>) {
    let mut simple_in_compound = 0;
    let mut pending_space = false;
    let mut chars = selector.chars().peekable();
    let end_compound = |simple: &mut usize, found: &mut BTreeSet<String>| {
        if *simple > 1 {
            add(found, "css-selector-chaining");
        }
        *simple = 0;
    };
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                if simple_in_compound > 0 {
                    pending_space = true;
                }
            }
            '>' | '+' | '~' => {
                pending_space = false;
                end_compound(&mut simple_in_compound, found);
                add(
                    found,
                    match c {
                        '>' => "css-selector-child",
                        '+' => "css-selector-adjacent-sibling",
                        _ => "css-selector-general-sibling",
                    },
                );
            }
            _ => {
                if pending_space {
                    pending_space = false;
                    end_compound(&mut simple_in_compound, found);
                    add(found, "css-selector-descendant");
                }
                simple_in_compound += 1;
                match c {
                    '#' => {
                        add(found, "css-selector-id");
                        skip_ident(&mut chars);
                    }
                    '.' => {
                        add(found, "css-selector-class");
                        skip_ident(&mut chars);
                    }
                    '*' => add(found, "css-selector-universal"),
                    '[' => {
                        add(found, "css-selector-attribute");
                        for c in chars.by_ref() {
                            if c == ']' {
                                break;
                            }
                        }
                    }
                    ':' => {
                        let element = chars.next_if_eq(&':').is_some();
                        let name = take_ident(&mut chars);
                        if chars.next_if_eq(&'(').is_some() {
                            // `:not(…)`, `:nth-child(…)`: the argument's selectors count too.
                            let mut depth = 1;
                            let mut argument = String::new();
                            for c in chars.by_ref() {
                                match c {
                                    '(' => depth += 1,
                                    ')' => depth -= 1,
                                    _ => {}
                                }
                                if depth == 0 {
                                    break;
                                }
                                argument.push(c);
                            }
                            if matches!(name.as_str(), "not" | "has" | "is" | "where") {
                                selectors(&argument, found);
                            }
                        }
                        let legacy_element = matches!(
                            name.as_str(),
                            "before" | "after" | "first-letter" | "first-line"
                        );
                        if element || legacy_element {
                            add(found, format!("css-pseudo-element-{name}"));
                        } else {
                            add(found, format!("css-pseudo-class-{name}"));
                        }
                    }
                    c if c.is_alphabetic() || c == '-' || c == '_' => {
                        add(found, "css-selector-type");
                        skip_ident(&mut chars);
                    }
                    _ => {}
                }
            }
        }
    }
    end_compound(&mut simple_in_compound, found);
}

fn take_ident(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut ident = String::new();
    while let Some(c) = chars.next_if(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '\\')) {
        ident.push(c.to_ascii_lowercase());
    }
    ident
}

fn skip_ident(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    take_ident(chars);
}

/// Properties caniemail scores under another name.
const ALIASES: &[(&str, &str)] = &[
    ("inline-size", "css-block-inline-size"),
    ("block-size", "css-block-inline-size"),
    ("top", "css-left-right-top-bottom"),
    ("right", "css-left-right-top-bottom"),
    ("bottom", "css-left-right-top-bottom"),
    ("left", "css-left-right-top-bottom"),
    ("row-gap", "css-gap"),
    ("column-gap", "css-gap"),
    ("grid-gap", "css-gap"),
    ("columns", "css-column-layout-properties"),
    ("column-fill", "css-column-layout-properties"),
    ("column-span", "css-column-layout-properties"),
    ("column-width", "css-column-layout-properties"),
    ("flex-flow", "css-flex-direction"),
    ("contain-intrinsic-size", "css-intrinsic-size"),
    ("margin-inline", "css-margin-inline-block"),
    ("margin-block", "css-margin-inline-block"),
    ("padding-inline", "css-padding-inline-block"),
    ("padding-block", "css-padding-inline-block"),
    ("margin-inline-start", "css-margin-inline-start-end"),
    ("margin-inline-end", "css-margin-inline-start-end"),
    ("margin-block-start", "css-margin-block-start-end"),
    ("margin-block-end", "css-margin-block-start-end"),
    ("padding-inline-start", "css-padding-inline-start-end"),
    ("padding-inline-end", "css-padding-inline-start-end"),
    ("padding-block-start", "css-padding-block-start-end"),
    ("padding-block-end", "css-padding-block-start-end"),
    ("border-inline", "css-border-inline-block"),
    ("border-block", "css-border-inline-block"),
    ("border-inline-start", "css-border-inline-block-individual"),
    ("border-inline-end", "css-border-inline-block-individual"),
    ("border-block-start", "css-border-inline-block-individual"),
    ("border-block-end", "css-border-inline-block-individual"),
];

/// Property prefixes whose longhands caniemail scores under the shorthand: `margin-top` is
/// `css-margin`.
const SHORTHANDS: &[&str] = &[
    "margin",
    "padding",
    "border",
    "outline",
    "overflow",
    "transition",
    "animation",
    "transform",
    "text-emphasis",
    "column-rule",
    "inset",
    "grid-template",
    "scroll-snap",
];

fn declaration_features(declaration: &str, found: &mut BTreeSet<String>) {
    let Some((property, value)) = declaration.split_once(':') else {
        return;
    };
    let property = property.trim().to_ascii_lowercase();
    if property.is_empty() {
        return;
    }
    let value = value.trim().to_ascii_lowercase();
    if property.starts_with("--") {
        add(found, "css-variables");
    }
    if let Some(slug) = property_slug(&property) {
        add(found, slug);
    }
    value_features(&property, &value, found);
}

/// The feature a property is scored under: its own, an alias, or its shorthand's.
fn property_slug(property: &str) -> Option<String> {
    let property = property
        .strip_prefix("-webkit-")
        .or_else(|| property.strip_prefix("-moz-"))
        .or_else(|| property.strip_prefix("-ms-"))
        .unwrap_or(property);
    if let Some((_, slug)) = ALIASES.iter().find(|(p, _)| *p == property) {
        return Some((*slug).to_owned());
    }
    if property.ends_with("-radius") {
        let logical = ["start-start", "start-end", "end-start", "end-end"]
            .iter()
            .any(|corner| property.contains(corner));
        return Some(
            if logical {
                "css-border-radius-logical"
            } else {
                "css-border-radius"
            }
            .to_owned(),
        );
    }
    if property.starts_with("border-inline-") || property.starts_with("border-block-") {
        return Some("css-border-inline-block-longhand".to_owned());
    }
    if property.starts_with("column-rule") {
        return Some("css-column-layout-properties".to_owned());
    }
    if property.starts_with("inset") {
        return Some("css-inset".to_owned());
    }
    let own = format!("css-{property}");
    if super::feature(&own).is_some() {
        return Some(own);
    }
    SHORTHANDS
        .iter()
        .find(|s| property.starts_with(&format!("{s}-")))
        .map(|s| format!("css-{s}"))
        .filter(|slug| super::feature(slug).is_some())
}

fn value_features(property: &str, value: &str, found: &mut BTreeSet<String>) {
    if value.contains("!important") {
        add(found, "css-important");
    }
    if property == "display" {
        let shown = value.trim_end_matches("!important").trim();
        match shown {
            "flex" | "inline-flex" => add(found, "css-display-flex"),
            "grid" | "inline-grid" => add(found, "css-display-grid"),
            "none" => add(found, "css-display-none"),
            _ => {}
        }
    }
    if property == "flex-flow" {
        add(found, "css-flex-wrap");
    }
    if property.starts_with("font") || property == "font-family" {
        for keyword in [
            "system-ui",
            "ui-serif",
            "ui-sans-serif",
            "ui-rounded",
            "ui-monospace",
        ] {
            if value.contains(keyword) {
                add(found, "css-sytem-ui");
            }
        }
    }
    for (function, slug) in [
        ("var(", "css-variables"),
        ("calc(", "css-unit-calc"),
        ("clamp(", "css-function-clamp"),
        ("min(", "css-function-min"),
        ("max(", "css-function-max"),
        ("light-dark(", "css-function-light-dark"),
        ("linear-gradient(", "css-linear-gradient"),
        ("radial-gradient(", "css-radial-gradient"),
        ("conic-gradient(", "css-conic-gradient"),
        ("rgba(", "css-rgba"),
        ("lch(", "css-modern-color"),
        ("lab(", "css-modern-color"),
    ] {
        if contains_function(value, function) {
            add(found, slug);
        }
    }
    if contains_function(value, "rgb(") {
        add(found, "css-rgb");
    }
    let words: Vec<&str> = value
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')' | '/'))
        .filter(|w| !w.is_empty())
        .collect();
    if words.contains(&"initial") {
        add(found, "css-unit-initial");
    }
    if !property.starts_with("--") {
        for word in words {
            if let Some(unit) = unit_of(word) {
                add(found, format!("css-unit-{unit}"));
            }
        }
    }
}

/// Whether `value` calls `function` (which ends in `(`), not just one ending in its name:
/// `rgb(` isn't found in `rgba(`, nor `min(` in `fmin(`.
fn contains_function(value: &str, function: &str) -> bool {
    value.match_indices(function).any(|(at, _)| {
        !value[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// The unit of a length such as `12px` or `50%`.
fn unit_of(word: &str) -> Option<&'static str> {
    let number_end = word
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+')))
        .map_or(word.len(), |(i, _)| i);
    let (number, unit) = word.split_at(number_end);
    if !number.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    [
        "px", "em", "rem", "ex", "ch", "vh", "vw", "vmin", "vmax", "pt", "pc", "cm", "mm", "in",
    ]
    .into_iter()
    .find(|u| *u == unit)
    .or_else(|| (unit == "%").then_some("percent"))
}

/// The index of the first of `targets` not inside brackets or a string.
fn find_top_level(s: &str, targets: &[char]) -> Option<usize> {
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => depth -= 1,
            (None, c) if depth <= 0 && targets.contains(&c) => return Some(i),
            _ => {}
        }
    }
    None
}

/// The index of the `}` closing the `{` at `open`.
fn matching_brace(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    for (i, c) in s.get(open..)?.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, '{') => depth += 1,
            (None, '}') => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// `s` split at each top-level `separator`.
fn split_top_level(s: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = s;
    while let Some(i) = find_top_level(rest, &[separator]) {
        parts.push(&rest[..i]);
        rest = &rest[i + separator.len_utf8()..];
    }
    parts.push(rest);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of_sheet(css: &str) -> Vec<String> {
        let mut found = BTreeSet::new();
        sheet(css, &mut found);
        found.into_iter().collect()
    }

    fn of_style(css: &str) -> Vec<String> {
        let mut found = BTreeSet::new();
        declarations(css, &mut found);
        found.into_iter().collect()
    }

    #[test]
    fn inline_styles_name_their_properties_values_and_units() {
        assert_eq!(
            of_style(
                "display: flex; margin-top: 4px !important; color: rgb(0 0 0); border-top-left-radius: 50%"
            ),
            vec![
                "css-border-radius",
                "css-display",
                "css-display-flex",
                "css-important",
                "css-margin",
                "css-rgb",
                "css-unit-percent",
                "css-unit-px",
            ]
        );
    }

    #[test]
    fn style_sheets_name_their_at_rules_and_selectors() {
        let found = of_sheet(
            "/* dark */ @media (prefers-color-scheme: dark) { .a > p:hover, #b::before { color: red } }\
             @import url(x.css); @font-face { font-family: X; src: url(x.woff) }\
             div.c ~ span[title] { width: calc(100% - 2em) }",
        );
        for slug in [
            "css-comments",
            "css-at-media",
            "css-at-media-prefers-color-scheme",
            "css-selector-class",
            "css-selector-child",
            "css-pseudo-class-hover",
            "css-selector-grouping",
            "css-selector-id",
            "css-pseudo-element-before",
            "css-at-import",
            "css-at-font-face",
            "css-selector-chaining",
            "css-selector-general-sibling",
            "css-selector-attribute",
            "css-selector-type",
            "css-width",
            "css-unit-calc",
            "css-unit-em",
        ] {
            assert!(
                found.contains(&slug.to_owned()),
                "{slug} missing from {found:?}"
            );
        }
        assert!(
            !found.contains(&"css-font".to_owned()),
            "@font-face descriptors aren't properties: {found:?}"
        );
    }

    #[test]
    fn nested_rules_are_read() {
        let found = of_sheet(".a { color: red; & .b { display: grid } }");
        assert!(found.contains(&"css-nesting".to_owned()), "{found:?}");
        assert!(found.contains(&"css-display-grid".to_owned()), "{found:?}");
    }
}
