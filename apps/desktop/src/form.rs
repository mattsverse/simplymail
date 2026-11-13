//! The add-account form's fields, and turning them into a [`NewAccount`].

use rune::TextEdit;
use simplymail_app::NewAccount;
use simplymail_core::account::Security;
use tokio::task::AbortHandle;

/// The security choices, in the order the form shows them.
pub(crate) const SECURITY: [(&str, Security); 3] = [
    ("TLS", Security::Tls),
    ("STARTTLS", Security::StartTls),
    ("None", Security::None),
];

/// Which screen of the add-account form is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Step {
    /// Picking how to connect: Google, or IMAP and SMTP servers.
    #[default]
    Choose,
    /// Signing in with Google.
    Google,
    /// Typing in IMAP and SMTP servers and a password.
    Servers,
}

pub(crate) struct AccountForm {
    pub(crate) open: bool,
    pub(crate) step: Step,
    pub(crate) email: TextEdit,
    pub(crate) name: TextEdit,
    pub(crate) password: TextEdit,
    pub(crate) username: TextEdit,
    pub(crate) imap_host: TextEdit,
    pub(crate) imap_port: TextEdit,
    pub(crate) imap_security: Security,
    pub(crate) smtp_host: TextEdit,
    pub(crate) smtp_port: TextEdit,
    pub(crate) smtp_security: Security,
    /// Left empty when it's the IMAP one, as are the SMTP password's.
    pub(crate) smtp_username: TextEdit,
    pub(crate) smtp_password: TextEdit,
    /// A login check is running.
    pub(crate) checking: bool,
    /// A Google sign-in is waiting for the browser. Dropping it, as resetting the form does, stops it.
    pub(crate) google: Option<GoogleSignIn>,
    pub(crate) error: Option<String>,
}

/// A Google sign-in running in the background.
pub(crate) struct GoogleSignIn {
    pub(crate) task: AbortHandle,
    /// The sign-in page, once known, for when the browser didn't open.
    pub(crate) url: Option<String>,
}

impl Drop for GoogleSignIn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Default for AccountForm {
    fn default() -> Self {
        Self {
            open: false,
            step: Step::Choose,
            email: TextEdit::new(""),
            name: TextEdit::new(""),
            password: TextEdit::new(""),
            username: TextEdit::new(""),
            imap_host: TextEdit::new(""),
            imap_port: TextEdit::new(""),
            imap_security: Security::Tls,
            smtp_host: TextEdit::new(""),
            smtp_port: TextEdit::new(""),
            smtp_security: Security::Tls,
            smtp_username: TextEdit::new(""),
            smtp_password: TextEdit::new(""),
            checking: false,
            google: None,
            error: None,
        }
    }
}

impl AccountForm {
    /// The fields as an account to set up, or what's wrong with them.
    pub(crate) fn new_account(&self) -> Result<NewAccount, String> {
        let optional = |e: &TextEdit| Some(e.text.trim().to_owned()).filter(|s| !s.is_empty());
        let port = |e: &TextEdit, which: &str| {
            optional(e)
                .map(|p| {
                    p.parse::<u16>().map_err(|_invalid| {
                        format!("the {which} port must be a number up to 65535")
                    })
                })
                .transpose()
        };
        Ok(NewAccount {
            email: self.email.text.clone(),
            display_name: optional(&self.name),
            username: optional(&self.username),
            imap_host: self.imap_host.text.clone(),
            imap_port: port(&self.imap_port, "IMAP")?,
            imap_security: self.imap_security,
            smtp_host: optional(&self.smtp_host)
                .or_else(|| self.smtp_guess())
                .unwrap_or_default(),
            smtp_port: port(&self.smtp_port, "SMTP")?,
            smtp_security: self.smtp_security,
            smtp_username: optional(&self.smtp_username),
        })
    }

    /// The SMTP host guessed from the IMAP one (`imap.example.com` → `smtp.example.com`), used when the
    /// SMTP field is left empty.
    pub(crate) fn smtp_guess(&self) -> Option<String> {
        let domain = self.imap_host.text.trim().strip_prefix("imap.")?;
        domain.contains('.').then(|| format!("smtp.{domain}"))
    }
}

/// The index of `security` in [`SECURITY`].
pub(crate) fn security_index(security: Security) -> usize {
    SECURITY
        .iter()
        .position(|(_, s)| *s == security)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled() -> AccountForm {
        let mut form = AccountForm::default();
        form.email.replace_text("me@example.com");
        form.imap_host.replace_text("imap.example.com");
        form.smtp_host.replace_text("smtp.example.com");
        form
    }

    #[test]
    fn blank_optional_fields_are_left_out() {
        let account = filled().new_account().unwrap();
        assert_eq!(account.display_name, None);
        assert_eq!(account.imap_port, None);
        assert_eq!(account.username, None);
    }

    #[test]
    fn ports_must_be_numbers() {
        let mut form = filled();
        form.imap_port.replace_text("1143");
        assert_eq!(form.new_account().unwrap().imap_port, Some(1143));
        form.smtp_port.replace_text("smtp");
        assert!(
            form.new_account().unwrap_err().contains("SMTP port"),
            "port refused"
        );
    }

    #[test]
    fn an_empty_smtp_host_is_guessed_from_the_imap_one() {
        let mut form = filled();
        form.smtp_host.replace_text("");
        assert_eq!(form.new_account().unwrap().smtp_host, "smtp.example.com");

        form.smtp_host.replace_text("mail.example.com");
        assert_eq!(
            form.new_account().unwrap().smtp_host,
            "mail.example.com",
            "a typed host wins"
        );

        form.imap_host.replace_text("mail.example.com");
        assert_eq!(form.smtp_guess(), None, "no imap. prefix, no guess");
    }
}
