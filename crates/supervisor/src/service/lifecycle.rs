//! A service's life as a small state machine.
//!
//! ```text
//! Running --stop / preshutdown--> Draining --deadline--> Terminating
//!    |                               |                        |
//!    |                               +--- quiescent ----+     +--- quiescent / patience gone --+
//!    |                                                  v                                      v
//!    +--------------------------------------------- Stopped { quiescent }  <-------------------+
//! ```
//!
//! The operating system's control handler runs on a thread of its own and
//! must return at once, so it only calls [`Signal::request`]; the service's
//! worker is the only one that moves the machine, by calling
//! [`Lifecycle::poll`]. Admission ends the moment a request is seen, the
//! drain runs to a deadline on a monotonic [`Clock`], and the service may say
//! it has stopped in quiescence only after the caller has confirmed that
//! nothing it was responsible for remains.
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

/// A source of monotonic time. Only differences between two readings mean
/// anything; a reading never goes backwards.
pub trait Clock {
    /// The time since an origin this clock chose.
    fn now(&self) -> Duration;
}

/// The operating system's monotonic clock.
#[derive(Debug, Clone, Copy)]
pub struct Monotonic(Instant);

impl Monotonic {
    /// A clock whose origin is now.
    pub fn new() -> Self {
        Self(Instant::now())
    }
}

impl Default for Monotonic {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for Monotonic {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// Why the service was asked to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// An ordinary stop of this service.
    Stop,
    /// The machine is shutting down and has given the service a limited
    /// time before it ends the service itself.
    Preshutdown,
}

const STOP: u8 = 1;
const PRESHUTDOWN: u8 = 2;

/// What a control handler leaves for the worker. Recording is one atomic
/// operation: nothing here waits, allocates or fails.
#[derive(Debug, Default)]
pub struct Signal(AtomicU8);

impl Signal {
    /// Records `request`. Both kinds may arrive, in either order.
    pub fn request(&self, request: Request) {
        let bit = match request {
            Request::Stop => STOP,
            Request::Preshutdown => PRESHUTDOWN,
        };
        self.0.fetch_or(bit, Ordering::SeqCst);
    }

    /// Whether a stop request was recorded.
    pub fn stop_requested(&self) -> bool {
        self.0.load(Ordering::SeqCst) & STOP != 0
    }

    /// Whether a preshutdown request was recorded.
    pub fn preshutdown_requested(&self) -> bool {
        self.0.load(Ordering::SeqCst) & PRESHUTDOWN != 0
    }
}

/// How long each stage may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The drain after a stop request.
    pub stop_grace: Duration,
    /// The drain after a preshutdown request. It is shorter than the time
    /// the operating system gives the service, so termination fits in it.
    pub preshutdown_grace: Duration,
    /// How long termination may take after the deadline before the service
    /// gives up waiting for quiescence.
    pub terminate_patience: Duration,
    /// How often the service reports its progress while it ends.
    pub status_interval: Duration,
}

/// Where the service is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Admitting work.
    Running,
    /// Admission ended; what is running is asked to end by `deadline`
    /// (a reading of the lifecycle's clock).
    Draining { reason: Request, deadline: Duration },
    /// The deadline passed: whatever remains is ended without asking, until
    /// `give_up`.
    Terminating { reason: Request, give_up: Duration },
    /// Over. `quiescent` says whether the caller confirmed that nothing
    /// remained; when false, the service ended with something unconfirmed.
    Stopped { quiescent: bool },
}

/// What the service tells the operating system about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Report {
    Running,
    /// Ending. `checkpoint` rises with every report, and `wait_hint` is how
    /// long the operating system may wait for the next one.
    Stopping {
        checkpoint: u32,
        wait_hint: Duration,
    },
    Stopped {
        quiescent: bool,
    },
}

/// Whether a request starts something new or acts on what already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Admission: a request that begins a new launch.
    Begins,
    /// A request about a launch that exists already.
    Continues,
}

/// A request refused because the service is ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused(pub Request);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cause = match self.0 {
            Request::Stop => "is stopping",
            Request::Preshutdown => "is shutting down with the machine",
        };
        write!(f, "the service {cause} and takes no new launch")
    }
}

impl std::error::Error for Refused {}

/// The worker's view of the service's life.
#[derive(Debug)]
pub struct Lifecycle<C: Clock> {
    clock: C,
    limits: Limits,
    phase: Phase,
    checkpoint: u32,
}

impl<C: Clock> Lifecycle<C> {
    /// A running service.
    pub fn new(clock: C, limits: Limits) -> Self {
        Self {
            clock,
            limits,
            phase: Phase::Running,
            checkpoint: 0,
        }
    }

    /// The current phase, as of the last [`Lifecycle::poll`].
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Takes in what `signal` holds and the clock says, and returns the
    /// phase. A request ends admission at once; a preshutdown that arrives
    /// during a stop's drain shortens its deadline, never lengthens it.
    pub fn poll(&mut self, signal: &Signal) -> Phase {
        let now = self.clock.now();
        let reason = if signal.preshutdown_requested() {
            Some(Request::Preshutdown)
        } else if signal.stop_requested() {
            Some(Request::Stop)
        } else {
            None
        };
        self.phase = match (self.phase, reason) {
            (Phase::Running, Some(reason)) => Phase::Draining {
                reason,
                deadline: now + self.grace(reason),
            },
            (Phase::Draining { deadline, .. }, Some(Request::Preshutdown)) => {
                let tightest = deadline.min(now + self.limits.preshutdown_grace);
                Phase::Draining {
                    reason: Request::Preshutdown,
                    deadline: tightest,
                }
            }
            (phase, _) => phase,
        };
        self.phase = match self.phase {
            Phase::Draining { reason, deadline } if now >= deadline => Phase::Terminating {
                reason,
                give_up: deadline + self.limits.terminate_patience,
            },
            other => other,
        };
        if let Phase::Terminating { give_up, .. } = self.phase
            && now >= give_up
        {
            self.phase = Phase::Stopped { quiescent: false };
        }
        self.phase
    }

    fn grace(&self, reason: Request) -> Duration {
        match reason {
            Request::Stop => self.limits.stop_grace,
            Request::Preshutdown => self.limits.preshutdown_grace,
        }
    }

    /// Admits a request of `kind`, or refuses it with the reason the
    /// service is ending. Only a running service admits what begins a
    /// launch; what acts on an existing launch is admitted until the
    /// service has stopped.
    pub fn admit(&mut self, signal: &Signal, kind: Kind) -> Result<(), Refused> {
        match (self.poll(signal), kind) {
            (Phase::Running, _) | (_, Kind::Continues) => Ok(()),
            (Phase::Draining { reason, .. } | Phase::Terminating { reason, .. }, Kind::Begins) => {
                Err(Refused(reason))
            }
            (Phase::Stopped { .. }, Kind::Begins) => Err(Refused(Request::Stop)),
        }
    }

    /// How long is left of the drain; zero once it is over.
    pub fn remaining(&self) -> Option<Duration> {
        match self.phase {
            Phase::Draining { deadline, .. } => Some(deadline.saturating_sub(self.clock.now())),
            Phase::Terminating { .. } => Some(Duration::ZERO),
            Phase::Running | Phase::Stopped { .. } => None,
        }
    }

    /// The quiescence check. `unconfirmed` is how many launches the
    /// service is still responsible for whose end Windows has not
    /// confirmed. At zero, a draining or terminating service stops in
    /// quiescence; at any other number nothing changes. Returns whether the
    /// service has stopped in quiescence.
    pub fn confirm(&mut self, unconfirmed: usize) -> bool {
        let ending = matches!(
            self.phase,
            Phase::Draining { .. } | Phase::Terminating { .. }
        );
        if ending && unconfirmed == 0 {
            self.phase = Phase::Stopped { quiescent: true };
        }
        self.phase == (Phase::Stopped { quiescent: true })
    }

    /// What to tell the operating system now. While ending, every call is a
    /// new checkpoint, so a service that keeps reporting is seen to make
    /// progress.
    pub fn report(&mut self) -> Report {
        match self.phase {
            Phase::Running => Report::Running,
            Phase::Stopped { quiescent } => Report::Stopped { quiescent },
            Phase::Draining { .. } | Phase::Terminating { .. } => {
                self.checkpoint += 1;
                Report::Stopping {
                    checkpoint: self.checkpoint,
                    wait_hint: self.limits.status_interval * 2,
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// A clock the test moves by hand; clones share one reading.
    #[derive(Debug, Clone, Default)]
    pub struct Fake(Rc<Cell<Duration>>);

    impl Fake {
        pub fn advance(&self, by: Duration) {
            self.0.set(self.0.get() + by);
        }
    }

    impl Clock for Fake {
        fn now(&self) -> Duration {
            self.0.get()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Fake;
    use super::*;

    const S: Duration = Duration::from_secs(1);

    fn limits() -> Limits {
        Limits {
            stop_grace: 30 * S,
            preshutdown_grace: 10 * S,
            terminate_patience: 5 * S,
            status_interval: 2 * S,
        }
    }

    fn started() -> (Lifecycle<Fake>, Fake, Signal) {
        let clock = Fake::default();
        (
            Lifecycle::new(clock.clone(), limits()),
            clock,
            Signal::default(),
        )
    }

    #[test]
    fn a_running_service_admits_everything_and_reports_running() {
        let (mut life, clock, signal) = started();
        clock.advance(1000 * S);
        assert_eq!(life.poll(&signal), Phase::Running);
        assert_eq!(life.admit(&signal, Kind::Begins), Ok(()));
        assert_eq!(life.report(), Report::Running);
        assert_eq!(life.remaining(), None);
        assert!(!life.confirm(0), "a running service is never quiescent");
        assert_eq!(life.phase(), Phase::Running);
    }

    #[test]
    fn a_request_ends_admission_at_once_but_not_work_on_existing_launches() {
        for (request, reason) in [
            (Request::Stop, Request::Stop),
            (Request::Preshutdown, Request::Preshutdown),
        ] {
            let (mut life, _, signal) = started();
            signal.request(request);
            let refused = life.admit(&signal, Kind::Begins).unwrap_err();
            assert_eq!(refused, Refused(reason));
            assert_eq!(life.admit(&signal, Kind::Continues), Ok(()));
        }
        let message = Refused(Request::Stop).to_string();
        assert!(message.contains("stopping"), "{message}");
    }

    #[test]
    fn the_drain_runs_to_a_deadline_on_the_clock_each_reason_sets() {
        let (mut life, clock, signal) = started();
        clock.advance(7 * S);
        signal.request(Request::Stop);
        let phase = life.poll(&signal);
        assert_eq!(
            phase,
            Phase::Draining {
                reason: Request::Stop,
                deadline: 37 * S
            }
        );
        assert_eq!(life.remaining(), Some(30 * S));
        clock.advance(29 * S);
        assert!(matches!(life.poll(&signal), Phase::Draining { .. }));
        assert_eq!(life.remaining(), Some(S));
        clock.advance(S);
        assert_eq!(
            life.poll(&signal),
            Phase::Terminating {
                reason: Request::Stop,
                give_up: 42 * S
            }
        );
        assert_eq!(life.remaining(), Some(Duration::ZERO));
    }

    #[test]
    fn preshutdown_uses_its_own_shorter_grace() {
        let (mut life, _, signal) = started();
        signal.request(Request::Preshutdown);
        let phase = life.poll(&signal);
        let deadline = 10 * S;
        let reason = Request::Preshutdown;
        assert_eq!(phase, Phase::Draining { reason, deadline });
    }

    #[test]
    fn a_preshutdown_during_a_stop_shortens_the_drain_and_never_lengthens_it() {
        let (mut life, clock, signal) = started();
        signal.request(Request::Stop);
        life.poll(&signal);
        clock.advance(25 * S);
        signal.request(Request::Preshutdown);
        // 25 s in, 30 s deadline; preshutdown's 10 s from now would be 35 s.
        let kept = Phase::Draining {
            reason: Request::Preshutdown,
            deadline: 30 * S,
        };
        assert_eq!(life.poll(&signal), kept);
        let (mut life, clock, signal) = started();
        signal.request(Request::Stop);
        life.poll(&signal);
        clock.advance(S);
        signal.request(Request::Preshutdown);
        assert_eq!(
            life.poll(&signal),
            Phase::Draining {
                reason: Request::Preshutdown,
                deadline: 11 * S
            }
        );
    }

    #[test]
    fn a_stop_during_a_preshutdown_changes_nothing() {
        let (mut life, clock, signal) = started();
        signal.request(Request::Preshutdown);
        let first = life.poll(&signal);
        clock.advance(2 * S);
        signal.request(Request::Stop);
        assert_eq!(life.poll(&signal), first);
    }

    #[test]
    fn quiescence_is_needed_to_stop_cleanly_and_is_checked_by_count() {
        let (mut life, clock, signal) = started();
        signal.request(Request::Stop);
        life.poll(&signal);
        assert!(!life.confirm(2));
        assert!(!life.confirm(1));
        assert!(matches!(life.phase(), Phase::Draining { .. }));
        clock.advance(31 * S);
        life.poll(&signal);
        assert!(!life.confirm(1));
        assert!(matches!(life.phase(), Phase::Terminating { .. }));
        assert!(life.confirm(0));
        assert_eq!(life.phase(), Phase::Stopped { quiescent: true });
        // Stopped is final.
        clock.advance(1000 * S);
        assert_eq!(life.poll(&signal), Phase::Stopped { quiescent: true });
    }

    #[test]
    fn a_service_whose_launches_ended_early_stops_without_waiting_for_the_deadline() {
        let (mut life, clock, signal) = started();
        signal.request(Request::Stop);
        life.poll(&signal);
        clock.advance(S);
        assert!(life.confirm(0));
        assert_eq!(life.report(), Report::Stopped { quiescent: true });
    }

    #[test]
    fn patience_ends_a_termination_that_never_confirms_and_says_so() {
        let (mut life, clock, signal) = started();
        signal.request(Request::Stop);
        life.poll(&signal);
        clock.advance(30 * S);
        assert!(matches!(life.poll(&signal), Phase::Terminating { .. }));
        clock.advance(4 * S);
        assert!(matches!(life.poll(&signal), Phase::Terminating { .. }));
        clock.advance(S);
        assert_eq!(life.poll(&signal), Phase::Stopped { quiescent: false });
        assert!(
            !life.confirm(0),
            "an abandoned stop is not made clean later"
        );
        assert_eq!(life.report(), Report::Stopped { quiescent: false });
    }

    #[test]
    fn a_service_that_is_polled_late_still_passes_through_the_deadline() {
        let (mut life, clock, signal) = started();
        signal.request(Request::Stop);
        life.poll(&signal);
        clock.advance(500 * S);
        // The deadline and the patience are both long gone.
        assert_eq!(life.poll(&signal), Phase::Stopped { quiescent: false });
    }

    #[test]
    fn every_report_while_ending_is_a_new_checkpoint_with_a_wait_hint() {
        let (mut life, _, signal) = started();
        signal.request(Request::Stop);
        life.poll(&signal);
        let hint = 4 * S;
        assert_eq!(
            life.report(),
            Report::Stopping {
                checkpoint: 1,
                wait_hint: hint
            }
        );
        assert_eq!(
            life.report(),
            Report::Stopping {
                checkpoint: 2,
                wait_hint: hint
            }
        );
    }

    #[test]
    fn what_begins_a_launch_stays_refused_after_stopping() {
        let (mut life, _, signal) = started();
        signal.request(Request::Stop);
        life.poll(&signal);
        assert!(life.confirm(0));
        assert!(life.admit(&signal, Kind::Begins).is_err());
    }

    #[test]
    fn signals_are_recorded_without_the_worker_and_in_any_order() {
        let signal = Signal::default();
        assert!(!signal.stop_requested() && !signal.preshutdown_requested());
        signal.request(Request::Preshutdown);
        signal.request(Request::Preshutdown);
        assert!(signal.preshutdown_requested() && !signal.stop_requested());
        signal.request(Request::Stop);
        assert!(signal.preshutdown_requested() && signal.stop_requested());
    }

    #[test]
    fn the_real_clock_does_not_go_backwards() {
        let clock = Monotonic::new();
        let (a, b) = (clock.now(), clock.now());
        assert!(b >= a);
    }
}
