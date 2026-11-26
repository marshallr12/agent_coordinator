//! Repository URL handling: stripping secrets before use or display, the
//! normal form used to match a repository to a project, and the default
//! project name taken from the URL.

/// A remote URL with anything secret or irrelevant removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sanitized {
    /// The URL that is safe to show, send and store.
    pub url: String,
    /// True when userinfo, a query or a fragment was removed.
    pub changed: bool,
}

/// Removes `user:password@` userinfo, the query and the fragment from a remote URL.
///
/// A remote such as `https://user:token@host/org/repo.git` must never reach
/// the dashboard link, the plan output or the service. The conventional
/// `git@host:org/repo` login is not a secret and is kept.
pub fn sanitize(raw: &str) -> Sanitized {
    let trimmed = raw.trim();
    let without_tail = trimmed.split(['?', '#']).next().unwrap_or_default();
    let url = match without_tail.split_once("://") {
        Some((scheme, rest)) => format!("{scheme}://{}", strip_userinfo(rest, scheme)),
        None => without_tail.to_owned(),
    };
    let changed = url != trimmed;
    Sanitized { url, changed }
}

/// Drops the userinfo before the host, keeping the bare `git@` login of SSH URLs.
fn strip_userinfo<'a>(rest: &'a str, scheme: &str) -> &'a str {
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let Some(at) = rest[..authority_end].rfind('@') else {
        return rest;
    };
    let userinfo = &rest[..at];
    if scheme.eq_ignore_ascii_case("ssh") && !userinfo.contains(':') {
        rest
    } else {
        &rest[at + 1..]
    }
}

/// Returns the form used to decide that two URLs name the same repository.
///
/// Lowercases the host, drops a trailing `/` and `.git`, and treats
/// `git@host:org/repo`, `ssh://git@host/org/repo` and `https://host/org/repo`
/// as equal. The path keeps its case because not every host ignores it.
pub fn normalize(raw: &str) -> String {
    let url = sanitize(raw).url;
    let (host, path) = split_host_path(&url);
    let path = path.trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    format!("{}/{path}", host.to_ascii_lowercase())
}

/// Splits a URL or an scp-style remote into its host and its path.
fn split_host_path(url: &str) -> (&str, &str) {
    if let Some((_, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = authority.rsplit('@').next().unwrap_or(authority);
        return (host, path);
    }
    match url.split_once(':') {
        Some((login, path)) if !login.contains('/') => {
            (login.rsplit('@').next().unwrap_or(login), path)
        }
        _ => ("", url),
    }
}

/// Derives the default project name: the last path segment without `.git`.
pub fn default_name(url: &str) -> Option<String> {
    let normal = normalize(url);
    let name = normal.rsplit('/').next().unwrap_or_default().trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn userinfo_query_and_fragment_are_stripped() {
        let cleaned = sanitize("https://user:secret-token@github.com/acme/billing.git?x=1#frag");
        assert_eq!(cleaned.url, "https://github.com/acme/billing.git");
        assert!(cleaned.changed);
        let token_only = sanitize("https://secret-token@github.com/acme/billing.git");
        assert_eq!(token_only.url, "https://github.com/acme/billing.git");
        let plain = sanitize("https://github.com/acme/billing.git");
        assert!(!plain.changed);
    }

    #[test]
    fn ssh_logins_are_kept_but_passwords_are_not() {
        assert!(!sanitize("git@github.com:acme/billing.git").changed);
        assert!(!sanitize("ssh://git@github.com/acme/billing.git").changed);
        assert_eq!(
            sanitize("ssh://git:hunter2@github.com/acme/billing.git").url,
            "ssh://github.com/acme/billing.git"
        );
    }

    #[test]
    fn equivalent_remotes_share_one_normal_form() {
        let expected = "github.com/acme/billing";
        for url in [
            "https://github.com/acme/billing",
            "https://GitHub.com/acme/billing.git",
            "https://github.com/acme/billing/",
            "https://github.com/acme/billing.git/",
            "git@github.com:acme/billing.git",
            "ssh://git@github.com/acme/billing",
            "https://user:pw@github.com/acme/billing.git",
        ] {
            assert_eq!(normalize(url), expected, "{url}");
        }
        assert_ne!(normalize("https://github.com/acme/other"), expected);
        assert_ne!(normalize("https://github.com/Acme/billing"), expected);
    }

    #[test]
    fn default_name_is_the_last_segment_without_git() {
        assert_eq!(
            default_name("https://github.com/acme/billing.git").as_deref(),
            Some("billing")
        );
        assert_eq!(
            default_name("git@github.com:acme/api-server").as_deref(),
            Some("api-server")
        );
        assert_eq!(default_name(""), None);
    }
}
