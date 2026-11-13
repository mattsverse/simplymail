//! Listing an account's mailboxes, and telling which is for what.

use async_imap::types::NameAttribute;
use futures::TryStreamExt as _;
use simplymail_core::message::{Mailbox, Role};

use super::connection::Session;
use super::watch::with_timeout;
use crate::Result;

/// What a Sent mailbox is called when the server doesn't mark one, in order of preference, compared
/// without regard to case against a mailbox's whole name and then its last part.
const SENT_NAMES: [&str; 4] = ["Sent", "Sent Items", "Sent Messages", "Sent Mail"];

/// Every mailbox on the account that can be opened, with the inbox and the Sent mailbox marked.
///
/// # Errors
///
/// Returns an error when the mailboxes can't be listed.
pub(crate) async fn list_mailboxes(session: &mut Session) -> Result<Vec<Mailbox>> {
    let names: Vec<_> =
        with_timeout(async { session.list(Some(""), Some("*")).await?.try_collect().await })
            .await?;
    let listed: Vec<Listed> = names
        .iter()
        .map(|n| Listed {
            name: n.name().to_owned(),
            delimiter: n.delimiter().map(str::to_owned),
            marked_sent: n.attributes().contains(&NameAttribute::Sent),
            selectable: !n.attributes().contains(&NameAttribute::NoSelect),
        })
        .collect();
    Ok(classify(&listed))
}

/// The name of the account's Sent mailbox, if it has one.
///
/// # Errors
///
/// Returns an error when the mailboxes can't be listed.
pub(crate) async fn find_sent(session: &mut Session) -> Result<Option<String>> {
    Ok(list_mailboxes(session)
        .await?
        .into_iter()
        .find(|m| m.role == Role::Sent)
        .map(|m| m.name))
}

/// The selectable mailboxes, each with its role.
fn classify(listed: &[Listed]) -> Vec<Mailbox> {
    let sent = pick_sent(listed);
    listed
        .iter()
        .filter(|l| l.selectable)
        .map(|l| Mailbox {
            name: l.name.clone(),
            delimiter: l.delimiter.clone(),
            // INBOX is case-insensitive (RFC 3501 section 5.1).
            role: if l.name.eq_ignore_ascii_case("INBOX") {
                Role::Inbox
            } else if sent == Some(l.name.as_str()) {
                Role::Sent
            } else {
                Role::Other
            },
        })
        .collect()
}

/// A mailbox as LIST reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed {
    name: String,
    delimiter: Option<String>,
    /// Marked `\Sent` (RFC 6154).
    marked_sent: bool,
    selectable: bool,
}

/// The mailbox the server marks as Sent, or else the first that's named like one.
fn pick_sent(listed: &[Listed]) -> Option<&str> {
    let usable = || listed.iter().filter(|l| l.selectable);
    if let Some(marked) = usable().find(|l| l.marked_sent) {
        return Some(&marked.name);
    }
    let last_part = |l: &Listed| match l.delimiter.as_deref() {
        Some(d) if !d.is_empty() => l.name.rsplit(d).next().unwrap_or(&l.name).to_owned(),
        _ => l.name.clone(),
    };
    SENT_NAMES.iter().find_map(|wanted| {
        usable()
            .find(|l| l.name.eq_ignore_ascii_case(wanted))
            .or_else(|| usable().find(|l| last_part(l).eq_ignore_ascii_case(wanted)))
            .map(|l| l.name.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(name: &str, marked_sent: bool) -> Listed {
        Listed {
            name: name.into(),
            delimiter: Some("/".into()),
            marked_sent,
            selectable: true,
        }
    }

    #[test]
    fn the_marked_mailbox_wins() {
        let names = [
            listed("INBOX", false),
            listed("Sent", false),
            listed("[Gmail]/Sent Mail", true),
        ];
        assert_eq!(pick_sent(&names), Some("[Gmail]/Sent Mail"));
    }

    #[test]
    fn without_a_mark_a_familiar_name_is_used() {
        let names = [listed("INBOX", false), listed("Sent Items", false)];
        assert_eq!(pick_sent(&names), Some("Sent Items"));
        let nested = [Listed {
            delimiter: Some(".".into()),
            ..listed("INBOX.Sent", false)
        }];
        assert_eq!(pick_sent(&nested), Some("INBOX.Sent"));
        assert_eq!(pick_sent(&[listed("INBOX", false)]), None);
    }

    #[test]
    fn a_mailbox_that_cant_be_selected_is_skipped() {
        let names = [
            Listed {
                selectable: false,
                ..listed("[Gmail]", true)
            },
            listed("sent", false),
        ];
        assert_eq!(pick_sent(&names), Some("sent"));
    }

    #[test]
    fn every_openable_mailbox_is_listed_with_its_role() {
        let names = [
            listed("INBOX", false),
            Listed {
                selectable: false,
                ..listed("[Gmail]", false)
            },
            listed("[Gmail]/Sent Mail", true),
            listed("Receipts", false),
        ];
        let roles: Vec<(String, Role)> = classify(&names)
            .into_iter()
            .map(|m| (m.name, m.role))
            .collect();
        assert_eq!(
            roles,
            [
                ("INBOX".to_owned(), Role::Inbox),
                ("[Gmail]/Sent Mail".to_owned(), Role::Sent),
                ("Receipts".to_owned(), Role::Other),
            ]
        );
    }
}
