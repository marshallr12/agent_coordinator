//! Progress-gated renewal (autonomy plan §2.3). While a launch runs, the
//! loop renews its attempt at the service's `renew_after_seconds` cadence
//! only while the launch is alive, its harness wrote an event to
//! `$RUN/events.jsonl` in the last 15 minutes, the attempt recorded a
//! checkpoint in the last 60 minutes, and the launch is within `[run]
//! budget_minutes`. Once a gate fails it stops renewing, logs why, and
//! drains the launch, as it does on a stop request: SIGTERM to its process
//! group, then SIGKILL after `[run] drain_seconds`; the caller then releases
//! the attempt. A host suspend counts as wall-clock time, so a suspend longer
//! than the event window drains the launch on resume. A renewal the service
//! refuses for good ([`AttemptEnded`]: the agent submitted or released the
//! attempt, or it expired or changed hands) drains the launch the same way at
//! once; an attempt the agent submitted is left as it is, not released.
//! Any other renewal failure is logged and retried at the next cadence.
use super::renewal::AttemptEnded;
use super::{Driver, Launch, RunConfig};
use anyhow::{Context, Result};
use serde_json::Value;
use std::time::Duration;

/// The oldest harness event that still earns a renewal.
pub const MAX_EVENT_AGE_MS: i64 = 15 * 60 * 1000;
/// The oldest checkpoint (or claim) that still earns a renewal.
pub const MAX_CHECKPOINT_AGE_MS: i64 = 60 * 60 * 1000;
/// How often the loop looks at a running launch.
pub const TICK: Duration = Duration::from_secs(1);

/// A claimed attempt as the service last described it.
#[derive(Debug, Clone, PartialEq)]
pub struct Lease {
    pub attempt: String,
    pub generation: u64,
    pub renew_after_seconds: u64,
    /// How old the attempt's last checkpoint (or its claim) was when the
    /// service answered, by the service's own clock.
    pub progress_age_ms: i64,
}

impl Lease {
    /// The lease in a `claim --json` (`data.claim`) or `renew --json`
    /// (`data`) response. The service's clock is `expires_at` less
    /// `lease_remaining_ms`, so the checkpoint age needs no local clock.
    pub fn parse(body: &Value) -> Result<Self> {
        let data = &body["data"];
        let holder = data.get("claim").unwrap_or(data);
        let attempt = &holder["attempt"];
        let remaining = holder["lease_remaining_ms"].as_i64();
        let now = millis(&attempt["expires_at"])? - remaining.context("no lease_remaining_ms")?;
        Ok(Self {
            attempt: attempt["id"].as_str().context("no attempt id")?.to_owned(),
            generation: attempt["generation"].as_u64().context("no generation")?,
            renew_after_seconds: data["renew_after_seconds"].as_u64().unwrap_or(60).max(1),
            progress_age_ms: (now - millis(&attempt["last_progress_at"])?).max(0),
        })
    }
}

/// Milliseconds since the epoch of an RFC 3339 timestamp.
fn millis(value: &Value) -> Result<i64> {
    let text = value.as_str().context("a timestamp is missing")?;
    Ok(chrono::DateTime::parse_from_rfc3339(text)?.timestamp_millis())
}

/// The progress facts renewal depends on, in milliseconds.
#[derive(Debug)]
pub struct Gates {
    pub event_age_ms: i64,
    pub progress_age_ms: i64,
    pub elapsed_ms: i64,
    pub budget_ms: i64,
}

impl Gates {
    /// Why the lease must not be renewed now, if it must not.
    pub fn refusal(&self) -> Option<String> {
        let checks = [
            (
                self.event_age_ms > MAX_EVENT_AGE_MS,
                "no harness event for",
                self.event_age_ms,
            ),
            (
                self.progress_age_ms > MAX_CHECKPOINT_AGE_MS,
                "no checkpoint for",
                self.progress_age_ms,
            ),
            (
                self.elapsed_ms > self.budget_ms,
                "the budget is spent after",
                self.elapsed_ms,
            ),
        ];
        let (_, what, ms) = checks.into_iter().find(|(failed, ..)| *failed)?;
        Some(format!("{what} {} min", ms / 60_000))
    }
}

/// What [`supervise`] tracks between ticks, in driver milliseconds.
struct Watch {
    started: i64,
    /// When the service last described the lease.
    seen: i64,
    /// When to renew next; `None` once renewal has stopped.
    renew_at: Option<i64>,
    /// When a draining launch is killed; `None` until it drains.
    kill_at: Option<i64>,
    /// Why the launch is draining, once it is.
    drain_reason: Option<String>,
    /// Whether a refused renewal showed the agent submitted the attempt.
    submitted: bool,
}

/// How a supervised launch ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Ended {
    pub code: i32,
    /// Why the loop drained it, if it did.
    pub drained: Option<String>,
    /// Whether the agent submitted the attempt, so it needs no release.
    pub submitted: bool,
}

impl Watch {
    /// A watch over a launch that started at `now` holding `lease`.
    fn new(now: i64, lease: &Lease) -> Self {
        Self {
            started: now,
            seen: now,
            renew_at: Some(now.saturating_add(cadence(lease))),
            kill_at: None,
            drain_reason: None,
            submitted: false,
        }
    }

    /// The gates at `now`, given the harness's last event, if any.
    fn gates(&self, lease: &Lease, event: Option<i64>, budget_minutes: u64, now: i64) -> Gates {
        Gates {
            event_age_ms: now - event.unwrap_or(self.started),
            progress_age_ms: lease.progress_age_ms + now - self.seen,
            elapsed_ms: now - self.started,
            budget_ms: millis_of(budget_minutes, 60_000),
        }
    }
}

/// `count` units of `unit` milliseconds, saturating.
fn millis_of(count: u64, unit: i64) -> i64 {
    i64::try_from(count)
        .unwrap_or(i64::MAX)
        .saturating_mul(unit)
}

/// Waits for the running launch to exit, renewing `lease` while every gate
/// holds and draining the launch on a failed gate or a stop request.
pub fn supervise(
    driver: &mut impl Driver,
    launch: &Launch,
    mut lease: Lease,
    settings: &RunConfig,
) -> Ended {
    let mut watch = Watch::new(driver.now_ms(), &lease);
    loop {
        if let Some(code) = driver.exited() {
            let (drained, submitted) = (watch.drain_reason, watch.submitted);
            return Ended {
                code,
                drained,
                submitted,
            };
        }
        let now = driver.now_ms();
        drain(driver, &mut watch, settings, now);
        if watch.renew_at.is_some_and(|at| now >= at) {
            renew(driver, launch, &mut lease, &mut watch, settings, now);
        }
        driver.pause(TICK);
    }
}

/// The service's renewal cadence in milliseconds.
fn cadence(lease: &Lease) -> i64 {
    millis_of(lease.renew_after_seconds, 1000)
}

/// Once the launch must drain (a failed gate or a stop request), asks it to
/// end (SIGTERM) and kills it once `drain_seconds` have passed.
fn drain(driver: &mut impl Driver, watch: &mut Watch, settings: &RunConfig, now: i64) {
    if watch.drain_reason.is_none() && driver.stopping() {
        watch.drain_reason = Some("the host supervisor stopped".into());
    }
    let Some(reason) = &watch.drain_reason else {
        return;
    };
    match watch.kill_at {
        None => {
            eprintln!("agentc-supervisor run: draining the launch: {reason}");
            driver.signal(false);
            let grace = millis_of(settings.drain_seconds, 1000);
            watch.kill_at = Some(now.saturating_add(grace));
        }
        Some(at) if now >= at => {
            driver.signal(true);
            watch.kill_at = Some(i64::MAX);
        }
        Some(_) => {}
    }
}

/// Renews the lease if every gate holds; otherwise stops renewing and
/// drains the launch, saying why. A renewal refused for good drains it too;
/// any other failed renewal is retried at the next cadence.
fn renew(
    driver: &mut impl Driver,
    launch: &Launch,
    lease: &mut Lease,
    watch: &mut Watch,
    settings: &RunConfig,
    now: i64,
) {
    let event = driver.last_event_ms(launch);
    let gates = watch.gates(lease, event, settings.budget_minutes, now);
    if let Some(reason) = gates.refusal() {
        let attempt = &lease.attempt;
        eprintln!("agentc-supervisor run: stopped renewing attempt {attempt}: {reason}");
        (watch.renew_at, watch.drain_reason) = (None, Some(reason));
        return;
    }
    match driver.renew(launch, lease) {
        Ok(renewed) => (*lease, watch.seen) = (renewed, now),
        Err(error) => {
            if let Some(ended) = error.downcast_ref::<AttemptEnded>() {
                return stop(watch, &lease.attempt, ended);
            }
            eprintln!("agentc-supervisor run: renew {}: {error:#}", lease.attempt);
        }
    }
    watch.renew_at = Some(now.saturating_add(cadence(lease)));
}

/// Stops renewing an attempt the service says has ended and drains its
/// launch, noting whether the agent submitted it.
fn stop(watch: &mut Watch, attempt: &str, ended: &AttemptEnded) {
    let reason = if ended.submitted() {
        format!("the agent submitted attempt {attempt}; it is not released")
    } else {
        format!("attempt {attempt}: {ended}")
    };
    eprintln!("agentc-supervisor run: stopped renewing: {reason}");
    (watch.renew_at, watch.drain_reason) = (None, Some(reason));
    watch.submitted = ended.submitted();
}
