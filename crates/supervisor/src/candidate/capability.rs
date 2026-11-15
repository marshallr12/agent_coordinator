//! Which launches run candidate code, and whether a host can keep it away
//! from the launch's secrets (phase 5, step 5a).
//!
//! A reviewer builds and tests the implementer's candidate, so its command
//! tool runs hostile code. The answer depends on the host, the role, the
//! harness and, on Windows, the exact harness build: only a build whose
//! command paths were observed is known to send every command through the
//! prefix. Every answer that is not a supported isolation is a refusal with
//! its reason; there is no default that admits.
use crate::profile::{Harness, Role};

/// The hosts a launch can run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    Linux,
    Windows,
}

/// How a launch's candidate commands are kept apart from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// The launch's tools run no candidate code through a prefix.
    NotNeeded,
    /// The generated `candidate-shell` and an inner Bubblewrap (Linux).
    Bubblewrap,
    /// The shim, the launch's candidate pipe and a separate AppContainer
    /// launch per command (Windows).
    CandidateRunner,
}

/// A harness build with recorded command-path evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recorded {
    pub version: &'static str,
    /// SHA-256 of the harness executable, lower-case hex.
    pub sha256: &'static str,
    /// Where the observation is written down.
    pub evidence: &'static str,
}

/// Native Claude Code builds whose command paths were observed on Windows.
pub const WINDOWS_CLAUDE: &[Recorded] = &[Recorded {
    version: "2.1.325",
    sha256: "87e0cb6ae0f4775da388cd3043f4d8097ef961dcddbf55fe90bc61ffc3f57a30",
    evidence: "windows-host-design.md, Native Claude Code command paths, 2026-11-14",
}];

/// Tools a reviewer may be offered on Windows: the file tools, which run no
/// code, and Bash, which reaches the prefix. The PowerShell tool bypasses
/// the prefix, so it and any other tool are refused.
pub const ROUTED_TOOLS: &[&str] = &["Read", "Glob", "Grep", "Bash"];

/// Whether the launch's candidate runner answered its readiness check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runner {
    Ready,
    Missing,
    Broken(String),
}

/// What a decision is made from. The build and the runner matter only on
/// Windows; `None` is a build nobody identified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts<'a> {
    pub host: Host,
    pub role: Role,
    pub harness: Harness,
    pub build: Option<(&'a str, &'a str)>,
    pub tools: &'a [&'a str],
    pub runner: Runner,
}

/// The Linux refusal of a Codex reviewer, unchanged.
pub const LINUX_CODEX: &str = "Codex reviewer launches cannot isolate candidate code from \
     reviewer secrets (R-P3b.3); use a Claude reviewer";

/// How `facts`' launch isolates candidate code, or why it cannot launch.
pub fn decide(facts: &Facts<'_>) -> Result<Isolation, String> {
    if facts.role != Role::Reviewer {
        return Ok(Isolation::NotNeeded);
    }
    match (facts.host, facts.harness) {
        (Host::Linux, Harness::Claude) => Ok(Isolation::Bubblewrap),
        (Host::Linux, Harness::Codex) => Err(LINUX_CODEX.into()),
        (Host::Windows, Harness::Codex) => Err(
            "Codex reviewer launches are refused on Windows: Codex has no command prefix, \
             so candidate code cannot be kept from reviewer secrets"
                .into(),
        ),
        (Host::Windows, Harness::Claude) => windows_claude(facts),
    }
}

/// A Claude reviewer on Windows: a recorded build, only routed tools and a
/// ready candidate runner, each checked in turn.
fn windows_claude(facts: &Facts<'_>) -> Result<Isolation, String> {
    let (version, sha256) = facts.build.ok_or(
        "the Claude Code build of this reviewer launch is unknown, so its command paths \
         are not known to reach the candidate runner",
    )?;
    recorded(version, sha256)?;
    routed(facts.tools)?;
    match &facts.runner {
        Runner::Ready => Ok(Isolation::CandidateRunner),
        Runner::Missing => Err("the candidate runner is missing; the reviewer is refused".into()),
        Runner::Broken(why) => Err(format!(
            "the candidate runner is broken ({why}); the reviewer is refused"
        )),
    }
}

/// Ok when `version` with `sha256` has recorded evidence on Windows.
fn recorded(version: &str, sha256: &str) -> Result<(), String> {
    let known = WINDOWS_CLAUDE
        .iter()
        .any(|r| r.version == version && r.sha256.eq_ignore_ascii_case(sha256));
    known.then_some(()).ok_or_else(|| {
        format!(
            "Claude Code {version} (SHA-256 {sha256}) has no recorded command-path evidence \
             on Windows; only a recorded build may review"
        )
    })
}

/// Ok when every offered tool is one whose code reaches the prefix or runs
/// no code.
fn routed(tools: &[&str]) -> Result<(), String> {
    let other: Vec<&str> = (tools.iter().copied())
        .filter(|tool| !ROUTED_TOOLS.contains(tool))
        .collect();
    other.is_empty().then_some(()).ok_or_else(|| {
        format!(
            "the reviewer would be offered {} outside the candidate runner",
            other.join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "87e0cb6ae0f4775da388cd3043f4d8097ef961dcddbf55fe90bc61ffc3f57a30";

    /// A ready Windows Claude reviewer of the recorded build.
    fn windows() -> Facts<'static> {
        Facts {
            host: Host::Windows,
            role: Role::Reviewer,
            harness: Harness::Claude,
            build: Some(("2.1.325", SHA)),
            tools: ROUTED_TOOLS,
            runner: Runner::Ready,
        }
    }

    #[test]
    fn linux_answers_are_unchanged_whatever_the_build_or_runner() {
        for (build, runner) in [(None, Runner::Missing), (Some(("9", "x")), Runner::Ready)] {
            let facts = |harness| Facts {
                host: Host::Linux,
                harness,
                build,
                runner: runner.clone(),
                ..windows()
            };
            assert_eq!(decide(&facts(Harness::Claude)), Ok(Isolation::Bubblewrap));
            assert_eq!(decide(&facts(Harness::Codex)), Err(LINUX_CODEX.into()));
        }
    }

    #[test]
    fn implementers_need_no_candidate_isolation_on_either_host() {
        for host in [Host::Linux, Host::Windows] {
            for harness in [Harness::Claude, Harness::Codex] {
                let facts = Facts {
                    host,
                    harness,
                    role: Role::Implementer,
                    build: None,
                    runner: Runner::Missing,
                    ..windows()
                };
                assert_eq!(decide(&facts), Ok(Isolation::NotNeeded));
            }
        }
    }

    #[test]
    fn a_recorded_windows_claude_reviewer_with_a_ready_runner_is_admitted() {
        assert_eq!(decide(&windows()), Ok(Isolation::CandidateRunner));
        let upper = Facts {
            build: Some((
                "2.1.325",
                "87E0CB6AE0F4775DA388CD3043F4D8097EF961DCDDBF55FE90BC61FFC3F57A30",
            )),
            ..windows()
        };
        assert_eq!(decide(&upper), Ok(Isolation::CandidateRunner));
    }

    #[test]
    fn every_other_windows_reviewer_is_refused_with_its_reason() {
        let cases: [(Facts<'_>, &str); 7] = [
            (
                Facts {
                    harness: Harness::Codex,
                    ..windows()
                },
                "no command prefix",
            ),
            (
                Facts {
                    build: None,
                    ..windows()
                },
                "build of this reviewer launch is unknown",
            ),
            (
                Facts {
                    build: Some(("2.1.326", SHA)),
                    ..windows()
                },
                "2.1.326",
            ),
            (
                Facts {
                    build: Some(("2.1.325", "00")),
                    ..windows()
                },
                "no recorded",
            ),
            (
                Facts {
                    tools: &["Read", "Bash", "PowerShell"],
                    ..windows()
                },
                "PowerShell",
            ),
            (
                Facts {
                    runner: Runner::Missing,
                    ..windows()
                },
                "missing",
            ),
            (
                Facts {
                    runner: Runner::Broken("no pipe".into()),
                    ..windows()
                },
                "no pipe",
            ),
        ];
        for (facts, reason) in cases {
            let refusal = decide(&facts).unwrap_err();
            assert!(refusal.contains(reason), "{refusal}");
        }
    }

    #[test]
    fn the_reviewer_allowlist_offers_only_routed_tools() {
        let tools = crate::role_settings::tools(Role::Reviewer);
        assert_eq!(tools, ROUTED_TOOLS);
        assert_eq!(routed(&tools), Ok(()));
    }
}
