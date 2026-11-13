//! Writing [`Block`]s out as Markdown, to show, or as plain text, to send alongside HTML.

use crate::dom::{Block, Inline, Style};

/// `blocks` as `CommonMark` (with GFM tables and strikethrough). Text is escaped, so nothing in it
/// reads as Markdown or HTML.
pub(crate) fn markdown(blocks: &[Block]) -> String {
    let mut out = String::new();
    for block in blocks {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&markdown_block(block));
    }
    out
}

fn markdown_block(block: &Block) -> String {
    match block {
        Block::Para(inlines) => escape_line_starts(&md_inlines(inlines, false)),
        Block::Heading(level, inlines) => {
            let level = usize::from((*level).clamp(1, 6));
            format!("{} {}", "#".repeat(level), md_inlines(inlines, true))
        }
        Block::Quote(blocks) => prefix_lines(&markdown(blocks), "> ", ">"),
        Block::List {
            ordered,
            start,
            items,
        } => {
            let mut out = String::new();
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                let marker = if *ordered {
                    format!(
                        "{}. ",
                        start.saturating_add(u64::try_from(i).unwrap_or(u64::MAX))
                    )
                } else {
                    "- ".to_owned()
                };
                let indent = " ".repeat(marker.len());
                let body = markdown(item);
                let mut lines = body.lines();
                out.push_str(&marker);
                out.push_str(lines.next().unwrap_or_default());
                for line in lines {
                    out.push('\n');
                    if !line.is_empty() {
                        out.push_str(&indent);
                        out.push_str(line);
                    }
                }
            }
            out
        }
        Block::Code(code) => {
            let fence = "`".repeat(longest_run(code, '`').max(2) + 1);
            format!("{fence}\n{code}\n{fence}")
        }
        Block::Rule => "---".to_owned(),
        Block::Table { head, rows } => {
            let columns = head
                .iter()
                .chain(rows)
                .map(Vec::len)
                .max()
                .unwrap_or_default()
                .max(1);
            let line = |cells: &[Vec<Inline>]| {
                let mut line = String::from("|");
                for i in 0..columns {
                    let cell = cells
                        .get(i)
                        .map(|c| md_inlines(c, true))
                        .unwrap_or_default();
                    line.push(' ');
                    line.push_str(&cell);
                    line.push_str(" |");
                }
                line
            };
            let mut out = line(head.as_deref().unwrap_or_default());
            out.push('\n');
            out.push('|');
            out.push_str(&" --- |".repeat(columns));
            for row in rows {
                out.push('\n');
                out.push_str(&line(row));
            }
            out
        }
    }
}

/// Inline Markdown. `one_line` turns line breaks into spaces, for headings and table cells.
fn md_inlines(inlines: &[Inline], one_line: bool) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(text) => escape(text, &mut out),
            Inline::Break if one_line => out.push(' '),
            Inline::Break => out.push_str("\\\n"),
            Inline::Code(code) => {
                let ticks = "`".repeat(longest_run(code, '`') + 1);
                let pad = if code.starts_with('`') || code.ends_with('`') {
                    " "
                } else {
                    ""
                };
                out.extend([ticks.as_str(), pad, code.as_str(), pad, ticks.as_str()]);
            }
            Inline::Styled(style, inner) => {
                let marker = match style {
                    Style::Bold => "**",
                    Style::Italic => "*",
                    Style::Strike => "~~",
                };
                // A delimiter next to a space doesn't open or close emphasis: keep spaces outside.
                let content = md_inlines(inner, one_line);
                let start = content.len() - content.trim_start().len();
                let trimmed = content.trim();
                if trimmed.is_empty() || trimmed.ends_with('\\') {
                    out.push_str(&content);
                    continue;
                }
                out.push_str(&content[..start]);
                out.extend([marker, trimmed, marker]);
                out.push_str(&content[start + trimmed.len()..]);
            }
            Inline::Link(href, inner) => {
                let mut text = md_inlines(inner, one_line);
                if text.trim().is_empty() {
                    text.clear();
                    escape(href, &mut text);
                }
                out.push('[');
                out.push_str(&text);
                out.push_str("](<");
                for c in href.chars() {
                    match c {
                        '<' => out.push_str("%3C"),
                        '>' => out.push_str("%3E"),
                        ' ' => out.push_str("%20"),
                        '\n' | '\r' => {}
                        c => out.push(c),
                    }
                }
                out.push_str(">)");
            }
        }
    }
    out
}

/// Backslash-escapes what Markdown or HTML would read in `text`.
fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|' | '~' | '&' | '#' | '!'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
}

/// Escapes what would start a list, heading underline or rule at the start of a line.
fn escape_line_starts(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let after = line.get(digits..).unwrap_or_default();
        if line.starts_with(['-', '+', '=']) {
            out.push('\\');
            out.push_str(line);
        } else if digits > 0 && after.starts_with(['.', ')']) {
            out.push_str(&line[..digits]);
            out.push('\\');
            out.push_str(after);
        } else {
            out.push_str(line);
        }
    }
    out
}

fn prefix_lines(text: &str, prefix: &str, empty: &str) -> String {
    text.split('\n')
        .map(|l| {
            if l.is_empty() {
                empty.to_owned()
            } else {
                format!("{prefix}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn longest_run(text: &str, of: char) -> usize {
    let (mut longest, mut run) = (0, 0);
    for c in text.chars() {
        run = if c == of { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// `blocks` as plain text: quotes behind "> ", list items behind their markers, and each link's
/// address after its text.
pub(crate) fn text(blocks: &[Block]) -> String {
    let mut out = String::new();
    for block in blocks {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&text_block(block));
    }
    out
}

fn text_block(block: &Block) -> String {
    match block {
        Block::Para(inlines) | Block::Heading(_, inlines) => text_inlines(inlines),
        Block::Quote(blocks) => prefix_lines(&text(blocks), "> ", ">"),
        Block::List {
            ordered,
            start,
            items,
        } => {
            let mut out = String::new();
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                let marker = if *ordered {
                    format!(
                        "{}. ",
                        start.saturating_add(u64::try_from(i).unwrap_or(u64::MAX))
                    )
                } else {
                    "- ".to_owned()
                };
                let indent = " ".repeat(marker.chars().count());
                let body = text(item);
                for (j, line) in body.lines().enumerate() {
                    if j == 0 {
                        out.push_str(&marker);
                    } else {
                        out.push('\n');
                        if !line.is_empty() {
                            out.push_str(&indent);
                        }
                    }
                    out.push_str(line);
                }
            }
            out
        }
        Block::Code(code) => code.clone(),
        Block::Rule => "---".to_owned(),
        Block::Table { head, rows } => head
            .iter()
            .chain(rows)
            .map(|row| {
                row.iter()
                    .map(|c| text_inlines(c).replace('\n', " "))
                    .collect::<Vec<_>>()
                    .join(" | ")
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn text_inlines(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(t) | Inline::Code(t) => out.push_str(t),
            Inline::Break => out.push('\n'),
            Inline::Styled(_, inner) => out.push_str(&text_inlines(inner)),
            Inline::Link(href, inner) => {
                let label = text_inlines(inner);
                let shown = href.strip_prefix("mailto:").unwrap_or(href);
                if label.trim().is_empty() {
                    out.push_str(shown);
                } else if label.trim() == shown || label.trim() == href {
                    out.push_str(&label);
                } else {
                    out.push_str(&label);
                    out.extend([" <", href.as_str(), ">"]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::dom::parse;

    use super::*;

    #[test]
    fn markdown_escapes_text_and_keeps_formatting() {
        let md = markdown(&parse(
            "<p>1. not a list, *not* <b>bold </b><a href='https://x.test/a b'>link</a></p>\
             <p>- dash<br>2) two</p>",
        ));
        assert_eq!(
            md,
            "1\\. not a list, \\*not\\* **bold** [link](<https://x.test/a%20b>)\n\n\\- dash\\\n2\\) two"
        );
    }

    #[test]
    fn markdown_nests_lists_and_quotes() {
        let md = markdown(&parse(
            "<ul><li>a<ul><li>b</li></ul></li><li><p>c</p><p>d</p></li></ul><blockquote><p>q</p><p>r</p></blockquote>",
        ));
        assert_eq!(md, "- a\n\n  - b\n- c\n\n  d\n\n> q\n>\n> r");
    }

    #[test]
    fn markdown_tables_and_code() {
        let md = markdown(&parse(
            "<table><tr><th>A|B</th></tr><tr><td>1</td></tr></table><pre>```x```</pre><p><code>a`b</code></p>",
        ));
        assert_eq!(
            md,
            "| A\\|B |\n| --- |\n| 1 |\n\n````\n```x```\n````\n\n``a`b``"
        );
    }

    #[test]
    fn a_newsletter_reads_cleanly() {
        #[expect(
            clippy::literal_string_with_formatting_args,
            reason = "CSS, not a format string"
        )]
        let md = markdown(&parse(
            r#"<!DOCTYPE html><html><head><title>News</title><style>td{padding:0}</style></head>
            <body><div style="display:none;max-height:0">Preheader text&zwnj;&nbsp;&zwnj;</div>
            <table role="presentation" width="100%"><tr><td align="center">
              <table width="600"><tr><td><img src="https://cdn.test/logo.png" alt="Acme"></td></tr>
                <tr><td><h1 style="font-size:24px">Big   news</h1>
                  <p style="font-family:Arial">We shipped <strong>v2</strong>. <a href="https://acme.test/v2?utm=1">Read more</a></p>
                  <table><tr><th>Plan</th><th>Price</th></tr><tr><td>Pro</td><td>$9</td></tr></table>
                </td></tr>
                <tr><td><span style="font-weight:bold">Thanks,</span><br>The team
                  <img src="https://track.test/open.gif" width="1" height="1"></td></tr>
              </table></td></tr></table></body></html>"#,
        ));
        assert_eq!(
            md,
            "\\[Acme\\]\n\n# Big news\n\nWe shipped **v2**. [Read more](<https://acme.test/v2?utm=1>)\n\n\
             | Plan | Price |\n| --- | --- |\n| Pro | $9 |\n\n**Thanks,**\\\nThe team"
        );
    }

    #[test]
    fn plain_text_shows_links_and_list_markers() {
        let t = text(&parse(
            "<p>See <a href='https://x.test'>the site</a> or <a href='mailto:a@x.test'>a@x.test</a></p>\
             <ol><li>one</li><li>two<br>more</li></ol><blockquote>quoted</blockquote>",
        ));
        assert_eq!(
            t,
            "See the site <https://x.test> or a@x.test\n\n1. one\n2. two\n   more\n\n> quoted"
        );
    }
}
