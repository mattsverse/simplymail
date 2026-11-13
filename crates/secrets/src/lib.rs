//! Credential storage backends for SimplyMail.
//!
//! - [`KeyringStore`]: the OS keyring (macOS Keychain, Windows Credential Manager, or Secret Service on
//!   Linux/BSD). This is the default.
//! - [`CommandStore`]: a read-only, user-supplied password command such as `pass show …` or `op read …`.
//!   This is the fallback for machines without a keyring, such as SSH sessions or headless Linux.

mod command;
mod keyring;

pub use command::CommandStore;
pub use keyring::KeyringStore;
use simplymail_core::secrets::{SecretError, SecretStore};

/// Which backend holds the account secrets, as chosen in config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Backend {
    /// The OS keyring.
    #[default]
    Keyring,
    /// A shell command whose stdout is the secret. See [`CommandStore`].
    Command { command: String },
}

/// Opens the configured backend.
///
/// # Errors
///
/// Returns [`SecretError::Unavailable`] when the OS keyring can't be reached. The caller should then
/// suggest configuring [`Backend::Command`].
pub fn open(backend: &Backend) -> Result<Box<dyn SecretStore>, SecretError> {
    Ok(match backend {
        Backend::Keyring => Box::new(KeyringStore::native()?),
        Backend::Command { command } => Box::new(CommandStore::new(command.clone())),
    })
}
