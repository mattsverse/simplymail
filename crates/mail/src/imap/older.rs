//! Fetching mail older than what's cached, as the list is scrolled past its end.
//!
//! A sync only ever fetches the newest messages; this reaches back from the oldest one cached.

use futures::TryStreamExt as _;
use simplymail_core::account::Server;
use simplymail_core::message::Envelope;

use super::connection::connect;
use super::envelope;
use super::watch::with_timeout;
use crate::{Credentials, Error, Result};

/// Messages fetched by [`fetch_older`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Older {
    /// The messages, oldest first.
    pub messages: Vec<Envelope>,
    /// Whether these were the oldest left, so there's nothing more to fetch.
    pub done: bool,
}

/// Fetches up to `count` of the newest messages in `mailbox` with a UID below `before`.
///
/// `uid_validity` is the mailbox's UIDVALIDITY as cached; if the server's differs, the UIDs mean
/// something else now, so nothing is fetched.
///
/// # Errors
///
/// Returns an error when the connection fails or the mailbox changed.
pub async fn fetch_older(
    server: &Server,
    credentials: &Credentials,
    mailbox: &str,
    uid_validity: u32,
    before: u32,
    count: usize,
) -> Result<Older> {
    if before <= 1 || count == 0 {
        return Ok(Older {
            messages: Vec::new(),
            done: true,
        });
    }
    let mut session = connect(server, credentials).await?;
    let result = async {
        let selected = with_timeout(session.select(mailbox)).await?;
        if selected.uid_validity != Some(uid_validity) {
            return Err(Error::UidValidityChanged(mailbox.to_owned()));
        }
        let found = with_timeout(session.uid_search(format!("UID 1:{}", before - 1))).await?;
        let (uids, done) = newest_below(found, before, count);
        if uids.is_empty() {
            return Ok(Older {
                messages: Vec::new(),
                done: true,
            });
        }
        let set = uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let fetches: Vec<_> = with_timeout(async {
            session
                .uid_fetch(set, envelope::QUERY)
                .await?
                .try_collect()
                .await
        })
        .await?;
        let mut messages: Vec<Envelope> = fetches
            .iter()
            .filter_map(envelope::from_fetch)
            .filter(|m| m.uid < before)
            .collect();
        messages.sort_by_key(|m| m.uid);
        Ok(Older { messages, done })
    }
    .await;
    let _ignored = session.logout().await;
    result
}

/// The `count` highest of `found` below `before`, ascending, and whether they're all there are.
fn newest_below(
    found: impl IntoIterator<Item = u32>,
    before: u32,
    count: usize,
) -> (Vec<u32>, bool) {
    let mut uids: Vec<u32> = found.into_iter().filter(|&uid| uid < before).collect();
    uids.sort_unstable();
    let done = uids.len() <= count;
    let uids = uids.split_off(uids.len().saturating_sub(count));
    (uids, done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_newest_below_the_oldest_cached() {
        assert_eq!(
            newest_below([5, 1, 9, 3, 7], 9, 2),
            (vec![5, 7], false),
            "9 is already cached"
        );
    }

    #[test]
    fn says_when_nothing_older_remains() {
        assert_eq!(newest_below([1, 2], 10, 5), (vec![1, 2], true));
        assert_eq!(newest_below([], 10, 5), (vec![], true));
        assert_eq!(newest_below([1, 2, 3], 10, 3), (vec![1, 2, 3], true));
    }
}
