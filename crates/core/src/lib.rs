//! Domain types and traits shared by every SimplyMail crate. Has no I/O and no UI.

pub mod account;
pub mod message;
pub mod secrets;

/// Errors shared across SimplyMail crates.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not implemented yet")]
    Unimplemented,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
