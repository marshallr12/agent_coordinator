//! The launch description the host writes into the read-only control directory.
//!
//! It is `key=value` text. Peers are grouped as `peer.NAME.FIELD` and path
//! aliases of the host canary as `alias.NAME`.

use std::collections::BTreeMap;

/// Parsed targets for one launch.
pub struct Spec {
    values: BTreeMap<String, String>,
}

impl Spec {
    /// Parses `key=value` lines, refusing malformed or repeated keys.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut values = BTreeMap::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| format!("malformed spec line: {line}"))?;
            if values.insert(key.to_string(), value.to_string()).is_some() {
                return Err(format!("repeated spec key: {key}"));
            }
        }
        Ok(Self { values })
    }

    /// The value of an optional key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// The value of a key every launch must receive.
    pub fn require(&self, key: &str) -> Result<&str, String> {
        self.get(key)
            .ok_or_else(|| format!("spec is missing {key}"))
    }

    /// Every `PREFIX.NAME` entry as `(NAME, value)`, in key order.
    pub fn group(&self, prefix: &str) -> Vec<(&str, &str)> {
        let start = format!("{prefix}.");
        self.values
            .iter()
            .filter_map(|(key, value)| Some((key.strip_prefix(&start)?, value.as_str())))
            .collect()
    }

    /// The distinct peer names, in key order.
    pub fn peers(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .group("peer")
            .into_iter()
            .filter_map(|(rest, _)| rest.split_once('.').map(|(name, _)| name))
            .collect();
        names.dedup();
        names
    }

    /// One field of one peer.
    pub fn peer(&self, name: &str, field: &str) -> Option<&str> {
        self.get(&format!("peer.{name}.{field}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "launch=impl-1\nalias.upper=C:\\HOST\\NOTE.TXT\nalias.verbatim=\\\\?\\C:\\host\\note.txt\n\
                        peer.rev-1.pid=42\npeer.rev-1.role=reviewer\npeer.impl-2.pid=7\n\n";

    #[test]
    fn reads_required_and_optional_keys() {
        let spec = Spec::parse(TEXT).unwrap();
        assert_eq!(spec.require("launch"), Ok("impl-1"));
        assert_eq!(spec.get("absent"), None);
        assert_eq!(
            spec.require("absent"),
            Err("spec is missing absent".to_string())
        );
    }

    #[test]
    fn groups_aliases_and_peers() {
        let spec = Spec::parse(TEXT).unwrap();
        assert_eq!(
            spec.group("alias"),
            vec![
                ("upper", "C:\\HOST\\NOTE.TXT"),
                ("verbatim", "\\\\?\\C:\\host\\note.txt")
            ]
        );
        assert_eq!(spec.peers(), vec!["impl-2", "rev-1"]);
        assert_eq!(spec.peer("rev-1", "role"), Some("reviewer"));
        assert_eq!(spec.peer("rev-1", "dir"), None);
    }

    #[test]
    fn a_value_may_contain_an_equals_sign() {
        let spec = Spec::parse("station=WinSta=1\\Desk").unwrap();
        assert_eq!(spec.get("station"), Some("WinSta=1\\Desk"));
    }

    #[test]
    fn refuses_malformed_and_repeated_lines() {
        assert!(Spec::parse("no separator").is_err());
        assert!(Spec::parse("a=1\na=2").is_err());
    }
}
