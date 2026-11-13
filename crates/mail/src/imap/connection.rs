//! Opening an authenticated IMAP session over TLS, STARTTLS or plain TCP.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use simplymail_core::account::{Security, Server};
use simplymail_core::secrets::ExposeSecret as _;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;

use crate::auth::Secret;
use crate::{Credentials, Error, Result};

/// How long to wait for the server during connection setup.
const SETUP_TIMEOUT: Duration = Duration::from_secs(30);

/// An authenticated IMAP session.
pub type Session = async_imap::Session<Stream>;

/// The transport under a [`Session`]: TLS, or plain TCP for [`Security::None`].
#[derive(Debug)]
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

/// Connects to `server` and logs in with `credentials`: `LOGIN` with a password, `AUTHENTICATE
/// XOAUTH2` with OAuth.
///
/// Certificates are checked against the operating system's trust store.
///
/// # Errors
///
/// Returns an error when the server can't be reached, TLS fails, or the login is refused.
pub async fn connect(server: &Server, credentials: &Credentials) -> Result<Session> {
    timeout(SETUP_TIMEOUT, connect_inner(server, credentials))
        .await
        .map_err(|_elapsed| Error::Timeout)?
}

async fn connect_inner(server: &Server, credentials: &Credentials) -> Result<Session> {
    let tcp = TcpStream::connect((server.host.as_str(), server.port))
        .await
        .map_err(|source| Error::Connect {
            host: server.host.clone(),
            port: server.port,
            source,
        })?;
    let mut client = match server.security {
        Security::Tls => async_imap::Client::new(Stream::Tls(tls(tcp, &server.host).await?)),
        Security::StartTls => {
            let mut plain = async_imap::Client::new(tcp);
            greeting(&mut plain).await?;
            plain.run_command_and_check_ok("STARTTLS", None).await?;
            let tcp = plain.into_inner();
            // The greeting was read before STARTTLS, so don't wait for another one below.
            let client = async_imap::Client::new(Stream::Tls(tls(tcp, &server.host).await?));
            return login(client, server, credentials).await;
        }
        Security::None => async_imap::Client::new(Stream::Plain(tcp)),
    };
    greeting(&mut client).await?;
    login(client, server, credentials).await
}

async fn login(
    client: async_imap::Client<Stream>,
    server: &Server,
    credentials: &Credentials,
) -> Result<Session> {
    let logged_in = match credentials.secret().await? {
        Secret::Password(password) => {
            client
                .login(&server.username, password.expose_secret())
                .await
        }
        Secret::Bearer(token) => {
            let response = crate::oauth::xoauth2(&server.username, token.expose_secret());
            client
                .authenticate("XOAUTH2", XOAuth2(Some(response)))
                .await
        }
    };
    match logged_in {
        Ok(session) => Ok(session),
        Err((e, _client)) => {
            credentials.refused().await;
            Err(Error::Login(e))
        }
    }
}

/// Answers the server's first `XOAUTH2` challenge with the initial response. Answers any later one,
/// which carries the reason a token was refused, with nothing, as the mechanism asks.
struct XOAuth2(Option<String>);

impl async_imap::Authenticator for XOAuth2 {
    type Response = String;

    fn process(&mut self, _challenge: &[u8]) -> String {
        self.0.take().unwrap_or_default()
    }
}

async fn greeting<T>(client: &mut async_imap::Client<T>) -> Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin + std::fmt::Debug + Send,
{
    client
        .read_response()
        .await
        .map_err(|e| Error::Imap(e.into()))?
        .ok_or(Error::Closed)?;
    Ok(())
}

async fn tls(tcp: TcpStream, host: &str) -> Result<Box<TlsStream<TcpStream>>> {
    let config = crate::tls::client_config()?;
    let name =
        ServerName::try_from(host.to_owned()).map_err(|_invalid| Error::ServerName(host.into()))?;
    let stream = TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map_err(Error::Tls)?;
    Ok(Box::new(stream))
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// Checks that `server` accepts `credentials`, then logs out.
///
/// # Errors
///
/// Returns the reason the login failed, as [`connect`] does.
pub async fn check_login(server: &Server, credentials: &Credentials) -> Result<()> {
    let mut session = connect(server, credentials).await?;
    // The login worked; a failed goodbye doesn't change that.
    let _ignored = session.logout().await;
    Ok(())
}
