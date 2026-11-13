//! SMTP: sending a message, and checking that a server accepts a login.
//!
//! Each call opens a connection of its own; nothing is kept open between sends.

use std::str::FromStr as _;
use std::time::Duration;

use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, Mailboxes, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::{Credentials as Login, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport as _, Message, Tokio1Executor};
use simplymail_core::account::{Security, Server};
use simplymail_core::message::{Address, Outgoing};
use simplymail_core::secrets::ExposeSecret as _;
use tokio::time::timeout;

use crate::auth::Secret;
use crate::{Credentials, Error, Result};

/// How long to wait for the server to answer any one command.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a whole send may take, connecting included.
const SEND_TIMEOUT: Duration = Duration::from_secs(120);

/// A message that was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    /// The `Message-ID` it was sent with, without angle brackets.
    pub message_id: String,
    /// The message as sent, plus its Bcc header, to keep in the Sent mailbox.
    pub copy: Vec<u8>,
}

/// Sends `message` through `server`, logging in with `credentials`.
///
/// Bcc recipients get the message but aren't listed in what's sent; they are in the returned copy.
/// Certificates are checked against the operating system's trust store.
///
/// # Errors
///
/// Returns an error when an address is invalid, there are no recipients, the server can't be
/// reached, the login is refused, or the server refuses the message.
pub async fn send(server: &Server, credentials: &Credentials, message: &Outgoing) -> Result<Sent> {
    let from = mailbox(&message.from)?;
    let message_id = new_message_id(&from);
    let sent = build(message, &message_id, false)?;
    let copy = build(message, &message_id, true)?.formatted();
    let transport = transport(server, credentials).await?;
    timeout(SEND_TIMEOUT, transport.send(sent))
        .await
        .map_err(|_elapsed| Error::Timeout)?
        .map_err(Error::Smtp)?;
    Ok(Sent { message_id, copy })
}

/// Checks that `server` accepts `credentials`, then says goodbye.
///
/// # Errors
///
/// Returns the reason the connection or login failed.
pub async fn check_login(server: &Server, credentials: &Credentials) -> Result<()> {
    let transport = transport(server, credentials).await?;
    // `test_connection` logs in before checking the connection is alive.
    let alive = timeout(SEND_TIMEOUT, transport.test_connection())
        .await
        .map_err(|_elapsed| Error::Timeout)?
        .map_err(Error::Smtp)?;
    if alive { Ok(()) } else { Err(Error::Closed) }
}

/// Reads a comma-separated list of addresses as someone types them: `Ada <ada@example.com>, bob@example.com`.
/// Blank input is an empty list.
///
/// # Errors
///
/// Returns [`Error::Address`] when the list can't be read.
pub fn parse_addresses(list: &str) -> Result<Vec<Address>> {
    if list.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mailboxes =
        Mailboxes::from_str(list).map_err(|_invalid| Error::Address(list.trim().to_owned()))?;
    Ok(mailboxes
        .into_iter()
        .map(|m| Address {
            name: m.name.filter(|n| !n.is_empty()),
            email: Some(m.email.to_string()),
        })
        .collect())
}

/// A transport that logs in with `credentials`: `AUTH PLAIN` or `LOGIN` with a password, `AUTH
/// XOAUTH2` with OAuth.
async fn transport(
    server: &Server,
    credentials: &Credentials,
) -> Result<AsyncSmtpTransport<Tokio1Executor>> {
    let (secret, mechanisms) = match credentials.secret().await? {
        Secret::Password(password) => (password, vec![Mechanism::Plain, Mechanism::Login]),
        Secret::Bearer(token) => (token, vec![Mechanism::Xoauth2]),
    };
    let tls = match server.security {
        Security::Tls => Tls::Wrapper(tls_parameters(&server.host)?),
        Security::StartTls => Tls::Required(tls_parameters(&server.host)?),
        Security::None => Tls::None,
    };
    Ok(
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&server.host)
            .port(server.port)
            .tls(tls)
            .credentials(Login::new(
                server.username.clone(),
                secret.expose_secret().to_owned(),
            ))
            .authentication(mechanisms)
            .timeout(Some(COMMAND_TIMEOUT))
            .build(),
    )
}

fn tls_parameters(host: &str) -> Result<TlsParameters> {
    TlsParameters::new_rustls(host.to_owned()).map_err(Error::Smtp)
}

/// Builds the message to send under `id`, keeping its Bcc header if `keep_bcc`.
fn build(message: &Outgoing, id: &str, keep_bcc: bool) -> Result<Message> {
    if message.to.is_empty() && message.cc.is_empty() && message.bcc.is_empty() {
        return Err(Error::NoRecipients);
    }
    let mut builder = Message::builder()
        .from(mailbox(&message.from)?)
        .subject(message.subject.clone())
        .message_id(Some(format!("<{id}>")));
    for to in &message.to {
        builder = builder.to(mailbox(to)?);
    }
    for cc in &message.cc {
        builder = builder.cc(mailbox(cc)?);
    }
    for bcc in &message.bcc {
        builder = builder.bcc(mailbox(bcc)?);
    }
    if let Some(parent) = &message.in_reply_to {
        builder = builder.in_reply_to(format!("<{parent}>"));
    }
    if !message.references.is_empty() {
        let ids: Vec<String> = message
            .references
            .iter()
            .map(|r| format!("<{r}>"))
            .collect();
        builder = builder.references(ids.join(" "));
    }
    if keep_bcc {
        builder = builder.keep_bcc();
    }
    let built = match &message.html {
        Some(html) if message.images.is_empty() => builder.multipart(
            MultiPart::alternative_plain_html(message.text.clone(), html.clone()),
        ),
        // The images go with the HTML that shows them, in a multipart/related.
        Some(html) => {
            let mut related = MultiPart::related().singlepart(SinglePart::html(html.clone()));
            for image in &message.images {
                let content_type = ContentType::parse(&image.mime).map_err(|_invalid| {
                    Error::Message(format!("{} isn't a MIME type", image.mime))
                })?;
                related = related.singlepart(
                    Attachment::new_inline_with_name(image.cid.clone(), image.name.clone())
                        .body(image.data.to_vec(), content_type),
                );
            }
            builder.multipart(
                MultiPart::alternative()
                    .singlepart(SinglePart::plain(message.text.clone()))
                    .multipart(related),
            )
        }
        None => builder
            .header(ContentType::TEXT_PLAIN)
            .body(message.text.clone()),
    };
    built.map_err(|e| Error::Message(e.to_string()))
}

fn mailbox(address: &Address) -> Result<Mailbox> {
    let email = address.email.as_deref().unwrap_or_default();
    let email = email
        .parse()
        .map_err(|_invalid| Error::Address(email.to_owned()))?;
    Ok(Mailbox::new(
        address.name.clone().filter(|n| !n.is_empty()),
        email,
    ))
}

/// A fresh `Message-ID`, under the sender's domain so it says nothing about this machine.
fn new_message_id(from: &Mailbox) -> String {
    let random: String = std::iter::repeat_with(fastrand::alphanumeric)
        .take(24)
        .collect();
    format!("{random}@{}", from.email.domain())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::net::TcpListener;

    use super::*;

    fn address(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(str::to_owned),
            email: Some(email.to_owned()),
        }
    }

    fn outgoing() -> Outgoing {
        Outgoing {
            from: address(Some("Me"), "me@example.com"),
            to: vec![address(Some("Ada"), "ada@example.com")],
            cc: vec![address(None, "bob@example.com")],
            bcc: vec![address(None, "secret@example.com")],
            subject: "Café?".into(),
            text: "Hello there.\n".into(),
            html: None,
            images: Vec::new(),
            in_reply_to: Some("parent@example.com".into()),
            references: vec!["root@example.com".into(), "parent@example.com".into()],
        }
    }

    #[test]
    fn typed_address_lists_are_read() {
        assert_eq!(
            parse_addresses("Ada <ada@example.com>, bob@example.com").unwrap(),
            vec![
                address(Some("Ada"), "ada@example.com"),
                address(None, "bob@example.com")
            ]
        );
        assert_eq!(parse_addresses("  ").unwrap(), vec![]);
        assert!(parse_addresses("not an address").is_err(), "no @");
    }

    #[test]
    fn a_message_needs_recipients_and_valid_addresses() {
        let none = Outgoing {
            to: vec![],
            cc: vec![],
            bcc: vec![],
            ..outgoing()
        };
        assert!(matches!(
            build(&none, "x@y", false),
            Err(Error::NoRecipients)
        ));
        let bad = Outgoing {
            to: vec![address(None, "nope")],
            ..outgoing()
        };
        assert!(matches!(build(&bad, "x@y", false), Err(Error::Address(a)) if a == "nope"));
    }

    #[test]
    fn replies_carry_the_thread_and_ids_name_no_machine() {
        let id = new_message_id(&mailbox(&outgoing().from).unwrap());
        let raw = String::from_utf8(build(&outgoing(), &id, false).unwrap().formatted()).unwrap();
        assert!(id.ends_with("@example.com"), "got {id}");
        assert!(raw.contains(&format!("Message-ID: <{id}>")), "{raw}");
        assert!(raw.contains("In-Reply-To: <parent@example.com>"), "{raw}");
        assert!(
            raw.contains("References: <root@example.com> <parent@example.com>"),
            "{raw}"
        );
        assert!(!raw.contains("secret@example.com"), "Bcc is hidden: {raw}");
        let copy = String::from_utf8(build(&outgoing(), &id, true).unwrap().formatted()).unwrap();
        assert!(
            copy.contains("Bcc: secret@example.com"),
            "the copy keeps it: {copy}"
        );
    }

    #[test]
    fn html_is_sent_alongside_the_text() {
        let message = Outgoing {
            html: Some("<p>Hello <b>there</b>.</p>".into()),
            ..outgoing()
        };
        let raw = String::from_utf8(build(&message, "x@example.com", false).unwrap().formatted())
            .unwrap();
        assert!(raw.contains("multipart/alternative"), "{raw}");
        assert!(raw.contains("Content-Type: text/plain"), "{raw}");
        assert!(raw.contains("Content-Type: text/html"), "{raw}");
        assert!(raw.contains("<b>there</b>"), "{raw}");
        let plain = String::from_utf8(
            build(&outgoing(), "x@example.com", false)
                .unwrap()
                .formatted(),
        )
        .unwrap();
        assert!(
            !plain.contains("multipart"),
            "text alone stays one part: {plain}"
        );
    }

    #[test]
    fn inline_images_go_with_the_html() {
        let message = Outgoing {
            html: Some("<p>Ada</p><img src=\"cid:logo@simplymail\">".into()),
            images: vec![simplymail_core::message::InlineImage {
                cid: "logo@simplymail".into(),
                name: "logo.png".into(),
                mime: "image/png".into(),
                data: b"\x89PNG".as_slice().into(),
            }],
            ..outgoing()
        };
        let raw = String::from_utf8(build(&message, "x@example.com", false).unwrap().formatted())
            .unwrap();
        let (alternative, related) = (
            raw.find("multipart/alternative").unwrap(),
            raw.find("multipart/related").unwrap(),
        );
        assert!(
            alternative < related,
            "related sits inside alternative: {raw}"
        );
        assert!(raw.contains("Content-ID: <logo@simplymail>"), "{raw}");
        assert!(raw.contains("Content-Disposition: inline"), "{raw}");
        assert!(raw.contains("Content-Type: image/png"), "{raw}");

        let text_only = Outgoing {
            html: None,
            ..message
        };
        let raw = String::from_utf8(
            build(&text_only, "x@example.com", false)
                .unwrap()
                .formatted(),
        )
        .unwrap();
        assert!(!raw.contains("image/png"), "no HTML, no images: {raw}");
    }

    /// What a fake SMTP server was told.
    #[derive(Debug, Default)]
    struct Transcript {
        auth: Vec<String>,
        recipients: Vec<String>,
        data: String,
    }

    /// Starts an SMTP server for one connection that accepts the login `me` / `secret` and any mail.
    async fn fake_server() -> (Server, Arc<Mutex<Transcript>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let transcript = Arc::new(Mutex::new(Transcript::default()));
        let seen = Arc::clone(&transcript);
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (read, mut write) = socket.into_split();
            let mut lines = BufReader::new(read).lines();
            write.write_all(b"220 fake ESMTP\r\n").await.unwrap();
            let mut in_data = false;
            while let Some(line) = lines.next_line().await.unwrap() {
                let reply: &[u8] = if in_data {
                    if line == "." {
                        in_data = false;
                        b"250 queued\r\n"
                    } else {
                        seen.lock().unwrap().data.extend([line.as_str(), "\n"]);
                        continue;
                    }
                } else if line.starts_with("EHLO") {
                    b"250-fake\r\n250 AUTH PLAIN LOGIN XOAUTH2\r\n"
                } else if let Some(auth) = line.strip_prefix("AUTH PLAIN ") {
                    seen.lock().unwrap().auth.push(auth.to_owned());
                    // base64("\0me\0secret")
                    if auth == "AG1lAHNlY3JldA==" {
                        b"235 ok\r\n"
                    } else {
                        b"535 5.7.8 bad credentials\r\n"
                    }
                } else if let Some(auth) = line.strip_prefix("AUTH XOAUTH2 ") {
                    seen.lock().unwrap().auth.push(auth.to_owned());
                    // base64("user=me\x01auth=Bearer ya29.token\x01\x01")
                    if auth == "dXNlcj1tZQFhdXRoPUJlYXJlciB5YTI5LnRva2VuAQE=" {
                        b"235 ok\r\n"
                    } else {
                        b"535 5.7.8 bad token\r\n"
                    }
                } else if line.starts_with("MAIL FROM") || line == "NOOP" || line == "RSET" {
                    b"250 ok\r\n"
                } else if let Some(to) = line.strip_prefix("RCPT TO:") {
                    seen.lock().unwrap().recipients.push(to.to_owned());
                    b"250 ok\r\n"
                } else if line == "DATA" {
                    in_data = true;
                    b"354 go on\r\n"
                } else if line == "QUIT" {
                    write.write_all(b"221 bye\r\n").await.unwrap();
                    break;
                } else {
                    b"500 what?\r\n"
                };
                write.write_all(reply).await.unwrap();
            }
        });
        let server = Server {
            host: "127.0.0.1".into(),
            port,
            security: Security::None,
            username: "me".into(),
        };
        (server, transcript)
    }

    #[tokio::test]
    async fn mail_goes_to_every_recipient() {
        let (server, transcript) = fake_server().await;
        let sent = send(&server, &password("secret"), &outgoing())
            .await
            .unwrap();
        let t = transcript.lock().unwrap();
        assert_eq!(
            t.recipients,
            vec![
                "<ada@example.com>",
                "<bob@example.com>",
                "<secret@example.com>"
            ]
        );
        assert!(
            t.data.contains(&sent.message_id),
            "sent with the returned id"
        );
        assert!(!t.data.contains("Bcc:"), "{}", t.data);
        assert!(t.data.contains("Hello there."), "{}", t.data);
    }

    #[tokio::test]
    async fn a_wrong_password_is_reported() {
        let (server, _) = fake_server().await;
        let error = check_login(&server, &password("wrong")).await.unwrap_err();
        assert!(error.to_string().contains("535"), "got {error}");
    }

    fn password(p: &str) -> Credentials {
        Credentials::Password(p.into())
    }

    #[tokio::test]
    async fn an_oauth_token_logs_in_with_xoauth2() {
        let (server, transcript) = fake_server().await;
        let tokens = crate::oauth::Tokens::with_access("ya29.token", Duration::from_secs(3600));
        check_login(&server, &Credentials::OAuth(Arc::new(tokens)))
            .await
            .unwrap();
        assert_eq!(transcript.lock().unwrap().auth.len(), 1, "logged in once");
    }

    #[tokio::test]
    async fn the_right_password_checks_out() {
        let (server, transcript) = fake_server().await;
        check_login(&server, &password("secret")).await.unwrap();
        assert_eq!(transcript.lock().unwrap().auth.len(), 1, "logged in once");
    }
}
