//! Adding and removing accounts: their settings in the store, their passwords or OAuth tokens in the
//! secret store.

use std::sync::Arc;

use simplymail_core::account::{Account, Auth, Security, Server};
use simplymail_core::secrets::{SecretError, SecretKey, SecretKind, SecretStore, SecretString};
use simplymail_mail::Credentials;
use simplymail_mail::oauth::{Client, Tokens};
use simplymail_store::Store;

use crate::{Error, Result};

/// The key an account's (IMAP) password is filed under.
#[must_use]
pub fn password_key(account: &str) -> SecretKey {
    SecretKey::new(account, SecretKind::Password)
}

/// The key an account's SMTP password is filed under, when it differs from the IMAP one.
#[must_use]
pub fn smtp_password_key(account: &str) -> SecretKey {
    SecretKey::new(account, SecretKind::SmtpPassword)
}

/// The key the refresh token of an account that signs in with Google is filed under.
#[must_use]
pub fn oauth_key(account: &str) -> SecretKey {
    SecretKey::new(account, SecretKind::OAuthRefreshToken)
}

/// The password to send mail with: the SMTP password if one is stored, else the IMAP one.
///
/// # Errors
///
/// Returns an error when the secret store can't be read.
pub fn smtp_password(secrets: &dyn SecretStore, account: &str) -> Result<Option<SecretString>> {
    match secrets.get(&smtp_password_key(account))? {
        Some(password) => Ok(Some(password)),
        None => Ok(secrets.get(&password_key(account))?),
    }
}

/// An account's passwords, as entered.
#[derive(Debug, Clone)]
pub struct Passwords {
    pub imap: SecretString,
    /// The SMTP password, or `None` when it's the IMAP one.
    pub smtp: Option<SecretString>,
}

/// What an account logs in with, as just typed in or granted.
#[derive(Debug, Clone)]
pub enum Login {
    Passwords(Passwords),
    /// A Google sign-in: the refresh token to keep, and tokens ready to log in with now.
    Google {
        refresh_token: SecretString,
        tokens: Arc<Tokens>,
    },
}

impl From<Passwords> for Login {
    fn from(passwords: Passwords) -> Self {
        Self::Passwords(passwords)
    }
}

impl Login {
    /// What to log in to IMAP and to SMTP with.
    fn credentials(&self) -> (Credentials, Credentials) {
        match self {
            Self::Passwords(p) => (
                Credentials::Password(p.imap.clone()),
                Credentials::Password(p.smtp.clone().unwrap_or_else(|| p.imap.clone())),
            ),
            Self::Google { tokens, .. } => (
                Credentials::OAuth(Arc::clone(tokens)),
                Credentials::OAuth(Arc::clone(tokens)),
            ),
        }
    }
}

/// What `account` logs in to IMAP and to SMTP with, read from `secrets`. Both share one access
/// token for an account that signs in with Google.
///
/// # Errors
///
/// Returns an error when nothing is stored for the account, the secret store can't be read, or
/// this build can't sign in to Google.
pub fn credentials(
    secrets: &dyn SecretStore,
    account: &Account,
) -> Result<(Credentials, Credentials)> {
    match account.auth {
        Auth::Password => {
            let password = secrets
                .get(&password_key(&account.id))?
                .ok_or_else(|| Error::NoPassword(account.id.clone()))?;
            // Most accounts have no SMTP password of their own, and a password command may fail
            // rather than print nothing, so any failure falls back to the IMAP one; a wrong one
            // shows up as a refused login when sending.
            let smtp = secrets
                .get(&smtp_password_key(&account.id))
                .ok()
                .flatten()
                .unwrap_or_else(|| password.clone());
            Ok((Credentials::Password(password), Credentials::Password(smtp)))
        }
        Auth::Google => {
            let refresh_token = secrets
                .get(&oauth_key(&account.id))?
                .ok_or_else(|| Error::NotSignedIn(account.id.clone()))?;
            let tokens = Arc::new(Tokens::new(Client::google()?, refresh_token));
            Ok((
                Credentials::OAuth(Arc::clone(&tokens)),
                Credentials::OAuth(tokens),
            ))
        }
    }
}

/// An account as someone types it in: ports and user names may be left out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewAccount {
    pub email: String,
    pub display_name: Option<String>,
    /// The user name for both servers. Defaults to the email address.
    pub username: Option<String>,
    pub imap_host: String,
    /// Defaults to the conventional port for `imap_security`.
    pub imap_port: Option<u16>,
    pub imap_security: Security,
    pub smtp_host: String,
    /// Defaults to the conventional port for `smtp_security`.
    pub smtp_port: Option<u16>,
    pub smtp_security: Security,
    /// The SMTP user name, if it differs from `username`.
    pub smtp_username: Option<String>,
}

impl NewAccount {
    /// Fills in the defaults.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] when the email address or a host is missing.
    pub fn build(self) -> Result<Account> {
        let email = self.email.trim().to_owned();
        if !email.contains('@') {
            return Err(Error::Invalid(format!("{email:?} isn't an email address")));
        }
        let non_empty =
            |s: Option<String>| s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
        let host = |h: String, which: &str| {
            let h = h.trim().to_owned();
            if h.is_empty() {
                Err(Error::Invalid(format!("the {which} server is missing")))
            } else {
                Ok(h)
            }
        };
        let username = non_empty(self.username).unwrap_or_else(|| email.clone());
        Ok(Account {
            display_name: non_empty(self.display_name),
            imap: Server {
                host: host(self.imap_host, "IMAP")?,
                port: self
                    .imap_port
                    .unwrap_or_else(|| self.imap_security.default_imap_port()),
                security: self.imap_security,
                username: username.clone(),
            },
            smtp: Server {
                host: host(self.smtp_host, "SMTP")?,
                port: self
                    .smtp_port
                    .unwrap_or_else(|| self.smtp_security.default_smtp_port()),
                security: self.smtp_security,
                username: non_empty(self.smtp_username).unwrap_or(username),
            },
            auth: Auth::Password,
            id: email,
        })
    }
}

/// Checks that `account`'s IMAP and then SMTP servers accept `login`.
///
/// # Errors
///
/// Returns the first login that failed, saying which server refused it.
pub async fn check_logins(account: &Account, login: &Login) -> Result<()> {
    let labeled = |which: &str, server: &Server, e: simplymail_mail::Error| {
        Error::Invalid(format!(
            "{which} login at {}:{} failed: {e}",
            server.host, server.port
        ))
    };
    let (imap, smtp) = login.credentials();
    simplymail_mail::imap::check_login(&account.imap, &imap)
        .await
        .map_err(|e| labeled("IMAP", &account.imap, e))?;
    simplymail_mail::smtp::check_login(&account.smtp, &smtp)
        .await
        .map_err(|e| labeled("SMTP", &account.smtp, e))
}

/// Saves `account`'s settings, and what it logs in with, then forgets any other way it logged in
/// before.
///
/// With a read-only secret store (a password command), passwords aren't saved: they come from the
/// command. Without an SMTP password, any stored one is deleted, so sending falls back to the IMAP
/// password. Secrets are saved first, so an account is never listed without them.
///
/// # Errors
///
/// Returns an error when a write fails, or for a Google sign-in when `secrets` is read-only, as
/// the refresh token couldn't be kept.
pub fn save_account(
    store: &Store,
    secrets: &dyn SecretStore,
    account: &Account,
    login: &Login,
) -> Result<()> {
    let smtp_key = smtp_password_key(&account.id);
    let written = match login {
        Login::Passwords(passwords) => secrets
            .set(&password_key(&account.id), &passwords.imap)
            .and_then(|()| {
                passwords.smtp.as_ref().map_or_else(
                    || secrets.delete(&smtp_key),
                    |smtp| secrets.set(&smtp_key, smtp),
                )
            })
            .and_then(|()| secrets.delete(&oauth_key(&account.id))),
        Login::Google { refresh_token, .. } => {
            match secrets.set(&oauth_key(&account.id), refresh_token) {
                Err(SecretError::ReadOnly) => {
                    return Err(Error::Invalid(
                        "signing in with Google needs the OS keyring to keep the sign-in, but \
                     passwords come from a command (SIMPLYMAIL_PASSWORD_COMMAND)"
                            .into(),
                    ));
                }
                written => written
                    .and_then(|()| secrets.delete(&password_key(&account.id)))
                    .and_then(|()| secrets.delete(&smtp_key)),
            }
        }
    };
    match written {
        Ok(()) | Err(SecretError::ReadOnly) => {}
        Err(e) => return Err(e.into()),
    }
    store.save_account(account)?;
    Ok(())
}

/// Removes an account, its cached mail and its stored passwords or tokens. Returns whether it
/// existed.
///
/// # Errors
///
/// Returns an error when the account can't be removed. A password that can't be deleted is
/// reported only after the account is gone, so nothing is left half-removed in the store.
pub fn remove_account(store: &Store, secrets: &dyn SecretStore, id: &str) -> Result<bool> {
    if !store.remove_account(id)? {
        return Ok(false);
    }
    match secrets
        .delete(&password_key(id))
        .and_then(|()| secrets.delete(&smtp_password_key(id)))
        .and_then(|()| secrets.delete(&oauth_key(id)))
    {
        Ok(()) | Err(SecretError::ReadOnly) => Ok(true),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new() -> NewAccount {
        NewAccount {
            email: " me@example.com ".into(),
            imap_host: "imap.example.com".into(),
            smtp_host: "smtp.example.com".into(),
            smtp_security: Security::StartTls,
            ..NewAccount::default()
        }
    }

    #[test]
    fn defaults_fill_in_ports_and_user_names() {
        let a = new().build().unwrap();
        assert_eq!(a.id, "me@example.com");
        assert_eq!((a.imap.port, a.smtp.port), (993, 587));
        assert_eq!(a.imap.username, "me@example.com");
        assert_eq!(a.smtp.username, "me@example.com");
        assert_eq!(a.display_name, None);
    }

    #[test]
    fn given_values_win() {
        let a = NewAccount {
            username: Some("me".into()),
            smtp_username: Some("  ".into()),
            imap_port: Some(1143),
            ..new()
        }
        .build()
        .unwrap();
        assert_eq!(a.imap.port, 1143);
        assert_eq!(a.smtp.username, "me", "a blank SMTP user name falls back");
    }

    /// A secret store in memory.
    #[derive(Debug, Default)]
    struct Memory(std::sync::Mutex<std::collections::HashMap<SecretKey, String>>);

    impl Memory {
        fn secrets(
            &self,
        ) -> Result<
            std::sync::MutexGuard<'_, std::collections::HashMap<SecretKey, String>>,
            SecretError,
        > {
            self.0
                .lock()
                .map_err(|e| SecretError::Unavailable(e.to_string()))
        }
    }

    impl SecretStore for Memory {
        fn get(&self, key: &SecretKey) -> Result<Option<SecretString>, SecretError> {
            Ok(self
                .secrets()?
                .get(key)
                .map(|s| SecretString::from(s.clone())))
        }
        fn set(&self, key: &SecretKey, secret: &SecretString) -> Result<(), SecretError> {
            use simplymail_core::secrets::ExposeSecret as _;
            self.secrets()?
                .insert(key.clone(), secret.expose_secret().to_owned());
            Ok(())
        }
        fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
            self.secrets()?.remove(key);
            Ok(())
        }
    }

    fn smtp(secrets: &Memory) -> String {
        use simplymail_core::secrets::ExposeSecret as _;
        smtp_password(secrets, "me@example.com")
            .unwrap()
            .unwrap()
            .expose_secret()
            .to_owned()
    }

    #[test]
    fn the_smtp_password_falls_back_to_the_imap_one() {
        let (store, secrets) = (Store::open_in_memory().unwrap(), Memory::default());
        let account = new().build().unwrap();
        let passwords = |smtp: Option<&str>| {
            Login::from(Passwords {
                imap: SecretString::from("imap"),
                smtp: smtp.map(SecretString::from),
            })
        };

        save_account(&store, &secrets, &account, &passwords(Some("smtp"))).unwrap();
        assert_eq!(smtp(&secrets), "smtp");
        save_account(&store, &secrets, &account, &passwords(None)).unwrap();
        assert_eq!(smtp(&secrets), "imap", "clearing it falls back");

        save_account(&store, &secrets, &account, &passwords(Some("smtp"))).unwrap();
        assert!(remove_account(&store, &secrets, &account.id).unwrap());
        assert!(
            secrets.0.lock().unwrap().is_empty(),
            "both passwords are gone"
        );
    }

    #[test]
    fn switching_to_google_forgets_the_passwords_and_back() {
        let (store, secrets) = (Store::open_in_memory().unwrap(), Memory::default());
        let mut account = new().build().unwrap();
        let passwords = Login::from(Passwords {
            imap: SecretString::from("imap"),
            smtp: Some(SecretString::from("smtp")),
        });
        save_account(&store, &secrets, &account, &passwords).unwrap();

        account.auth = Auth::Google;
        let google = Login::Google {
            refresh_token: SecretString::from("1//refresh"),
            tokens: Arc::new(Tokens::new(
                Client::new("id", None),
                SecretString::from("1//refresh"),
            )),
        };
        save_account(&store, &secrets, &account, &google).unwrap();
        let kept: Vec<SecretKind> = secrets.0.lock().unwrap().keys().map(|k| k.kind).collect();
        assert_eq!(kept, [SecretKind::OAuthRefreshToken]);
        assert_eq!(
            store.account(&account.id).unwrap().unwrap().auth,
            Auth::Google
        );

        account.auth = Auth::Password;
        save_account(&store, &secrets, &account, &passwords).unwrap();
        assert!(
            secrets.get(&oauth_key(&account.id)).unwrap().is_none(),
            "the token is gone"
        );
        assert!(remove_account(&store, &secrets, &account.id).unwrap());
        assert!(secrets.0.lock().unwrap().is_empty(), "nothing is left");
    }

    /// A password command: it can be read but not written.
    #[derive(Debug)]
    struct ReadOnly;

    impl SecretStore for ReadOnly {
        fn get(&self, _: &SecretKey) -> Result<Option<SecretString>, SecretError> {
            Ok(None)
        }
        fn set(&self, _: &SecretKey, _: &SecretString) -> Result<(), SecretError> {
            Err(SecretError::ReadOnly)
        }
        fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
            Err(SecretError::ReadOnly)
        }
    }

    #[test]
    fn a_google_sign_in_needs_a_writable_store() {
        let store = Store::open_in_memory().unwrap();
        let account = crate::google_account("me@gmail.com", None);
        let google = Login::Google {
            refresh_token: SecretString::from("1//refresh"),
            tokens: Arc::new(Tokens::new(
                Client::new("id", None),
                SecretString::from("1//refresh"),
            )),
        };
        let error = save_account(&store, &ReadOnly, &account, &google).unwrap_err();
        assert!(error.to_string().contains("keyring"), "got {error}");
        assert!(store.accounts().unwrap().is_empty(), "nothing is saved");
    }

    #[test]
    fn missing_fields_are_refused() {
        assert!(
            NewAccount {
                email: "me".into(),
                ..new()
            }
            .build()
            .is_err(),
            "not an address"
        );
        assert!(
            NewAccount {
                smtp_host: " ".into(),
                ..new()
            }
            .build()
            .is_err(),
            "no SMTP host"
        );
    }
}
