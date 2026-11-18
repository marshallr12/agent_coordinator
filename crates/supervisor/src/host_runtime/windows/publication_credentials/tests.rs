//! The broker's push credentials without GitHub: a fake mints and records
//! revocations. Minting a real token is the owner-attended run on MiniAir.
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Credentials that count mints, record revocations and can be broken.
#[derive(Default)]
struct Fake {
    minted: AtomicU32,
    revoked: Mutex<Vec<String>>,
    broken: AtomicBool,
}

impl Credentials for Arc<Fake> {
    /// `token-<n>`, or an error while broken.
    fn mint(&self) -> Result<String> {
        ensure!(!self.broken.load(Ordering::SeqCst), "GitHub is down");
        let n = self.minted.fetch_add(1, Ordering::SeqCst);
        Ok(format!("token-{n}"))
    }

    /// Records `token`.
    fn revoke(&self, token: &str) -> Result<()> {
        self.revoked.lock().unwrap().push(token.to_owned());
        Ok(())
    }
}

/// An authority over a fake, and the fake.
fn authority() -> (PushAuthority, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let askpass = Path::new(r"C:\Program Files\agentc\agentc-supervisor.exe");
    (
        PushAuthority::new(Box::new(fake.clone()), askpass).unwrap(),
        fake,
    )
}

#[test]
fn a_token_is_scoped_to_the_repository_with_contents_write_only() {
    let scope = token_scope("agent_coordinator");
    let expected = json!({"repositories": ["agent_coordinator"],
        "permissions": {"contents": "write"}});
    assert_eq!(scope, expected);
}

#[test]
fn a_push_s_token_reaches_git_only_through_askpass_and_is_revoked_after() {
    let (authority, fake) = authority();
    let token = authority.mint_for("l").unwrap();
    let environment: BTreeMap<_, _> = token.git_environment().unwrap().into_iter().collect();
    assert_eq!(
        environment["GIT_ASKPASS"],
        r"C:\Program Files\agentc\agentc-supervisor.exe"
    );
    assert_eq!(environment[push_askpass::TOKEN_VARIABLE], "token-0");
    assert_eq!(
        environment["GIT_CONFIG_VALUE_0"], "",
        "no credential helper"
    );
    assert!(fake.revoked.lock().unwrap().is_empty(), "revoked too early");
    drop(token);
    assert_eq!(*fake.revoked.lock().unwrap(), ["token-0"]);
}

#[test]
fn each_launch_has_its_own_mint_budget() {
    let (authority, fake) = authority();
    for _ in 0..MINT_BURST {
        drop(authority.mint_for("looping").unwrap());
    }
    let spent = authority.mint_for("looping").err();
    assert_eq!(spent, Some(NoToken::RateLimited));
    assert!(
        authority.mint_for("other").is_ok(),
        "another launch still mints"
    );
    assert_eq!(fake.minted.load(Ordering::SeqCst), MINT_BURST + 1);
}

#[test]
fn a_failed_mint_gives_no_token_and_revokes_nothing() {
    let (authority, fake) = authority();
    fake.broken.store(true, Ordering::SeqCst);
    assert_eq!(authority.mint_for("l").err(), Some(NoToken::Unavailable));
    assert!(fake.revoked.lock().unwrap().is_empty());
}

#[test]
fn the_askpass_program_must_be_an_absolute_path() {
    let fake = Arc::new(Fake::default());
    let relative = PushAuthority::new(Box::new(fake), Path::new("agentc-supervisor.exe"));
    assert!(relative.is_err());
}

/// App settings with the key at `key`.
fn app(key: &Path) -> GithubConfig {
    GithubConfig {
        api_base: "https://api.github.com".into(),
        app_id: 1,
        installation_id: 2,
        private_key: key.to_path_buf(),
    }
}

#[test]
fn credentials_need_a_github_remote_and_a_key_that_signs() {
    let dir = tempfile::tempdir().unwrap();
    let (missing, garbage) = (
        dir.path().join("missing.pem"),
        dir.path().join("garbage.pem"),
    );
    std::fs::write(&garbage, b"not a key").unwrap();
    let github = "https://github.com/owner/repository.git";
    let refused = |config: &GithubConfig, remote: &str| {
        let made = GithubCredentials::new(config, remote);
        format!("{:#}", made.err().expect("credentials were made"))
    };
    assert!(refused(&app(&garbage), r"C:\remote.git").contains("github.com"));
    assert!(refused(&app(&missing), github).contains("read the push key"));
    assert!(refused(&app(&garbage), github).contains("cannot sign"));
}
