//! Keeping an account's mailboxes other than the inbox in sync, on a timer rather than live: one
//! IDLE connection per account is enough, and mail rarely arrives anywhere but the inbox.

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::sync::Arc;

use simplymail_core::account::Account;
use simplymail_core::message::{MailboxState, Role};
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::sleep;

use super::connection::connect;
use super::mailboxes::list_mailboxes;
use super::watch::{Event, WatchConfig, sync, with_timeout};
use crate::{Credentials, Error, Result};

/// Lists `account`'s mailboxes and syncs every one but the inbox, then again every
/// [`WatchConfig::refresh_interval`] or as soon as `nudge` is notified, until `events` is closed.
///
/// Starts from `cached`, each mailbox's state as last synced, by name. Sends
/// [`Event::Mailboxes`] after each listing and [`Event::Messages`] for what each sync finds.
/// Failures aren't reported, as the inbox's watcher already says whether the account works; the
/// next round tries again.
pub async fn refresh<T: From<Event>, S: BuildHasher + Send>(
    account: Account,
    credentials: Credentials,
    mut cached: HashMap<String, MailboxState, S>,
    config: WatchConfig,
    nudge: Arc<Notify>,
    events: UnboundedSender<T>,
) {
    loop {
        // An error ends this round only; the next one reconnects.
        let _failed = round(&account, &credentials, &mut cached, &config, &events).await;
        if events.is_closed() {
            return;
        }
        tokio::select! {
            () = sleep(config.refresh_interval) => {}
            () = nudge.notified() => {}
        }
    }
}

/// One listing and sync of every mailbox but the inbox.
async fn round<T: From<Event>, S: BuildHasher>(
    account: &Account,
    credentials: &Credentials,
    cached: &mut HashMap<String, MailboxState, S>,
    config: &WatchConfig,
    events: &UnboundedSender<T>,
) -> Result<()> {
    let mut session = connect(&account.imap, credentials).await?;
    let result = async {
        let mailboxes = list_mailboxes(&mut session).await?;
        cached.retain(|name, _| mailboxes.iter().any(|m| m.name == *name));
        let others: Vec<_> = mailboxes
            .iter()
            .filter(|m| m.role != Role::Inbox)
            .cloned()
            .collect();
        send(
            events,
            Event::Mailboxes {
                account: account.id.clone(),
                mailboxes,
            },
        )?;
        for mailbox in others {
            let mut state = cached.get(&mailbox.name).copied();
            match with_timeout(sync(&mut session, &mailbox.name, config, &mut state)).await {
                Ok(synced) => {
                    if let Some(state) = state {
                        cached.insert(mailbox.name.clone(), state);
                    }
                    if let Some((uid_validity, messages)) = synced {
                        send(
                            events,
                            Event::Messages {
                                account: account.id.clone(),
                                mailbox: mailbox.name,
                                role: mailbox.role,
                                uid_validity,
                                messages,
                            },
                        )?;
                    }
                }
                // The server refused this mailbox; the others may still work.
                Err(e) if is_refusal(&e) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    .await;
    let _ignored = session.logout().await;
    result
}

/// Sends `event`, or fails with [`Error::Closed`] once nobody is listening.
fn send<T: From<Event>>(events: &UnboundedSender<T>, event: Event) -> Result<()> {
    events.send(event.into()).map_err(|_closed| Error::Closed)
}

/// Whether the server turned one command down, leaving the connection usable.
const fn is_refusal(e: &Error) -> bool {
    matches!(
        e,
        Error::NoUidValidity(_)
            | Error::Imap(async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_))
    )
}
