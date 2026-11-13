//! Account credentials, independent of the backend that stores them.
//!
//! Secrets are held as [`SecretString`] so they're zeroed on drop and never show up in `Debug` output.

pub use secrecy::{ExposeSecret, SecretString};

/// What a secret is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKind {
    /// The IMAP password or app password. It's also the SMTP one, unless there's an
    /// [`SecretKind::SmtpPassword`].
    Password,
    /// The SMTP password, for accounts whose outgoing server takes a different one.
    SmtpPassword,
    /// The OAuth refresh token of an account that signs in with Google, used for IMAP and SMTP.
    OAuthRefreshToken,
}

impl SecretKind {
    /// A stable name for this kind, safe to persist in a backend's key.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::SmtpPassword => "smtp-password",
            Self::OAuthRefreshToken => "oauth-refresh-token",
        }
    }
}

/// Identifies one secret: which account it belongs to, and what it's for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretKey {
    /// The account's stable identifier, such as its email address.
    pub account: String,
    pub kind: SecretKind,
}

impl SecretKey {
    #[must_use]
    pub fn new(account: impl Into<String>, kind: SecretKind) -> Self {
        Self {
            account: account.into(),
            kind,
        }
    }
}

/// Why a [`SecretStore`] call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SecretError {
    /// The backend can't be reached, for example no keyring daemon is running or it's locked.
    #[error("secret store unavailable: {0}")]
    Unavailable(String),
    /// The backend can only read secrets, for example a password command.
    #[error("this secret store is read-only")]
    ReadOnly,
    /// The backend was reached but the operation failed.
    #[error("secret store failed: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Somewhere account secrets can be read from and, unless it's read-only, written to.
pub trait SecretStore: Send + Sync + std::fmt::Debug {
    /// Reads a secret, returning `None` when none is stored under `key`.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend is unavailable or the read fails.
    fn get(&self, key: &SecretKey) -> Result<Option<SecretString>, SecretError>;

    /// Stores `secret` under `key`, replacing any existing value.
    ///
    /// # Errors
    ///
    /// Returns [`SecretError::ReadOnly`] for read-only backends, or an error when the write fails.
    fn set(&self, key: &SecretKey, secret: &SecretString) -> Result<(), SecretError>;

    /// Removes the secret under `key`. Deleting a missing secret succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`SecretError::ReadOnly`] for read-only backends, or an error when the delete fails.
    fn delete(&self, key: &SecretKey) -> Result<(), SecretError>;
}
