//! Release versions, as SimplyMail tags them: `v` and a stable `major.minor.patch`.

use std::fmt;
use std::str::FromStr;

/// A stable `major.minor.patch` version, ordered as semver orders them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// The version of a release tag such as `v0.2.0`. `None` for anything else, such as a
    /// pre-release (`v0.2.0-rc.1`).
    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        tag.strip_prefix('v')?.parse().ok()
    }
}

impl FromStr for Version {
    type Err = InvalidVersion;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.split('.').map(|part| {
            // No sign, no leading zeros, as release.py also insists.
            let canonical = !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'));
            canonical.then(|| part.parse::<u64>().ok()).flatten()
        });
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) => Ok(Self {
                major,
                minor,
                patch,
            }),
            _ => Err(InvalidVersion(s.to_owned())),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A version that isn't a stable `major.minor.patch`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} isn't a major.minor.patch version")]
pub struct InvalidVersion(String);

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn versions_parse_and_print() {
        assert_eq!(
            v("1.22.333"),
            Version {
                major: 1,
                minor: 22,
                patch: 333
            }
        );
        assert_eq!(v("0.10.0").to_string(), "0.10.0");
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "1.02.3",
            "1.2.x",
            "+1.2.3",
            "1.2.3-rc.1",
        ] {
            assert!(bad.parse::<Version>().is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn versions_order_numerically() {
        assert!(v("0.10.0") > v("0.9.9"), "minor compares as a number");
        assert!(v("1.0.0") > v("0.99.99"), "major wins");
        assert!(v("0.1.1") > v("0.1.0"), "patch breaks ties");
    }

    #[test]
    fn tags_need_the_v_and_a_stable_version() {
        assert_eq!(Version::from_tag("v0.2.0"), Some(v("0.2.0")));
        assert_eq!(Version::from_tag("0.2.0"), None);
        assert_eq!(Version::from_tag("v0.2.0-rc.1"), None);
    }
}
