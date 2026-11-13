//! The local, on-device cache of accounts, mailboxes and messages, in SQLite.
//!
//! Account passwords never land here; they live in a [`simplymail_core::secrets::SecretStore`].

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, params};
use simplymail_core::account::{Account, Auth, Security, Server, Signature};
pub use simplymail_core::message::MailboxState;
use simplymail_core::message::{Address, Envelope, InlineImage, Mailbox, Role};

/// Why a [`Store`] call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A row holds a value this version can't read, for example from a newer SimplyMail.
    #[error("corrupt row: {0}")]
    Corrupt(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A message as listed across accounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub account: String,
    pub mailbox: String,
    pub envelope: Envelope,
}

const SCHEMA: &str = "
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS accounts (
    id            TEXT PRIMARY KEY,
    display_name  TEXT,
    imap_host     TEXT NOT NULL,
    imap_port     INTEGER NOT NULL,
    imap_security TEXT NOT NULL,
    imap_username TEXT NOT NULL,
    smtp_host     TEXT NOT NULL,
    smtp_port     INTEGER NOT NULL,
    smtp_security TEXT NOT NULL,
    smtp_username TEXT NOT NULL,
    -- How the account logs in: 'password' or 'google'.
    auth          TEXT NOT NULL DEFAULT 'password',
    -- What the UI calls the account, if not its display name or address.
    label         TEXT
) STRICT;

CREATE TABLE IF NOT EXISTS mailboxes (
    account      TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    uid_validity INTEGER NOT NULL,
    role         TEXT NOT NULL DEFAULT 'inbox',
    PRIMARY KEY (account, name)
) STRICT;

CREATE TABLE IF NOT EXISTS messages (
    account    TEXT NOT NULL,
    mailbox    TEXT NOT NULL,
    uid        INTEGER NOT NULL,
    message_id TEXT,
    from_name  TEXT,
    from_email TEXT,
    subject    TEXT,
    date       INTEGER,
    seen       INTEGER NOT NULL,
    -- The To header: each address as name, U+001F, email; addresses separated by U+001E.
    to_list    TEXT,
    PRIMARY KEY (account, mailbox, uid),
    FOREIGN KEY (account, mailbox) REFERENCES mailboxes(account, name) ON DELETE CASCADE
) STRICT;

CREATE INDEX IF NOT EXISTS messages_by_date ON messages(date DESC);

-- Every mailbox the server last listed for an account, synced or not.
CREATE TABLE IF NOT EXISTS listed_mailboxes (
    account   TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name      TEXT NOT NULL,
    delimiter TEXT,
    role      TEXT NOT NULL,
    PRIMARY KEY (account, name)
) STRICT;

-- The app's preferences, such as its theme, by name.
CREATE TABLE IF NOT EXISTS settings (
    name  TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

-- What's added below the mail each account sends.
CREATE TABLE IF NOT EXISTS signatures (
    account TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    html    TEXT NOT NULL,
    -- 1 when written as HTML source, 0 in the visual editor.
    source  INTEGER NOT NULL
) STRICT;

-- The images a signature shows, in order.
CREATE TABLE IF NOT EXISTS signature_images (
    account  TEXT NOT NULL REFERENCES signatures(account) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    cid      TEXT NOT NULL,
    name     TEXT NOT NULL,
    mime     TEXT NOT NULL,
    data     BLOB NOT NULL,
    PRIMARY KEY (account, cid)
) STRICT;
";

/// The schema version [`SCHEMA`] creates, kept in `PRAGMA user_version`.
const VERSION: i64 = 3;

/// Columns added since version 0, for databases created before them. Tables added since are
/// created by [`SCHEMA`] itself.
const MIGRATIONS: [(&str, &str, &str); 4] = [
    ("mailboxes", "role", "TEXT NOT NULL DEFAULT 'inbox'"),
    ("messages", "to_list", "TEXT"),
    // Version 2.
    ("accounts", "auth", "TEXT NOT NULL DEFAULT 'password'"),
    // Version 3.
    ("accounts", "label", "TEXT"),
];

const MESSAGE_COLUMNS: &str = "m.account, m.mailbox, m.uid, m.message_id, m.from_name, m.from_email, m.subject, m.date, m.seen, m.to_list";

/// The SQLite-backed cache.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens or creates the database at `path`, creating its tables if needed.
    ///
    /// # Errors
    ///
    /// Returns an error when the file can't be opened or the schema can't be applied.
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// An in-memory database, for tests.
    ///
    /// # Errors
    ///
    /// Returns an error when the schema can't be applied.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < VERSION {
            // Tables made by an older version lack the newer columns; fresh ones already have them.
            for (table, column, definition) in MIGRATIONS {
                let present: bool = conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
                    [table, column],
                    |row| row.get(0),
                )?;
                if !present {
                    conn.execute_batch(&format!(
                        "ALTER TABLE {table} ADD COLUMN {column} {definition}"
                    ))?;
                }
            }
            conn.execute_batch(&format!("PRAGMA user_version = {VERSION}"))?;
        }
        Ok(Self { conn })
    }

    /// Adds `account`, or replaces the settings of the account with the same id.
    /// Replacing keeps its cached mail, its signature and its label, which only
    /// [`Store::set_names`] changes, so adding an account again doesn't forget what it was called.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn save_account(&self, account: &Account) -> Result<()> {
        self.conn.execute(
            "INSERT INTO accounts (id, display_name, imap_host, imap_port, imap_security,
                imap_username, smtp_host, smtp_port, smtp_security, smtp_username, auth, label)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
                display_name = excluded.display_name,
                imap_host = excluded.imap_host, imap_port = excluded.imap_port,
                imap_security = excluded.imap_security, imap_username = excluded.imap_username,
                smtp_host = excluded.smtp_host, smtp_port = excluded.smtp_port,
                smtp_security = excluded.smtp_security, smtp_username = excluded.smtp_username,
                auth = excluded.auth",
            params![
                account.id,
                account.display_name,
                account.imap.host,
                account.imap.port,
                account.imap.security.as_str(),
                account.imap.username,
                account.smtp.host,
                account.smtp.port,
                account.smtp.security.as_str(),
                account.smtp.username,
                account.auth.as_str(),
                account.label,
            ],
        )?;
        Ok(())
    }

    /// Sets what the account is called in the UI and the name its mail is sent under. Returns
    /// whether the account exists.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn set_names(
        &self,
        id: &str,
        label: Option<&str>,
        display_name: Option<&str>,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE accounts SET label = ?2, display_name = ?3 WHERE id = ?1",
            params![id, label, display_name],
        )? > 0)
    }

    /// The account's signature, or `None` when it has none.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn signature(&self, account: &str) -> Result<Option<Signature>> {
        let found: Option<(String, bool)> = self
            .conn
            .query_row(
                "SELECT html, source FROM signatures WHERE account = ?1",
                [account],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((html, source)) = found else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare(
            "SELECT cid, name, mime, data FROM signature_images WHERE account = ?1
             ORDER BY position",
        )?;
        let images = stmt
            .query_map([account], |row| {
                Ok(InlineImage {
                    cid: row.get(0)?,
                    name: row.get(1)?,
                    mime: row.get(2)?,
                    data: row.get::<_, Vec<u8>>(3)?.into(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(Signature {
            html,
            source,
            images,
        }))
    }

    /// Replaces the account's signature, or removes it with `None`.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails, such as for an account that doesn't exist. Nothing
    /// is written in that case.
    pub fn save_signature(&mut self, account: &str, signature: Option<&Signature>) -> Result<()> {
        let tx = self.conn.transaction()?;
        // Its images go with it.
        tx.execute("DELETE FROM signatures WHERE account = ?1", [account])?;
        if let Some(signature) = signature {
            tx.execute(
                "INSERT INTO signatures (account, html, source) VALUES (?1, ?2, ?3)",
                params![account, signature.html, signature.source],
            )?;
            let mut insert = tx.prepare(
                "INSERT INTO signature_images (account, position, cid, name, mime, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for (position, image) in signature.images.iter().enumerate() {
                let position = i64::try_from(position).unwrap_or(i64::MAX);
                insert.execute(params![
                    account,
                    position,
                    image.cid,
                    image.name,
                    image.mime,
                    &*image.data
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every account, ordered by id.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails or a row is corrupt.
    pub fn accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self.conn.prepare("SELECT * FROM accounts ORDER BY id")?;
        let rows = stmt.query_map([], |row| Ok(account_from_row(row)))?;
        rows.map(|r| r?).collect()
    }

    /// The account with this id, if there is one.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails or the row is corrupt.
    pub fn account(&self, id: &str) -> Result<Option<Account>> {
        self.conn
            .query_row("SELECT * FROM accounts WHERE id = ?1", [id], |row| {
                Ok(account_from_row(row))
            })
            .optional()?
            .transpose()
    }

    /// Removes the account and all its cached mail. Returns whether it existed.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn remove_account(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM accounts WHERE id = ?1", [id])?
            > 0)
    }

    /// What's cached of `mailbox`, or `None` if it was never synced.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn mailbox_state(&self, account: &str, mailbox: &str) -> Result<Option<MailboxState>> {
        Ok(self
            .conn
            .query_row(
                "SELECT b.uid_validity, MAX(m.uid) FROM mailboxes b
                 LEFT JOIN messages m ON m.account = b.account AND m.mailbox = b.name
                 WHERE b.account = ?1 AND b.name = ?2
                 GROUP BY b.account, b.name",
                [account, mailbox],
                |row| {
                    Ok(MailboxState {
                        uid_validity: row.get(0)?,
                        last_uid: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// The account's mailbox with this role, and what's cached of it, or `None` if none was synced.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn mailbox_for_role(
        &self,
        account: &str,
        role: Role,
    ) -> Result<Option<(String, MailboxState)>> {
        let name: Option<String> = self
            .conn
            .query_row(
                "SELECT name FROM mailboxes WHERE account = ?1 AND role = ?2 LIMIT 1",
                [account, role.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(name) = name else { return Ok(None) };
        Ok(self
            .mailbox_state(account, &name)?
            .map(|state| (name, state)))
    }

    /// What's cached of each of the account's synced mailboxes, by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn mailbox_states(&self, account: &str) -> Result<HashMap<String, MailboxState>> {
        let mut stmt = self.conn.prepare(
            "SELECT b.name, b.uid_validity, MAX(m.uid) FROM mailboxes b
             LEFT JOIN messages m ON m.account = b.account AND m.mailbox = b.name
             WHERE b.account = ?1
             GROUP BY b.account, b.name",
        )?;
        let rows = stmt.query_map([account], |row| {
            Ok((
                row.get::<_, String>(0)?,
                MailboxState {
                    uid_validity: row.get(1)?,
                    last_uid: row.get(2)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replaces the account's list of mailboxes with what the server listed. Cached mail from a
    /// mailbox that's no longer listed (deleted or renamed on the server) is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails. Nothing is written in that case.
    pub fn save_mailbox_list(&mut self, account: &str, mailboxes: &[Mailbox]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM listed_mailboxes WHERE account = ?1", [account])?;
        {
            let mut insert = tx.prepare("INSERT INTO listed_mailboxes VALUES (?1, ?2, ?3, ?4)")?;
            for m in mailboxes {
                insert.execute(params![account, m.name, m.delimiter, m.role.as_str()])?;
            }
        }
        tx.execute(
            "DELETE FROM mailboxes WHERE account = ?1
             AND name NOT IN (SELECT name FROM listed_mailboxes WHERE account = ?1)",
            [account],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The account's mailboxes as last listed: the inbox, then Sent, then the rest by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails or a row is corrupt.
    pub fn mailboxes(&self, account: &str) -> Result<Vec<Mailbox>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, delimiter, role FROM listed_mailboxes WHERE account = ?1
             ORDER BY CASE role WHEN 'inbox' THEN 0 WHEN 'sent' THEN 1 ELSE 2 END, name",
        )?;
        let rows = stmt.query_map([account], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (name, delimiter, role) = row?;
            let role = Role::from_name(&role)
                .ok_or_else(|| Error::Corrupt(format!("unknown mailbox role {role:?}")))?;
            Ok(Mailbox {
                name,
                delimiter,
                role,
            })
        })
        .collect()
    }

    /// Caches `messages` fetched from `mailbox` under `uid_validity`, and files the mailbox under
    /// `role`. For the inbox and Sent roles, another mailbox the account had with that role, such as
    /// a renamed Sent folder, is dropped along with its mail.
    ///
    /// If `uid_validity` differs from what was cached, the old messages are dropped first: their UIDs
    /// no longer mean anything. Messages already cached are updated in place.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails. Nothing is written in that case.
    pub fn save_messages(
        &mut self,
        account: &str,
        mailbox: &str,
        role: Role,
        uid_validity: u32,
        messages: &[Envelope],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        if role != Role::Other {
            tx.execute(
                "DELETE FROM mailboxes WHERE account = ?1 AND role = ?2 AND name != ?3",
                [account, role.as_str(), mailbox],
            )?;
        }
        let cached: Option<u32> = tx
            .query_row(
                "SELECT uid_validity FROM mailboxes WHERE account = ?1 AND name = ?2",
                [account, mailbox],
                |row| row.get(0),
            )
            .optional()?;
        if cached != Some(uid_validity) {
            tx.execute(
                "DELETE FROM messages WHERE account = ?1 AND mailbox = ?2",
                [account, mailbox],
            )?;
        }
        tx.execute(
            "INSERT INTO mailboxes VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(account, name) DO UPDATE SET
                uid_validity = excluded.uid_validity, role = excluded.role",
            params![account, mailbox, uid_validity, role.as_str()],
        )?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO messages VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(account, mailbox, uid) DO UPDATE SET
                    message_id = excluded.message_id, from_name = excluded.from_name,
                    from_email = excluded.from_email, subject = excluded.subject,
                    date = excluded.date, seen = excluded.seen, to_list = excluded.to_list",
            )?;
            for m in messages {
                insert.execute(params![
                    account,
                    mailbox,
                    m.uid,
                    m.message_id,
                    m.from.name,
                    m.from.email,
                    m.subject,
                    m.date,
                    m.seen,
                    encode_addresses(&m.to),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Marks a cached message read or unread. Returns whether it's cached.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn set_seen(&self, account: &str, mailbox: &str, uid: u32, seen: bool) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE messages SET seen = ?4 WHERE account = ?1 AND mailbox = ?2 AND uid = ?3",
            params![account, mailbox, uid, seen],
        )? > 0)
    }

    /// The `limit` most recent messages in every account's mailbox with this role, newest first.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn recent_messages(&self, role: Role, limit: usize) -> Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages m
             JOIN mailboxes b ON b.account = m.account AND b.name = m.mailbox
             WHERE b.role = ?1
             ORDER BY m.date IS NULL, m.date DESC, m.uid DESC LIMIT ?2"
        ))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![role.as_str(), limit], message_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The `limit` most recent messages in one of the account's mailboxes, newest first.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn mailbox_messages(
        &self,
        account: &str,
        mailbox: &str,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages m
             WHERE m.account = ?1 AND m.mailbox = ?2
             ORDER BY m.date IS NULL, m.date DESC, m.uid DESC LIMIT ?3"
        ))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![account, mailbox, limit], message_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// How many messages each account has unread in its inbox, by account id. Accounts with none
    /// are left out.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn unread_counts(&self) -> Result<HashMap<String, usize>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.account, COUNT(*) FROM messages m
             JOIN mailboxes b ON b.account = m.account AND b.name = m.mailbox
             WHERE b.role = 'inbox' AND NOT m.seen
             GROUP BY m.account",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.map(|row| {
            let (account, count) = row?;
            let count = usize::try_from(count).map_err(|e| Error::Corrupt(e.to_string()))?;
            Ok((account, count))
        })
        .collect()
    }

    /// The setting called `name`, if it was ever set.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn setting(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE name = ?1",
                [name],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Sets the setting called `name` to `value`, replacing what it was.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn set_setting(&self, name: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (name, value) VALUES (?1, ?2)
             ON CONFLICT (name) DO UPDATE SET value = excluded.value",
            [name, value],
        )?;
        Ok(())
    }
}

fn message_from_row(row: &Row<'_>) -> rusqlite::Result<StoredMessage> {
    Ok(StoredMessage {
        account: row.get(0)?,
        mailbox: row.get(1)?,
        envelope: Envelope {
            uid: row.get(2)?,
            message_id: row.get(3)?,
            from: Address {
                name: row.get(4)?,
                email: row.get(5)?,
            },
            subject: row.get(6)?,
            date: row.get(7)?,
            seen: row.get(8)?,
            to: decode_addresses(
                row.get::<_, Option<String>>(9)?
                    .as_deref()
                    .unwrap_or_default(),
            ),
        },
    })
}

/// Addresses as one column: each as name, U+001F, email; separated by U+001E. Missing parts are
/// empty.
fn encode_addresses(addresses: &[Address]) -> String {
    addresses
        .iter()
        .map(|a| {
            format!(
                "{}\u{1f}{}",
                a.name.as_deref().unwrap_or_default(),
                a.email.as_deref().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\u{1e}")
}

fn decode_addresses(column: &str) -> Vec<Address> {
    let part = |s: &str| (!s.is_empty()).then(|| s.to_owned());
    column
        .split('\u{1e}')
        .filter(|a| !a.is_empty())
        .map(|a| {
            let (name, email) = a.split_once('\u{1f}').unwrap_or(("", a));
            Address {
                name: part(name),
                email: part(email),
            }
        })
        .collect()
}

fn account_from_row(row: &Row<'_>) -> Result<Account> {
    let server = |prefix: &str| -> Result<Server> {
        let security: String = row.get(format!("{prefix}_security").as_str())?;
        Ok(Server {
            host: row.get(format!("{prefix}_host").as_str())?,
            port: row.get(format!("{prefix}_port").as_str())?,
            security: security
                .parse::<Security>()
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            username: row.get(format!("{prefix}_username").as_str())?,
        })
    };
    let auth: String = row.get("auth")?;
    Ok(Account {
        id: row.get("id")?,
        display_name: row.get("display_name")?,
        label: row.get("label")?,
        imap: server("imap")?,
        smtp: server("smtp")?,
        auth: auth
            .parse::<Auth>()
            .map_err(|e| Error::Corrupt(e.to_string()))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(id: &str) -> Account {
        let server = |host: &str, port, security| Server {
            host: host.into(),
            port,
            security,
            username: id.into(),
        };
        Account {
            id: id.into(),
            display_name: Some("Me".into()),
            label: None,
            imap: server("imap.example.com", 993, Security::Tls),
            smtp: server("smtp.example.com", 587, Security::StartTls),
            auth: Auth::Password,
        }
    }

    fn envelope(uid: u32, date: i64) -> Envelope {
        Envelope {
            uid,
            message_id: Some(format!("{uid}@example.com")),
            from: Address {
                name: Some("Ada".into()),
                email: Some("ada@example.com".into()),
            },
            subject: Some(format!("message {uid}")),
            date: Some(date),
            seen: false,
            to: vec![Address {
                name: None,
                email: Some("me@example.com".into()),
            }],
        }
    }

    #[test]
    fn accounts_round_trip_and_update_in_place() {
        let store = Store::open_in_memory().unwrap();
        let mut a = account("me@example.com");
        store.save_account(&a).unwrap();
        assert_eq!(store.accounts().unwrap(), vec![a.clone()]);

        a.smtp.port = 465;
        a.smtp.security = Security::Tls;
        a.auth = Auth::Google;
        store.save_account(&a).unwrap();
        assert_eq!(store.account(&a.id).unwrap(), Some(a));
    }

    #[test]
    fn names_are_set_and_adding_again_keeps_the_label() {
        let store = Store::open_in_memory().unwrap();
        let mut a = account("me@example.com");
        store.save_account(&a).unwrap();
        assert!(
            store
                .set_names(&a.id, Some("Work"), Some("Ada Lovelace"))
                .unwrap()
        );
        let saved = store.account(&a.id).unwrap().unwrap();
        assert_eq!(saved.label.as_deref(), Some("Work"));
        assert_eq!(saved.display_name.as_deref(), Some("Ada Lovelace"));

        a.display_name = None;
        store.save_account(&a).unwrap();
        let saved = store.account(&a.id).unwrap().unwrap();
        assert_eq!(saved.label.as_deref(), Some("Work"), "the label stays");
        assert_eq!(saved.display_name, None, "the rest is replaced");
        assert!(
            !store.set_names("nobody@example.com", None, None).unwrap(),
            "no such account"
        );
    }

    fn image(cid: &str, data: &[u8]) -> InlineImage {
        InlineImage {
            cid: cid.into(),
            name: format!("{cid}.png"),
            mime: "image/png".into(),
            data: data.into(),
        }
    }

    #[test]
    fn signatures_keep_their_images_in_order_and_go_with_the_account() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        assert_eq!(store.signature("me@example.com").unwrap(), None);

        let signature = Signature {
            html: "<p><b>Ada</b></p>".into(),
            source: false,
            images: vec![image("logo", &[1, 2, 3]), image("badge", &[4])],
        };
        store
            .save_signature("me@example.com", Some(&signature))
            .unwrap();
        assert_eq!(
            store.signature("me@example.com").unwrap().as_ref(),
            Some(&signature)
        );

        let replaced = Signature {
            html: "<table><tr><td><img src=\"cid:badge\"></td></tr></table>".into(),
            source: true,
            images: vec![image("badge", &[4])],
        };
        store
            .save_signature("me@example.com", Some(&replaced))
            .unwrap();
        assert_eq!(store.signature("me@example.com").unwrap(), Some(replaced));

        assert!(
            store
                .save_signature("nobody@example.com", Some(&signature))
                .is_err(),
            "no such account"
        );
        store.remove_account("me@example.com").unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        assert_eq!(
            store.signature("me@example.com").unwrap(),
            None,
            "removing the account removed it"
        );
        store
            .save_signature("me@example.com", Some(&signature))
            .unwrap();
        store.save_signature("me@example.com", None).unwrap();
        assert_eq!(store.signature("me@example.com").unwrap(), None);
    }

    #[test]
    fn removing_an_account_drops_its_mail() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(1, 10)],
            )
            .unwrap();

        assert!(
            store.remove_account("me@example.com").unwrap(),
            "it existed"
        );
        assert!(
            store.recent_messages(Role::Inbox, 10).unwrap().is_empty(),
            "mail is gone"
        );
        assert!(
            !store.remove_account("me@example.com").unwrap(),
            "already gone"
        );
    }

    #[test]
    fn mailbox_state_tracks_the_highest_uid() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        assert_eq!(
            store.mailbox_state("me@example.com", "INBOX").unwrap(),
            None
        );

        store
            .save_messages("me@example.com", "INBOX", Role::Inbox, 7, &[])
            .unwrap();
        assert_eq!(
            store.mailbox_state("me@example.com", "INBOX").unwrap(),
            Some(MailboxState {
                uid_validity: 7,
                last_uid: None
            })
        );

        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                7,
                &[envelope(3, 10), envelope(9, 20)],
            )
            .unwrap();
        assert_eq!(
            store
                .mailbox_state("me@example.com", "INBOX")
                .unwrap()
                .unwrap()
                .last_uid,
            Some(9)
        );
    }

    #[test]
    fn a_new_uid_validity_discards_stale_messages() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(1, 10), envelope(2, 20)],
            )
            .unwrap();
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                2,
                &[envelope(1, 30)],
            )
            .unwrap();

        let messages = store.recent_messages(Role::Inbox, 10).unwrap();
        assert_eq!(messages.len(), 1, "only the refetched message is left");
        assert_eq!(messages[0].envelope.date, Some(30));
    }

    #[test]
    fn messages_can_be_marked_read_and_unread() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(1, 10)],
            )
            .unwrap();
        let seen = |store: &Store| {
            store.recent_messages(Role::Inbox, 1).unwrap()[0]
                .envelope
                .seen
        };

        assert!(store.set_seen("me@example.com", "INBOX", 1, true).unwrap());
        assert!(seen(&store));
        assert!(store.set_seen("me@example.com", "INBOX", 1, false).unwrap());
        assert!(!seen(&store));
        assert!(
            !store.set_seen("me@example.com", "INBOX", 2, true).unwrap(),
            "not cached"
        );
    }

    #[test]
    fn recent_messages_are_newest_first_across_accounts() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("a@example.com")).unwrap();
        store.save_account(&account("b@example.com")).unwrap();
        store
            .save_messages(
                "a@example.com",
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(1, 10), envelope(2, 30)],
            )
            .unwrap();
        store
            .save_messages("b@example.com", "INBOX", Role::Inbox, 1, &[envelope(1, 20)])
            .unwrap();

        let order: Vec<_> = store
            .recent_messages(Role::Inbox, 2)
            .unwrap()
            .into_iter()
            .map(|m| (m.account, m.envelope.date))
            .collect();
        assert_eq!(
            order,
            vec![
                ("a@example.com".to_owned(), Some(30)),
                ("b@example.com".to_owned(), Some(20))
            ]
        );
    }

    #[test]
    fn mailboxes_are_listed_by_role_and_keep_their_recipients() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(1, 10)],
            )
            .unwrap();
        let mut sent = envelope(1, 20);
        sent.to = vec![
            Address {
                name: Some("Ada".into()),
                email: Some("ada@example.com".into()),
            },
            Address {
                name: None,
                email: Some("bob@example.com".into()),
            },
        ];
        sent.seen = true;
        store
            .save_messages("me@example.com", "Sent", Role::Sent, 4, &[sent.clone()])
            .unwrap();

        let inbox = store.recent_messages(Role::Inbox, 10).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].mailbox, "INBOX");
        let listed = store.recent_messages(Role::Sent, 10).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].envelope, sent, "recipients survive");
        assert_eq!(
            store
                .mailbox_for_role("me@example.com", Role::Sent)
                .unwrap(),
            Some((
                "Sent".to_owned(),
                MailboxState {
                    uid_validity: 4,
                    last_uid: Some(1)
                }
            ))
        );
        assert_eq!(
            store.unread_counts().unwrap(),
            HashMap::from([("me@example.com".to_owned(), 1)]),
            "only the inbox counts"
        );
    }

    #[test]
    fn a_renamed_sent_mailbox_replaces_the_old_one() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        store
            .save_messages("me@example.com", "Sent", Role::Sent, 1, &[envelope(1, 10)])
            .unwrap();
        store
            .save_messages("me@example.com", "Sent Items", Role::Sent, 1, &[])
            .unwrap();
        assert_eq!(
            store.recent_messages(Role::Sent, 10).unwrap(),
            vec![],
            "old mail is gone"
        );
        assert_eq!(
            store
                .mailbox_for_role("me@example.com", Role::Sent)
                .unwrap()
                .map(|(name, _)| name),
            Some("Sent Items".to_owned())
        );
    }

    #[test]
    fn settings_are_kept_and_replaced() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.setting("theme").unwrap(), None);
        store.set_setting("theme", "dark").unwrap();
        store.set_setting("theme", "light").unwrap();
        assert_eq!(store.setting("theme").unwrap().as_deref(), Some("light"));
    }

    #[test]
    fn a_version_0_database_gains_the_new_columns() {
        let dir = std::env::temp_dir().join(format!("simplymail-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        let _ignored = std::fs::remove_file(&path);
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch(
                "CREATE TABLE accounts (
                    id TEXT PRIMARY KEY, display_name TEXT,
                    imap_host TEXT NOT NULL, imap_port INTEGER NOT NULL,
                    imap_security TEXT NOT NULL, imap_username TEXT NOT NULL,
                    smtp_host TEXT NOT NULL, smtp_port INTEGER NOT NULL,
                    smtp_security TEXT NOT NULL, smtp_username TEXT NOT NULL) STRICT;
                 CREATE TABLE mailboxes (
                    account TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                    name TEXT NOT NULL, uid_validity INTEGER NOT NULL,
                    PRIMARY KEY (account, name)) STRICT;
                 CREATE TABLE messages (
                    account TEXT NOT NULL, mailbox TEXT NOT NULL, uid INTEGER NOT NULL,
                    message_id TEXT, from_name TEXT, from_email TEXT, subject TEXT,
                    date INTEGER, seen INTEGER NOT NULL,
                    PRIMARY KEY (account, mailbox, uid),
                    FOREIGN KEY (account, mailbox) REFERENCES mailboxes(account, name)
                        ON DELETE CASCADE) STRICT;
                 INSERT INTO accounts VALUES ('me@example.com', NULL, 'imap.example.com', 993,
                    'tls', 'me', 'smtp.example.com', 465, 'tls', 'me');
                 INSERT INTO mailboxes VALUES ('me@example.com', 'INBOX', 1);
                 INSERT INTO messages VALUES ('me@example.com', 'INBOX', 1, NULL, NULL,
                    'ada@example.com', 'hi', 10, 0);",
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let inbox = store.recent_messages(Role::Inbox, 10).unwrap();
        assert_eq!(inbox.len(), 1, "old mail is filed as the inbox");
        assert_eq!(inbox[0].envelope.to, vec![]);
        let old = store.account("me@example.com").unwrap().unwrap();
        assert_eq!(
            old.auth,
            Auth::Password,
            "old accounts log in with a password"
        );
        assert_eq!(old.label, None, "and have no label");
        assert_eq!(store.signature("me@example.com").unwrap(), None);
        drop(store);
        Store::open(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn listed_mailboxes_are_kept_and_unlisted_ones_dropped() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        let mailbox = |name: &str, role| Mailbox {
            name: name.to_owned(),
            delimiter: Some("/".to_owned()),
            role,
        };
        let listed = [
            mailbox("Receipts", Role::Other),
            mailbox("INBOX", Role::Inbox),
            mailbox("Archive", Role::Other),
            mailbox("Sent", Role::Sent),
        ];
        store.save_mailbox_list("me@example.com", &listed).unwrap();
        let names: Vec<String> = store
            .mailboxes("me@example.com")
            .unwrap()
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert_eq!(names, ["INBOX", "Sent", "Archive", "Receipts"]);

        for name in ["Archive", "Receipts"] {
            store
                .save_messages("me@example.com", name, Role::Other, 1, &[envelope(1, 10)])
                .unwrap();
        }
        assert_eq!(
            store.mailbox_states("me@example.com").unwrap().len(),
            2,
            "two Other mailboxes coexist"
        );
        assert_eq!(
            store
                .mailbox_messages("me@example.com", "Archive", 10)
                .unwrap()
                .len(),
            1
        );

        store
            .save_mailbox_list("me@example.com", &listed[1..3])
            .unwrap();
        assert_eq!(
            store
                .mailbox_messages("me@example.com", "Receipts", 10)
                .unwrap(),
            vec![],
            "a mailbox gone from the server takes its mail with it"
        );
        assert_eq!(
            store
                .mailbox_messages("me@example.com", "Archive", 10)
                .unwrap()
                .len(),
            1
        );
    }
}
