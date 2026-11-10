//! Who may use a service's named pipe, decided from text and reported
//! facts alone. Nothing here calls the operating system: the Windows pipe
//! asks Windows who connected and passes the answers to [`admit`], and
//! carries out the access list [`descriptor`] writes.
//!
//! There is no open fallback anywhere in this file. A SID that is not plain
//! SID text, a pipe whose list cannot be written, a client Windows did not
//! identify and a client that is not on the list are each refused.
use anyhow::{Result, ensure};

/// The most bytes of a pipe's name below `\\.\pipe\`.
const NAME_LIMIT: usize = 200;
/// What the one admitted client may do with the pipe: read and write data,
/// read attributes, and synchronize. Not `FILE_CREATE_PIPE_INSTANCE`
/// (`0x4`, the same bit as `FILE_APPEND_DATA`): a client must not be able
/// to make an instance of the server's pipe.
const CLIENT_RIGHTS: &str = "0x100083";

/// `sid` if it is plain `S-1-...` SID text. A SID is spliced into SDDL, so
/// anything else could add an entry of its own.
pub fn checked_sid(sid: &str) -> Result<&str> {
    let plain = sid.starts_with("S-1-")
        && sid.len() <= 184
        && sid[4..].split('-').all(|part| {
            !part.is_empty() && part.len() <= 10 && part.bytes().all(|b| b.is_ascii_digit())
        });
    ensure!(plain, "{sid:?} is not SID text");
    Ok(sid)
}

/// The full path of the pipe called `name`: `\\.\pipe\<name>`. The name is
/// one plain path component; anything that could name another pipe or leave
/// the pipe namespace is refused.
pub fn pipe_path(name: &str) -> Result<String> {
    let plain = !name.is_empty()
        && name.len() <= NAME_LIMIT
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    ensure!(plain, "{name:?} cannot name a pipe");
    Ok(format!(r"\\.\pipe\{name}"))
}

/// The access list of the pipe: `runner` in full and `client` as
/// [`CLIENT_RIGHTS`], and nobody else. It is protected, so nothing is
/// inherited, and the default list that would let everyone connect is
/// never applied.
pub fn descriptor(runner: &str, client: &str) -> Result<String> {
    let (runner, client) = (checked_sid(runner)?, checked_sid(client)?);
    if runner == client {
        return Ok(format!("D:P(A;;GA;;;{runner})"));
    }
    Ok(format!(
        "D:P(A;;GA;;;{runner})(A;;{CLIENT_RIGHTS};;;{client})"
    ))
}

/// The accounts a pipe serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted {
    allowed: Vec<String>,
}

impl Admitted {
    /// The pipe's client and the runner itself, each as SID text.
    pub fn new(runner: &str, client: &str) -> Result<Self> {
        let mut allowed = vec![checked_sid(runner)?.to_owned()];
        if client != runner {
            allowed.push(checked_sid(client)?.to_owned());
        }
        Ok(Self { allowed })
    }
}

/// What Windows reported about a connected client. `None` means Windows
/// was asked and did not answer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClientFacts {
    /// The account of the client's token, read back from the connection.
    pub account: Option<String>,
    /// The client's process id, read back from the connection.
    pub process: Option<u32>,
}

/// A client that was identified and admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub account: String,
    pub process: u32,
}

/// Why a client is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Windows did not say which account connected.
    AccountUnknown,
    /// Windows did not say which process connected.
    ProcessUnknown,
    /// The account is not one the pipe serves.
    NotAllowed(String),
    /// The client connected and sent nothing for too long. Windows names a
    /// connection's account only once something was read from it, so a
    /// silent client cannot be identified.
    Silent,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AccountUnknown => write!(f, "Windows did not identify the client's account"),
            Self::ProcessUnknown => write!(f, "Windows did not identify the client's process"),
            Self::NotAllowed(account) => write!(f, "the account {account} is not served"),
            Self::Silent => write!(f, "the client sent nothing, so it was never identified"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Admits `facts` only when both the account and the process are known and
/// the account is one `served` lists. The comparison is of SID text as
/// Windows printed it; a reported value that is not SID text is refused.
pub fn admit(served: &Admitted, facts: &ClientFacts) -> Result<Client, Refusal> {
    let account = facts.account.as_deref().ok_or(Refusal::AccountUnknown)?;
    let process = facts.process.ok_or(Refusal::ProcessUnknown)?;
    let known = checked_sid(account).is_ok();
    let listed = served
        .allowed
        .iter()
        .any(|sid| sid.eq_ignore_ascii_case(account));
    if !known || !listed {
        return Err(Refusal::NotAllowed(account.to_owned()));
    }
    Ok(Client {
        account: account.to_owned(),
        process,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNER: &str = "S-1-5-21-1-2-3-1001";
    const CLIENT: &str = "S-1-5-21-1-2-3-1002";
    const OTHER: &str = "S-1-5-21-1-2-3-1003";

    fn served() -> Admitted {
        Admitted::new(RUNNER, CLIENT).unwrap()
    }

    fn facts(account: Option<&str>, process: Option<u32>) -> ClientFacts {
        ClientFacts {
            account: account.map(str::to_owned),
            process,
        }
    }

    #[test]
    fn the_configured_client_and_the_runner_itself_are_admitted() {
        for account in [CLIENT, RUNNER] {
            let admitted = admit(&served(), &facts(Some(account), Some(4242)));
            let client = admitted.unwrap();
            assert_eq!((client.account.as_str(), client.process), (account, 4242));
        }
    }

    #[test]
    fn any_other_account_is_refused() {
        for account in [OTHER, "S-1-1-0", "S-1-5-7", "S-1-5-32-544", "S-1-5-18"] {
            let refused = admit(&served(), &facts(Some(account), Some(7))).unwrap_err();
            assert_eq!(
                refused,
                Refusal::NotAllowed(account.to_owned()),
                "{account}"
            );
        }
    }

    #[test]
    fn a_client_windows_could_not_identify_is_refused_never_served() {
        let unknown = admit(&served(), &facts(None, Some(7))).unwrap_err();
        assert_eq!(unknown, Refusal::AccountUnknown);
        let faceless = admit(&served(), &facts(Some(CLIENT), None)).unwrap_err();
        assert_eq!(faceless, Refusal::ProcessUnknown);
        let nothing = admit(&served(), &ClientFacts::default()).unwrap_err();
        assert_eq!(nothing, Refusal::AccountUnknown);
        for refusal in [unknown, faceless] {
            assert!(refusal.to_string().contains("did not identify"));
        }
    }

    #[test]
    fn text_that_only_looks_like_an_allowed_account_is_refused() {
        for forged in [
            "",
            "S-1-5-21-1-2-3-100",
            "S-1-5-21-1-2-3-10020",
            "S-1-5-21-1-2-3-1002 ",
            "s-1-5-21-1-2-3-1002x",
            "S-1-5-21-1-2-3-1002)(A;;GA;;;WD",
        ] {
            let outcome = admit(&served(), &facts(Some(forged), Some(7)));
            assert!(outcome.is_err(), "{forged:?}");
        }
    }

    #[test]
    fn a_client_that_is_also_the_runner_is_listed_once() {
        let same = Admitted::new(RUNNER, RUNNER).unwrap();
        assert_eq!(same.allowed, [RUNNER]);
        assert!(admit(&same, &facts(Some(RUNNER), Some(1))).is_ok());
        assert!(admit(&same, &facts(Some(CLIENT), Some(1))).is_err());
    }

    #[test]
    fn the_access_list_names_the_runner_and_one_client_and_nobody_else() {
        let list = descriptor(RUNNER, CLIENT).unwrap();
        assert_eq!(
            list,
            format!("D:P(A;;GA;;;{RUNNER})(A;;0x100083;;;{CLIENT})")
        );
        assert!(list.starts_with("D:P("), "the list is protected");
        assert_eq!(list.matches("(A;").count(), 2, "exactly two entries");
        assert!(!list.contains("WD") && !list.contains("AN") && !list.contains("BU"));
        assert_eq!(
            descriptor(RUNNER, RUNNER).unwrap(),
            format!("D:P(A;;GA;;;{RUNNER})")
        );
    }

    #[test]
    fn the_client_may_not_create_instances_of_the_pipe() {
        let rights = u32::from_str_radix(CLIENT_RIGHTS.trim_start_matches("0x"), 16).unwrap();
        let (read, write, create_instance) = (0x1, 0x2, 0x4);
        assert_eq!(rights & read, read);
        assert_eq!(rights & write, write);
        assert_eq!(rights & create_instance, 0);
        let generic_all = 0x1000_0000;
        assert_eq!(rights & generic_all, 0);
    }

    #[test]
    fn only_plain_sid_text_reaches_the_access_list() {
        for forged in ["", "WD", "S-1-5-32)(A;;GA;;;WD", "S-1-5--32", "S-1-5-3x"] {
            assert!(descriptor(forged, CLIENT).is_err(), "{forged:?}");
            assert!(descriptor(RUNNER, forged).is_err(), "{forged:?}");
            assert!(Admitted::new(RUNNER, forged).is_err(), "{forged:?}");
            assert!(Admitted::new(forged, CLIENT).is_err(), "{forged:?}");
        }
    }

    #[test]
    fn a_pipe_name_is_one_plain_component_in_the_pipe_namespace() {
        assert_eq!(
            pipe_path("agentc-runner").unwrap(),
            r"\\.\pipe\agentc-runner"
        );
        for bad in ["", ".x", "a\\b", "a/b", "..\\x", "a b", "a:b", "x\0"] {
            assert!(pipe_path(bad).is_err(), "{bad:?}");
        }
        assert!(pipe_path(&"a".repeat(201)).is_err());
    }
}
