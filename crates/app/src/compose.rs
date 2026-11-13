//! Writing mail: a [`Draft`] as someone types it, started blank or as a reply, and the [`Quote`]
//! a reply carries of the message it answers.

use jiff::Timestamp;
use jiff::tz::TimeZone;
use simplymail_core::account::Account;
use simplymail_core::message::{Address, Body, Envelope, Outgoing};
use simplymail_mail::smtp::parse_addresses;

use crate::{Error, Result};

/// A message being written. Recipients are kept as typed, comma-separated, until it's sent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Draft {
    /// The account it's sent from.
    pub account: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    /// What's written, as plain text.
    pub text: String,
    /// What's written, as an HTML fragment, when it was written with formatting. `text` is then
    /// its plain-text version.
    pub html: Option<String>,
    /// The message this replies to, quoted below what's written.
    pub quote: Option<Quote>,
    /// The `Message-ID` this replies to.
    pub in_reply_to: Option<String>,
    /// The thread it continues, oldest first.
    pub references: Vec<String>,
}

/// The message a reply answers, quoted below it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Quote {
    /// "On Thu 12 Nov 2026 09:30, Ada <ada@example.com> wrote:"
    pub header: String,
    /// The message's text.
    pub text: String,
    /// The message's HTML, made safe to quote ([`simplymail_html::quotable`]), if it had any.
    pub html: Option<String>,
}

impl Quote {
    /// The quote in plain text: the header, then each line behind "> ".
    #[must_use]
    pub fn as_text(&self) -> String {
        let quoted: Vec<String> = self
            .text
            .lines()
            .map(|l| {
                if l.is_empty() {
                    ">".to_owned()
                } else {
                    format!("> {l}")
                }
            })
            .collect();
        format!("{}\n{}\n", self.header, quoted.join("\n"))
    }

    /// The quote in HTML: the header, then the message in a `<blockquote>`, with its own
    /// formatting when it had HTML.
    #[must_use]
    pub fn as_html(&self) -> String {
        let quoted = self
            .html
            .clone()
            .unwrap_or_else(|| simplymail_html::from_text(&self.text));
        format!(
            "<p>{}</p>\n<blockquote type=\"cite\" style=\"margin:0 0 0 0.8ex;border-left:1px solid #ccc;padding-left:1ex\">\n{quoted}\n</blockquote>",
            simplymail_html::escape(&self.header)
        )
    }
}

impl Draft {
    /// A blank message from `account`.
    #[must_use]
    pub fn new(account: &str) -> Self {
        Self {
            account: account.to_owned(),
            ..Self::default()
        }
    }

    /// A reply from `account` to a message, quoting it. With `all`, everyone else it went to is
    /// copied in, except `account` itself. Replying to a message `account` sent goes to its
    /// recipients again, as when following up.
    #[must_use]
    pub fn reply(
        account: &Account,
        envelope: &Envelope,
        body: &Body,
        all: bool,
        tz: &TimeZone,
    ) -> Self {
        let from_me = envelope
            .from
            .email
            .as_deref()
            .is_some_and(|e| is_same(e, &account.id));
        let to = if from_me {
            body.to.clone()
        } else if body.reply_to.is_empty() {
            vec![envelope.from.clone()]
        } else {
            body.reply_to.clone()
        };
        let cc = if all {
            let mine = |a: &Address| a.email.as_deref().is_some_and(|e| is_same(e, &account.id));
            let listed = |a: &Address, list: &[Address]| {
                list.iter().any(|b| match (&a.email, &b.email) {
                    (Some(x), Some(y)) => is_same(x, y),
                    _ => false,
                })
            };
            let mut cc: Vec<Address> = Vec::new();
            for a in body.to.iter().chain(&body.cc) {
                if a.email.is_some() && !mine(a) && !listed(a, &to) && !listed(a, &cc) {
                    cc.push(a.clone());
                }
            }
            cc
        } else {
            Vec::new()
        };
        let subject = envelope.subject.as_deref().unwrap_or_default().trim();
        let mut references = body.references.clone();
        if let Some(id) = &envelope.message_id
            && !references.contains(id)
        {
            references.push(id.clone());
        }
        Self {
            account: account.id.clone(),
            to: join(&to),
            cc: join(&cc),
            bcc: String::new(),
            subject: if has_reply_prefix(subject) {
                subject.to_owned()
            } else {
                format!("Re: {subject}")
            },
            text: String::new(),
            html: None,
            quote: Some(quote(envelope, body, tz)),
            in_reply_to: envelope.message_id.clone(),
            references,
        }
    }

    /// Moves the quote into the text, for editing it as plain text with the rest.
    pub fn inline_quote(&mut self) {
        if let Some(quote) = self.quote.take() {
            self.text = format!("{}\n\n{}", self.text, quote.as_text());
        }
    }

    /// The HTML to send: what's written, then the quote, as a whole document. `None` when
    /// neither has formatting, so the message goes as plain text.
    #[must_use]
    pub fn html(&self) -> Option<String> {
        let quote = self.quote.as_ref().filter(|q| q.html.is_some());
        if self.html.is_none() && quote.is_none() {
            return None;
        }
        let mut body = self
            .html
            .clone()
            .unwrap_or_else(|| simplymail_html::from_text(&self.text));
        if let Some(quote) = &self.quote {
            body.push_str("\n<br>\n");
            body.push_str(&quote.as_html());
        }
        Some(simplymail_html::document(&body))
    }

    /// The plain text to send: what's written, then the quote.
    #[must_use]
    pub fn plain_text(&self) -> String {
        self.quote.as_ref().map_or_else(
            || self.text.clone(),
            |quote| format!("{}\n\n{}", self.text.trim_end(), quote.as_text()),
        )
    }

    /// The message to send, from `account`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] when an address can't be read or there are no recipients.
    pub fn outgoing(&self, account: &Account) -> Result<Outgoing> {
        let parse = |list: &str| parse_addresses(list).map_err(|e| Error::Invalid(e.to_string()));
        let message = Outgoing {
            from: Address {
                name: account.display_name.clone(),
                email: Some(account.id.clone()),
            },
            to: parse(&self.to)?,
            cc: parse(&self.cc)?,
            bcc: parse(&self.bcc)?,
            subject: self.subject.trim().to_owned(),
            text: self.plain_text(),
            html: self.html(),
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
        };
        if message.to.is_empty() && message.cc.is_empty() && message.bcc.is_empty() {
            return Err(Error::Invalid("add at least one recipient".into()));
        }
        Ok(message)
    }
}

const fn is_same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn has_reply_prefix(subject: &str) -> bool {
    subject
        .get(..3)
        .is_some_and(|p| p.eq_ignore_ascii_case("re:"))
}

fn join(addresses: &[Address]) -> String {
    addresses
        .iter()
        .map(Address::full)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The original under "On Thu 12 Nov 2026 09:30, Ada wrote:".
fn quote(envelope: &Envelope, body: &Body, tz: &TimeZone) -> Quote {
    let who = envelope.from.full();
    let when = envelope
        .date
        .and_then(|d| Timestamp::from_second(d).ok())
        .map(|d| {
            d.to_zoned(tz.clone())
                .strftime("%a %-d %b %Y %H:%M")
                .to_string()
        });
    let header = when.map_or_else(
        || format!("{who} wrote:"),
        |when| format!("On {when}, {who} wrote:"),
    );
    Quote {
        header,
        text: body.text.clone(),
        html: body
            .html
            .as_deref()
            .map(simplymail_html::quotable)
            .filter(|h| !h.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use simplymail_core::account::{Auth, Security, Server};

    use super::*;

    fn address(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(str::to_owned),
            email: Some(email.to_owned()),
        }
    }

    fn account() -> Account {
        let server = Server {
            host: "localhost".into(),
            port: 1,
            security: Security::None,
            username: "me".into(),
        };
        Account {
            id: "me@example.com".into(),
            display_name: Some("Me".into()),
            imap: server.clone(),
            smtp: server,
            auth: Auth::Password,
        }
    }

    fn envelope() -> Envelope {
        Envelope {
            uid: 1,
            message_id: Some("parent@example.com".into()),
            from: address(Some("Ada"), "ada@example.com"),
            to: Vec::new(),
            subject: Some("Lunch".into()),
            date: Some(1_794_475_800),
            seen: true,
        }
    }

    fn body() -> Body {
        Body {
            to: vec![
                address(None, "ME@example.com"),
                address(Some("Bob"), "bob@example.com"),
            ],
            cc: vec![
                address(None, "carol@example.com"),
                address(None, "ada@example.com"),
            ],
            references: vec!["root@example.com".into()],
            text: "Noon?\n\nBring snacks.".into(),
            ..Body::default()
        }
    }

    #[test]
    fn a_reply_goes_to_the_sender_and_continues_the_thread() {
        let d = Draft::reply(&account(), &envelope(), &body(), false, &TimeZone::UTC);
        assert_eq!(d.to, "Ada <ada@example.com>");
        assert_eq!(d.cc, "");
        assert_eq!(d.subject, "Re: Lunch");
        assert_eq!(d.in_reply_to.as_deref(), Some("parent@example.com"));
        assert_eq!(d.references, vec!["root@example.com", "parent@example.com"]);
        assert_eq!(d.text, "");
        let quote = d.quote.clone().unwrap();
        assert_eq!(
            quote.as_text(),
            "On Thu 12 Nov 2026 09:30, Ada <ada@example.com> wrote:\n> Noon?\n>\n> Bring snacks.\n"
        );
        assert_eq!(d.html(), None, "a reply to plain text can stay plain");

        let mut inline = d;
        inline.text = "Yes!".into();
        inline.inline_quote();
        assert!(inline.quote.is_none());
        assert_eq!(
            inline.text,
            "Yes!\n\nOn Thu 12 Nov 2026 09:30, Ada <ada@example.com> wrote:\n> Noon?\n>\n> Bring snacks.\n"
        );
    }

    #[test]
    fn a_reply_to_html_quotes_its_formatting() {
        let body = Body {
            html: Some(
                "<html><body><p>Noon <b>sharp</b>?</p><script>x()</script></body></html>".into(),
            ),
            ..body()
        };
        let mut d = Draft::reply(&account(), &envelope(), &body, false, &TimeZone::UTC);
        d.to = "ada@example.com".into();
        d.text = "Yes".into();
        let m = d.outgoing(&account()).unwrap();
        let html = m.html.unwrap();
        assert!(
            html.contains("<p>Yes</p>"),
            "plain text becomes HTML: {html}"
        );
        assert!(html.contains("<blockquote type=\"cite\""), "{html}");
        assert!(html.contains("<p>Noon <b>sharp</b>?</p>"), "{html}");
        assert!(!html.contains("script"), "{html}");
        assert!(m.text.starts_with("Yes\n\nOn Thu"), "{}", m.text);
        assert!(m.text.contains("> Noon?"), "{}", m.text);
    }

    #[test]
    fn formatted_text_is_sent_as_html_with_a_text_version() {
        let mut d = Draft::new("me@example.com");
        d.to = "ada@example.com".into();
        d.text = "Hi there".into();
        d.html = Some("<p>Hi <b>there</b></p>".into());
        let m = d.outgoing(&account()).unwrap();
        assert_eq!(m.text, "Hi there");
        let html = m.html.unwrap();
        assert!(html.starts_with("<!DOCTYPE html>"), "{html}");
        assert!(html.contains("<p>Hi <b>there</b></p>"), "{html}");
    }

    #[test]
    fn reply_all_copies_everyone_else_once() {
        let d = Draft::reply(&account(), &envelope(), &body(), true, &TimeZone::UTC);
        assert_eq!(d.cc, "Bob <bob@example.com>, carol@example.com");
    }

    #[test]
    fn reply_to_wins_and_re_isnt_doubled() {
        let envelope = Envelope {
            subject: Some("RE: Lunch".into()),
            ..envelope()
        };
        let body = Body {
            reply_to: vec![address(None, "list@example.com")],
            ..body()
        };
        let d = Draft::reply(&account(), &envelope, &body, false, &TimeZone::UTC);
        assert_eq!(d.to, "list@example.com");
        assert_eq!(d.subject, "RE: Lunch");
    }

    #[test]
    fn a_reply_to_my_own_message_goes_to_its_recipients() {
        let envelope = Envelope {
            from: address(Some("Me"), "me@example.com"),
            ..envelope()
        };
        let body = Body {
            to: vec![address(Some("Ada"), "ada@example.com")],
            cc: vec![],
            ..body()
        };
        let d = Draft::reply(&account(), &envelope, &body, true, &TimeZone::UTC);
        assert_eq!(d.to, "Ada <ada@example.com>");
        assert_eq!(d.cc, "", "nobody is copied twice");
    }

    #[test]
    fn a_draft_needs_readable_recipients() {
        let mut d = Draft::new("me@example.com");
        assert!(d.outgoing(&account()).is_err(), "no recipients");
        d.to = "nope".into();
        assert!(d.outgoing(&account()).is_err(), "not an address");
        d.to = "Ada <ada@example.com>".into();
        d.subject = " Hi ".into();
        let m = d.outgoing(&account()).unwrap();
        assert_eq!(m.from, address(Some("Me"), "me@example.com"));
        assert_eq!(m.to, vec![address(Some("Ada"), "ada@example.com")]);
        assert_eq!(m.subject, "Hi");
    }
}
