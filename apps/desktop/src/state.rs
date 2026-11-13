//! The window's state, and everything that changes it.

use std::collections::{HashMap, HashSet};

use jiff::Zoned;
use rune::{AppHandle, Tx};
use rune_kit::shell::PaneState;
use simplymail_app::{
    Draft, Login, Mailroom, Passwords, Settings, Theme, Update, check_logins,
    finish_google_sign_in, password_key, remove_account, save_account, smtp_password_key,
    start_google_sign_in,
};
use simplymail_core::account::Account;
use simplymail_core::message::{Body, Mailbox, Role};
use simplymail_core::secrets::{SecretStore, SecretString};
use simplymail_mail::imap::{Status, WatchConfig};
use simplymail_secrets::Backend;
use simplymail_store::{Store, StoredMessage};

use crate::compose::ComposeForm;
use crate::form::{AccountForm, GoogleSignIn, Step};

/// How many messages the list shows.
const LIST_LIMIT: usize = 300;

/// What the app runs on once started.
pub(crate) struct Services {
    pub(crate) store: Store,
    pub(crate) secrets: Box<dyn SecretStore>,
    /// Passwords come from a command and are never written.
    pub(crate) read_only: bool,
    pub(crate) mailroom: Mailroom,
}

/// A message open for reading.
pub(crate) struct Reader {
    /// The message as listed. Its `seen` follows the list.
    pub(crate) message: StoredMessage,
    /// The downloaded body, or why it couldn't be downloaded. `None` while downloading.
    pub(crate) body: Option<Result<Body, String>>,
    /// The body's HTML as Markdown, to show, converted once when it arrives.
    pub(crate) markdown: Option<String>,
}

impl Reader {
    fn is(&self, account: &str, mailbox: &str, uid: u32) -> bool {
        is_message(&self.message, account, mailbox, uid)
    }
}

fn is_message(m: &StoredMessage, account: &str, mailbox: &str, uid: u32) -> bool {
    m.account == account && m.mailbox == mailbox && m.envelope.uid == uid
}

/// What the message list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Showing {
    /// Every account's mailbox with this role: all inboxes, or all Sent mailboxes.
    All(Role),
    /// One mailbox of one account.
    Mailbox { account: String, name: String },
}

impl Showing {
    /// An account's inbox.
    pub(crate) fn inbox(account: &str) -> Self {
        Self::Mailbox {
            account: account.to_owned(),
            name: INBOX.to_owned(),
        }
    }

    /// The account whose mail is shown, if just one's is.
    pub(crate) fn account(&self) -> Option<&str> {
        match self {
            Self::All(_) => None,
            Self::Mailbox { account, .. } => Some(account),
        }
    }

    pub(crate) fn is(&self, account: &str, name: &str) -> bool {
        matches!(self, Self::Mailbox { account: a, name: n } if a == account && n == name)
    }
}

/// The inbox's name on every server.
pub(crate) const INBOX: &str = "INBOX";

/// How long a notice shows before it closes on its own.
const NOTICE_SECS: u64 = 6;

pub(crate) struct Mail {
    handle: Option<AppHandle<Self>>,
    /// The services, or why they couldn't start.
    pub(crate) services: Result<Services, String>,
    pub(crate) accounts: Vec<Account>,
    /// Each account's connection status, by account id, or why its watcher couldn't start.
    pub(crate) statuses: HashMap<String, Result<Status, String>>,
    /// What the list shows, and its mail.
    pub(crate) showing: Showing,
    pub(crate) messages: Vec<StoredMessage>,
    /// Each account's mailboxes as the server last listed them, by account id.
    pub(crate) mailboxes: HashMap<String, Vec<Mailbox>>,
    /// Unread inbox messages, by account id.
    unread: HashMap<String, usize>,
    /// The latest thing worth telling the user, such as new mail arriving.
    /// It closes on its own after [`NOTICE_SECS`], or when its close button is pressed.
    pub(crate) notice: Option<String>,
    /// Counts notices, so a timer only closes the one it was started for.
    notice_id: u64,
    pub(crate) form: AccountForm,
    /// The account whose Remove button was pressed once and awaits a second press.
    pub(crate) confirm_remove: Option<String>,
    pub(crate) sidebar: PaneState,
    /// The message open in the main pane, if any.
    pub(crate) reader: Option<Reader>,
    /// Accounts whose mailboxes are hidden in the sidebar.
    pub(crate) collapsed: HashSet<String>,
    /// The toolbar's ⋯ menu is open.
    pub(crate) actions_open: bool,
    /// The message being written, shown over the reader and the list.
    pub(crate) compose: ComposeForm,
    /// The app's preferences, as last saved.
    pub(crate) settings: Settings,
    /// The settings page is shown over everything else in the main pane.
    pub(crate) settings_open: bool,
}

impl Mail {
    /// The state before [`Mail::start`] runs.
    pub(crate) fn not_started() -> Self {
        Self::failed(None, "starting…".into())
    }

    fn failed(handle: Option<AppHandle<Self>>, error: String) -> Self {
        Self {
            handle,
            services: Err(error),
            accounts: Vec::new(),
            statuses: HashMap::new(),
            messages: Vec::new(),
            showing: Showing::All(Role::Inbox),
            mailboxes: HashMap::new(),
            unread: HashMap::new(),
            notice: None,
            notice_id: 0,
            form: AccountForm::default(),
            confirm_remove: None,
            sidebar: PaneState::new(260.0).range(200.0, 420.0),
            reader: None,
            compose: ComposeForm::default(),
            actions_open: false,
            collapsed: HashSet::new(),
            settings: Settings::default(),
            settings_open: false,
        }
    }

    /// Opens the store and starts watching every account. Failing that, the window says why.
    pub(crate) fn start(handle: AppHandle<Self>) -> Self {
        match Self::try_start(&handle) {
            Ok((services, settings)) => {
                let mut mail = Self::failed(Some(handle), String::new());
                mail.services = Ok(services);
                mail.settings = settings;
                mail.reload();
                let failed = mail
                    .services
                    .as_mut()
                    .map(|s| s.mailroom.watch_all(&s.store, s.secrets.as_ref()));
                match failed {
                    Ok(Ok(failed)) => {
                        for (account, error) in failed {
                            mail.statuses.insert(account, Err(error.to_string()));
                        }
                    }
                    Ok(Err(e)) => mail.notify(e.to_string()),
                    Err(_) => {}
                }
                mail.form.open = mail.accounts.is_empty();
                mail
            }
            Err(e) => Self::failed(Some(handle), e),
        }
    }

    /// Shows `notice` in place of the last one, and closes it after [`NOTICE_SECS`] unless a newer
    /// one replaced it by then.
    fn notify(&mut self, notice: String) {
        self.notice = Some(notice);
        self.notice_id += 1;
        let Some(handle) = &self.handle else { return };
        let (handle, id) = (handle.clone(), self.notice_id);
        // Without a thread the notice stays until it's closed by hand or replaced.
        drop(rune::task::spawn_after(
            "notice-timeout",
            std::time::Duration::from_secs(NOTICE_SECS),
            move || {
                handle.update(move |s: &mut Self, _| {
                    if s.notice_id == id {
                        s.notice = None;
                    }
                });
            },
        ));
    }

    /// Closes the notice now.
    pub(crate) fn dismiss_notice(&mut self) {
        self.notice = None;
    }

    fn try_start(handle: &AppHandle<Self>) -> Result<(Services, Settings), String> {
        let dir = simplymail_app::data_dir(None).map_err(|e| e.to_string())?;
        let database = simplymail_app::database_path(&dir);
        let store = Store::open(&database)
            .map_err(|e| format!("couldn't open {}: {e}", database.display()))?;
        let backend = simplymail_app::secrets_backend(None);
        let secrets = simplymail_secrets::open(&backend).map_err(|e| {
            format!("{e}. Without a keyring, set SIMPLYMAIL_PASSWORD_COMMAND to a command that prints the password.")
        })?;
        let settings = Settings::load(&store).map_err(|e| e.to_string())?;
        let config = WatchConfig {
            poll_interval: settings.poll_interval,
            ..WatchConfig::default()
        };
        let notify = handle.clone();
        let mailroom = Mailroom::start(&database, config, move |update| {
            notify.update(move |s: &mut Self, _| s.apply(update));
        })
        .map_err(|e| e.to_string())?;
        let services = Services {
            store,
            secrets,
            read_only: matches!(backend, Backend::Command { .. }),
            mailroom,
        };
        Ok((services, settings))
    }

    /// Rereads accounts and mail from the store.
    fn reload(&mut self) {
        let Ok(services) = &self.services else { return };
        let messages = match &self.showing {
            Showing::All(role) => services.store.recent_messages(*role, LIST_LIMIT),
            Showing::Mailbox { account, name } => {
                services.store.mailbox_messages(account, name, LIST_LIMIT)
            }
        };
        let mailboxes = services.store.accounts().and_then(|accounts| {
            accounts
                .into_iter()
                .map(|a| Ok((services.store.mailboxes(&a.id)?, a)))
                .collect::<Result<Vec<_>, _>>()
        });
        match (mailboxes, messages, services.store.unread_counts()) {
            (Ok(accounts), Ok(messages), Ok(unread)) => {
                self.mailboxes = accounts
                    .iter()
                    .map(|(mailboxes, a)| (a.id.clone(), mailboxes.clone()))
                    .collect();
                self.accounts = accounts.into_iter().map(|(_, a)| a).collect();
                self.messages = messages;
                self.unread = unread;
                if let Some(reader) = &mut self.reader {
                    let m = &reader.message;
                    let (account, mailbox, uid) = (&m.account, &m.mailbox, m.envelope.uid);
                    if let Some(listed) = self
                        .messages
                        .iter()
                        .find(|l| is_message(l, account, mailbox, uid))
                    {
                        reader.message = listed.clone();
                    }
                }
            }
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                self.notify(format!("couldn't read mail: {e}"));
            }
        }
    }

    fn apply(&mut self, update: Update) {
        match update {
            Update::Status { account, status } => {
                self.statuses.insert(account, Ok(status));
            }
            Update::Mail {
                account,
                role: Role::Inbox,
                count,
                first_sync,
                ..
            } => {
                let label = self.label(&account).to_owned();
                self.notify(simplymail_app::mail_notice(
                    &label,
                    count,
                    first_sync,
                    &Zoned::now(),
                ));
                self.reload();
            }
            Update::CopyFailed { account, error } => {
                let label = self.label(&account).to_owned();
                self.notify(format!(
                    "{label}: sent, but couldn't file it in Sent: {error}"
                ));
            }
            Update::SaveFailed { account, error } => {
                self.notify(format!("{account}: couldn't save new mail: {error}"));
            }
            Update::Opened {
                account,
                mailbox,
                uid,
                body,
            } => {
                if let Some(reader) = &mut self.reader
                    && reader.is(&account, &mailbox, uid)
                {
                    reader.markdown = body
                        .as_ref()
                        .ok()
                        .and_then(|b| b.html.as_deref())
                        .map(simplymail_html::to_markdown)
                        .filter(|md| !md.trim().is_empty());
                    reader.body = Some(body);
                }
                // Read now, or, if the download failed, unread again as the store still says.
                self.reload();
            }
            // Sent mail is shown as it's listed, without a notice.
            Update::Mail { .. } | Update::Seen { .. } | Update::Mailboxes { .. } => self.reload(),
            Update::SeenFailed { account, error } => {
                self.notify(format!("{account}: {error}"));
                self.reload();
            }
            Update::Sent { account, result } => self.sent(&account, result),
        }
    }

    /// Starts a new message from the open message's account, else the selected one, else the first.
    pub(crate) fn compose(&mut self) {
        let account = self
            .reader
            .as_ref()
            .map(|r| r.message.account.clone())
            .or_else(|| self.showing.account().map(str::to_owned))
            .or_else(|| self.accounts.first().map(|a| a.id.clone()));
        if let Some(account) = account {
            self.form.open = false;
            self.settings_open = false;
            self.compose = ComposeForm::new(Draft::new(&account));
        }
    }

    /// Starts a reply to the open message, once it's downloaded.
    pub(crate) fn reply(&mut self, all: bool) {
        let Some(Reader {
            message,
            body: Some(Ok(body)),
            ..
        }) = &self.reader
        else {
            return;
        };
        let Some(account) = self.accounts.iter().find(|a| a.id == message.account) else {
            return;
        };
        let draft = Draft::reply(
            account,
            &message.envelope,
            body,
            all,
            &jiff::tz::TimeZone::system(),
        );
        self.compose = ComposeForm::new(draft);
    }

    /// Hands the message being written to the mailroom.
    pub(crate) fn send(&mut self) {
        let (Ok(services), c) = (&self.services, &mut self.compose) else {
            return;
        };
        if !c.open || c.sending {
            return;
        }
        let Some(account) = self.accounts.iter().find(|a| a.id == c.account) else {
            c.error = Some(format!("{} is no longer set up", c.account));
            return;
        };
        let result = c
            .draft()
            .outgoing(account)
            .and_then(|message| services.mailroom.send(&account.id, message));
        match result {
            Ok(()) => {
                c.sending = true;
                c.error = None;
            }
            Err(e) => c.error = Some(e.to_string()),
        }
    }

    /// The mailroom finished sending. Only one message is written at a time, so it's the one shown,
    /// if it's still shown.
    fn sent(&mut self, account: &str, result: Result<String, String>) {
        let label = self.label(account).to_owned();
        match result {
            Ok(_) => {
                if self.compose.is_sending() {
                    self.compose = ComposeForm::default();
                }
                self.notify(format!("Sent from {label}"));
            }
            Err(error) if self.compose.is_sending() => {
                self.compose.sending = false;
                self.compose.error = Some(format!("Couldn't send: {error}"));
            }
            Err(error) => self.notify(format!("{label}: couldn't send: {error}")),
        }
    }

    pub(crate) fn discard(&mut self) {
        if self.compose.is_sending() {
            self.notify("Still sending; you'll be told how it went".into());
        }
        self.compose = ComposeForm::default();
    }

    /// Opens a message in the main pane, downloading it and marking it read.
    pub(crate) fn open_message(&mut self, account: &str, mailbox: &str, uid: u32) {
        let Some(message) = self
            .messages
            .iter_mut()
            .find(|m| is_message(m, account, mailbox, uid))
        else {
            return;
        };
        let listed = message.clone();
        let mut reader = Reader {
            message: listed,
            body: None,
            markdown: None,
        };
        if let Ok(services) = &self.services {
            if let Err(e) = services
                .mailroom
                .open(&services.store, account, mailbox, uid)
            {
                reader.body = Some(Err(e.to_string()));
            } else {
                // Shown read right away; the store catches up once the server agrees.
                message.envelope.seen = true;
            }
        }
        self.reader = Some(reader);
    }

    /// Opens a link from a message in the browser.
    pub(crate) fn open_link(&mut self, url: &str, tx: &mut Tx) {
        if let Err(e) = tx.open_url(url) {
            self.notify(format!("couldn't open {url}: {e}"));
        }
    }

    pub(crate) fn close_reader(&mut self) {
        self.reader = None;
    }

    /// Flips the open message between read and unread.
    pub(crate) fn toggle_seen(&mut self) {
        let (Ok(services), Some(reader)) = (&self.services, &mut self.reader) else {
            return;
        };
        let m = &mut reader.message;
        let seen = !m.envelope.seen;
        match services.mailroom.set_seen(
            &services.store,
            &m.account,
            &m.mailbox,
            m.envelope.uid,
            seen,
        ) {
            Ok(()) => {
                m.envelope.seen = seen;
                let (account, mailbox, uid) =
                    (m.account.clone(), m.mailbox.clone(), m.envelope.uid);
                if let Some(listed) = self
                    .messages
                    .iter_mut()
                    .find(|l| is_message(l, &account, &mailbox, uid))
                {
                    listed.envelope.seen = seen;
                }
            }
            Err(e) => self.notify(e.to_string()),
        }
    }

    /// Downloads the open message again, after a failure.
    pub(crate) fn retry_open(&mut self) {
        if let Some(r) = self.reader.take() {
            let m = r.message;
            self.open_message(&m.account, &m.mailbox, m.envelope.uid);
        }
    }

    /// The messages to list.
    pub(crate) fn shown(&self) -> impl Iterator<Item = &StoredMessage> {
        self.messages.iter()
    }

    /// Whether the list shows sent mail, so it says whom each message went to.
    pub(crate) fn showing_sent(&self) -> bool {
        match &self.showing {
            Showing::All(role) => *role == Role::Sent,
            Showing::Mailbox { account, name } => self
                .mailboxes
                .get(account)
                .and_then(|list| list.iter().find(|m| m.name == *name))
                .is_some_and(|m| m.role == Role::Sent),
        }
    }

    /// How many inbox messages are unread, in one account or all of them.
    pub(crate) fn unread(&self, account: Option<&str>) -> usize {
        account.map_or_else(
            || self.unread.values().sum(),
            |id| self.unread.get(id).copied().unwrap_or_default(),
        )
    }

    /// Shows or hides an account's mailboxes in the sidebar.
    pub(crate) fn toggle_mailboxes(&mut self, account: &str) {
        if !self.collapsed.remove(account) {
            self.collapsed.insert(account.to_owned());
        }
    }

    /// Lists `showing`. A mailbox other than an inbox is also refreshed from the server, as those
    /// are otherwise only synced every few minutes.
    pub(crate) fn show(&mut self, showing: Showing) {
        self.form.open = false;
        self.settings_open = false;
        self.reader = None;
        self.confirm_remove = None;
        if let (Ok(services), Showing::Mailbox { account, name }) = (&self.services, &showing)
            && name != INBOX
        {
            services.mailroom.refresh(account);
        }
        if self.showing != showing {
            self.showing = showing;
            self.reload();
        }
    }

    /// The name to show for an account.
    pub(crate) fn label<'a>(&'a self, id: &'a str) -> &'a str {
        self.accounts
            .iter()
            .find(|a| a.id == id)
            .map_or(id, Account::label)
    }

    pub(crate) fn open_form(&mut self) {
        self.reader = None;
        self.settings_open = false;
        self.form = AccountForm {
            open: true,
            ..AccountForm::default()
        };
    }

    /// Moves the add-account form to `step`, starting it afresh.
    pub(crate) fn form_step(&mut self, step: Step) {
        self.form = AccountForm {
            open: true,
            step,
            ..AccountForm::default()
        };
        self.form.email.focused = step != Step::Choose;
    }

    /// Submits the form's current screen: signs in with Google, or checks the typed-in login.
    pub(crate) fn submit_form(&mut self, _: &mut Tx) {
        match self.form.step {
            Step::Choose => {}
            Step::Google => self.sign_in_with_google(),
            Step::Servers => self.check_servers(),
        }
    }

    /// Checks the typed-in login in the background, then saves the account if it works.
    fn check_servers(&mut self) {
        let (Ok(services), Some(handle)) = (&self.services, &self.handle) else {
            return;
        };
        if self.form.checking {
            return;
        }
        let account = match self
            .form
            .new_account()
            .and_then(|a| a.build().map_err(|e| e.to_string()))
        {
            Ok(account) => account,
            Err(e) => {
                self.form.error = Some(e);
                return;
            }
        };
        let password = if services.read_only {
            match services.secrets.get(&password_key(&account.id)) {
                Ok(Some(password)) => password,
                Ok(None) => {
                    self.form.error = Some(format!(
                        "the password command printed nothing for {}",
                        account.id
                    ));
                    return;
                }
                Err(e) => {
                    self.form.error = Some(e.to_string());
                    return;
                }
            }
        } else if self.form.password.text.is_empty() {
            self.form.error = Some("the password is missing".into());
            return;
        } else {
            SecretString::from(self.form.password.text.clone())
        };
        let smtp = if services.read_only {
            // A command may fail for a secret it doesn't have; then the IMAP one is used.
            services
                .secrets
                .get(&smtp_password_key(&account.id))
                .ok()
                .flatten()
        } else {
            (!self.form.smtp_password.text.is_empty())
                .then(|| SecretString::from(self.form.smtp_password.text.clone()))
        };
        let login = Login::from(Passwords {
            imap: password,
            smtp,
        });

        self.form.checking = true;
        self.form.error = None;
        let handle = handle.clone();
        services.mailroom.handle().spawn(async move {
            let result = check_logins(&account, &login)
                .await
                .map_err(|e| e.to_string());
            handle.update(move |s: &mut Self, _| s.login_checked(&account, &login, result));
        });
    }

    /// Signs in with Google in the browser, then checks the logins and saves the Gmail account.
    /// The email field, if filled, is suggested to Google; the name field names the account.
    fn sign_in_with_google(&mut self) {
        let (Ok(services), Some(handle)) = (&self.services, &self.handle) else {
            return;
        };
        if self.form.checking {
            return;
        }
        if services.read_only {
            self.form.error = Some(
                "Signing in with Google needs the OS keyring to keep the sign-in, but passwords \
                 come from SIMPLYMAIL_PASSWORD_COMMAND."
                    .into(),
            );
            return;
        }
        let optional = |t: &str| Some(t.trim().to_owned()).filter(|t| !t.is_empty());
        let hint = optional(&self.form.email.text);
        let name = optional(&self.form.name.text);
        let handle = handle.clone();
        let task = services.mailroom.handle().spawn(async move {
            let sign_in = match start_google_sign_in(hint.as_deref()).await {
                Ok(sign_in) => sign_in,
                Err(e) => {
                    let error = e.to_string();
                    handle.update(move |s: &mut Self, _| s.signed_in(Err(error)));
                    return;
                }
            };
            let url = sign_in.url().to_owned();
            handle.update(move |s: &mut Self, tx| s.open_sign_in(url, tx));
            let result = finish_google_sign_in(sign_in, name)
                .await
                .map_err(|e| e.to_string());
            handle.update(move |s: &mut Self, _| s.signed_in(result));
        });
        self.form.checking = true;
        self.form.error = None;
        self.form.google = Some(GoogleSignIn {
            task: task.abort_handle(),
            url: None,
        });
    }

    fn open_sign_in(&mut self, url: String, tx: &mut Tx) {
        let Some(google) = &mut self.form.google else {
            return;
        };
        if let Err(e) = tx.open_url(&url) {
            self.form.error = Some(format!("Couldn't open the browser: {e}"));
        }
        google.url = Some(url);
    }

    fn signed_in(&mut self, result: Result<(Account, Login), String>) {
        // Cancelled meanwhile.
        if self.form.google.take().is_none() {
            return;
        }
        match result {
            Ok((account, login)) => self.login_checked(&account, &login, Ok(())),
            Err(e) => {
                self.form.checking = false;
                self.form.error = Some(e);
            }
        }
    }

    /// Stops a Google sign-in that's waiting for the browser.
    pub(crate) fn cancel_sign_in(&mut self) {
        self.form.google = None;
        self.form.checking = false;
    }

    fn login_checked(&mut self, account: &Account, login: &Login, result: Result<(), String>) {
        self.form.checking = false;
        if let Err(e) = result {
            self.form.error = Some(e);
            return;
        }
        let Ok(services) = &mut self.services else {
            return;
        };
        let (id, label) = (account.id.clone(), account.label().to_owned());
        let saved = save_account(&services.store, services.secrets.as_ref(), account, login)
            .and_then(|()| {
                services
                    .mailroom
                    .watch(account, &services.store, services.secrets.as_ref())
            });
        if let Err(e) = saved {
            self.form.error = Some(e.to_string());
            return;
        }
        self.statuses.insert(id, Ok(Status::Connecting));
        self.notify(format!("Added {label}"));
        self.form = AccountForm::default();
        self.reload();
    }

    /// Shows the settings page. A message being written stays, and shows again once it closes.
    pub(crate) fn open_settings(&mut self) {
        self.form.open = false;
        self.confirm_remove = None;
        self.settings_open = true;
    }

    pub(crate) fn close_settings(&mut self) {
        self.settings_open = false;
        self.confirm_remove = None;
    }

    /// Whether to draw the window dark, given whether the system is.
    pub(crate) const fn is_dark(&self, system_dark: bool) -> bool {
        self.settings.theme.is_dark(system_dark)
    }

    pub(crate) fn set_theme(&mut self, theme: Theme) {
        if self.settings.theme != theme {
            self.settings.theme = theme;
            self.save_settings();
        }
    }

    /// Checks mail every `every` on servers that can't push it, reconnecting every account.
    pub(crate) fn set_poll_interval(&mut self, every: std::time::Duration) {
        let Ok(services) = &mut self.services else {
            return;
        };
        if services.mailroom.poll_interval() == every {
            return;
        }
        let restarted =
            services
                .mailroom
                .set_poll_interval(every, &services.store, services.secrets.as_ref());
        self.settings.poll_interval = every;
        match restarted {
            Ok(failed) => {
                for account in &self.accounts {
                    self.statuses
                        .insert(account.id.clone(), Ok(Status::Connecting));
                }
                for (account, error) in failed {
                    self.statuses.insert(account, Err(error.to_string()));
                }
            }
            Err(e) => self.notify(e.to_string()),
        }
        self.save_settings();
    }

    fn save_settings(&mut self) {
        let Ok(services) = &self.services else { return };
        if let Err(e) = self.settings.save(&services.store) {
            self.notify(format!("couldn't save settings: {e}"));
        }
    }

    /// Removes an account on the second press of its Remove button.
    pub(crate) fn remove(&mut self, id: &str) {
        if self.confirm_remove.as_deref() != Some(id) {
            self.confirm_remove = Some(id.to_owned());
            return;
        }
        self.confirm_remove = None;
        let Ok(services) = &mut self.services else {
            return;
        };
        services.mailroom.unwatch(id);
        let notice = match remove_account(&services.store, services.secrets.as_ref(), id) {
            Ok(_) => format!("Removed {id}"),
            Err(e) => format!("Removed {id}, but: {e}"),
        };
        self.notify(notice);
        self.statuses.remove(id);
        if self.showing.account() == Some(id) {
            self.showing = Showing::All(Role::Inbox);
        }
        if self
            .reader
            .as_ref()
            .is_some_and(|r| r.message.account == id)
        {
            self.reader = None;
        }
        self.reload();
    }
}
