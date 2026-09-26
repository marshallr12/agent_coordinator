//! Refuses candidate pushes whose new commits add likely secrets.
//!
//! Supervised agents may not run raw `git push`; their only way to publish is
//! the candidate checkpoint push, which scans every outgoing commit's patch
//! first (autonomy plan §2.3). Rules are deliberately high-confidence token
//! shapes plus the caller's own coordinator credential (matched by SHA-256
//! digest, so the raw token is never passed in), keeping false positives rare.
//! Findings name the rule, commit and path, never the matched text.
use regex::Regex;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// One detected secret: which rule matched, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: &'static str,
    pub commit: String,
    pub path: String,
}

/// Token shapes that are almost never legitimate in source.
const RULES: &[(&str, &str)] = &[
    ("aws_access_key", r"\bAKIA[0-9A-Z]{16}\b"),
    ("github_token", r"\bgh[pousr]_[A-Za-z0-9]{36,}"),
    (
        "github_fine_grained_token",
        r"\bgithub_pat_[A-Za-z0-9_]{40,}",
    ),
    ("private_key", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    ("anthropic_key", r"\bsk-ant-[A-Za-z0-9_-]{20,}"),
    ("openai_key", r"\bsk-(?:proj-)?[A-Za-z0-9]{20,}"),
    ("slack_token", r"\bxox[baprs]-[A-Za-z0-9-]{10,}"),
    ("google_api_key", r"\bAIza[0-9A-Za-z_-]{35}"),
];

/// File names that hold credentials and must never be committed.
const FORBIDDEN_FILES: &[&str] = &["credentials.toml", "id_rsa", "id_ed25519", ".env"];

/// Compiles the rules once per process.
fn compiled() -> &'static [(&'static str, Regex)] {
    static CELL: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    CELL.get_or_init(|| {
        RULES
            .iter()
            .map(|(name, pattern)| (*name, Regex::new(pattern).expect("valid secret rule")))
            .collect()
    })
}

/// Scans `git log -p --format=commit:%H` output. `known_digests` are lowercase
/// SHA-256 hex digests of coordinator tokens that must not appear.
pub fn scan_patch(patch: &str, known_digests: &[&str]) -> Vec<Finding> {
    let mut findings = Vec::new();
    let (mut commit, mut path) = (String::new(), String::new());
    for line in patch.lines() {
        if let Some(hash) = line.strip_prefix("commit:") {
            commit = hash.trim().to_owned();
        } else if let Some(name) = line.strip_prefix("+++ b/") {
            path = name.to_owned();
            if forbidden_file(&path) {
                findings.push(finding("credential_file", &commit, &path));
            }
        } else if let Some(added) = line.strip_prefix('+')
            && let Some(rule) = matching_rule(added, known_digests)
        {
            findings.push(finding(rule, &commit, &path));
        }
    }
    findings.dedup();
    findings
}

/// The first rule an added line matches, if any.
fn matching_rule(added: &str, known_digests: &[&str]) -> Option<&'static str> {
    if contains_known_token(added, known_digests) {
        return Some("coordinator_credential");
    }
    compiled()
        .iter()
        .find(|(_, regex)| regex.is_match(added))
        .map(|(name, _)| *name)
}

/// True when a 64-hex word in the line hashes to one of `known_digests`
/// (coordinator tokens are 32 random bytes in hex).
fn contains_known_token(added: &str, known_digests: &[&str]) -> bool {
    if known_digests.is_empty() {
        return false;
    }
    added
        .split(|c: char| !c.is_ascii_hexdigit())
        .filter(|word| word.len() == 64)
        .any(|word| known_digests.contains(&hex::encode(Sha256::digest(word.as_bytes())).as_str()))
}

/// True when the path's file name is a known credential file.
fn forbidden_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    FORBIDDEN_FILES.contains(&name) || name.ends_with(".pem")
}

/// Builds one finding.
fn finding(rule: &'static str, commit: &str, path: &str) -> Finding {
    Finding {
        rule,
        commit: commit.to_owned(),
        path: path.to_owned(),
    }
}

/// A one-line refusal naming up to five findings without their contents.
pub fn describe(findings: &[Finding]) -> String {
    let shown: Vec<String> = findings
        .iter()
        .take(5)
        .map(|f| format!("{} in {}:{}", f.rule, short(&f.commit), f.path))
        .collect();
    format!(
        "candidate push refused: {} likely secret(s) in outgoing commits ({}); remove them from history and retry",
        findings.len(),
        shown.join(", ")
    )
}

/// The first 12 characters of a commit id.
fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake AWS key assembled at runtime so this file never matches itself.
    fn fake_key() -> String {
        format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP")
    }

    #[test]
    fn detects_token_shapes_with_location() {
        let patch = format!(
            "commit:0123456789abcdef\n+++ b/src/a.rs\n+k = \"{}\";\n+fine\n",
            fake_key()
        );
        let found = scan_patch(&patch, &[]);
        assert_eq!(
            found,
            vec![finding("aws_access_key", "0123456789abcdef", "src/a.rs")]
        );
        assert!(!describe(&found).contains("AKIA"));
    }

    #[test]
    fn detects_known_credentials_and_files() {
        let token = "f".repeat(64);
        let digest = hex::encode(Sha256::digest(token.as_bytes()));
        let patch = format!("commit:c1\n+++ b/deploy/credentials.toml\n+token='{token}'\n");
        let rules: Vec<_> = scan_patch(&patch, &[&digest])
            .into_iter()
            .map(|f| f.rule)
            .collect();
        assert_eq!(rules, vec!["credential_file", "coordinator_credential"]);
    }

    #[test]
    fn ignores_removed_lines_and_ordinary_hashes() {
        let patch = format!(
            "commit:c1\n+++ b/x\n-{}\n+sha 0123456789abcdef0123456789abcdef01234567\n",
            fake_key()
        );
        assert!(scan_patch(&patch, &[]).is_empty());
    }
}
