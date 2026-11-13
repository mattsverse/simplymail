//! IMAP: connecting, fetching message headers, watching the inbox for new mail, refreshing the
//! other mailboxes on a timer, opening and flagging single messages, and filing sent mail.

mod connection;
mod envelope;
mod mailboxes;
mod message;
mod refresh;
mod sent;
mod watch;

pub use connection::{Session, check_login, connect};
pub use message::{open_message, set_seen};
pub use refresh::refresh;
pub use sent::save_sent_copy;
pub use watch::{Event, Status, WatchConfig, watch};
