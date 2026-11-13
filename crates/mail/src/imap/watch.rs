//! Keeping an account's inbox in sync and reporting new mail as it arrives.

use std::time::Duration;

use async_imap::types::Fetch;
use futures::TryStreamExt as _;
use simplymail_core::account::Account;
use simplymail_core::message::{Envelope, Mailbox, MailboxState, Role};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::{sleep, timeout};

use super::connection::{Session, connect};
use super::envelope;
use crate::{Credentials, Error, Result};

/// The inbox's name on every server.
const INBOX: &str = "INBOX";
/// How long one sync (SELECT and FETCH) may take before the connection is presumed dead.
pub(super) const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// How long to IDLE before re-issuing it. RFC 2177 asks for under 29 minutes; shorter also gets
/// past NAT boxes that drop quiet connections, and notices a dead connection sooner.
const IDLE_TIMEOUT: Duration = Duration::from_mins(9);
const MIN_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_mins(5);

/// How [`watch`] syncs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchConfig {
    /// How often to check for new mail when the server doesn't support IDLE.
    pub poll_interval: Duration,
    /// How many of the newest messages to fetch when nothing is cached yet.
    pub initial_fetch: u32,
    /// How often mailboxes other than the inbox are listed and synced.
    pub refresh_interval: Duration,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(60),
            initial_fetch: 200,
            refresh_interval: Duration::from_mins(5),
        }
    }
}

/// What a watcher is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Connecting,
    /// Connected, and the server pushes new mail as it arrives (IMAP IDLE).
    Push,
    /// Connected, and checking for new mail on this interval because the server lacks IDLE.
    Polling(Duration),
    /// The connection failed and will be retried.
    Failed {
        error: String,
        retry_in: Duration,
    },
}

/// Something a watcher reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The watcher of `account`'s inbox changed what it's doing.
    Status { account: String, status: Status },
    /// Messages fetched from `mailbox`, oldest first. Save them under `uid_validity`; if it differs from
    /// what was cached, the cache for that mailbox is stale and `messages` is the fresh start. That's
    /// also why this can be empty.
    Messages {
        account: String,
        mailbox: String,
        role: Role,
        uid_validity: u32,
        messages: Vec<Envelope>,
    },
    /// The account's mailboxes, as the server just listed them.
    Mailboxes {
        account: String,
        mailboxes: Vec<Mailbox>,
    },
}

/// Watches `account`'s inbox until `events` is closed, sending what arrives.
///
/// Starts from `state`, what's already cached, so only newer mail is fetched. Connection failures are
/// reported as [`Status::Failed`] and retried with backoff, so this only returns once nobody is
/// listening any more.
///
/// `events` can carry any type an [`Event`] converts into, so a caller can share one channel between
/// watchers and other work.
pub async fn watch<T: From<Event>>(
    account: Account,
    credentials: Credentials,
    mut state: Option<MailboxState>,
    config: WatchConfig,
    events: UnboundedSender<T>,
) {
    let mut watcher = Watcher {
        account: &account,
        config: &config,
        events: &events,
        backoff: MIN_BACKOFF,
    };
    loop {
        if !watcher.status(Status::Connecting) {
            return;
        }
        let error = match watcher.run(&credentials, &mut state).await {
            Ok(()) => return,
            Err(e) => e,
        };
        let retry_in = watcher.backoff;
        if !watcher.status(Status::Failed {
            error: error.to_string(),
            retry_in,
        }) {
            return;
        }
        sleep(retry_in).await;
        watcher.backoff = (retry_in.saturating_mul(2)).min(MAX_BACKOFF);
    }
}

struct Watcher<'a, T> {
    account: &'a Account,
    config: &'a WatchConfig,
    events: &'a UnboundedSender<T>,
    backoff: Duration,
}

impl<T: From<Event>> Watcher<'_, T> {
    /// Reports a status change. Returns `false` when nobody is listening.
    fn status(&self, status: Status) -> bool {
        self.send(Event::Status {
            account: self.account.id.clone(),
            status,
        })
    }

    fn send(&self, event: Event) -> bool {
        self.events.send(event.into()).is_ok()
    }

    /// One connection's lifetime. Returns `Ok` only once nobody is listening.
    async fn run(
        &mut self,
        credentials: &Credentials,
        state: &mut Option<MailboxState>,
    ) -> Result<()> {
        let mut session = connect(&self.account.imap, credentials).await?;
        self.backoff = MIN_BACKOFF;
        let push = with_timeout(session.capabilities()).await?.has_str("IDLE");
        let status = if push {
            Status::Push
        } else {
            Status::Polling(self.config.poll_interval)
        };
        if !self.status(status) {
            return Ok(());
        }
        loop {
            let synced = with_timeout(sync(&mut session, INBOX, self.config, state)).await?;
            if let Some((uid_validity, messages)) = synced
                && !self.send(Event::Messages {
                    account: self.account.id.clone(),
                    mailbox: INBOX.to_owned(),
                    role: Role::Inbox,
                    uid_validity,
                    messages,
                })
            {
                return Ok(());
            }
            if self.events.is_closed() {
                return Ok(());
            }
            session = if push {
                idle(session).await?
            } else {
                sleep(self.config.poll_interval).await;
                session
            };
        }
    }
}

pub(super) async fn with_timeout<T>(
    f: impl Future<Output = Result<T, impl Into<Error>>>,
) -> Result<T> {
    timeout(COMMAND_TIMEOUT, f)
        .await
        .map_err(|_elapsed| Error::Timeout)?
        .map_err(Into::into)
}

/// Waits until the server reports a change, or [`IDLE_TIMEOUT`] passes.
async fn idle(session: Session) -> Result<Session> {
    let mut handle = session.idle();
    with_timeout(handle.init()).await?;
    let (wait, _stop) = handle.wait_with_timeout(IDLE_TIMEOUT);
    // Whatever woke us (new mail, a flag change, or the timeout), the next sync finds out what changed.
    wait.await?;
    with_timeout(handle.done()).await
}

/// Fetches what's new in the mailbox since `state`, and advances `state` past it.
///
/// Returns the mailbox's UIDVALIDITY and the new messages, or `None` when nothing changed.
pub(super) async fn sync(
    session: &mut Session,
    name: &str,
    config: &WatchConfig,
    state: &mut Option<MailboxState>,
) -> Result<Option<(u32, Vec<Envelope>)>> {
    let mailbox = session.select(name).await?;
    let uid_validity = mailbox
        .uid_validity
        .ok_or_else(|| Error::NoUidValidity(name.to_owned()))?;
    let plan = plan(*state, uid_validity, mailbox.exists, config.initial_fetch);
    let fetches: Vec<Fetch> = match plan {
        None => Vec::new(),
        Some(Plan::After(uid)) => {
            session
                .uid_fetch(format!("{}:*", uid.saturating_add(1)), envelope::QUERY)
                .await?
                .try_collect()
                .await?
        }
        Some(Plan::Newest { from_seq }) => {
            session
                .fetch(format!("{from_seq}:*"), envelope::QUERY)
                .await?
                .try_collect()
                .await?
        }
    };
    let after = match plan {
        Some(Plan::After(uid)) => uid,
        _ => 0,
    };
    let mut messages: Vec<Envelope> = fetches
        .iter()
        .filter_map(envelope::from_fetch)
        // `n:*` always matches the last message, even when its UID is below n.
        .filter(|m| m.uid > after)
        .collect();
    messages.sort_by_key(|m| m.uid);

    let reset = state.is_none_or(|s| s.uid_validity != uid_validity);
    let last_uid = messages
        .last()
        .map(|m| m.uid)
        .or_else(|| state.filter(|_| !reset).and_then(|s| s.last_uid));
    *state = Some(MailboxState {
        uid_validity,
        last_uid,
    });
    Ok((reset || !messages.is_empty()).then_some((uid_validity, messages)))
}

/// What to fetch after selecting a mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// Every message with a UID above this one.
    After(u32),
    /// The newest messages, from this sequence number on.
    Newest { from_seq: u32 },
}

/// Decides what to fetch, given what's cached and what the server reports on SELECT.
fn plan(
    state: Option<MailboxState>,
    uid_validity: u32,
    exists: u32,
    initial_fetch: u32,
) -> Option<Plan> {
    if exists == 0 {
        return None;
    }
    if let Some(MailboxState {
        uid_validity: cached,
        last_uid: Some(last),
    }) = state
        && cached == uid_validity
    {
        return Some(Plan::After(last));
    }
    let count = initial_fetch.min(exists);
    (count > 0).then(|| Plan::Newest {
        from_seq: exists - count + 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn state(uid_validity: u32, last_uid: Option<u32>) -> MailboxState {
        MailboxState {
            uid_validity,
            last_uid,
        }
    }

    #[test]
    fn an_empty_mailbox_fetches_nothing() {
        assert_eq!(plan(None, 1, 0, 200), None);
        assert_eq!(plan(Some(state(1, Some(5))), 1, 0, 200), None);
    }

    #[test]
    fn a_first_sync_fetches_only_the_newest_messages() {
        assert_eq!(
            plan(None, 1, 1000, 200),
            Some(Plan::Newest { from_seq: 801 })
        );
        assert_eq!(plan(None, 1, 50, 200), Some(Plan::Newest { from_seq: 1 }));
        assert_eq!(plan(None, 1, 50, 0), None, "initial fetch disabled");
    }

    #[test]
    fn a_cached_mailbox_fetches_what_came_after() {
        assert_eq!(
            plan(Some(state(1, Some(41))), 1, 1000, 200),
            Some(Plan::After(41))
        );
    }

    #[test]
    fn a_changed_uid_validity_starts_over() {
        assert_eq!(
            plan(Some(state(1, Some(41))), 2, 1000, 200),
            Some(Plan::Newest { from_seq: 801 })
        );
    }

    #[test]
    fn a_mailbox_that_was_empty_starts_with_the_newest() {
        assert_eq!(
            plan(Some(state(1, None)), 1, 3, 200),
            Some(Plan::Newest { from_seq: 1 })
        );
    }
}
