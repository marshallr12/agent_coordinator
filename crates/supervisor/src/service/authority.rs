//! Whether a launch still has coordinator authority (phase 6, step 6c).
//!
//! On Linux the candidate push helper is spawned for one launch and lives
//! only as long as it, so its authority is fixed at spawn. A Windows
//! publication broker is a long-running service, so it asks the supervisor
//! before each push whether the launch it serves may still publish. The
//! supervisor answers from [`Authority`]: the loop driver holds each launch
//! it claimed an attempt for, with the task and the attempt's id and
//! generation, and forgets it before it releases the attempt.
//!
//! A judgement never trusts that record alone: it reads the attempt again
//! through an [`AttemptReader`] and allows the launch only while the
//! attempt is `active` at the recorded generation. A launch the supervisor
//! does not hold (one of another supervisor, one from before a restart, or
//! one whose attempt it released), a task that is not the launch's, an
//! attempt in any other state or generation, and a read that fails are
//! each refused: authority fails closed.
use super::stand_in::Ledger;
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The only attempt state that lets a launch publish.
pub const ACTIVE: &str = "active";

/// What the supervisor holds for one launch it claimed an attempt for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub task: String,
    pub attempt: String,
    pub generation: u64,
}

/// An attempt as its coordinator describes it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptView {
    pub state: String,
    pub generation: u64,
}

/// Reads an attempt from its coordinator, every time it is asked.
pub trait AttemptReader: Send + Sync {
    /// `attempt` of `task` as it is now, or `None` when there is no such
    /// attempt. An error means the attempt could not be read.
    fn read(&self, task: &str, attempt: &str) -> Result<Option<AttemptView>>;
}

/// The answer to one question about a launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The launch's attempt is active at the generation it was claimed at.
    Current,
    /// The launch may not publish, and why.
    Refused(String),
}

/// The launches a supervisor holds attempts for, and how it reads them.
pub struct Authority {
    held: Mutex<BTreeMap<String, Held>>,
    reader: Box<dyn AttemptReader>,
}

impl Authority {
    /// An authority holding no launch, reading attempts through `reader`.
    pub fn new(reader: Box<dyn AttemptReader>) -> Self {
        Self {
            held: Mutex::default(),
            reader,
        }
    }

    /// Records that `launch` works for `held`'s attempt; a launch held
    /// already is recorded again.
    pub fn hold(&self, launch: &str, held: Held) {
        self.lock().insert(launch.to_owned(), held);
    }

    /// Forgets `launch`, so it is refused from now on.
    pub fn forget(&self, launch: &str) {
        self.lock().remove(launch);
    }

    /// Whether `launch`, which says it works on `task`, may publish now.
    /// The attempt is read with no lock held, so a slow coordinator holds
    /// up only this question.
    pub fn judge(&self, launch: &str, task: &str) -> Verdict {
        let Some(held) = self.lock().get(launch).cloned() else {
            return refused("the supervisor holds no attempt for this launch");
        };
        if held.task != task {
            return refused("the launch works on another task");
        }
        judged(&held, self.reader.read(&held.task, &held.attempt))
    }

    /// The record, even if a thread panicked while holding it: each change
    /// to it is a single insert or remove.
    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Held>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The verdict on `held` given what reading its attempt gave.
fn judged(held: &Held, read: Result<Option<AttemptView>>) -> Verdict {
    match read {
        Err(error) => refused(&format!("the attempt could not be read: {error:#}")),
        Ok(None) => refused("the coordinator has no such attempt"),
        Ok(Some(view)) if view.state != ACTIVE => {
            refused(&format!("the attempt is {}, not active", view.state))
        }
        Ok(Some(view)) if view.generation != held.generation => refused(&format!(
            "the attempt is at generation {}, not {}",
            view.generation, held.generation
        )),
        Ok(Some(_)) => Verdict::Current,
    }
}

/// A refusal saying `why`.
fn refused(why: &str) -> Verdict {
    Verdict::Refused(why.to_owned())
}

impl AttemptReader for Ledger {
    /// The stand-in's attempts, read from its ledger each time: an attempt
    /// is active from its claim until its release, always at generation 1,
    /// as the stand-in's leases are.
    fn read(&self, task: &str, attempt: &str) -> Result<Option<AttemptView>> {
        let events = self.events();
        let names = |event: &Value| event["attempt"] == attempt && event["task"] == task;
        let of = |kind: &str| (events.iter()).any(|e| e["event"] == kind && names(e));
        if !of("claim") {
            return Ok(None);
        }
        let state = if of("release") { "released" } else { ACTIVE };
        Ok(Some(AttemptView {
            state: state.to_owned(),
            generation: 1,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    use serde_json::json;
    use std::sync::Arc;

    /// A coordinator whose one attempt `a1` of task `t` is in a state the
    /// test sets; reading it fails while `broken`.
    #[derive(Default)]
    struct Fake {
        view: Mutex<Option<AttemptView>>,
        broken: Mutex<bool>,
        reads: Mutex<u32>,
    }

    impl Fake {
        /// Puts `a1` in `state` at `generation`.
        fn set(&self, state: &str, generation: u64) {
            let view = AttemptView {
                state: state.into(),
                generation,
            };
            *self.view.lock().unwrap() = Some(view);
        }
    }

    impl AttemptReader for Arc<Fake> {
        fn read(&self, task: &str, attempt: &str) -> Result<Option<AttemptView>> {
            *self.reads.lock().unwrap() += 1;
            if *self.broken.lock().unwrap() {
                bail!("the coordinator is unreachable");
            }
            let known = task == "t" && attempt == "a1";
            Ok(known.then(|| self.view.lock().unwrap().clone()).flatten())
        }
    }

    /// An authority over a fake coordinator, holding launch `l` for `a1`.
    fn holding() -> (Authority, Arc<Fake>) {
        let fake = Arc::new(Fake::default());
        fake.set(ACTIVE, 2);
        let authority = Authority::new(Box::new(fake.clone()));
        let held = Held {
            task: "t".into(),
            attempt: "a1".into(),
            generation: 2,
        };
        authority.hold("l", held);
        (authority, fake)
    }

    /// Whether `verdict` is a refusal whose reason contains `why`.
    fn refused_for(verdict: &Verdict, why: &str) -> bool {
        matches!(verdict, Verdict::Refused(reason) if reason.contains(why))
    }

    #[test]
    fn a_held_launch_whose_attempt_is_active_at_its_generation_may_publish() {
        let (authority, fake) = holding();
        assert_eq!(authority.judge("l", "t"), Verdict::Current);
        assert_eq!(authority.judge("l", "t"), Verdict::Current);
        assert_eq!(*fake.reads.lock().unwrap(), 2, "each question reads again");
    }

    #[test]
    fn a_launch_not_held_or_naming_another_task_is_refused_unread() {
        let (authority, fake) = holding();
        let stranger = authority.judge("m", "t");
        assert!(refused_for(&stranger, "holds no attempt"), "{stranger:?}");
        let other = authority.judge("l", "u");
        assert!(refused_for(&other, "another task"), "{other:?}");
        authority.forget("l");
        let forgotten = authority.judge("l", "t");
        assert!(refused_for(&forgotten, "holds no attempt"), "{forgotten:?}");
        assert_eq!(*fake.reads.lock().unwrap(), 0);
    }

    #[test]
    fn an_attempt_that_ended_moved_on_or_cannot_be_read_is_refused() {
        let (authority, fake) = holding();
        for state in ["submitted", "released", "expired", "recovery_required"] {
            fake.set(state, 2);
            let verdict = authority.judge("l", "t");
            assert!(refused_for(&verdict, "not active"), "{state}: {verdict:?}");
        }
        fake.set(ACTIVE, 3);
        let moved = authority.judge("l", "t");
        assert!(refused_for(&moved, "generation 3, not 2"), "{moved:?}");
        *fake.view.lock().unwrap() = None;
        let gone = authority.judge("l", "t");
        assert!(refused_for(&gone, "no such attempt"), "{gone:?}");
        fake.set(ACTIVE, 2);
        *fake.broken.lock().unwrap() = true;
        let unread = authority.judge("l", "t");
        assert!(refused_for(&unread, "could not be read"), "{unread:?}");
    }

    #[test]
    fn the_stand_ins_attempt_is_active_from_its_claim_to_its_release() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::at(&directory.path().join("ledger.jsonl"));
        assert_eq!(ledger.read("t", "a1").unwrap(), None, "nothing claimed");
        let claim = json!({"task": "t", "session": "s", "attempt": "a1"});
        ledger.append("claim", claim.clone()).unwrap();
        let active = ledger.read("t", "a1").unwrap().unwrap();
        assert_eq!((active.state.as_str(), active.generation), (ACTIVE, 1));
        assert_eq!(ledger.read("u", "a1").unwrap(), None, "another task");
        ledger.append("release", claim).unwrap();
        assert_eq!(ledger.read("t", "a1").unwrap().unwrap().state, "released");
    }
}
