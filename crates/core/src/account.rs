//! Mail accounts: where to read mail from (IMAP) and where to send it through (SMTP).
//!
//! Only connection settings live here. Passwords and OAuth tokens are kept in a
//! [`crate::secrets::SecretStore`], keyed by [`Account::id`].

use std::fmt;
use std::str::FromStr;

/// How the connection to a mail server is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Security {
    /// TLS from the first byte ("implicit TLS"), such as IMAP on 993 and SMTP on 465.
    #[default]
    Tls,
    /// A plain connection upgraded with `STARTTLS`, such as IMAP on 143 and SMTP on 587.
    StartTls,
    /// No encryption. Only sensible for a local bridge such as Proton Mail Bridge.
    None,
}

impl Security {
    /// A stable name for this mode, safe to persist.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tls => "tls",
            Self::StartTls => "starttls",
            Self::None => "none",
        }
    }

    /// The conventional IMAP port for this mode.
    #[must_use]
    pub const fn default_imap_port(self) -> u16 {
        match self {
            Self::Tls => 993,
            Self::StartTls | Self::None => 143,
        }
    }

    /// The conventional SMTP submission port for this mode.
    #[must_use]
    pub const fn default_smtp_port(self) -> u16 {
        match self {
            Self::Tls => 465,
            Self::StartTls | Self::None => 587,
        }
    }
}

impl fmt::Display for Security {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A [`Security`] name that isn't one of `tls`, `starttls` or `none`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown security mode {0:?}: expected tls, starttls or none")]
pub struct UnknownSecurity(pub String);

impl FromStr for Security {
    type Err = UnknownSecurity;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "tls" | "ssl" => Ok(Self::Tls),
            "starttls" => Ok(Self::StartTls),
            "none" | "plain" => Ok(Self::None),
            _ => Err(UnknownSecurity(s.to_owned())),
        }
    }
}

/// How an account logs in to its servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Auth {
    /// A password or app password, sent with IMAP `LOGIN` and SMTP `AUTH PLAIN`/`LOGIN`.
    #[default]
    Password,
    /// Google OAuth: a stored refresh token buys short-lived access tokens, sent with `XOAUTH2`.
    Google,
}

impl Auth {
    /// A stable name for this method, safe to persist.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Google => "google",
        }
    }
}

impl fmt::Display for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An [`Auth`] name that isn't one of `password` or `google`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown login method {0:?}: expected password or google")]
pub struct UnknownAuth(pub String);

impl FromStr for Auth {
    type Err = UnknownAuth;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "password" => Ok(Self::Password),
            "google" => Ok(Self::Google),
            _ => Err(UnknownAuth(s.to_owned())),
        }
    }
}

/// One mail server endpoint and the user name to log in with.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: String,
}

/// A mail account: an IMAP server to read from and an SMTP server to send through.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Account {
    /// The account's stable identifier, its email address. Secrets are filed under it.
    pub id: String,
    /// The name shown on sent mail and in the UI, if set.
    pub display_name: Option<String>,
    pub imap: Server,
    pub smtp: Server,
    /// How both servers are logged in to.
    pub auth: Auth,
}

impl Account {
    /// The name to show for this account: its display name, or else its address.
    #[must_use]
    pub fn label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_round_trips_through_its_name() {
        for s in [Security::Tls, Security::StartTls, Security::None] {
            assert_eq!(s.as_str().parse::<Security>().unwrap(), s);
        }
        assert_eq!("SSL".parse::<Security>().unwrap(), Security::Tls);
        assert!("smoke-signals".parse::<Security>().is_err(), "unknown mode");
    }

    #[test]
    fn auth_round_trips_through_its_name() {
        for a in [Auth::Password, Auth::Google] {
            assert_eq!(a.as_str().parse::<Auth>().unwrap(), a);
        }
        assert!("carrier-pigeon".parse::<Auth>().is_err(), "unknown method");
    }
}
