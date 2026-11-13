//! Keeping a copy of sent mail in the account's Sent mailbox.

use std::time::Duration;

use simplymail_core::account::Server;

use super::connection::{Session, connect};
use super::mailboxes::find_sent;
use super::watch::with_timeout;
use crate::{Credentials, Result};

/// The mailbox created for sent mail when an account has none.
const NEW_SENT: &str = "Sent";

/// How long to give a server that files sent mail itself (as Gmail does) before looking for it.
const FILING_GRACE: Duration = Duration::from_secs(3);

/// Files `raw`, a message just sent with `message_id`, in the account's Sent mailbox.
///
/// A Sent mailbox is created if there's none. Nothing is filed when the server already did it, as
/// some do for mail sent through their SMTP server.
///
/// # Errors
///
/// Returns an error when the connection fails or the server refuses the message.
pub async fn save_sent_copy(
    server: &Server,
    credentials: &Credentials,
    message_id: &str,
    raw: &[u8],
) -> Result<()> {
    let mut session = connect(server, credentials).await?;
    let result = async {
        let mailbox = if let Some(name) = find_sent(&mut session).await? {
            tokio::time::sleep(FILING_GRACE).await;
            if already_filed(&mut session, &name, message_id).await? {
                return Ok(());
            }
            name
        } else {
            with_timeout(session.create(NEW_SENT)).await?;
            NEW_SENT.to_owned()
        };
        with_timeout(session.append(&mailbox, Some("(\\Seen)"), None, raw)).await
    }
    .await;
    let _ignored = session.logout().await;
    result
}

async fn already_filed(session: &mut Session, mailbox: &str, message_id: &str) -> Result<bool> {
    with_timeout(session.examine(mailbox)).await?;
    // Message-IDs we make are alphanumeric, an `@`, and a domain, so they need no escaping.
    let query = format!("HEADER Message-ID \"<{message_id}>\"");
    let found = with_timeout(session.uid_search(query)).await?;
    Ok(!found.is_empty())
}
