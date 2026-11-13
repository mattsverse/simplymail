//! Turning IMAP FETCH responses into [`Envelope`]s.

use async_imap::types::{Fetch, Flag};
use mail_parser::MessageParser;
use simplymail_core::message::{Address, Envelope};

/// The FETCH items an [`Envelope`] is built from. `PEEK` leaves the message unread.
pub(super) const QUERY: &str =
    "(UID FLAGS INTERNALDATE BODY.PEEK[HEADER.FIELDS (FROM TO SUBJECT DATE MESSAGE-ID)])";

/// Builds an envelope from a response to a [`QUERY`] fetch, or `None` if it has no UID.
pub(super) fn from_fetch(fetch: &Fetch) -> Option<Envelope> {
    let uid = fetch.uid?;
    let seen = fetch.flags().any(|f| f == Flag::Seen);
    let received = fetch.internal_date().map(|d| d.timestamp());
    Some(from_header(
        uid,
        seen,
        fetch.header().unwrap_or_default(),
        received,
    ))
}

/// Builds an envelope from raw header bytes. Missing or unparsable headers are left empty, except the
/// date, which falls back to `received`: when the server got the message.
pub(super) fn from_header(uid: u32, seen: bool, header: &[u8], received: Option<i64>) -> Envelope {
    let parsed = MessageParser::default().parse_headers(header);
    let parsed = parsed.as_ref();
    let address = |m: mail_parser::Mailbox<'_>| Address {
        name: m.name().map(str::to_owned),
        email: m.address().map(str::to_owned),
    };
    let from = parsed
        .and_then(mail_parser::Message::from)
        .and_then(|list| list.first())
        .map(address)
        .unwrap_or_default();
    let to = parsed
        .and_then(mail_parser::Message::to)
        .map(|list| list.mailboxes().map(address).collect())
        .unwrap_or_default();
    Envelope {
        uid,
        message_id: parsed.and_then(|m| m.message_id()).map(str::to_owned),
        from,
        to,
        subject: parsed.and_then(|m| m.subject()).map(str::to_owned),
        date: parsed
            .and_then(mail_parser::Message::date)
            .map(|d| d.to_timestamp())
            .or(received),
        seen,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_decoded() {
        let header = b"From: =?UTF-8?Q?Ad=C3=A9le?= <adele@example.com>\r\n\
            Subject: =?UTF-8?B?Q2Fmw6k/?=\r\n\
            Date: Thu, 12 Nov 2026 09:30:00 +0000\r\n\
            To: Me <me@example.com>, bob@example.com\r\n\
            Message-ID: <abc@example.com>\r\n\r\n";
        let envelope = from_header(42, true, header, Some(1));
        assert_eq!(
            envelope,
            Envelope {
                uid: 42,
                message_id: Some("abc@example.com".into()),
                from: Address {
                    name: Some("Adéle".into()),
                    email: Some("adele@example.com".into()),
                },
                to: vec![
                    Address {
                        name: Some("Me".into()),
                        email: Some("me@example.com".into()),
                    },
                    Address {
                        name: None,
                        email: Some("bob@example.com".into()),
                    },
                ],
                subject: Some("Café?".into()),
                date: Some(1_794_475_800),
                seen: true,
            }
        );
    }

    #[test]
    fn missing_headers_leave_fields_empty() {
        let envelope = from_header(1, false, b"", None);
        assert_eq!(envelope.from, Address::default());
        assert_eq!(envelope.subject, None);
        assert_eq!(envelope.date, None);
    }

    #[test]
    fn without_a_date_header_the_received_time_is_used() {
        let envelope = from_header(1, false, b"Subject: hi\r\n\r\n", Some(1_794_475_800));
        assert_eq!(envelope.date, Some(1_794_475_800));
    }
}
