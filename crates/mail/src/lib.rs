//! Talks to mail servers: IMAP for reading, SMTP for sending, and Google OAuth for Gmail accounts.
//!
//! [`imap::watch`] keeps a mailbox in sync and reports new mail as it arrives, using IMAP IDLE when
//! the server supports it and polling on a timer otherwise. [`smtp::send`] sends a message. Both log
//! in with [`Credentials`]: a password, or OAuth tokens from [`oauth`].

mod auth;
pub mod imap;
pub mod oauth;
pub mod smtp;
mod tls;

pub use auth::Credentials;

/// Why talking to a mail server failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("couldn't connect to {host}:{port}: {source}")]
    Connect {
        host: String,
        port: u16,
        #[source]
        source: std::io::Error,
    },
    #[error("the server took too long to respond")]
    Timeout,
    #[error("{0:?} isn't a valid server name for TLS")]
    ServerName(String),
    #[error("TLS failed: {0}")]
    Tls(#[source] std::io::Error),
    #[error("couldn't set up TLS: {0}")]
    TlsConfig(#[from] tokio_rustls::rustls::Error),
    #[error("the server closed the connection")]
    Closed,
    #[error("login failed: {0}")]
    Login(#[source] async_imap::error::Error),
    #[error("IMAP error: {0}")]
    Imap(#[from] async_imap::error::Error),
    #[error("the server didn't report a UIDVALIDITY for {0}")]
    NoUidValidity(String),
    /// The mailbox's UIDVALIDITY changed since the message was listed, so its UID means nothing now.
    #[error("{0} changed on the server; wait for it to resync")]
    UidValidityChanged(String),
    #[error("the message is no longer on the server")]
    MessageGone,
    #[error("SMTP error: {0}")]
    Smtp(#[source] lettre::transport::smtp::Error),
    #[error("{0:?} isn't a valid email address")]
    Address(String),
    #[error("the message has no recipients")]
    NoRecipients,
    #[error("couldn't build the message: {0}")]
    Message(String),
    #[error(
        "Google sign-in isn't set up in this build: set SIMPLYMAIL_GOOGLE_CLIENT_ID (and \
         SIMPLYMAIL_GOOGLE_CLIENT_SECRET) to an OAuth client's"
    )]
    NoOAuthClient,
    #[error("couldn't listen for the browser's sign-in: {0}")]
    Listen(#[source] std::io::Error),
    #[error("couldn't reach Google: {0}")]
    Http(#[source] reqwest::Error),
    #[error("Google sign-in failed: {0}")]
    OAuth(String),
    /// Google no longer accepts the refresh token (revoked, expired, or the password changed).
    #[error("Google refused the sign-in ({0}); sign in with Google again")]
    SignInRefused(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
