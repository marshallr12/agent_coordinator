//! The tip monitor's provenance check (p4-design §2 "Tip monitor",
//! `unreviewed_landing`): the commits of a forward move of the target that
//! lie outside the landing ranges (`t0..R`) of this integrator's own results
//! are out-of-band, and those that carry an agent trailer are reported for
//! post-hoc review. A commit without one is
//! the user's own work and is not reported (U2). The check is best effort,
//! not a security control: a session can strip its trailers, and commits of
//! unknown provenance are not detected.
use crate::checks::ChecksSource;
use crate::gates::{short_digest, target_report};
use crate::git::{self, CommitInfo};
use crate::integrate::Integrator;
use crate::service::{NewReport, Target};
use crate::state::Published;
use anyhow::Result;
use serde::Serialize;
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;

/// Co-author name words that mark an agent.
const AGENT_NAMES: &[&str] = &["claude", "codex"];
/// Co-author addresses that mark an agent.
const AGENT_ADDRESSES: &[&str] = &["noreply@anthropic.com", "noreply@openai.com"];
/// Trailer keys only agent sessions write (compared lowercase).
const SESSION_KEYS: &[&str] = &["claude-session"];
/// Flagged commits listed in one report; the rest are only counted.
const MAX_LISTED: usize = 50;
/// Agent trailers listed per flagged commit.
const MAX_TRAILERS: usize = 3;
/// Bytes kept of a listed subject or trailer (cut on a character boundary).
const MAX_TEXT: usize = 160;
/// Human commits of one move whose files are recorded; the service keeps a
/// file for a day, so a long run of older commits adds nothing.
const MAX_SHIPS: usize = 50;
/// Files recorded per human commit (the service accepts 1,000).
const MAX_SHIP_FILES: usize = 1_000;
/// Serialized details size the report stays within, below the service's
/// 65,536-byte limit with room for the envelope.
const MAX_DETAILS_BYTES: usize = 60_000;

/// A landed commit that carries at least one agent trailer.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Flagged {
    pub sha: String,
    pub subject: String,
    /// The matched trailers, as `Key: value`.
    pub trailers: Vec<String>,
}

/// One forward move of a target from tip `from` to tip `to`.
pub struct Landing<'a> {
    pub target: &'a Target,
    pub from: &'a str,
    pub to: &'a str,
    /// The results of this integrator the move contains; their own landing
    /// ranges are not out-of-band.
    pub results: Vec<String>,
}

impl<C: ChecksSource> Integrator<C> {
    /// Reports the agent-trailer commits of the forward move `from..to` of
    /// `target` that this integrator did not publish. True when the move is
    /// settled (nothing to report, stored, or refused); false when the
    /// report was neither stored nor refused, so the caller keeps the old tip and
    /// the next cycle classifies the move again.
    pub(crate) async fn classify_move(
        &self,
        project: &str,
        mirror: &Path,
        target: &Target,
        (from, to): (&str, &str),
    ) -> Result<bool> {
        let published = self.state.published(&target.key());
        let (commits, results) = out_of_band(mirror, (from, to), published)?;
        self.record_human_ships(project, mirror, &commits).await;
        let landing = Landing {
            target,
            from,
            to,
            results,
        };
        match landing_report(&landing, &commits) {
            Some(report) => Ok(self.report_best_effort(project, report).await),
            None => Ok(true),
        }
    }
}

impl<C: ChecksSource> Integrator<C> {
    /// Tells the service which files the user's own out-of-band commits (no
    /// agent trailer) changed. Advisory, like the rest of this check: a
    /// failure is logged and never holds the move.
    async fn record_human_ships(&self, project: &str, mirror: &Path, commits: &[CommitInfo]) {
        for commit in commits.iter().filter(|c| flag(c).is_none()).take(MAX_SHIPS) {
            let files = match git::changed_files(mirror, &commit.sha) {
                Ok(files) => files,
                Err(error) => {
                    eprintln!(
                        "agentc-integrator: {project}: files of {}: {error:#}",
                        commit.sha
                    );
                    continue;
                }
            };
            let files: Vec<String> = files
                .into_iter()
                .filter(|file| file.len() <= 1024 && !file.contains('\\'))
                .take(MAX_SHIP_FILES)
                .collect();
            if files.is_empty() {
                continue;
            }
            match self.service.human_ship(project, &commit.sha, &files).await {
                Ok(Ok(_)) => {}
                Ok(Err(refusal)) => eprintln!(
                    "agentc-integrator: {project}: human ship {}: {}",
                    commit.sha, refusal.code
                ),
                Err(error) => eprintln!(
                    "agentc-integrator: {project}: human ship {}: {error:#}",
                    commit.sha
                ),
            }
        }
    }
}

/// The commits of `from..to` outside every contained result's own landing
/// range (`t0..R`, or R alone when `t0` is unknown), with the SHAs of the
/// contained results.
fn out_of_band(
    mirror: &Path,
    (from, to): (&str, &str),
    published: &[Published],
) -> Result<(Vec<CommitInfo>, Vec<String>)> {
    let commits = git::commits_between(mirror, from, to)?;
    let contained = |p: &&Published| commits.iter().any(|c| c.sha == p.r);
    let results: Vec<&Published> = published.iter().filter(contained).collect();
    let mut own = HashSet::new();
    for result in &results {
        own.extend(own_range(mirror, result)?);
    }
    let foreign = commits.into_iter().filter(|c| !own.contains(&c.sha));
    let shas = results.iter().map(|result| result.r.clone()).collect();
    Ok((foreign.collect(), shas))
}

/// The commits a result landed itself: `t0..R`, or R alone when `t0` is
/// unknown.
fn own_range(mirror: &Path, result: &Published) -> Result<Vec<String>> {
    match &result.t0 {
        Some(t0) => git::landing_range(mirror, t0, &result.r),
        None => Ok(vec![result.r.clone()]),
    }
}

/// The `unreviewed_landing` report for a landing, or `None` when no commit
/// carries an agent trailer. Keyed by target, `from` and `to`, so a replay
/// after a crash stores nothing new. Listed commits are dropped from the
/// end until the details fit [`MAX_DETAILS_BYTES`]; `flagged_count` stays
/// the total.
pub fn landing_report(landing: &Landing, commits: &[CommitInfo]) -> Option<NewReport> {
    let flagged: Vec<Flagged> = commits.iter().filter_map(flag).collect();
    if flagged.is_empty() {
        return None;
    }
    let target = landing.target;
    let mut details = json!({"repository_url": target.repository_url,
        "target_branch": target.target_branch, "from": landing.from, "to": landing.to,
        "integrator_results": landing.results, "flagged_count": flagged.len(),
        "unflagged_count": commits.len() - flagged.len(),
        "flagged": &flagged[..flagged.len().min(MAX_LISTED)]});
    fit(&mut details);
    let key = format!(
        "{}:{}:{}",
        short_digest(&target.key()),
        landing.from,
        landing.to
    );
    Some(target_report("unreviewed_landing", key, details))
}

/// Drops listed commits from the end of `details.flagged` until the
/// serialized details fit [`MAX_DETAILS_BYTES`].
fn fit(details: &mut serde_json::Value) {
    while details.to_string().len() > MAX_DETAILS_BYTES {
        let Some(listed) = details["flagged"].as_array_mut() else {
            return;
        };
        if listed.pop().is_none() {
            return;
        }
    }
}

/// The commit as a flagged entry when any of its trailers names an agent.
fn flag(commit: &CommitInfo) -> Option<Flagged> {
    let agent = commit.trailers.iter().filter(|line| is_agent_trailer(line));
    let trailers: Vec<String> = agent.take(MAX_TRAILERS).map(|l| clip(l)).collect();
    (!trailers.is_empty()).then(|| Flagged {
        sha: commit.sha.clone(),
        subject: clip(&commit.subject),
        trailers,
    })
}

/// True when a `Key: value` trailer line marks an agent: a session key, or
/// a `Co-authored-by` naming Claude or Codex or an agent vendor's no-reply
/// address (keys compared ignoring case).
pub fn is_agent_trailer(line: &str) -> bool {
    let Some((key, value)) = line.split_once(':') else {
        return false;
    };
    let key = key.trim().to_ascii_lowercase();
    if SESSION_KEYS.contains(&key.as_str()) {
        return true;
    }
    key == "co-authored-by" && names_agent(&value.to_ascii_lowercase())
}

/// True when a lowercase co-author value names an agent word or address.
fn names_agent(value: &str) -> bool {
    if AGENT_ADDRESSES
        .iter()
        .any(|address| value.contains(address))
    {
        return true;
    }
    let mut words = value.split(|c: char| !c.is_ascii_alphanumeric());
    words.any(|word| AGENT_NAMES.contains(&word))
}

/// `text` cut to at most [`MAX_TEXT`] bytes on a character boundary.
fn clip(text: &str) -> String {
    let mut end = text.len().min(MAX_TEXT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testing::{commit_message, git, remote};

    #[test]
    fn agent_co_authors_and_session_trailers_match() {
        let positives = [
            "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>",
            "co-authored-by: Someone <noreply@anthropic.com>",
            "Claude-Session: https://claude.ai/code/session_01",
            "Co-authored-by: Codex <codex@example.invalid>",
            "Co-authored-by: Bot <noreply@openai.com>",
        ];
        for line in positives {
            assert!(is_agent_trailer(line), "{line}");
        }
    }

    #[test]
    fn human_co_authors_and_other_trailers_do_not_match() {
        let negatives = [
            "Co-authored-by: Jane Doe <jane@example.com>",
            "Co-authored-by: Claudette Roe <claudette@example.com>",
            "Signed-off-by: Claude Bot <claude@example.com>",
            "Reviewed-by: Codex <codex@example.com>",
            "not a trailer",
        ];
        for line in negatives {
            assert!(!is_agent_trailer(line), "{line}");
        }
    }

    #[test]
    fn only_trailers_are_read_not_the_subject_or_body() {
        let remote = remote();
        let base = git(&remote.source, &["rev-parse", "HEAD"]);
        let body = "Ask Claude\n\nClaude-Session: quoted in prose, not a trailer.\n\nMore prose.";
        let plain = commit_message(&remote.source, "a.txt", "a\n", body);
        let trailer = "Add b\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>";
        let agent = commit_message(&remote.source, "b.txt", "b\n", trailer);
        let commits = git::commits_between(&remote.source, &base, &agent).unwrap();
        let shas: Vec<&str> = commits.iter().map(|c| c.sha.as_str()).collect();
        assert_eq!(shas, [agent.as_str(), plain.as_str()], "newest first");
        let flagged: Vec<Flagged> = commits.iter().filter_map(flag).collect();
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].sha, agent);
        assert_eq!(flagged[0].subject, "Add b");
        assert_eq!(flagged[0].trailers, [trailer.lines().last().unwrap()]);
    }

    /// A remote whose `main` is P, then an agent commit O (T0), then R
    /// merging an agent candidate C onto O, then an agent commit L. Returns
    /// the repository and (P, O, C, R, L).
    fn merged_history() -> (crate::git::testing::Remote, [String; 5]) {
        let remote = remote();
        let dir = remote.source.clone();
        let p = git(&dir, &["rev-parse", "HEAD"]);
        git(&dir, &["checkout", "--quiet", "-b", "candidate"]);
        let c = commit_message(&dir, "c.txt", "c\n", AGENT);
        git(&dir, &["checkout", "--quiet", "main"]);
        let o = commit_message(&dir, "o.txt", "o\n", AGENT);
        git(
            &dir,
            &[
                "merge",
                "--quiet",
                "--no-ff",
                "-m",
                "Integrate",
                "candidate",
            ],
        );
        let r = git(&dir, &["rev-parse", "HEAD"]);
        let l = commit_message(&dir, "l.txt", "l\n", AGENT);
        (remote, [p, o, c, r, l])
    }

    const AGENT: &str = "Agent\n\nClaude-Session: https://claude.ai/code/session_x";

    #[test]
    fn only_a_results_own_landing_range_is_the_integrators() {
        let (remote, [p, o, c, r, l]) = merged_history();
        let pair = Published {
            r: r.clone(),
            t0: Some(o.clone()),
            result_id: None,
        };
        let (foreign, results) = out_of_band(&remote.source, (&p, &l), &[pair]).unwrap();
        let shas: Vec<&str> = foreign.iter().map(|c| c.sha.as_str()).collect();
        assert_eq!(
            shas,
            [l.as_str(), o.as_str()],
            "C and R are the integrator's"
        );
        assert_eq!(results, std::slice::from_ref(&r));
        let bare = Published {
            r,
            t0: None,
            result_id: None,
        };
        let (foreign, _) = out_of_band(&remote.source, (&p, &l), &[bare]).unwrap();
        assert!(
            foreign.iter().any(|commit| commit.sha == c),
            "a bare R owns only R"
        );
        assert_eq!(foreign.len(), 3);
    }

    #[test]
    fn details_fit_the_service_limit_with_worst_case_text() {
        let wide = "\u{1D11E}".repeat(400);
        let escaped = "\u{1}".repeat(400);
        let commit = |n: usize| CommitInfo {
            sha: format!("{n:040}"),
            subject: wide.clone(),
            trailers: vec![format!("Co-authored-by: Claude <noreply@anthropic.com> {escaped}"); 5],
        };
        let commits: Vec<CommitInfo> = (0..200).map(commit).collect();
        let target = Target {
            repository_url: "https://example.test/r.git".into(),
            target_branch: "main".into(),
        };
        let landing = Landing {
            target: &target,
            from: "a",
            to: "b",
            results: vec![],
        };
        let report = landing_report(&landing, &commits).unwrap();
        assert!(report.details.to_string().len() <= 65_536);
        assert_eq!(report.details["flagged_count"], 200);
        let listed = report.details["flagged"].as_array().unwrap();
        assert!(!listed.is_empty());
        let subject = listed[0]["subject"].as_str().unwrap();
        assert!(subject.len() <= MAX_TEXT && subject.len() > MAX_TEXT - 4);
    }
}
