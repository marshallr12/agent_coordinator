//! Carrying a service's launches through its ending.
//!
//! [`Drain::step`] is called again and again by the service's worker. While
//! the service runs it does nothing. Once a stop or preshutdown request is
//! seen it asks each launch to end by itself; a launch whose root ended is
//! killed at once; at the deadline it kills whatever is left. Killing does
//! not wait: every remaining launch is killed in the same step, and each
//! later step only looks whether its job has emptied, so the waits run side
//! by side against the service's one deadline and the service keeps
//! reporting meanwhile. A launch counts as ended only when
//! [`Fleet::emptied`] says the operating system confirmed it (and the fleet
//! journaled it), so the service reports itself stopped in quiescence only
//! when every launch it was responsible for is confirmed.
use super::lifecycle::{Clock, Lifecycle, Phase, Report, Signal};
use anyhow::Result;
use std::collections::BTreeSet;

/// The launches a service answers for.
pub trait Fleet {
    /// The launches whose end has not been confirmed and journaled.
    fn unconfirmed(&self) -> Vec<String>;
    /// Asks `launch` to end by itself.
    fn ask_stop(&mut self, launch: &str) -> Result<()>;
    /// Whether the root of `launch` has been seen to end. A failure to look
    /// is `false`: a lost observer proves nothing.
    fn root_ended(&mut self, launch: &str) -> bool;
    /// Kills everything `launch` started, without waiting for it to go.
    fn kill(&mut self, launch: &str) -> Result<()>;
    /// Whether the operating system confirms that nothing of `launch` is
    /// left. `Ok(true)` only then, and only once the end was journaled;
    /// anything else leaves the launch unconfirmed.
    fn emptied(&mut self, launch: &str) -> Result<bool>;
}

/// What one step did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// What to tell the operating system.
    pub report: Report,
    /// What went wrong in this step, for the service's log.
    pub problems: Vec<String>,
}

/// The progress of one service's ending.
#[derive(Debug, Default)]
pub struct Drain {
    /// Launches already asked to end.
    asked: BTreeSet<String>,
    /// Launches that were killed. Each is killed once.
    killed: BTreeSet<String>,
    /// Launches a failure to look was already reported for.
    complained: BTreeSet<String>,
}

impl Drain {
    /// A drain that has done nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Moves the service's ending one step on.
    pub fn step<C: Clock>(
        &mut self,
        life: &mut Lifecycle<C>,
        signal: &Signal,
        fleet: &mut impl Fleet,
    ) -> Step {
        let mut problems = Vec::new();
        match life.poll(signal) {
            Phase::Running | Phase::Stopped { .. } => {}
            Phase::Draining { .. } => {
                for launch in fleet.unconfirmed() {
                    if self.asked.insert(launch.clone()) {
                        if let Err(error) = fleet.ask_stop(&launch) {
                            problems.push(format!("ask {launch} to stop: {error:#}"));
                        }
                    } else if fleet.root_ended(&launch) {
                        self.end(fleet, &launch, &mut problems);
                    }
                }
                life.confirm(fleet.unconfirmed().len());
            }
            Phase::Terminating { .. } => {
                for launch in fleet.unconfirmed() {
                    self.end(fleet, &launch, &mut problems);
                }
                life.confirm(fleet.unconfirmed().len());
            }
        }
        let report = life.report();
        if report == (Report::Stopped { quiescent: false }) {
            let left = fleet.unconfirmed().into_iter();
            let unconfirmed =
                |launch| format!("{launch}: the operating system did not confirm an end");
            problems.extend(left.map(unconfirmed));
        }
        Step { report, problems }
    }

    /// Kills `launch` if that was not done yet, then looks whether it has
    /// gone. A failure to kill or to look is noted once.
    fn end(&mut self, fleet: &mut impl Fleet, launch: &str, problems: &mut Vec<String>) {
        if self.killed.insert(launch.to_owned())
            && let Err(error) = fleet.kill(launch)
        {
            problems.push(format!("terminate {launch}: {error:#}"));
        }
        if let Err(error) = fleet.emptied(launch)
            && self.complained.insert(launch.to_owned())
        {
            problems.push(format!("look at {launch}: {error:#}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::lifecycle::fake::Fake;
    use super::super::lifecycle::{Limits, Request};
    use super::*;
    use anyhow::bail;
    use std::collections::BTreeMap;
    use std::time::Duration;

    const S: Duration = Duration::from_secs(1);

    /// What a fake launch does when it is asked to stop and terminated.
    #[derive(Debug, Clone, Copy, Default)]
    struct Behaviour {
        /// Ends by itself once asked to stop.
        obeys: bool,
        /// The operating system cannot confirm its end.
        unconfirmable: bool,
        /// Terminating it fails outright.
        fails: bool,
    }

    #[derive(Debug, Default)]
    struct Launch {
        behaviour: Behaviour,
        asked: bool,
        ended: bool,
        killed: bool,
        confirmed: bool,
        terminations: u32,
    }

    #[derive(Debug, Default)]
    struct Pool(BTreeMap<String, Launch>);

    impl Pool {
        fn with(launches: &[(&str, Behaviour)]) -> Self {
            let launches = launches.iter().map(|(name, behaviour)| {
                let launch = Launch {
                    behaviour: *behaviour,
                    ..Launch::default()
                };
                (name.to_string(), launch)
            });
            Self(launches.collect())
        }
    }

    impl Fleet for Pool {
        fn unconfirmed(&self) -> Vec<String> {
            let pending = self.0.iter().filter(|(_, launch)| !launch.confirmed);
            pending.map(|(name, _)| name.clone()).collect()
        }

        fn ask_stop(&mut self, launch: &str) -> Result<()> {
            let launch = self.0.get_mut(launch).unwrap();
            launch.asked = true;
            launch.ended |= launch.behaviour.obeys;
            Ok(())
        }

        fn root_ended(&mut self, launch: &str) -> bool {
            self.0[launch].ended
        }

        fn kill(&mut self, launch: &str) -> Result<()> {
            let launch = self.0.get_mut(launch).unwrap();
            launch.terminations += 1;
            if launch.behaviour.fails {
                bail!("access denied");
            }
            launch.killed = true;
            Ok(())
        }

        fn emptied(&mut self, launch: &str) -> Result<bool> {
            let launch = self.0.get_mut(launch).unwrap();
            launch.confirmed = launch.killed && !launch.behaviour.unconfirmable;
            Ok(launch.confirmed)
        }
    }

    fn limits() -> Limits {
        Limits {
            stop_grace: 30 * S,
            preshutdown_grace: 10 * S,
            terminate_patience: 5 * S,
            status_interval: 2 * S,
        }
    }

    fn service() -> (Lifecycle<Fake>, Fake, Signal, Drain) {
        let clock = Fake::default();
        let life = Lifecycle::new(clock.clone(), limits());
        (life, clock, Signal::default(), Drain::new())
    }

    const OBEYS: Behaviour = Behaviour {
        obeys: true,
        unconfirmable: false,
        fails: false,
    };
    const STUBBORN: Behaviour = Behaviour {
        obeys: false,
        unconfirmable: false,
        fails: false,
    };

    #[test]
    fn a_running_service_touches_no_launch() {
        let (mut life, clock, signal, mut drain) = service();
        let mut pool = Pool::with(&[("a", OBEYS)]);
        clock.advance(100 * S);
        let step = drain.step(&mut life, &signal, &mut pool);
        assert_eq!(step.report, Report::Running);
        assert!(step.problems.is_empty());
        assert!(!pool.0["a"].asked && pool.0["a"].terminations == 0);
    }

    #[test]
    fn a_stop_asks_every_launch_then_confirms_the_ones_that_ended() {
        let (mut life, clock, signal, mut drain) = service();
        let mut pool = Pool::with(&[("a", OBEYS), ("b", STUBBORN)]);
        signal.request(Request::Stop);
        let first = drain.step(&mut life, &signal, &mut pool);
        assert!(pool.0["a"].asked && pool.0["b"].asked);
        assert!(matches!(
            first.report,
            Report::Stopping { checkpoint: 1, .. }
        ));
        clock.advance(S);
        drain.step(&mut life, &signal, &mut pool);
        assert!(
            pool.0["a"].confirmed,
            "an obedient launch is emptied at once"
        );
        assert!(!pool.0["b"].confirmed && pool.0["b"].terminations == 0);
        assert_eq!(pool.unconfirmed(), ["b"]);
    }

    #[test]
    fn a_launch_is_asked_once_and_whatever_remains_is_terminated_at_the_deadline() {
        let (mut life, clock, signal, mut drain) = service();
        let mut pool = Pool::with(&[("b", STUBBORN)]);
        signal.request(Request::Stop);
        for _ in 0..3 {
            clock.advance(S);
            drain.step(&mut life, &signal, &mut pool);
        }
        assert_eq!(pool.0["b"].terminations, 0);
        clock.advance(28 * S);
        let step = drain.step(&mut life, &signal, &mut pool);
        assert_eq!(pool.0["b"].terminations, 1);
        assert_eq!(step.report, Report::Stopped { quiescent: true });
        assert!(step.problems.is_empty());
    }

    #[test]
    fn the_service_reports_stopped_only_once_every_launch_is_confirmed() {
        let (mut life, clock, signal, mut drain) = service();
        let mut pool = Pool::with(&[("a", OBEYS), ("b", STUBBORN)]);
        signal.request(Request::Stop);
        let mut reports = Vec::new();
        for _ in 0..40 {
            clock.advance(S);
            let step = drain.step(&mut life, &signal, &mut pool);
            let quiet = pool.unconfirmed().is_empty();
            if matches!(step.report, Report::Stopped { quiescent: true }) {
                assert!(quiet, "stopped while a launch was unconfirmed");
            }
            reports.push(step.report);
            if matches!(step.report, Report::Stopped { .. }) {
                break;
            }
        }
        assert_eq!(reports.last(), Some(&Report::Stopped { quiescent: true }));
        // The request is seen at the first step, one second in, so the
        // deadline is at second 31: `b` is terminated there and not before.
        assert_eq!(reports.len(), 31);
    }

    #[test]
    fn every_remaining_launch_is_killed_in_one_step_before_any_is_waited_for() {
        let (mut life, clock, signal, mut drain) = service();
        let slow = Behaviour {
            unconfirmable: true,
            ..STUBBORN
        };
        let mut pool = Pool::with(&[("a", slow), ("b", STUBBORN), ("c", slow)]);
        signal.request(Request::Stop);
        drain.step(&mut life, &signal, &mut pool);
        clock.advance(30 * S);
        let step = drain.step(&mut life, &signal, &mut pool);
        // One step at the deadline killed all three, though `a` never goes.
        let kills: Vec<u32> = pool.0.values().map(|l| l.terminations).collect();
        assert_eq!(kills, [1, 1, 1]);
        assert!(pool.0["b"].confirmed);
        assert!(matches!(step.report, Report::Stopping { .. }));
    }

    #[test]
    fn an_end_the_system_did_not_confirm_is_not_a_clean_stop_and_is_not_retried() {
        let (mut life, clock, signal, mut drain) = service();
        let unconfirmable = Behaviour {
            unconfirmable: true,
            ..STUBBORN
        };
        let mut pool = Pool::with(&[("a", OBEYS), ("c", unconfirmable)]);
        signal.request(Request::Preshutdown);
        drain.step(&mut life, &signal, &mut pool);
        clock.advance(10 * S);
        let step = drain.step(&mut life, &signal, &mut pool);
        assert_eq!(pool.0["c"].terminations, 1);
        assert!(step.problems.is_empty(), "{:?}", step.problems);
        for _ in 0..4 {
            clock.advance(S);
            let step = drain.step(&mut life, &signal, &mut pool);
            assert!(step.problems.is_empty());
            assert!(matches!(step.report, Report::Stopping { .. }));
        }
        assert_eq!(pool.0["c"].terminations, 1);
        clock.advance(S);
        let last = drain.step(&mut life, &signal, &mut pool);
        assert_eq!(last.report, Report::Stopped { quiescent: false });
        assert_eq!(pool.unconfirmed(), ["c"]);
        assert_eq!(last.problems.len(), 1);
        assert!(
            last.problems[0].contains("did not confirm"),
            "{:?}",
            last.problems
        );
    }

    #[test]
    fn a_failed_termination_is_reported_and_leaves_the_launch_unconfirmed() {
        let (mut life, clock, signal, mut drain) = service();
        let fails = Behaviour {
            fails: true,
            ..STUBBORN
        };
        let mut pool = Pool::with(&[("d", fails)]);
        signal.request(Request::Stop);
        drain.step(&mut life, &signal, &mut pool);
        clock.advance(30 * S);
        let step = drain.step(&mut life, &signal, &mut pool);
        assert!(
            step.problems[0].contains("access denied"),
            "{:?}",
            step.problems
        );
        assert!(!pool.0["d"].confirmed);
        assert!(matches!(step.report, Report::Stopping { .. }));
    }

    #[test]
    fn a_service_with_nothing_to_end_stops_at_the_first_step() {
        let (mut life, _, signal, mut drain) = service();
        let mut pool = Pool::default();
        signal.request(Request::Stop);
        let step = drain.step(&mut life, &signal, &mut pool);
        assert_eq!(step.report, Report::Stopped { quiescent: true });
    }
}
