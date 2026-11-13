//! What an account logs in with: a password, or Google OAuth tokens.

use std::sync::Arc;

use simplymail_core::secrets::SecretString;

use crate::Result;
use crate::oauth::Tokens;

/// How to log in to a server. Cheap to clone: an account's clones share one access token.
#[derive(Debug, Clone)]
pub enum Credentials {
    /// A password or app password.
    Password(SecretString),
    /// Google OAuth, logged in with `XOAUTH2` and an access token refreshed as needed.
    OAuth(Arc<Tokens>),
}

/// What to send for one login.
pub(crate) enum Secret {
    Password(SecretString),
    /// An OAuth access token, for `XOAUTH2`.
    Bearer(SecretString),
}

impl Credentials {
    /// The secret for a login now, refreshing an expired access token first.
    pub(crate) async fn secret(&self) -> Result<Secret> {
        Ok(match self {
            Self::Password(password) => Secret::Password(password.clone()),
            Self::OAuth(tokens) => Secret::Bearer(tokens.access_token().await?),
        })
    }

    /// Notes that a server refused the login, so an access token isn't tried again.
    pub(crate) async fn refused(&self) {
        if let Self::OAuth(tokens) = self {
            tokens.forget().await;
        }
    }
}

impl From<SecretString> for Credentials {
    fn from(password: SecretString) -> Self {
        Self::Password(password)
    }
}
