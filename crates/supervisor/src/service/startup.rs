//! Judging what an earlier runner left in its journal when the service
//! starts.
//!
//! Every journaled launch is judged. One that Windows says ended has what it
//! left behind removed. One that may still run is left exactly as it is and
//! reported: a service that cannot prove a launch ended never touches it.
//! The [`Ledger`] has no way to start a launch, so nothing here can start
//! one again.
use anyhow::Result;

/// What is concluded about one journaled launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Windows says the launch ended.
    Ended,
    /// The launch may still run, or Windows would not say.
    MayRun,
}

/// One journaled launch and the judgment on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    pub launch: String,
    pub verdict: Verdict,
}

/// The journal, as the start-up needs it.
pub trait Ledger {
    /// Every journaled launch with its verdict, and the journal files that
    /// could not be read at all.
    fn judge_all(&self) -> Result<(Vec<Judged>, Vec<String>)>;
    /// Removes what an ended launch left behind. Refused for any launch that
    /// is not judged ended.
    fn reclaim(&mut self, launch: &str) -> Result<()>;
}

/// What start-up did and found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settled {
    /// Launches that ended and were reclaimed.
    pub reclaimed: Vec<String>,
    /// Launches that may still run: untouched, and reported.
    pub may_run: Vec<String>,
    /// Ended launches whose reclaim failed, with the reason; they stay
    /// journaled for the next start.
    pub failed: Vec<(String, String)>,
    /// Journal files that could not be read; a file of unknown origin is
    /// neither trusted nor removed.
    pub unreadable: Vec<String>,
}

impl Settled {
    /// One line per finding, for the service's log.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "start-up: {} reclaimed, {} may still run, {} failed, {} unreadable",
            self.reclaimed.len(),
            self.may_run.len(),
            self.failed.len(),
            self.unreadable.len()
        )];
        lines.extend(self.reclaimed.iter().map(|l| format!("reclaimed {l}")));
        lines.extend(
            self.may_run
                .iter()
                .map(|l| format!("{l} may still run; left untouched")),
        );
        lines.extend(
            self.failed
                .iter()
                .map(|(l, why)| format!("could not reclaim {l}: {why}")),
        );
        lines.extend(
            self.unreadable
                .iter()
                .map(|f| format!("unreadable journal file {f}")),
        );
        lines
    }
}

/// Judges the whole journal and reclaims the launches that ended.
pub fn settle(ledger: &mut impl Ledger) -> Result<Settled> {
    let (judged, unreadable) = ledger.judge_all()?;
    let mut settled = Settled {
        unreadable,
        ..Settled::default()
    };
    for Judged { launch, verdict } in judged {
        match verdict {
            Verdict::MayRun => settled.may_run.push(launch),
            Verdict::Ended => match ledger.reclaim(&launch) {
                Ok(()) => settled.reclaimed.push(launch),
                Err(error) => settled.failed.push((launch, format!("{error:#}"))),
            },
        }
    }
    Ok(settled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    use std::collections::BTreeMap;

    /// A journal of fake launches; `broken` ones cannot be reclaimed.
    #[derive(Default)]
    struct Fake {
        launches: BTreeMap<String, Verdict>,
        broken: Vec<String>,
        unreadable: Vec<String>,
        reclaim_calls: Vec<String>,
    }

    impl Ledger for Fake {
        fn judge_all(&self) -> Result<(Vec<Judged>, Vec<String>)> {
            let all = self.launches.iter().map(|(launch, verdict)| Judged {
                launch: launch.clone(),
                verdict: *verdict,
            });
            Ok((all.collect(), self.unreadable.clone()))
        }

        fn reclaim(&mut self, launch: &str) -> Result<()> {
            self.reclaim_calls.push(launch.to_owned());
            if self.launches.get(launch) != Some(&Verdict::Ended) {
                bail!("{launch} is not judged ended");
            }
            if self.broken.iter().any(|b| b == launch) {
                bail!("access denied");
            }
            self.launches.remove(launch);
            Ok(())
        }
    }

    fn journal(entries: &[(&str, Verdict)]) -> Fake {
        let launches = entries.iter().map(|(l, v)| (l.to_string(), *v));
        Fake {
            launches: launches.collect(),
            ..Fake::default()
        }
    }

    #[test]
    fn ended_launches_are_reclaimed_and_the_others_are_left_and_reported() {
        let mut ledger = journal(&[
            ("a", Verdict::Ended),
            ("b", Verdict::MayRun),
            ("c", Verdict::Ended),
        ]);
        let settled = settle(&mut ledger).unwrap();
        assert_eq!(settled.reclaimed, ["a", "c"]);
        assert_eq!(settled.may_run, ["b"]);
        assert_eq!(ledger.reclaim_calls, ["a", "c"], "b was never touched");
        assert!(ledger.launches.contains_key("b"));
        assert!(
            settled
                .lines()
                .iter()
                .any(|l| l == "b may still run; left untouched")
        );
    }

    #[test]
    fn a_failed_reclaim_is_reported_and_the_launch_stays_journaled() {
        let mut ledger = journal(&[("a", Verdict::Ended), ("b", Verdict::Ended)]);
        ledger.broken.push("a".into());
        let settled = settle(&mut ledger).unwrap();
        assert_eq!(settled.reclaimed, ["b"]);
        assert_eq!(settled.failed.len(), 1);
        assert_eq!(settled.failed[0].0, "a");
        assert!(settled.failed[0].1.contains("access denied"));
        assert!(ledger.launches.contains_key("a"));
    }

    #[test]
    fn an_unreadable_file_is_reported_and_nothing_is_done_about_it() {
        let mut ledger = journal(&[]);
        ledger.unreadable.push("torn.json".into());
        let settled = settle(&mut ledger).unwrap();
        assert_eq!(settled.unreadable, ["torn.json"]);
        assert!(ledger.reclaim_calls.is_empty());
        assert!(settled.lines().iter().any(|l| l.contains("torn.json")));
    }

    #[test]
    fn an_empty_journal_settles_to_nothing() {
        let settled = settle(&mut Fake::default()).unwrap();
        assert_eq!(settled, Settled::default());
        assert!(settled.lines()[0].starts_with("start-up: 0 reclaimed"));
    }
}
