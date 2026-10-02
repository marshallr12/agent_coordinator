//! Git credential prompts answered by this same binary. At startup the
//! integrator points `GIT_ASKPASS` at itself and sets a marker variable; Git
//! then runs `agentc-integrator "<prompt>"`, which prints the App user name or
//! a freshly minted installation token. No token lands in a URL, a Git config
//! or a file. (The marker avoids `COORDINATOR_*` names, which the Git helpers
//! in `coordinator-local` strip from Git's environment.)
use crate::config::Config;
use crate::github::GithubApp;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Marker variable; its value is the config path (empty = default lookup).
pub const MARKER: &str = "AGENTC_INTEGRATOR_ASKPASS_CONFIG";
/// GitHub's user name for installation tokens.
pub const APP_USER: &str = "x-access-token";
/// The only host the helper hands credentials to.
pub const GITHUB_HOST: &str = "github.com";

/// Makes Git ask this binary for credentials. Must run before any thread
/// starts (it mutates the process environment).
pub fn install(config_path: Option<&Path>) -> Result<()> {
    let exe = std::env::current_exe()?;
    let marker = config_path
        .map(Path::as_os_str)
        .unwrap_or_default()
        .to_owned();
    // SAFETY: called from `main` before the Tokio runtime or any other thread exists.
    unsafe {
        std::env::set_var("GIT_ASKPASS", exe);
        std::env::set_var(MARKER, marker);
        std::env::set_var("GIT_TERMINAL_PROMPT", "0");
    }
    Ok(())
}

/// The prompt when this process was started by Git as its askpass helper.
pub fn invoked_prompt() -> Option<(Option<PathBuf>, String)> {
    let marker = std::env::var_os(MARKER)?;
    let mut args = std::env::args().skip(1);
    let prompt = args.next()?;
    let git_prompt = prompt.starts_with("Username for ") || prompt.starts_with("Password for ");
    if args.next().is_some() || !git_prompt {
        return None;
    }
    let config = (!marker.is_empty()).then(|| PathBuf::from(marker));
    Some((config, prompt))
}

/// Answers one Git prompt for GitHub HTTPS remotes only.
pub fn answer(config_path: Option<&Path>, prompt: &str) -> Result<String> {
    if prompt_host(prompt) != Some(GITHUB_HOST) {
        bail!("askpass: refusing a prompt for a non-GitHub remote");
    }
    if prompt.starts_with("Username") {
        return Ok(APP_USER.into());
    }
    if !prompt.starts_with("Password") {
        bail!("askpass: unexpected prompt");
    }
    let config = Config::load(config_path)?;
    let app = GithubApp::new(&config.github)?;
    tokio::runtime::Runtime::new()?.block_on(app.installation_token())
}

/// The exact host of the `'https://[user@]host[/…]'` URL in a Git prompt.
pub fn prompt_host(prompt: &str) -> Option<&str> {
    let url = prompt.split('\'').nth(1)?.strip_prefix("https://")?;
    let authority = url.split('/').next()?;
    Some(
        authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_is_the_app_user_and_foreign_hosts_are_refused() {
        let user = answer(None, "Username for 'https://github.com': ").unwrap();
        assert_eq!(user, APP_USER);
        assert!(answer(None, "Password for 'https://evil.example': ").is_err());
        assert!(answer(None, "Enter passphrase for key: ").is_err());
        let lookalike = "Password for 'https://x-access-token@github.com.evil.example': ";
        assert!(answer(None, lookalike).is_err());
        let real = "Password for 'https://x-access-token@github.com': ";
        assert_eq!(prompt_host(real), Some(GITHUB_HOST));
    }
}
