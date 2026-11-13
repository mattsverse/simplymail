//! Watching every account's inbox in the background, refreshing its other mailboxes on a timer,
//! and saving what arrives; and opening, flagging and sending messages on the same background
//! runtime.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use simplymail_core::account::{Account, Server};
use simplymail_core::message::{Body, Outgoing, Role};
use simplymail_core::secrets::SecretStore;
use simplymail_mail::imap::{
    Event, Status, WatchConfig, open_message, refresh, save_sent_copy, set_seen, watch,
};
use simplymail_mail::{Credentials, smtp};
use simplymail_store::Store;
use tokio::runtime::Runtime;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::AbortHandle;

use crate::{Error, Result, credentials};

/// Something the UI should show. By the time it's sent, the mail is already in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// An account's connection changed.
    Status { account: String, status: Status },
    /// The server listed an account's mailboxes. They're in the store.
    Mailboxes { account: String },
    /// Mail was saved. `first_sync` is true when this is the mailbox's first fetch (or a restart after
    /// its UIDVALIDITY changed), so the messages aren't "new", just newly cached.
    Mail {
        account: String,
        mailbox: String,
        role: Role,
        count: usize,
        first_sync: bool,
    },
    /// Fetched mail couldn't be saved.
    SaveFailed { account: String, error: String },
    /// A message asked for with [`Mailroom::open`] was downloaded, or couldn't be. Once downloaded
    /// it's read, on the server and in the store.
    Opened {
        account: String,
        mailbox: String,
        uid: u32,
        body: Result<Body, String>,
    },
    /// A message was marked read or unread, on the server and in the store.
    Seen {
        account: String,
        mailbox: String,
        uid: u32,
        seen: bool,
    },
    /// Marking a message read or unread failed. The store still says what it did before.
    SeenFailed { account: String, error: String },
    /// A message handed to [`Mailroom::send`] was accepted by the account's SMTP server, or wasn't.
    /// On success, it's the `Message-ID` it was sent with.
    Sent {
        account: String,
        result: Result<String, String>,
    },
    /// A sent message couldn't be filed in the account's Sent mailbox. It was still sent.
    CopyFailed { account: String, error: String },
}

/// Work for the store thread: what watchers report, and results of work done for the UI.
#[derive(Debug)]
enum Job {
    Watched(Event),
    Opened {
        account: String,
        mailbox: String,
        uid: u32,
        body: Result<Body, String>,
    },
    Seen {
        account: String,
        mailbox: String,
        uid: u32,
        seen: bool,
        result: Result<(), String>,
    },
    Sent {
        account: String,
        result: Result<String, String>,
    },
    CopyFailed {
        account: String,
        error: String,
    },
}

impl From<Event> for Job {
    fn from(event: Event) -> Self {
        Self::Watched(event)
    }
}

/// An account being watched, with what's needed to do more work for it.
#[derive(Debug)]
struct Watched {
    server: Server,
    credentials: Credentials,
    smtp: Server,
    smtp_credentials: Credentials,
    /// The inbox's watcher and the other mailboxes' refresher.
    tasks: [AbortHandle; 2],
    /// Wakes the refresher.
    nudge: Arc<Notify>,
}

/// Runs a watcher for each account's inbox and a refresher for its other mailboxes on a background
/// runtime, and saves their mail on a thread of its own.
///
/// Each [`Update`] goes to the `notify` callback given to [`Mailroom::start`], from that background
/// thread. Dropping the mailroom stops every watcher.
#[derive(Debug)]
pub struct Mailroom {
    /// Only taken by `drop`, to shut it down without waiting.
    runtime: Option<Runtime>,
    handle: tokio::runtime::Handle,
    jobs: UnboundedSender<Job>,
    watchers: HashMap<String, Watched>,
    config: WatchConfig,
}

impl Mailroom {
    /// Starts the background runtime and the thread that saves mail into the database at `database`.
    ///
    /// # Errors
    ///
    /// Returns an error when the database can't be opened or a thread can't be started.
    pub fn start(
        database: &Path,
        config: WatchConfig,
        notify: impl Fn(Update) + Send + 'static,
    ) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("simplymail-mail")
            .enable_all()
            .build()?;
        // A connection of its own: SQLite connections can't be shared across threads.
        let mut store = Store::open(database)?;
        let (jobs, mut received) = unbounded_channel();
        std::thread::Builder::new()
            .name("simplymail-store".into())
            .spawn(move || {
                while let Some(job) = received.blocking_recv() {
                    for update in save(&mut store, job) {
                        notify(update);
                    }
                }
            })?;
        Ok(Self {
            handle: runtime.handle().clone(),
            runtime: Some(runtime),
            jobs,
            watchers: HashMap::new(),
            config,
        })
    }

    /// How often watchers check for new mail when the server doesn't push it.
    #[must_use]
    pub const fn poll_interval(&self) -> Duration {
        self.config.poll_interval
    }

    /// Checks for new mail every `every` on servers that don't push it, restarting every account's
    /// watchers to apply it. Returns the accounts that couldn't be restarted, and why.
    ///
    /// # Errors
    ///
    /// Returns an error when the accounts can't be read.
    pub fn set_poll_interval(
        &mut self,
        every: Duration,
        store: &Store,
        secrets: &dyn SecretStore,
    ) -> Result<Vec<(String, Error)>> {
        if self.config.poll_interval == every {
            return Ok(Vec::new());
        }
        self.config.poll_interval = every;
        self.watch_all(store, secrets)
    }

    /// Starts watching every account in `store`. Returns the accounts that couldn't be started, and
    /// why, such as a missing password or Google sign-in.
    ///
    /// # Errors
    ///
    /// Returns an error when the accounts can't be read.
    pub fn watch_all(
        &mut self,
        store: &Store,
        secrets: &dyn SecretStore,
    ) -> Result<Vec<(String, Error)>> {
        let mut failed = Vec::new();
        for account in store.accounts()? {
            let id = account.id.clone();
            if let Err(e) = self.watch(&account, store, secrets) {
                failed.push((id, e));
            }
        }
        Ok(failed)
    }

    /// Starts watching `account`'s inbox and refreshing its other mailboxes, replacing any watchers it
    /// had. Only mail newer than what `store` already holds is fetched.
    ///
    /// # Errors
    ///
    /// Returns an error when its passwords, tokens or sync state can't be read.
    pub fn watch(
        &mut self,
        account: &Account,
        store: &Store,
        secrets: &dyn SecretStore,
    ) -> Result<()> {
        // Read now, not when sending: the keyring may prompt.
        let (imap_credentials, smtp_credentials) = credentials(secrets, account)?;
        let mut cached = store.mailbox_states(&account.id)?;
        let inbox = cached.remove("INBOX");
        self.unwatch(&account.id);
        let nudge = Arc::new(Notify::new());
        let watcher = self.handle.spawn(watch(
            account.clone(),
            imap_credentials.clone(),
            inbox,
            self.config.clone(),
            self.jobs.clone(),
        ));
        let refresher = self.handle.spawn(refresh(
            account.clone(),
            imap_credentials.clone(),
            cached,
            self.config.clone(),
            Arc::clone(&nudge),
            self.jobs.clone(),
        ));
        self.watchers.insert(
            account.id.clone(),
            Watched {
                server: account.imap.clone(),
                credentials: imap_credentials,
                smtp: account.smtp.clone(),
                smtp_credentials,
                tasks: [watcher.abort_handle(), refresher.abort_handle()],
                nudge,
            },
        );
        Ok(())
    }

    /// Stops watching an account. Returns whether it was being watched.
    pub fn unwatch(&mut self, account: &str) -> bool {
        self.watchers.remove(account).is_some_and(|w| {
            w.tasks.iter().for_each(AbortHandle::abort);
            true
        })
    }

    /// Downloads a message in the background and marks it read. The result arrives as
    /// [`Update::Opened`].
    ///
    /// # Errors
    ///
    /// Returns an error, and does nothing, when the account isn't watched or the mailbox never synced.
    pub fn open(&self, store: &Store, account: &str, mailbox: &str, uid: u32) -> Result<()> {
        let (server, credentials, uid_validity) = self.target(store, account, mailbox)?;
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        let jobs = self.jobs.clone();
        self.handle.spawn(async move {
            let body = open_message(&server, &credentials, &mailbox, uid_validity, uid)
                .await
                .map_err(|e| e.to_string());
            // Nobody is listening only as the app quits.
            let _ignored = jobs.send(Job::Opened {
                account,
                mailbox,
                uid,
                body,
            });
        });
        Ok(())
    }

    /// Marks a message read or unread in the background. The result arrives as [`Update::Seen`] or
    /// [`Update::SeenFailed`].
    ///
    /// # Errors
    ///
    /// As [`Mailroom::open`].
    pub fn set_seen(
        &self,
        store: &Store,
        account: &str,
        mailbox: &str,
        uid: u32,
        seen: bool,
    ) -> Result<()> {
        let (server, credentials, uid_validity) = self.target(store, account, mailbox)?;
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        let jobs = self.jobs.clone();
        self.handle.spawn(async move {
            let result = set_seen(&server, &credentials, &mailbox, uid_validity, uid, seen)
                .await
                .map_err(|e| e.to_string());
            let _ignored = jobs.send(Job::Seen {
                account,
                mailbox,
                uid,
                seen,
                result,
            });
        });
        Ok(())
    }

    /// Lists and syncs `account`'s mailboxes other than the inbox now, rather than at the next
    /// interval. Does nothing for an account that isn't watched.
    pub fn refresh(&self, account: &str) {
        if let Some(w) = self.watchers.get(account) {
            w.nudge.notify_one();
        }
    }

    /// Sends `message` from `account` in the background, then files it in the account's Sent mailbox.
    /// The result arrives as [`Update::Sent`], and a failure to file it as [`Update::CopyFailed`].
    ///
    /// # Errors
    ///
    /// Returns an error, and sends nothing, when the account isn't watched.
    pub fn send(&self, account: &str, message: Outgoing) -> Result<()> {
        let watched = self.watched(account)?;
        let (smtp, smtp_credentials) = (watched.smtp.clone(), watched.smtp_credentials.clone());
        let (imap, credentials) = (watched.server.clone(), watched.credentials.clone());
        let nudge = Arc::clone(&watched.nudge);
        let account = account.to_owned();
        let jobs = self.jobs.clone();
        self.handle.spawn(async move {
            let sent = match smtp::send(&smtp, &smtp_credentials, &message).await {
                Ok(sent) => sent,
                Err(e) => {
                    let _ignored = jobs.send(Job::Sent {
                        account,
                        result: Err(e.to_string()),
                    });
                    return;
                }
            };
            let _ignored = jobs.send(Job::Sent {
                account: account.clone(),
                result: Ok(sent.message_id.clone()),
            });
            match save_sent_copy(&imap, &credentials, &sent.message_id, &sent.copy).await {
                // Fetch it from Sent now, so it's listed there right away.
                Ok(()) => nudge.notify_one(),
                Err(e) => {
                    let _ignored = jobs.send(Job::CopyFailed {
                        account,
                        error: e.to_string(),
                    });
                }
            }
        });
        Ok(())
    }

    fn watched(&self, account: &str) -> Result<&Watched> {
        self.watchers
            .get(account)
            .ok_or_else(|| Error::Invalid(format!("{account} isn't connected")))
    }

    /// Where a message lives: its account's server and credentials, and the mailbox's UIDVALIDITY.
    fn target(
        &self,
        store: &Store,
        account: &str,
        mailbox: &str,
    ) -> Result<(Server, Credentials, u32)> {
        let watched = self.watched(account)?;
        let state = store
            .mailbox_state(account, mailbox)?
            .ok_or_else(|| Error::Invalid(format!("{mailbox} hasn't synced yet")))?;
        Ok((
            watched.server.clone(),
            watched.credentials.clone(),
            state.uid_validity,
        ))
    }

    /// The background runtime, for other mail work such as checking a login.
    #[must_use]
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.handle.clone()
    }
}

impl Drop for Mailroom {
    fn drop(&mut self) {
        // Don't wait for connections to wind down: the app may be quitting.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Saves what a job brought, and says what the UI should show.
fn save(store: &mut Store, job: Job) -> Vec<Update> {
    match job {
        Job::Watched(Event::Status { account, status }) => vec![Update::Status { account, status }],
        Job::Watched(Event::Mailboxes { account, mailboxes }) => {
            // A refresher stopped as its account was removed can still have a listing in flight.
            match store.account(&account).and_then(|a| {
                a.map(|_| store.save_mailbox_list(&account, &mailboxes))
                    .transpose()
            }) {
                Ok(Some(())) => vec![Update::Mailboxes { account }],
                Ok(None) => Vec::new(),
                Err(e) => vec![Update::SaveFailed {
                    account,
                    error: e.to_string(),
                }],
            }
        }
        Job::Watched(Event::Messages {
            account,
            mailbox,
            role,
            uid_validity,
            messages,
        }) => save_messages(store, account, mailbox, role, uid_validity, &messages)
            .into_iter()
            .collect(),
        Job::Opened {
            account,
            mailbox,
            uid,
            body,
        } => {
            let mut updates = Vec::new();
            if body.is_ok()
                && let Err(e) = store.set_seen(&account, &mailbox, uid, true)
            {
                updates.push(Update::SaveFailed {
                    account: account.clone(),
                    error: e.to_string(),
                });
            }
            updates.push(Update::Opened {
                account,
                mailbox,
                uid,
                body,
            });
            updates
        }
        Job::Seen {
            account,
            mailbox,
            uid,
            seen,
            result,
        } => {
            let result = result.and_then(|()| {
                store
                    .set_seen(&account, &mailbox, uid, seen)
                    .map_err(|e| e.to_string())
            });
            vec![match result {
                Ok(_) => Update::Seen {
                    account,
                    mailbox,
                    uid,
                    seen,
                },
                Err(error) => Update::SeenFailed { account, error },
            }]
        }
        Job::Sent { account, result } => vec![Update::Sent { account, result }],
        Job::CopyFailed { account, error } => vec![Update::CopyFailed { account, error }],
    }
}

fn save_messages(
    store: &mut Store,
    account: String,
    mailbox: String,
    role: Role,
    uid_validity: u32,
    messages: &[simplymail_core::message::Envelope],
) -> Option<Update> {
    let result = (|| -> Result<Option<bool>> {
        // A watcher stopped as its account was removed can still have mail in flight.
        if store.account(&account)?.is_none() {
            return Ok(None);
        }
        let first_sync = store
            .mailbox_state(&account, &mailbox)?
            .is_none_or(|s| s.uid_validity != uid_validity);
        store.save_messages(&account, &mailbox, role, uid_validity, messages)?;
        Ok(Some(first_sync))
    })();
    match result {
        Ok(None) => None,
        Ok(Some(first_sync)) => Some(Update::Mail {
            account,
            mailbox,
            role,
            count: messages.len(),
            first_sync,
        }),
        Err(e) => Some(Update::SaveFailed {
            account,
            error: e.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use simplymail_core::account::{Auth, Security, Server};
    use simplymail_core::message::{Address, Envelope};

    use super::*;

    fn account() -> Account {
        let server = Server {
            host: "localhost".into(),
            port: 1,
            security: Security::None,
            username: "me".into(),
        };
        Account {
            id: "me@example.com".into(),
            display_name: None,
            label: None,
            imap: server.clone(),
            smtp: server,
            auth: Auth::Password,
        }
    }

    fn messages(uid_validity: u32, uids: &[u32]) -> Job {
        Job::Watched(Event::Messages {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            role: Role::Inbox,
            uid_validity,
            messages: uids
                .iter()
                .map(|&uid| Envelope {
                    uid,
                    message_id: None,
                    from: Address::default(),
                    to: Vec::new(),
                    subject: None,
                    date: None,
                    seen: false,
                })
                .collect(),
        })
    }

    #[test]
    fn the_first_fetch_is_a_sync_and_later_ones_are_new_mail() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        let first = save(&mut store, messages(1, &[1, 2])).remove(0);
        assert!(
            matches!(
                first,
                Update::Mail {
                    count: 2,
                    first_sync: true,
                    ..
                }
            ),
            "got {first:?}"
        );
        let next = save(&mut store, messages(1, &[3])).remove(0);
        assert!(
            matches!(
                next,
                Update::Mail {
                    count: 1,
                    first_sync: false,
                    ..
                }
            ),
            "got {next:?}"
        );
        assert_eq!(store.recent_messages(Role::Inbox, 10).unwrap().len(), 3);
    }

    #[test]
    fn mail_for_a_removed_account_is_dropped() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(save(&mut store, messages(1, &[1])), vec![]);
    }

    #[test]
    fn an_opened_message_is_read_but_a_failed_open_changes_nothing() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1]));
        let opened = |body| Job::Opened {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid: 1,
            body,
        };
        let seen = |store: &Store| {
            store.recent_messages(Role::Inbox, 1).unwrap()[0]
                .envelope
                .seen
        };

        save(&mut store, opened(Err("offline".into())));
        assert!(!seen(&store), "still unread");
        let updates = save(&mut store, opened(Ok(Body::default())));
        assert!(
            matches!(updates.as_slice(), [Update::Opened { body: Ok(_), .. }]),
            "got {updates:?}"
        );
        assert!(seen(&store), "read once opened");
    }

    #[test]
    fn marking_unread_only_sticks_when_the_server_agreed() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1]));
        store.set_seen("me@example.com", "INBOX", 1, true).unwrap();
        let job = |result| Job::Seen {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid: 1,
            seen: false,
            result,
        };
        let seen = |store: &Store| {
            store.recent_messages(Role::Inbox, 1).unwrap()[0]
                .envelope
                .seen
        };

        let failed = save(&mut store, job(Err("offline".into())));
        assert!(matches!(failed.as_slice(), [Update::SeenFailed { .. }]));
        assert!(seen(&store));
        let done = save(&mut store, job(Ok(())));
        assert!(matches!(
            done.as_slice(),
            [Update::Seen { seen: false, .. }]
        ));
        assert!(!seen(&store));
    }
}
