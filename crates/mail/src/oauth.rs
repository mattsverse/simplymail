//! Google OAuth 2.0 for installed apps: signing in through the browser, and trading the refresh
//! token that grants for the short-lived access tokens IMAP and SMTP take (SASL `XOAUTH2`).
//!
//! Sign-in is the authorization-code flow with PKCE and a loopback redirect, as Google asks of
//! desktop apps. [`SignIn::start`] listens on a random port of 127.0.0.1 and builds the URL to open.
//! The user approves in their browser, which Google then sends back to that port with a code, and
//! [`SignIn::finish`] trades the code for tokens.

use std::fmt;
use std::net::Ipv4Addr;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Url;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::Deserialize;
use simplymail_core::secrets::{ExposeSecret as _, SecretString};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{Instant, timeout};

use crate::{Error, Result};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// Full mail access, the only scope IMAP and SMTP accept; and the address, to name the account.
const SCOPES: &str = "https://mail.google.com/ openid email";
/// The client SimplyMail was built with, if any. The environment can override it at run time.
const BUILT_CLIENT_ID: Option<&str> = option_env!("SIMPLYMAIL_GOOGLE_CLIENT_ID");
const BUILT_CLIENT_SECRET: Option<&str> = option_env!("SIMPLYMAIL_GOOGLE_CLIENT_SECRET");

/// How long to wait for the user to finish in the browser.
const SIGN_IN_TIMEOUT: Duration = Duration::from_mins(10);
/// How long the browser may take to send its redirect request once connected.
const REDIRECT_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// The most of a redirect request read: its request line is all that matters.
const REDIRECT_MAX_BYTES: usize = 16 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// An access token is refreshed this long before it expires, so it can't lapse mid-login.
const EXPIRY_MARGIN: Duration = Duration::from_mins(5);

/// The OAuth client SimplyMail identifies itself to Google as.
///
/// Google issues desktop apps a client secret too, but it can't stay secret in a shipped binary;
/// PKCE is what protects the sign-in.
#[derive(Debug, Clone)]
pub struct Client {
    id: String,
    secret: Option<SecretString>,
    auth_url: String,
    token_url: String,
}

impl Client {
    /// A client with this ID and, if Google issued one, secret.
    #[must_use]
    pub fn new(id: impl Into<String>, secret: Option<SecretString>) -> Self {
        Self {
            id: id.into(),
            secret,
            auth_url: AUTH_URL.to_owned(),
            token_url: TOKEN_URL.to_owned(),
        }
    }

    /// The client to sign in to Google with: `SIMPLYMAIL_GOOGLE_CLIENT_ID` and
    /// `SIMPLYMAIL_GOOGLE_CLIENT_SECRET` from the environment, else the ones this build was compiled
    /// with.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoOAuthClient`] when there's no client ID either way.
    pub fn google() -> Result<Self> {
        let nonempty = |s: String| Some(s).filter(|s| !s.trim().is_empty());
        let env = |name| std::env::var(name).ok().and_then(nonempty);
        if let Some(id) = env("SIMPLYMAIL_GOOGLE_CLIENT_ID") {
            return Ok(Self::new(
                id,
                env("SIMPLYMAIL_GOOGLE_CLIENT_SECRET").map(SecretString::from),
            ));
        }
        let id = BUILT_CLIENT_ID
            .map(str::to_owned)
            .and_then(nonempty)
            .ok_or(Error::NoOAuthClient)?;
        let secret = BUILT_CLIENT_SECRET
            .map(str::to_owned)
            .and_then(nonempty)
            .map(SecretString::from);
        Ok(Self::new(id, secret))
    }

    /// Asks the token endpoint for tokens with `form`, adding the client's credentials.
    async fn request_token(&self, form: &[(&str, &str)]) -> Result<TokenResponse> {
        let mut form = form.to_vec();
        form.push(("client_id", &self.id));
        if let Some(secret) = &self.secret {
            form.push(("client_secret", secret.expose_secret()));
        }
        let http = reqwest::Client::builder()
            .use_preconfigured_tls(crate::tls::client_config()?)
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(Error::Http)?;
        let response = http
            .post(&self.token_url)
            .form(&form)
            .send()
            .await
            .map_err(Error::Http)?;
        let status = response.status();
        let body = response.bytes().await.map_err(Error::Http)?;
        if status.is_success() {
            return serde_json::from_slice(&body)
                .map_err(|e| Error::OAuth(format!("Google's token response was unreadable: {e}")));
        }
        Err(match serde_json::from_slice::<ErrorResponse>(&body) {
            // The refresh token was revoked or expired, or the code was used or is stale.
            Ok(e) if e.error == "invalid_grant" => {
                Error::SignInRefused(e.error_description.unwrap_or(e.error))
            }
            Ok(ErrorResponse {
                error,
                error_description: Some(d),
            }) => Error::OAuth(format!("{error}: {d}")),
            Ok(ErrorResponse { error, .. }) => Error::OAuth(error),
            Err(_unreadable) => Error::OAuth(format!("Google's token endpoint answered {status}")),
        })
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: String,
    error_description: Option<String>,
}

/// A sign-in waiting for the user to approve it in their browser.
#[derive(Debug)]
pub struct SignIn {
    client: Client,
    listener: TcpListener,
    url: String,
    redirect_uri: String,
    verifier: SecretString,
    state: String,
}

impl SignIn {
    /// Starts listening for Google's redirect, and builds the URL to send the user to. With
    /// `login_hint`, an email address, Google suggests that account.
    ///
    /// # Errors
    ///
    /// Returns an error when no local port can be opened or no random numbers can be had.
    pub async fn start(client: Client, login_hint: Option<&str>) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(Error::Listen)?;
        let port = listener.local_addr().map_err(Error::Listen)?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}");
        let verifier = random_token()?;
        let state = random_token()?;
        let mut url = Url::parse(&client.auth_url)
            .map_err(|e| Error::OAuth(format!("bad authorization URL: {e}")))?;
        url.query_pairs_mut()
            .append_pair("client_id", &client.id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", SCOPES)
            .append_pair("code_challenge", &challenge(&verifier))
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state);
        if let Some(hint) = login_hint.map(str::trim).filter(|h| !h.is_empty()) {
            url.query_pairs_mut().append_pair("login_hint", hint);
        }
        Ok(Self {
            client,
            listener,
            url: url.into(),
            redirect_uri,
            verifier: SecretString::from(verifier),
            state,
        })
    }

    /// The page to open in the user's browser.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Waits for the user to approve or refuse in the browser, then trades the code for tokens.
    ///
    /// # Errors
    ///
    /// Returns an error when the user refuses, doesn't finish in time, or Google won't grant tokens.
    pub async fn finish(self) -> Result<Granted> {
        let code = timeout(SIGN_IN_TIMEOUT, receive_code(&self.listener, &self.state))
            .await
            .map_err(|_elapsed| Error::OAuth("the sign-in wasn't finished in time".into()))??;
        drop(self.listener);
        let response = self
            .client
            .request_token(&[
                ("grant_type", "authorization_code"),
                ("code", code.expose_secret()),
                ("redirect_uri", &self.redirect_uri),
                ("code_verifier", self.verifier.expose_secret()),
            ])
            .await?;
        let refresh_token = response
            .refresh_token
            .clone()
            .map(SecretString::from)
            .ok_or_else(|| Error::OAuth("Google didn't grant lasting access".into()))?;
        let email = response
            .id_token
            .as_deref()
            .and_then(email_from_id_token)
            .ok_or_else(|| Error::OAuth("Google didn't say which account signed in".into()))?;
        let tokens = Tokens::new(self.client, refresh_token.clone());
        tokens.store(response).await;
        Ok(Granted {
            email,
            refresh_token,
            tokens,
        })
    }
}

/// What a finished [`SignIn`] grants.
#[derive(Debug)]
pub struct Granted {
    /// The address of the Google account that signed in.
    pub email: String,
    /// The lasting token, to keep in the secret store.
    pub refresh_token: SecretString,
    /// Tokens ready to log in with, already holding a fresh access token.
    pub tokens: Tokens,
}

/// A refresh token, and the access token it last bought.
///
/// Shared between everything an account does, so the access token is refreshed once for all.
pub struct Tokens {
    client: Client,
    refresh_token: SecretString,
    access: Mutex<Option<Access>>,
}

struct Access {
    token: SecretString,
    expires: Instant,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("client", &self.client.id)
            .finish_non_exhaustive()
    }
}

impl Tokens {
    #[must_use]
    pub fn new(client: Client, refresh_token: SecretString) -> Self {
        Self {
            client,
            refresh_token,
            access: Mutex::new(None),
        }
    }

    /// An access token good for a few minutes at least, refreshed if need be.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SignInRefused`] when Google no longer accepts the refresh token, so the user
    /// must sign in again, or another error when Google can't be reached.
    pub async fn access_token(&self) -> Result<SecretString> {
        let mut access = self.access.lock().await;
        if let Some(a) = access.as_ref()
            && a.expires > Instant::now() + EXPIRY_MARGIN
        {
            return Ok(a.token.clone());
        }
        let response = self
            .client
            .request_token(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", self.refresh_token.expose_secret()),
            ])
            .await?;
        let fresh = Access::from(response);
        let token = fresh.token.clone();
        *access = Some(fresh);
        drop(access);
        Ok(token)
    }

    /// Drops the current access token, after a server refused it, so the next login gets a new one.
    pub async fn forget(&self) {
        *self.access.lock().await = None;
    }

    async fn store(&self, response: TokenResponse) {
        *self.access.lock().await = Some(Access::from(response));
    }

    /// Tokens holding `access_token`, good for `lasts`, for tests.
    #[cfg(test)]
    pub(crate) fn with_access(access_token: &str, lasts: Duration) -> Self {
        Self {
            client: Client::new("test-client", None),
            refresh_token: SecretString::from("refresh"),
            access: Mutex::new(Some(Access {
                token: SecretString::from(access_token),
                expires: Instant::now() + lasts,
            })),
        }
    }
}

impl From<TokenResponse> for Access {
    fn from(response: TokenResponse) -> Self {
        Self {
            token: SecretString::from(response.access_token),
            expires: Instant::now() + Duration::from_secs(response.expires_in),
        }
    }
}

/// Accepts connections to `listener` until one is Google's redirect for this sign-in (it carries
/// `state`), answers it, and returns its code.
async fn receive_code(listener: &TcpListener, state: &str) -> Result<SecretString> {
    loop {
        let (mut socket, _peer) = listener.accept().await.map_err(Error::Listen)?;
        // A browser that connects and stalls mustn't hold up the sign-in.
        let Ok(Ok(target)) = timeout(REDIRECT_READ_TIMEOUT, request_target(&mut socket)).await
        else {
            continue;
        };
        let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
            respond(&mut socket, "400 Bad Request", "That's not a sign-in.").await;
            continue;
        };
        let param = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        // Anything else, such as /favicon.ico, or a redirect for some other sign-in.
        if param("state").as_deref() != Some(state) {
            respond(&mut socket, "404 Not Found", "Nothing here.").await;
            continue;
        }
        if let Some(code) = param("code") {
            respond(
                &mut socket,
                "200 OK",
                "You're signed in. You can close this tab and go back to SimplyMail.",
            )
            .await;
            return Ok(SecretString::from(code));
        }
        let error = param("error").unwrap_or_else(|| "no code was given".into());
        respond(
            &mut socket,
            "200 OK",
            "The sign-in was cancelled. You can close this tab.",
        )
        .await;
        return Err(if error == "access_denied" {
            Error::OAuth("access was denied in the browser".into())
        } else {
            Error::OAuth(error)
        });
    }
}

/// Reads an HTTP request's head and returns its target, such as `/?code=…&state=…`.
async fn request_target(socket: &mut TcpStream) -> std::io::Result<String> {
    let mut head = Vec::new();
    let mut buf = [0; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < REDIRECT_MAX_BYTES {
        let n = socket.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(buf.get(..n).unwrap_or_default());
    }
    let head = String::from_utf8_lossy(&head);
    let line = head.lines().next().unwrap_or_default();
    match line.split(' ').collect::<Vec<_>>().as_slice() {
        ["GET", target, _version] if target.starts_with('/') => Ok((*target).to_owned()),
        _ => Err(std::io::Error::other("not a GET request")),
    }
}

/// Answers the browser with a short page. It may have gone already, which changes nothing.
async fn respond(socket: &mut TcpStream, status: &str, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>SimplyMail</title>\
         <body style=\"font:16px system-ui;margin:4em auto;max-width:32em\"><p>{message}</p>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ignored = socket.write_all(response.as_bytes()).await;
    let _ignored = socket.shutdown().await;
}

/// 32 random bytes, base64url: a PKCE code verifier (43 characters) or a `state`.
fn random_token() -> Result<String> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_unspecified| Error::OAuth("no random numbers available".into()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The PKCE `S256` challenge for `verifier`.
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()))
}

/// The `email` claim of an OIDC ID token.
///
/// The signature isn't checked: the token came straight from Google's token endpoint over TLS,
/// which OIDC Core (section 3.1.3.7) allows to stand in for it.
fn email_from_id_token(id_token: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Claims {
        email: String,
    }
    let payload = id_token.split('.').nth(1)?;
    let json = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice::<Claims>(&json)
        .ok()
        .map(|c| c.email)
}

/// The SASL `XOAUTH2` initial response, before base64: Google's format for IMAP and SMTP.
pub(crate) fn xoauth2(user: &str, access_token: &str) -> String {
    format!("user={user}\x01auth=Bearer {access_token}\x01\x01")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::io::{AsyncBufReadExt as _, BufReader};

    use super::*;

    #[test]
    fn the_challenge_is_rfc_7636s() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifiers_are_long_enough_and_url_safe() {
        let v = random_token().unwrap();
        assert_eq!(v.len(), 43, "{v}");
        assert!(
            v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "{v}"
        );
        assert_ne!(v, random_token().unwrap(), "random");
    }

    #[test]
    fn the_email_is_read_from_the_id_token() {
        let claims = URL_SAFE_NO_PAD.encode(r#"{"email":"me@gmail.com","sub":"1"}"#);
        assert_eq!(
            email_from_id_token(&format!("header.{claims}.signature")).as_deref(),
            Some("me@gmail.com")
        );
        assert_eq!(email_from_id_token("not-a-jwt"), None);
    }

    #[test]
    fn xoauth2_is_googles_format() {
        assert_eq!(
            xoauth2("me@gmail.com", "ya29.token"),
            "user=me@gmail.com\x01auth=Bearer ya29.token\x01\x01"
        );
    }

    #[tokio::test]
    async fn the_sign_in_url_asks_for_mail_with_pkce() {
        let sign_in = SignIn::start(Client::new("id-123", None), Some("me@gmail.com"))
            .await
            .unwrap();
        let url = Url::parse(sign_in.url()).unwrap();
        let param = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        assert_eq!(url.host_str(), Some("accounts.google.com"));
        assert_eq!(param("client_id").as_deref(), Some("id-123"));
        assert_eq!(param("scope").as_deref(), Some(SCOPES));
        assert_eq!(param("code_challenge_method").as_deref(), Some("S256"));
        assert_eq!(param("login_hint").as_deref(), Some("me@gmail.com"));
        assert_eq!(
            param("code_challenge"),
            Some(challenge(sign_in.verifier.expose_secret()))
        );
        let port = sign_in.listener.local_addr().unwrap().port();
        assert_eq!(
            param("redirect_uri"),
            Some(format!("http://127.0.0.1:{port}"))
        );
    }

    /// Sends a GET for `target` to `port`, returning the status line of the answer.
    async fn browse(port: u16, target: &str) -> String {
        let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        socket
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut status = String::new();
        BufReader::new(socket).read_line(&mut status).await.unwrap();
        status.trim_end().to_owned()
    }

    #[tokio::test]
    async fn only_the_redirect_for_this_sign_in_is_taken() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = tokio::spawn(async move {
            assert_eq!(browse(port, "/favicon.ico").await, "HTTP/1.1 404 Not Found");
            assert_eq!(
                browse(port, "/?code=forged&state=other").await,
                "HTTP/1.1 404 Not Found",
                "another state is ignored"
            );
            assert_eq!(
                browse(port, "/?code=4%2Fabc&state=s1").await,
                "HTTP/1.1 200 OK"
            );
        });
        let code = receive_code(&listener, "s1").await.unwrap();
        assert_eq!(code.expose_secret(), "4/abc");
        browser.await.unwrap();
    }

    #[tokio::test]
    async fn a_refusal_in_the_browser_is_an_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser =
            tokio::spawn(async move { browse(port, "/?error=access_denied&state=s1").await });
        let error = receive_code(&listener, "s1").await.unwrap_err();
        assert!(error.to_string().contains("denied"), "got {error}");
        browser.await.unwrap();
    }

    /// Starts a token endpoint that answers each request with the next of `answers` (status, JSON),
    /// and records the request bodies.
    async fn token_endpoint(
        answers: Vec<(&'static str, &'static str)>,
    ) -> (Client, Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&bodies);
        tokio::spawn(async move {
            for (status, json) in answers {
                let (socket, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(socket);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap();
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).await.unwrap();
                seen.lock().unwrap().push(String::from_utf8(body).unwrap());
                let answer = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                reader.get_mut().write_all(answer.as_bytes()).await.unwrap();
            }
        });
        let client = Client {
            token_url: format!("http://127.0.0.1:{port}/token"),
            ..Client::new("id-123", Some(SecretString::from("shh")))
        };
        (client, bodies)
    }

    #[tokio::test]
    async fn access_tokens_are_refreshed_once_and_reused() {
        let (client, bodies) = token_endpoint(vec![(
            "200 OK",
            r#"{"access_token":"ya29.fresh","expires_in":3599,"token_type":"Bearer"}"#,
        )])
        .await;
        let tokens = Tokens::new(client, SecretString::from("1//refresh"));
        assert_eq!(
            tokens.access_token().await.unwrap().expose_secret(),
            "ya29.fresh"
        );
        assert_eq!(
            tokens.access_token().await.unwrap().expose_secret(),
            "ya29.fresh",
            "cached"
        );
        let bodies = bodies.lock().unwrap().clone();
        assert_eq!(bodies.len(), 1, "one refresh: {bodies:?}");
        let body = bodies.first().unwrap();
        for part in [
            "grant_type=refresh_token",
            "refresh_token=1%2F%2Frefresh",
            "client_id=id-123",
            "client_secret=shh",
        ] {
            assert!(body.contains(part), "{part} missing from {body}");
        }
    }

    #[tokio::test]
    async fn a_revoked_refresh_token_asks_for_a_new_sign_in() {
        let (client, _) = token_endpoint(vec![(
            "400 Bad Request",
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        )])
        .await;
        let tokens = Tokens::new(client, SecretString::from("1//refresh"));
        let error = tokens.access_token().await.unwrap_err();
        assert!(matches!(error, Error::SignInRefused(_)), "got {error:?}");
        assert!(error.to_string().contains("sign in"), "got {error}");
    }

    #[tokio::test]
    async fn a_nearly_expired_token_is_replaced() {
        let (client, _) = token_endpoint(vec![(
            "200 OK",
            r#"{"access_token":"ya29.new","expires_in":3599}"#,
        )])
        .await;
        let tokens = Tokens {
            client,
            ..Tokens::with_access("ya29.old", Duration::from_secs(60))
        };
        assert_eq!(
            tokens.access_token().await.unwrap().expose_secret(),
            "ya29.new"
        );
    }
}
