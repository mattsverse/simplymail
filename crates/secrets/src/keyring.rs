//! The OS keyring, through `keyring-core` and one native store per platform.

use std::sync::Arc;

use keyring_core::{CredentialStore, Entry, Error};
use simplymail_core::secrets::{ExposeSecret, SecretError, SecretKey, SecretStore, SecretString};

/// The service name every SimplyMail secret is filed under in the keyring.
const SERVICE: &str = "simplymail";

/// Secrets kept in the OS keyring.
#[derive(Debug, Clone)]
pub struct KeyringStore {
    store: Arc<CredentialStore>,
}

impl KeyringStore {
    /// Connects to this platform's native keyring.
    ///
    /// # Errors
    ///
    /// Returns [`SecretError::Unavailable`] when the platform has no supported keyring or it can't be reached.
    pub fn native() -> Result<Self, SecretError> {
        native_store()
            .map(Self::with_store)
            .map_err(|e| SecretError::Unavailable(e.to_string()))
    }

    /// Uses the given `keyring-core` store, for example `keyring_core::mock::Store` in tests.
    #[must_use]
    pub fn with_store(store: Arc<CredentialStore>) -> Self {
        Self { store }
    }

    fn entry(&self, key: &SecretKey) -> Result<Entry, SecretError> {
        let user = format!("{}:{}", key.account, key.kind.as_str());
        self.store
            .build(SERVICE, &user, None)
            .map_err(into_secret_error)
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, key: &SecretKey) -> Result<Option<SecretString>, SecretError> {
        match self.entry(key)?.get_password() {
            Ok(password) => Ok(Some(SecretString::from(password))),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(into_secret_error(e)),
        }
    }

    fn set(&self, key: &SecretKey, secret: &SecretString) -> Result<(), SecretError> {
        self.entry(key)?
            .set_password(secret.expose_secret())
            .map_err(into_secret_error)
    }

    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(into_secret_error(e)),
        }
    }
}

fn into_secret_error(e: Error) -> SecretError {
    match e {
        Error::NoStorageAccess(_) | Error::NoDefaultStore => {
            SecretError::Unavailable(e.to_string())
        }
        e => SecretError::Backend(Box::new(e)),
    }
}

#[cfg(target_os = "macos")]
fn native_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(apple_native_keyring_store::keychain::Store::new()?)
}

#[cfg(target_os = "windows")]
fn native_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(windows_native_keyring_store::Store::new()?)
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn native_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(zbus_secret_service_keyring_store::Store::new()?)
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    )
)))]
fn native_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Err(Error::NotSupportedByStore(
        "no native keyring on this platform".into(),
    ))
}

#[cfg(test)]
mod tests {
    use simplymail_core::secrets::SecretKind;

    use super::*;

    fn store() -> KeyringStore {
        KeyringStore::with_store(keyring_core::mock::Store::new().unwrap())
    }

    #[test]
    fn a_stored_secret_reads_back_until_deleted() {
        let store = store();
        let key = SecretKey::new("me@example.com", SecretKind::Password);
        assert!(store.get(&key).unwrap().is_none(), "nothing is stored yet");

        store.set(&key, &SecretString::from("hunter2")).unwrap();
        assert_eq!(store.get(&key).unwrap().unwrap().expose_secret(), "hunter2");

        store.delete(&key).unwrap();
        assert!(store.get(&key).unwrap().is_none(), "the secret is gone");
        store.delete(&key).unwrap();
    }

    #[test]
    fn each_kind_of_secret_is_stored_separately() {
        let store = store();
        let password = SecretKey::new("me@example.com", SecretKind::Password);
        let token = SecretKey::new("me@example.com", SecretKind::OAuthRefreshToken);
        store
            .set(&password, &SecretString::from("hunter2"))
            .unwrap();
        assert!(
            store.get(&token).unwrap().is_none(),
            "the token slot stays empty"
        );
    }
}
