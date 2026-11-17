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
/// read attributes, read the pipe's security descriptor (so it can check
/// who owns the pipe before it trusts it) and synchronize. Not
/// `FILE_CREATE_PIPE_INSTANCE` (`0x4`, the same bit as `FILE_APPEND_DATA`):
/// a client must not be able to make an instance of the server's pipe.
pub const CLIENT_ACCESS: u32 = 0x12_0083;
/// [`CLIENT_ACCESS`] as the access list spells it.
const CLIENT_RIGHTS: &str = "0x120083";

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

/// A low integrity label that forbids writing up: an AppContainer token runs
/// at low integrity, and without it could not write its requests.
const LOW_LABEL: &str = "S:(ML;;NW;;;LW)";

/// The access list of one launch's candidate pipe: `runner` in full and the
/// launch's `package` as [`CLIENT_RIGHTS`], with a low label. The candidate
/// and the reviewer run as the runner's account, so the user part of the
/// check passes for both. Windows then also requires the token's package to
/// be granted, and only `package` is. The list names `runner` as the pipe's
/// owner: the shim uses a pipe only when its own account owns it, and the
/// owner Windows would pick by default is the Administrators group when the
/// runner's token is elevated (seen 2026-11-17).
pub fn package_descriptor(runner: &str, package: &str) -> Result<String> {
    let (runner, package) = (checked_sid(runner)?, checked_sid(package)?);
    ensure!(
        runner != package,
        "a package cannot be the runner's account"
    );
    Ok(format!(
        "O:{runner}D:P(A;;GA;;;{runner})(A;;{CLIENT_RIGHTS};;;{package}){LOW_LABEL}"
    ))
}

/// The NETWORK logon SID, which every token of a logon over the network
/// carries.
const NETWORK: &str = "S-1-5-2";

/// The access list of one launch's broker pipe. `broker`, the account the
/// broker runs as, owns the pipe and has it in full; the launch's `account`
/// and its `package` each get [`CLIENT_RIGHTS`]. An AppContainer token is
/// granted only what both its user check and its package check grant, so
/// the launch's token needs both entries. A NETWORK token is denied
/// outright, the list is protected and the low label lets a low-integrity
/// client write. When the launch runs as the broker's own account, that
/// account's one entry is the owner's.
pub fn broker_descriptor(broker: &str, account: &str, package: &str) -> Result<String> {
    let (broker, account) = (checked_sid(broker)?, checked_sid(account)?);
    let package = checked_sid(package)?;
    ensure!(
        package != broker && package != account,
        "a package cannot be an account"
    );
    let client = if account == broker {
        String::new()
    } else {
        format!("(A;;{CLIENT_RIGHTS};;;{account})")
    };
    Ok(format!(
        "O:{broker}D:P(D;;GA;;;{NETWORK})(A;;GA;;;{broker}){client}\
         (A;;{CLIENT_RIGHTS};;;{package}){LOW_LABEL}"
    ))
}

/// The accounts a pipe serves and, for a launch's candidate pipe, the one
/// package its client's token must carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted {
    allowed: Vec<String>,
    package: Option<String>,
}

impl Admitted {
    /// The pipe's client and the runner itself, each as SID text.
    pub fn new(runner: &str, client: &str) -> Result<Self> {
        let mut allowed = vec![checked_sid(runner)?.to_owned()];
        if client != runner {
            allowed.push(checked_sid(client)?.to_owned());
        }
        Ok(Self {
            allowed,
            package: None,
        })
    }

    /// A launch's candidate pipe: the runner's account, and only from a
    /// token of exactly `package`. A token of the account with no package,
    /// or with another, is refused.
    pub fn package(runner: &str, package: &str) -> Result<Self> {
        let allowed = vec![checked_sid(runner)?.to_owned()];
        let package = Some(checked_sid(package)?.to_owned());
        Ok(Self { allowed, package })
    }

    /// A launch's broker pipe: only a token whose user is the launch's
    /// `account` and whose package is exactly the launch's `package`. The
    /// broker's own account is not served, and neither is the launch's
    /// account without that package.
    pub fn launch(account: &str, package: &str) -> Result<Self> {
        let allowed = vec![checked_sid(account)?.to_owned()];
        let package = Some(checked_sid(package)?.to_owned());
        Ok(Self { allowed, package })
    }
}

/// What Windows said about the AppContainer package of a client's token.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Package {
    /// Windows was asked and did not answer.
    #[default]
    Unknown,
    /// The token is not an AppContainer token.
    None,
    /// The token's package SID.
    Sid(String),
}

/// What Windows reported about a connected client. `None` means Windows
/// was asked and did not answer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClientFacts {
    /// The account of the client's token, read back from the connection.
    pub account: Option<String>,
    /// The client's process id, read back from the connection.
    pub process: Option<u32>,
    /// The package of the client's token, read back from the connection.
    pub package: Package,
}

/// A client that was identified and admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub account: String,
    pub process: u32,
    /// The package the pipe required and the token carried; `None` on a
    /// pipe that requires none.
    pub package: Option<String>,
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
    /// Windows did not say which package the client's token carries.
    PackageUnknown,
    /// The client's token does not carry the package the pipe serves:
    /// another package, or none.
    WrongPackage(Option<String>),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AccountUnknown => write!(f, "Windows did not identify the client's account"),
            Self::ProcessUnknown => write!(f, "Windows did not identify the client's process"),
            Self::NotAllowed(account) => write!(f, "the account {account} is not served"),
            Self::Silent => write!(f, "the client sent nothing, so it was never identified"),
            Self::PackageUnknown => write!(f, "Windows did not identify the client's package"),
            Self::WrongPackage(Some(package)) => write!(f, "the package {package} is not served"),
            Self::WrongPackage(None) => write!(f, "the client has no package and is not served"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Admits `facts` only when both the account and the process are known, the
/// account is one `served` lists and, where `served` names a package, the
/// token carries exactly that one. The comparison is of SID text as Windows
/// printed it; a reported value that is not SID text is refused.
pub fn admit(served: &Admitted, facts: &ClientFacts) -> Result<Client, Refusal> {
    let account = facts.account.as_deref().ok_or(Refusal::AccountUnknown)?;
    let process = facts.process.ok_or(Refusal::ProcessUnknown)?;
    if !listed(&served.allowed, account) {
        return Err(Refusal::NotAllowed(account.to_owned()));
    }
    let package = match &served.package {
        None => None,
        Some(required) => Some(carried(required, &facts.package)?),
    };
    Ok(Client {
        account: account.to_owned(),
        process,
        package,
    })
}

/// Whether `sid` is SID text that `allowed` lists.
fn listed(allowed: &[String], sid: &str) -> bool {
    checked_sid(sid).is_ok() && allowed.iter().any(|a| a.eq_ignore_ascii_case(sid))
}

/// The token's package when it is exactly `required`, or why not.
fn carried(required: &str, package: &Package) -> Result<String, Refusal> {
    match package {
        Package::Unknown => Err(Refusal::PackageUnknown),
        Package::None => Err(Refusal::WrongPackage(None)),
        Package::Sid(sid) if listed(&[required.to_owned()], sid) => Ok(sid.clone()),
        Package::Sid(sid) => Err(Refusal::WrongPackage(Some(sid.clone()))),
    }
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
            package: Package::Unknown,
        }
    }

    const PACKAGE: &str = "S-1-15-2-1-2-3-4-5-6-7";
    const OTHER_PACKAGE: &str = "S-1-15-2-1-2-3-4-5-6-8";

    /// The runner's account with `package` in its token.
    fn packaged(package: Package) -> ClientFacts {
        ClientFacts {
            package,
            ..facts(Some(RUNNER), Some(9))
        }
    }

    #[test]
    fn a_candidate_pipe_serves_only_its_own_package_of_the_runners_account() {
        let served = Admitted::package(RUNNER, PACKAGE).unwrap();
        let client = admit(&served, &packaged(Package::Sid(PACKAGE.into()))).unwrap();
        assert_eq!(client.package.as_deref(), Some(PACKAGE));
        let refusals = [
            (
                Package::Sid(OTHER_PACKAGE.into()),
                Refusal::WrongPackage(Some(OTHER_PACKAGE.into())),
            ),
            (Package::None, Refusal::WrongPackage(None)),
            (Package::Unknown, Refusal::PackageUnknown),
            (
                Package::Sid(format!("{PACKAGE}0")),
                Refusal::WrongPackage(Some(format!("{PACKAGE}0"))),
            ),
            (
                Package::Sid("WD".into()),
                Refusal::WrongPackage(Some("WD".into())),
            ),
        ];
        for (package, refusal) in refusals {
            assert_eq!(admit(&served, &packaged(package)), Err(refusal));
        }
        let stranger = ClientFacts {
            package: Package::Sid(PACKAGE.into()),
            ..facts(Some(CLIENT), Some(9))
        };
        assert_eq!(
            admit(&served, &stranger),
            Err(Refusal::NotAllowed(CLIENT.into()))
        );
    }

    #[test]
    fn an_account_pipe_ignores_the_package() {
        let client = admit(&served(), &packaged(Package::Unknown)).unwrap();
        assert_eq!(client.package, None);
    }

    #[test]
    fn a_candidate_pipe_list_names_the_runner_and_the_package_with_a_low_label() {
        let list = package_descriptor(RUNNER, PACKAGE).unwrap();
        assert_eq!(
            list,
            format!("O:{RUNNER}D:P(A;;GA;;;{RUNNER})(A;;0x120083;;;{PACKAGE})S:(ML;;NW;;;LW)")
        );
        assert!(package_descriptor(RUNNER, RUNNER).is_err());
        assert!(package_descriptor(RUNNER, "S-1-15-2)(A;;GA;;;WD").is_err());
        assert!(Admitted::package(RUNNER, "WD").is_err());
    }

    /// The account a broker runs as, apart from the launch's.
    const BROKER: &str = "S-1-5-80-1-2-3-4-5";

    #[test]
    fn a_broker_pipe_list_names_its_owner_the_launchs_account_and_package_and_denies_network() {
        let list = broker_descriptor(BROKER, CLIENT, PACKAGE).unwrap();
        assert_eq!(
            list,
            format!(
                "O:{BROKER}D:P(D;;GA;;;S-1-5-2)(A;;GA;;;{BROKER})(A;;0x120083;;;{CLIENT})\
                 (A;;0x120083;;;{PACKAGE})S:(ML;;NW;;;LW)"
            )
        );
        assert_eq!(list.matches("(A;").count(), 3, "exactly three grants");
        assert!(!list.contains("WD") && !list.contains("AC") && !list.contains("BU"));
        let same = broker_descriptor(CLIENT, CLIENT, PACKAGE).unwrap();
        assert_eq!(
            same,
            format!(
                "O:{CLIENT}D:P(D;;GA;;;S-1-5-2)(A;;GA;;;{CLIENT})\
                 (A;;0x120083;;;{PACKAGE})S:(ML;;NW;;;LW)"
            )
        );
    }

    #[test]
    fn a_broker_pipe_list_takes_only_plain_sids_and_a_package_that_is_not_an_account() {
        assert!(broker_descriptor(BROKER, CLIENT, CLIENT).is_err());
        assert!(broker_descriptor(BROKER, CLIENT, BROKER).is_err());
        for forged in ["", "WD", "S-1-15-2)(A;;GA;;;WD"] {
            assert!(
                broker_descriptor(forged, CLIENT, PACKAGE).is_err(),
                "{forged:?}"
            );
            assert!(
                broker_descriptor(BROKER, forged, PACKAGE).is_err(),
                "{forged:?}"
            );
            assert!(
                broker_descriptor(BROKER, CLIENT, forged).is_err(),
                "{forged:?}"
            );
            assert!(Admitted::launch(forged, PACKAGE).is_err(), "{forged:?}");
            assert!(Admitted::launch(CLIENT, forged).is_err(), "{forged:?}");
        }
    }

    #[test]
    fn a_broker_pipe_serves_only_the_launchs_account_with_exactly_its_package() {
        let served = Admitted::launch(CLIENT, PACKAGE).unwrap();
        let token = |account: &str, package: Package| ClientFacts {
            package,
            ..facts(Some(account), Some(9))
        };
        let client = admit(&served, &token(CLIENT, Package::Sid(PACKAGE.into()))).unwrap();
        assert_eq!(
            (client.account.as_str(), client.package.as_deref()),
            (CLIENT, Some(PACKAGE))
        );
        let refusals = [
            (token(CLIENT, Package::None), Refusal::WrongPackage(None)),
            (
                token(CLIENT, Package::Sid(OTHER_PACKAGE.into())),
                Refusal::WrongPackage(Some(OTHER_PACKAGE.into())),
            ),
            (token(CLIENT, Package::Unknown), Refusal::PackageUnknown),
            (
                token(OTHER, Package::Sid(PACKAGE.into())),
                Refusal::NotAllowed(OTHER.into()),
            ),
            (
                token(BROKER, Package::Sid(PACKAGE.into())),
                Refusal::NotAllowed(BROKER.into()),
            ),
        ];
        for (facts, refusal) in refusals {
            assert_eq!(admit(&served, &facts), Err(refusal));
        }
    }

    #[test]
    fn package_refusals_say_what_was_wrong() {
        assert!(
            Refusal::PackageUnknown
                .to_string()
                .contains("did not identify")
        );
        assert!(
            Refusal::WrongPackage(None)
                .to_string()
                .contains("no package")
        );
        let other = Refusal::WrongPackage(Some(OTHER_PACKAGE.into()));
        assert!(other.to_string().contains(OTHER_PACKAGE));
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
            format!("D:P(A;;GA;;;{RUNNER})(A;;0x120083;;;{CLIENT})")
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
        assert_eq!(rights, CLIENT_ACCESS);
        let (write_dac, write_owner, delete) = (0x4_0000, 0x8_0000, 0x1_0000);
        assert_eq!(rights & (write_dac | write_owner | delete), 0);
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
