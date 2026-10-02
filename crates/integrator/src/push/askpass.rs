//! Git credential prompts answered by `agentc-push` itself. For each
//! connection the helper gives the Git children that contact the remote
//! `GIT_ASKPASS` pointing at this binary and the connection's installation
//! token in [`TOKEN_VARIABLE`]; Git then runs `agentc-push "<prompt>"`, which
//! prints the App user name or that token, for `github.com` prompts only. No
//! token lands in a URL, an argument, a Git config or a file.
use agentc_integrator::askpass::{APP_USER, GITHUB_HOST, prompt_host};
use anyhow::{Context, Result, bail, ensure};
use std::ffi::OsString;
use std::path::Path;

/// Holds the connection's token; its presence marks askpass mode. (The name
/// avoids `COORDINATOR_*`, which the Git helpers in `coordinator-local` strip
/// from Git's environment.)
pub const TOKEN_VARIABLE: &str = "AGENTC_PUSH_ASKPASS_TOKEN";

/// The token and prompt when Git runs this process as its askpass helper:
/// `marker` (the value of [`TOKEN_VARIABLE`]) is set and `arguments` (after
/// the program name) are exactly one Git user name or password prompt.
pub fn invoked_prompt(
    marker: Option<OsString>,
    mut arguments: impl Iterator<Item = String>,
) -> Option<(String, String)> {
    let token = marker?.into_string().ok()?;
    let prompt = arguments.next()?;
    let git_prompt = prompt.starts_with("Username for ") || prompt.starts_with("Password for ");
    if arguments.next().is_some() || !git_prompt {
        return None;
    }
    Some((token, prompt))
}

/// Answers one Git prompt for a `https://github.com` remote: the App user
/// name, or `token` as the password. Any other host is refused.
pub fn answer(prompt: &str, token: &str) -> Result<String> {
    if prompt_host(prompt) != Some(GITHUB_HOST) {
        bail!("askpass: refusing a prompt for a non-GitHub remote");
    }
    if prompt.starts_with("Username for ") {
        return Ok(APP_USER.into());
    }
    ensure!(
        prompt.starts_with("Password for "),
        "askpass: unexpected prompt"
    );
    ensure!(!token.is_empty(), "askpass: no token for this connection");
    Ok(token.to_owned())
}

/// Settings every Git child that contacts the remote gets: terminal prompts
/// off; configuration only from the repository and these variables (no
/// system or global file, no inherited `GIT_CONFIG_PARAMETERS`), with the
/// credential helpers cleared so none is asked first or stores the token;
/// and tracing off, so no trace writes the exchange anywhere. Variables Git
/// honours by mere presence, such as `GIT_SSL_NO_VERIFY`, cannot be
/// neutralised here; `process::clear_environment` removes them at startup.
const FIXED_ENVIRONMENT: &[(&str, &str)] = &[
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_PARAMETERS", ""),
    ("GIT_CONFIG_COUNT", "1"),
    ("GIT_CONFIG_KEY_0", "credential.helper"),
    ("GIT_CONFIG_VALUE_0", ""),
    ("GIT_TRACE", "0"),
    ("GIT_TRACE_PACKET", "0"),
    ("GIT_TRACE_CURL", "0"),
    ("GIT_TRACE2", "0"),
    ("GIT_TRACE2_EVENT", "0"),
    ("GIT_TRACE2_PERF", "0"),
];

/// The environment for Git children that contact the remote: this binary
/// (`askpass`) as `GIT_ASKPASS` with `token` beside it, plus
/// [`FIXED_ENVIRONMENT`].
pub fn git_environment(askpass: &Path, token: &str) -> Result<Vec<(String, String)>> {
    let askpass = askpass
        .to_str()
        .context("the askpass program path is not UTF-8")?;
    let pairs = [("GIT_ASKPASS", askpass), (TOKEN_VARIABLE, token)];
    Ok(pairs
        .iter()
        .chain(FIXED_ENVIRONMENT)
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "ghs_test_token";

    /// `invoked_prompt` with `marker` and one argument.
    fn invoked(marker: Option<&str>, argument: &str) -> Option<(String, String)> {
        invoked_prompt(
            marker.map(OsString::from),
            [argument.to_owned()].into_iter(),
        )
    }

    #[test]
    fn askpass_mode_needs_the_marker_and_one_git_prompt() {
        let prompt = "Password for 'https://x-access-token@github.com': ";
        assert_eq!(
            invoked(Some(TOKEN), prompt),
            Some((TOKEN.to_owned(), prompt.to_owned()))
        );
        assert_eq!(invoked(None, prompt), None);
        assert_eq!(invoked(Some(TOKEN), "serve"), None);
        let two = [prompt.to_owned(), "extra".to_owned()].into_iter();
        assert_eq!(invoked_prompt(Some(TOKEN.into()), two), None);
    }

    #[test]
    fn only_github_prompts_are_answered() {
        let user = answer("Username for 'https://github.com': ", TOKEN).unwrap();
        assert_eq!(user, "x-access-token");
        let password = "Password for 'https://x-access-token@github.com': ";
        assert_eq!(answer(password, TOKEN).unwrap(), TOKEN);
        for refused in [
            "Password for 'https://evil.example': ",
            "Password for 'https://x-access-token@github.com.evil.example': ",
            "Password for 'http://github.com': ",
            "Username for 'https://evil.example': ",
            "Enter passphrase for key: ",
        ] {
            assert!(answer(refused, TOKEN).is_err(), "{refused} answered");
        }
        assert!(answer(password, "").is_err());
    }

    #[test]
    fn git_environment_carries_the_token_only_in_its_variable() {
        let environment = git_environment(Path::new("/usr/bin/agentc-push"), TOKEN).unwrap();
        let carrying: Vec<&str> = environment
            .iter()
            .filter(|(_, value)| value.contains(TOKEN))
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(carrying, [TOKEN_VARIABLE]);
        let lookup = |name: &str| environment.iter().find(|(n, _)| n == name).unwrap();
        assert_eq!(lookup("GIT_ASKPASS").1, "/usr/bin/agentc-push");
        assert_eq!(lookup("GIT_TERMINAL_PROMPT").1, "0");
        assert_eq!(lookup("GIT_CONFIG_KEY_0").1, "credential.helper");
    }

    #[test]
    fn inherited_git_configuration_cannot_restore_a_credential_helper() {
        let hostile = [
            ("GIT_CONFIG_PARAMETERS", "'credential.helper'='store'"),
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "credential.helper"),
            ("GIT_CONFIG_VALUE_0", "cache"),
        ];
        let outside = tempfile::tempdir().unwrap();
        let helpers = |protected: bool| {
            let mut command = std::process::Command::new("git");
            command.current_dir(outside.path());
            command.args(["config", "--get-all", "credential.helper"]);
            command.envs(hostile);
            if protected {
                let environment = git_environment(Path::new("/x"), TOKEN).unwrap();
                command.envs(environment);
            }
            let output = command.output().unwrap();
            String::from_utf8(output.stdout).unwrap()
        };
        assert!(
            helpers(false).contains("store"),
            "control: hostile config unseen"
        );
        assert_eq!(helpers(true), "\n");
    }
}
