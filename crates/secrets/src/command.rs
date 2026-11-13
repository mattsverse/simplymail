//! A read-only store that runs a user-supplied password command.

use std::process::{Command, Stdio};

use simplymail_core::secrets::{SecretError, SecretKey, SecretStore, SecretString};

/// Secrets read from a shell command's stdout, for machines without a usable keyring.
///
/// The command runs through `sh -c` (`cmd /C` on Windows) with `SIMPLYMAIL_ACCOUNT` and
/// `SIMPLYMAIL_SECRET_KIND` set, so one command can serve every account, for example
/// `pass show "simplymail/$SIMPLYMAIL_ACCOUNT/$SIMPLYMAIL_SECRET_KIND"`.
///
/// The secret is the first line of stdout, which matches `pass`. Empty output means no secret.
/// This store is read-only: `set` and `delete` return [`SecretError::ReadOnly`].
#[derive(Debug, Clone)]
pub struct CommandStore {
    command: String,
}

impl CommandStore {
    #[must_use]
    pub const fn new(command: String) -> Self {
        Self { command }
    }

    fn shell(&self) -> Command {
        let (shell, flag) = if cfg!(windows) {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        };
        let mut cmd = Command::new(shell);
        cmd.arg(flag).arg(&self.command);
        cmd
    }
}

impl SecretStore for CommandStore {
    fn get(&self, key: &SecretKey) -> Result<Option<SecretString>, SecretError> {
        let output = self
            .shell()
            .env("SIMPLYMAIL_ACCOUNT", &key.account)
            .env("SIMPLYMAIL_SECRET_KIND", key.kind.as_str())
            .stdin(Stdio::null())
            .output()
            .map_err(|e| {
                SecretError::Unavailable(format!("couldn't run the password command: {e}"))
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::Backend(
                format!(
                    "the password command failed ({}): {}",
                    output.status,
                    stderr.trim()
                )
                .into(),
            ));
        }
        let stdout =
            String::from_utf8(output.stdout).map_err(|e| SecretError::Backend(Box::new(e)))?;
        let secret = stdout.lines().next().unwrap_or_default();
        Ok((!secret.is_empty()).then(|| SecretString::from(secret)))
    }

    fn set(&self, _: &SecretKey, _: &SecretString) -> Result<(), SecretError> {
        Err(SecretError::ReadOnly)
    }

    fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
        Err(SecretError::ReadOnly)
    }
}

#[cfg(test)]
#[cfg(unix)] // the commands below are POSIX shell
mod tests {
    use simplymail_core::secrets::{ExposeSecret, SecretKind};

    use super::*;

    fn key() -> SecretKey {
        SecretKey::new("me@example.com", SecretKind::Password)
    }

    fn get(command: &str) -> Result<Option<SecretString>, SecretError> {
        CommandStore::new(command.into()).get(&key())
    }

    #[test]
    fn the_secret_is_the_first_line_of_stdout() {
        let secret = get("printf 'hunter2\\nnotes: work'").unwrap().unwrap();
        assert_eq!(secret.expose_secret(), "hunter2");
    }

    #[test]
    fn the_command_sees_which_secret_is_wanted() {
        let secret = get("echo \"$SIMPLYMAIL_ACCOUNT/$SIMPLYMAIL_SECRET_KIND\"")
            .unwrap()
            .unwrap();
        assert_eq!(secret.expose_secret(), "me@example.com/password");
    }

    #[test]
    fn empty_output_means_no_secret() {
        assert!(get("true").unwrap().is_none(), "nothing printed");
    }

    #[test]
    fn a_failing_command_is_an_error_with_its_stderr() {
        let err = get("echo locked >&2; exit 3").unwrap_err();
        assert!(err.to_string().contains("locked"), "got: {err}");
    }

    #[test]
    fn it_never_writes() {
        let store = CommandStore::new("true".into());
        assert!(
            matches!(
                store.set(&key(), &SecretString::from("x")),
                Err(SecretError::ReadOnly)
            ),
            "set is refused"
        );
        assert!(
            matches!(store.delete(&key()), Err(SecretError::ReadOnly)),
            "delete is refused"
        );
    }
}
