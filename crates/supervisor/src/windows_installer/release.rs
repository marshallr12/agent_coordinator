//! Which release a host runs, which one it keeps to go back to, and which
//! releases were rolled back (phase 7, step 7b).
//!
//! A release is a binary, named by its SHA-256 and the source commit it was
//! built from. The installed binary is always at the same path, so the
//! services' command lines never change; each release a host keeps has a
//! copy of its binary and of the configuration it ran with in
//! `releases\<sha256>\` below the program directory. The record of them is
//! `installer\releases.json` below the state root.
use super::model::Layout;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

/// One build of `agentc-supervisor.exe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// The binary's SHA-256, in lower-case hex.
    pub sha256: String,
    /// The full source commit it was built from.
    pub commit: String,
}

impl Release {
    /// Refuses a release whose digest or commit is not well formed.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            hex_of(&self.sha256, 64),
            "{:?} is not a SHA-256 in lower-case hex",
            self.sha256
        );
        ensure!(
            hex_of(&self.commit, 40),
            "{:?} is not a full source commit in lower-case hex",
            self.commit
        );
        Ok(())
    }

    /// The short form a message names the release by.
    pub fn short(&self) -> String {
        let cut = |text: &str| text.chars().take(12).collect::<String>();
        format!("{} ({})", cut(&self.sha256), cut(&self.commit))
    }
}

/// Whether `text` is `length` lower-case hex digits.
fn hex_of(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// What a host records about its releases.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Releases {
    /// The release installed now.
    pub current: Option<Release>,
    /// The release `rollback` returns to, kept with its configuration.
    pub previous: Option<Release>,
    /// The SHA-256 of every release that was rolled back. An update to one
    /// of them is refused unless the owner asks for it again.
    pub rejected: Vec<String>,
}

impl Releases {
    /// The record's text.
    pub fn text(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)? + "\n")
    }

    /// The record a file holds.
    pub fn parse(text: &str) -> Result<Self> {
        serde_json::from_str(text).context("the release record is not one the installer wrote")
    }

    /// The text of the record `existing` with `release` as the current
    /// release; what it says of earlier releases is kept.
    pub fn with_current(existing: Option<&str>, release: Release) -> Result<String> {
        let mut releases = existing.map(Self::parse).transpose()?.unwrap_or_default();
        releases.current = Some(release);
        releases.text()
    }

    /// Whether `sha256` names a release that was rolled back.
    pub fn rejected(&self, sha256: &str) -> bool {
        self.rejected.iter().any(|seen| seen == sha256)
    }

    /// Remembers that the release `sha256` was rolled back.
    pub fn reject(&mut self, sha256: &str) {
        if !self.rejected(sha256) {
            self.rejected.push(sha256.to_owned());
        }
    }

    /// Forgets that the release `sha256` was rolled back: the owner asked
    /// for it again.
    pub fn forgive(&mut self, sha256: &str) {
        self.rejected.retain(|seen| seen != sha256);
    }
}

impl Layout {
    /// The record of the host's releases.
    pub fn releases(&self) -> String {
        self.state(r"installer\releases.json")
    }

    /// The record of an update or rollback that has not finished.
    pub fn update_record(&self) -> String {
        self.state(r"installer\update.json")
    }

    /// The directory of every kept release.
    pub fn kept_dir(&self) -> String {
        self.program("releases")
    }

    /// The directory of the kept release with this digest.
    pub fn kept(&self, sha256: &str) -> String {
        self.program(&format!(r"releases\{sha256}"))
    }

    /// The kept copy of a release's binary.
    pub fn kept_binary(&self, sha256: &str) -> String {
        format!(r"{}\{}", self.kept(sha256), super::model::BINARY)
    }

    /// The kept copy of the configuration a release ran with.
    pub fn kept_config(&self, sha256: &str) -> String {
        format!(r"{}\{}", self.kept(sha256), super::model::CONFIG)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A well-formed release whose digest and commit start with `digit`.
    fn release(digit: char) -> Release {
        Release {
            sha256: digit.to_string().repeat(64),
            commit: digit.to_string().repeat(40),
        }
    }

    #[test]
    fn a_release_must_name_a_whole_digest_and_commit() {
        release('a').validate().unwrap();
        let short = Release {
            commit: "abc".into(),
            ..release('a')
        };
        assert!(short.validate().is_err());
        let upper = Release {
            sha256: "A".repeat(64),
            ..release('a')
        };
        assert!(upper.validate().is_err());
    }

    #[test]
    fn a_rejected_release_is_remembered_once_and_can_be_forgiven() {
        let mut releases = Releases::default();
        releases.reject(&release('b').sha256);
        releases.reject(&release('b').sha256);
        assert_eq!(releases.rejected.len(), 1);
        assert!(releases.rejected(&release('b').sha256));
        releases.forgive(&release('b').sha256);
        assert!(!releases.rejected(&release('b').sha256));
    }

    #[test]
    fn the_record_reads_back_and_refuses_what_it_did_not_write() {
        let releases = Releases {
            current: Some(release('c')),
            previous: Some(release('d')),
            rejected: vec![release('e').sha256],
        };
        assert_eq!(
            Releases::parse(&releases.text().unwrap()).unwrap(),
            releases
        );
        assert!(Releases::parse(r#"{"current": null, "other": 1}"#).is_err());
    }
}
