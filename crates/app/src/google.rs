//! Adding a Gmail account by signing in with Google in the browser.

use std::sync::Arc;

use simplymail_core::account::{Account, Auth, Security, Server};
use simplymail_mail::oauth::{Client, SignIn};

use crate::{Login, Result, check_logins};

/// A Gmail account for `email`, with Google's IMAP and SMTP servers and Google sign-in.
#[must_use]
pub fn google_account(email: &str, display_name: Option<String>) -> Account {
    let server = |host: &str, port| Server {
        host: host.to_owned(),
        port,
        security: Security::Tls,
        username: email.to_owned(),
    };
    Account {
        id: email.to_owned(),
        display_name: display_name
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty()),
        imap: server("imap.gmail.com", 993),
        smtp: server("smtp.gmail.com", 465),
        auth: Auth::Google,
    }
}

/// Starts signing in with Google. Open [`SignIn::url`] in the browser, then pass the sign-in to
/// [`finish_google_sign_in`]. With `login_hint`, an email address, Google suggests that account.
///
/// # Errors
///
/// Returns an error when this build has no Google OAuth client, or no local port can be opened
/// for the browser to come back to.
pub async fn start_google_sign_in(login_hint: Option<&str>) -> Result<SignIn> {
    Ok(SignIn::start(Client::google()?, login_hint).await?)
}

/// Waits for the user to finish signing in, then checks the IMAP and SMTP logins. Returns the
/// account, named after the address Google signed in, and the login to save it with.
///
/// # Errors
///
/// Returns an error when the sign-in is refused or times out, or a login fails.
pub async fn finish_google_sign_in(
    sign_in: SignIn,
    display_name: Option<String>,
) -> Result<(Account, Login)> {
    let granted = sign_in.finish().await?;
    let account = google_account(&granted.email, display_name);
    let login = Login::Google {
        refresh_token: granted.refresh_token,
        tokens: Arc::new(granted.tokens),
    };
    check_logins(&account, &login).await?;
    Ok((account, login))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_accounts_use_googles_servers_and_sign_in() {
        let a = google_account("me@gmail.com", Some("  ".into()));
        assert_eq!((a.imap.host.as_str(), a.imap.port), ("imap.gmail.com", 993));
        assert_eq!((a.smtp.host.as_str(), a.smtp.port), ("smtp.gmail.com", 465));
        assert_eq!(a.smtp.username, "me@gmail.com");
        assert_eq!(a.auth, Auth::Google);
        assert_eq!(a.display_name, None, "a blank name is left out");
    }
}
