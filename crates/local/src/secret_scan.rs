//! Refuses candidate pushes whose new commits add likely secrets.
//!
//! Supervised agents may not run raw `git push`; their only way to publish is
//! the candidate checkpoint push, which first scans every outgoing commit's
//! patch, file names, and raw commit object (message, identities and other
//! headers) (autonomy plan §2.3). Rules are deliberately high-confidence token
//! shapes plus the caller's own coordinator credential (matched by SHA-256
//! digest, so the raw token is never passed in), keeping false positives rare.
//! Findings name the rule, commit and path (or [`MESSAGE_LOCATION`]), never
//! the matched text.
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

/// The location a finding in a commit message or identity reports.
pub const MESSAGE_LOCATION: &str = "(commit message or identity)";

/// The most characters of one path a refusal shows: room for any realistic
/// path, while the findings [`describe`] shows still fit a 64 KiB reply line.
const MAX_DESCRIBED_PATH_CHARS: usize = 256;

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
/// SHA-256 hex digests of coordinator tokens that must not appear. File-name
/// rules are applied separately by [`scan_paths`], so this only attributes
/// added content to the path named in each file's diff header.
pub fn scan_patch(patch: &str, known_digests: &[&str]) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut state = PatchState::default();
    for line in patch.lines() {
        if let Some(added) = state.advance(line)
            && let Some(rule) = matching_rule(added, known_digests)
        {
            findings.push(finding(rule, &state.commit, &state.path));
        }
    }
    findings.dedup();
    findings
}

/// Where the patch parser is: the current commit and file, and whether it is
/// still inside a file's diff header (before the first `@@` hunk).
#[derive(Default)]
struct PatchState {
    commit: String,
    path: String,
    in_header: bool,
}

impl PatchState {
    /// Consumes one patch line and returns its content when it is an added
    /// hunk line. Header lines (`+++ b/...`, mode and rename lines) are only
    /// recognised between `diff --git` and the first `@@`, so an added line
    /// whose text happens to begin with `++ b/` is still scanned as content.
    fn advance<'a>(&mut self, line: &'a str) -> Option<&'a str> {
        if let Some(hash) = line.strip_prefix("commit:") {
            self.commit = hash.trim().to_owned();
            self.in_header = false;
        } else if line.starts_with("diff --") {
            self.in_header = true;
        } else if self.in_header {
            if line.starts_with("@@") {
                self.in_header = false;
            } else if let Some(header) = line.strip_prefix("+++ ") {
                self.path = header_path(header);
            }
        } else if let Some(added) = line.strip_prefix('+') {
            return Some(added);
        }
        None
    }
}

/// The display path from a `+++` header value: C-style quoting (used for
/// names with tabs, quotes, backslashes or newlines) and the `b/` prefix removed.
fn header_path(header: &str) -> String {
    let unquoted = match header
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        Some(quoted) => unescape(quoted),
        None => header.to_owned(),
    };
    match unquoted.strip_prefix("b/") {
        Some(path) => path.to_owned(),
        None => unquoted,
    }
}

/// Undoes Git's C-style path escapes for display. Octal byte escapes are left
/// as written because the result only labels a finding.
fn unescape(quoted: &str) -> String {
    let mut output = String::with_capacity(quoted.len());
    let mut characters = quoted.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        match characters.next() {
            Some('t') => output.push('\t'),
            Some('n') => output.push('\n'),
            Some(other @ ('"' | '\\')) => output.push(other),
            Some(other) => output.extend(['\\', other]),
            None => output.push('\\'),
        }
    }
    output
}

/// Scans `git log --name-only -z --format=commit:%H` output for credential
/// file names. Paths come NUL-terminated and unquoted, so names containing
/// tabs, quotes or backslashes, and rename-only commits, are all covered.
pub fn scan_paths(listing: &[u8]) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut commit = String::new();
    for entry in listing.split(|byte| *byte == 0) {
        let entry = String::from_utf8_lossy(entry);
        // Git ends each commit header with NUL then starts its paths with '\n'.
        let entry = entry.strip_prefix('\n').unwrap_or(&entry);
        if let Some(hash) = commit_marker(entry) {
            commit = hash.to_owned();
        } else if forbidden_file(entry) {
            findings.push(finding("credential_file", &commit, entry));
        }
    }
    findings
}

/// Scans a listing of raw commit objects, each after a `commit:<oid>` marker
/// line: every line, marker lines included, is matched against the rules, so
/// a message line shaped like a marker is still scanned.
pub fn scan_messages(listing: &str, known_digests: &[&str]) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut commit = "";
    for line in listing.lines() {
        if let Some(hash) = commit_marker(line) {
            commit = hash;
        }
        if let Some(rule) = matching_rule(line, known_digests) {
            findings.push(finding(rule, commit, MESSAGE_LOCATION));
        }
    }
    findings.dedup();
    findings
}

/// The commit ID when `entry` is exactly a `commit:<full hex id>` marker;
/// anything else (including a path that merely starts with `commit:`) is a path.
fn commit_marker(entry: &str) -> Option<&str> {
    let hash = entry.strip_prefix("commit:")?;
    let full = matches!(hash.len(), 40 | 64) && hash.bytes().all(|byte| byte.is_ascii_hexdigit());
    full.then_some(hash)
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
/// (coordinator tokens are 32 random bytes in hex). The word is also tried
/// lowercased, so re-casing the token cannot slip it past the scan.
fn contains_known_token(added: &str, known_digests: &[&str]) -> bool {
    if known_digests.is_empty() {
        return false;
    }
    added
        .split(|c: char| !c.is_ascii_hexdigit())
        .filter(|word| word.len() == 64)
        .any(|word| {
            let lowered = word.to_ascii_lowercase();
            [word, lowered.as_str()].iter().any(|candidate| {
                known_digests.contains(&hex::encode(Sha256::digest(candidate.as_bytes())).as_str())
            })
        })
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
        .map(|f| format!("{} in {}:{}", f.rule, short(&f.commit), shorten(&f.path)))
        .collect();
    format!(
        "candidate push refused: {} likely secret(s) in outgoing commits ({}); remove them from history and retry",
        findings.len(),
        shown.join(", ")
    )
}

/// `path` cut to [`MAX_DESCRIBED_PATH_CHARS`] characters, marked with `…`
/// when cut.
fn shorten(path: &str) -> String {
    match path.char_indices().nth(MAX_DESCRIBED_PATH_CHARS) {
        Some((cut, _)) => format!("{}…", &path[..cut]),
        None => path.to_owned(),
    }
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

    /// A minimal `git log -p` patch adding `line` to `path` in commit `c1`.
    fn patch_adding(path: &str, line: &str) -> String {
        format!(
            "commit:c1\ndiff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -0,0 +1 @@\n+{line}\n"
        )
    }

    #[test]
    fn detects_token_shapes_with_location() {
        let patch = format!(
            "commit:0123456789abcdef\ndiff --git a/src/a.rs b/src/a.rs\n+++ b/src/a.rs\n@@ -0,0 +1,2 @@\n+k = \"{}\";\n+fine\n",
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
    fn detects_known_credentials_in_any_case() {
        let token = "f".repeat(64);
        let digest = hex::encode(Sha256::digest(token.as_bytes()));
        for written in [token.clone(), token.to_ascii_uppercase()] {
            let patch = patch_adding("deploy/config.toml", &format!("token='{written}'"));
            let rules: Vec<_> = scan_patch(&patch, &[&digest])
                .into_iter()
                .map(|f| f.rule)
                .collect();
            assert_eq!(rules, vec!["coordinator_credential"]);
        }
    }

    #[test]
    fn ignores_removed_lines_and_ordinary_hashes() {
        let patch = format!(
            "commit:c1\ndiff --git a/x b/x\n+++ b/x\n@@ -1 +1 @@\n-{}\n+sha 0123456789abcdef0123456789abcdef01234567\n",
            fake_key()
        );
        assert!(scan_patch(&patch, &[]).is_empty());
    }

    #[test]
    fn added_line_resembling_a_header_is_scanned_as_content() {
        // The added text is "++ b/<key>", which renders as "+++ b/<key>".
        let patch = patch_adding("src/a.rs", &format!("++ b/{}", fake_key()));
        assert_eq!(
            scan_patch(&patch, &[]),
            vec![finding("aws_access_key", "c1", "src/a.rs")]
        );
    }

    #[test]
    fn quoted_header_paths_are_unescaped_for_findings() {
        let patch = format!(
            "commit:c1\ndiff --git \"a/t\\tq\\\"\" \"b/t\\tq\\\"\"\n+++ \"b/t\\tq\\\"\"\n@@ -0,0 +1 @@\n+{}\n",
            fake_key()
        );
        assert_eq!(scan_patch(&patch, &[])[0].path, "t\tq\"");
    }

    #[test]
    fn message_listing_flags_secrets_in_messages_and_identities() {
        let commit = "b".repeat(40);
        let key = fake_key();
        let listing = format!(
            "commit:{commit}\nA U Thor <{key}@example.invalid>\nC O Mitter <c@example.invalid>\nsubject\n\nbody {key}\n"
        );
        let found = scan_messages(&listing, &[]);
        assert_eq!(
            found,
            vec![finding("aws_access_key", &commit, MESSAGE_LOCATION)]
        );
        assert!(!describe(&found).contains(&key));
        assert!(
            scan_messages(&format!("commit:{commit}\nA <a@b>\nC <c@d>\nfine\n"), &[]).is_empty()
        );
    }

    #[test]
    fn descriptions_cap_each_path() {
        let long = "p".repeat(20_000);
        let found = vec![finding("credential_file", "c1", &long); 5];
        let text = describe(&found);
        assert!(
            text.len() < 5 * (MAX_DESCRIBED_PATH_CHARS + 64),
            "{}",
            text.len()
        );
        assert!(text.contains(&format!("{}…", "p".repeat(MAX_DESCRIBED_PATH_CHARS))));
        assert_eq!(shorten("short/path.pem"), "short/path.pem");
    }

    #[test]
    fn path_listing_flags_credential_files_with_unusual_names() {
        let commit = "a".repeat(40);
        let listing = format!(
            "commit:{commit}\0\nok.txt\0we\"ird\\\tdir/id_rsa\0keys/server.pem\0commit:x/.env\0"
        );
        let found = scan_paths(listing.as_bytes());
        let paths: Vec<_> = found.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["we\"ird\\\tdir/id_rsa", "keys/server.pem", "commit:x/.env"]
        );
        assert!(found.iter().all(|f| f.commit == commit));
    }
}
