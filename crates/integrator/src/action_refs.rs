//! Line-based extraction of local-action references from workflow and
//! action files, without a YAML parser. It reads the spellings a human
//! writes on the approved target: a `uses` key with or without quotes and
//! with spaces before the colon, several keys on one line, flow maps and
//! sequences, and a value on the next line or in a block or folded scalar.
//! Anything it cannot read as a plain path (an alias, a backslash escape, an
//! expression, an unterminated quote) is reported as unparsed, so the caller
//! can fail closed.

/// What one `uses` value refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ref {
    /// A repository-relative local action directory (`./` removed, resolved).
    Local(String),
    /// A remote action or a Docker image; it cannot change with the result.
    Remote,
    /// A value that could not be read as a plain path.
    Unparsed,
}

/// Every `uses` reference in `text`.
pub fn uses_refs(text: &str) -> Vec<Ref> {
    key_values(text, "uses")
        .into_iter()
        .map(|value| value.map_or(Ref::Unparsed, |v| classify_uses(&v)))
        .collect()
}

/// Paths named by an action's `main`, `pre`, `post` and local `image`
/// keys, resolved against its directory `dir`, plus whether any value was
/// unreadable. A `Dockerfile` also brings in its directory.
pub fn runs_paths(text: &str, dir: &str) -> (Vec<String>, bool) {
    let (mut paths, mut unparsed) = (Vec::new(), false);
    for key in ["main", "pre", "post", "image"] {
        for value in key_values(text, key) {
            match value {
                None => unparsed = true,
                Some(v) if v.starts_with("docker://") => {}
                Some(v) => paths.extend(runs_target(dir, &v)),
            }
        }
    }
    (paths, unparsed)
}

/// The scope entries for one runs path: the path, and for a `Dockerfile`
/// its directory. An absolute path covers the whole repository.
fn runs_target(dir: &str, value: &str) -> Vec<String> {
    if value.starts_with('/') {
        return vec![String::new()];
    }
    let path = resolve(&format!("{dir}/{value}"));
    let is_dockerfile = path.to_ascii_lowercase().ends_with("dockerfile");
    match path.rsplit_once('/') {
        Some((parent, _)) if is_dockerfile => vec![path.clone(), parent.to_owned()],
        _ => vec![path],
    }
}

/// Classifies one extracted `uses` value.
fn classify_uses(value: &str) -> Ref {
    if let Some(local) = value.strip_prefix("./") {
        return Ref::Local(resolve(local));
    }
    let remote =
        value.starts_with("docker://") || value.starts_with(|c: char| c.is_ascii_alphanumeric());
    if remote { Ref::Remote } else { Ref::Unparsed }
}

/// Resolves `.` and `..` in a repository-relative path; climbing above the
/// root, or a path that ends up empty, yields the root (empty).
pub fn resolve(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return String::new();
                }
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// The value of every occurrence of `key` in `text`: `Some(value)` when
/// it reads as a plain scalar, `None` when it does not.
fn key_values(text: &str, key: &str) -> Vec<Option<String>> {
    let lines: Vec<&str> = text.lines().collect();
    let mut values = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        for at in key_ends(line, key) {
            values.push(value_after(&line[at..], &lines[n + 1..]));
        }
    }
    values
}

/// Byte offsets just past the colon of every `key` occurrence on `line`
/// (optionally quoted, with blanks before the colon, not part of a longer
/// word), ignoring letter case.
fn key_ends(line: &str, key: &str) -> Vec<usize> {
    let lower = line.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut ends = Vec::new();
    let mut from = 0;
    while let Some(found) = lower[from..].find(key) {
        let start = from + found;
        from = start + key.len();
        if word_before(bytes, start) {
            continue;
        }
        if let Some(end) = colon_after(bytes, start + key.len()) {
            ends.push(end);
        }
    }
    ends
}

/// True when the byte before `start` (skipping one opening quote) continues
/// a longer word, so `start` is not the beginning of a key.
fn word_before(bytes: &[u8], start: usize) -> bool {
    let mut i = start;
    if i > 0 && matches!(bytes[i - 1], b'"' | b'\'') {
        i -= 1;
    }
    i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || matches!(bytes[i - 1], b'_' | b'-'))
}

/// The offset past a colon that follows `at` after an optional closing
/// quote and blanks; `None` when the key is not followed by a colon.
fn colon_after(bytes: &[u8], mut at: usize) -> Option<usize> {
    if at < bytes.len() && matches!(bytes[at], b'"' | b'\'') {
        at += 1;
    }
    while at < bytes.len() && matches!(bytes[at], b' ' | b'\t') {
        at += 1;
    }
    (at < bytes.len() && bytes[at] == b':').then_some(at + 1)
}

/// The scalar after a key's colon; an empty value or a block/folded scalar
/// header takes its value from the first non-blank following line.
fn value_after(rest: &str, following: &[&str]) -> Option<String> {
    let value = rest.trim_start();
    if value.is_empty() || value.starts_with('#') || block_header(value) {
        let next = following.iter().map(|l| l.trim()).find(|l| !l.is_empty())?;
        return scalar(next);
    }
    scalar(value)
}

/// True for a block (`|`) or folded (`>`) scalar header with optional
/// chomping and indentation indicators and an optional comment.
fn block_header(value: &str) -> bool {
    let header = value.split('#').next().unwrap_or("").trim_end();
    let mut chars = header.chars();
    matches!(chars.next(), Some('|' | '>'))
        && chars.all(|c| matches!(c, '-' | '+') || c.is_ascii_digit())
}

/// A quoted or plain scalar at the start of `value`; `None` for anything
/// else (aliases, anchors, tags, expressions, escapes, open quotes).
fn scalar(value: &str) -> Option<String> {
    let first = value.chars().next()?;
    let content = match first {
        '"' | '\'' => value[1..].split_once(first)?.0,
        c if c.is_ascii_alphanumeric() || c == '.' || c == '/' => plain(value),
        _ => return None,
    };
    (!content.is_empty() && !content.contains('\\')).then(|| content.to_owned())
}

/// A plain scalar up to the first blank, flow indicator or quote.
fn plain(value: &str) -> &str {
    let end = value
        .find([' ', '\t', ',', '}', ']', '"', '\''])
        .unwrap_or(value.len());
    &value[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The local directories among `text`'s references, or `Unparsed`.
    fn refs(text: &str) -> Vec<Ref> {
        uses_refs(text)
            .into_iter()
            .filter(|r| *r != Ref::Remote)
            .collect()
    }

    fn local(dir: &str) -> Ref {
        Ref::Local(dir.into())
    }

    #[test]
    fn human_spellings_are_read() {
        let cases = [
            "  - uses: ./ci/x\n",
            "  - {uses: ./ci/x}\n",
            "  steps: [{uses: ./ci/x, with: {a: 1}}]\n",
            "  - \"uses\": ./ci/x\n",
            "  - 'uses' : './ci/x'\n",
            "  - uses :   \"./ci/x\"  # pinned\n",
            "  - uses:\n      ./ci/x\n",
            "  - uses: >-\n      ./ci/x\n",
            "  - uses: |\n      ./ci/x\n",
        ];
        for text in cases {
            assert_eq!(refs(text), [local("ci/x")], "{text:?}");
        }
        let two = "  steps: [{uses: ./ci/a}, {uses: actions/checkout@v4}, {uses: ./ci/b}]\n";
        assert_eq!(refs(two), [local("ci/a"), local("ci/b")]);
        assert_eq!(refs("  - uses: ./a/../../b\n"), [local("")]);
    }

    #[test]
    fn unreadable_values_are_unparsed() {
        for text in [
            "  - uses: *act\n",
            "  - uses: \"./ci/\\x78\"\n",
            "  - uses: &a ./ci/x\n",
            "  - uses: ${{ inputs.a }}\n",
            "  - uses: \"./ci/x\n",
            "  - uses: ../ci/x\n",
        ] {
            assert_eq!(refs(text), [Ref::Unparsed], "{text:?}");
        }
        assert!(refs("  - uses: docker://alpine:3\n  - uses: o/r@v1\n").is_empty());
        assert!(refs("  - reuses: x\n  - uses-x: y\n  - run: echo uses\n").is_empty());
    }

    #[test]
    fn runs_paths_resolve_against_the_action() {
        let text = "runs:\n  using: node20\n  main: ../../scripts/run.js\n  post: 'dist/post.js'\n  post-if: always()\n";
        let (paths, unparsed) = runs_paths(text, "ci/x");
        assert_eq!(paths, ["scripts/run.js", "ci/x/dist/post.js"]);
        assert!(!unparsed);
        let docker = "runs:\n  using: docker\n  image: docker/Dockerfile\n";
        assert_eq!(
            runs_paths(docker, "ci/x").0,
            ["ci/x/docker/Dockerfile", "ci/x/docker"]
        );
        assert!(
            runs_paths("runs:\n  image: docker://alpine\n", "ci/x")
                .0
                .is_empty()
        );
        assert!(runs_paths("runs:\n  main: *m\n", "ci/x").1);
    }
}
