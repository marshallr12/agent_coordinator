use super::*;
use std::fs;

/// A coordinator and host stand-in with a fake clock: one queued `next`,
/// steps recorded, and a launch that marks its run started at spawn and
/// terminal when it exits at `exit_at` (or on a signal).
struct Fake {
    next: Value,
    free: u64,
    steps: Vec<String>,
    prompt: String,
    fail_claim: bool,
    fail_register: bool,
    fail_start: bool,
    fail_release: bool,
    now: i64,
    /// When the running launch exits; `None` until it is told to.
    exit_at: Option<i64>,
    running: Option<Launch>,
    /// The harness's last event; `None` means it never wrote one.
    event_at: Option<i64>,
    /// When the agent last checkpointed, as renewals report it.
    checkpoint_at: i64,
    renewals: Vec<i64>,
    /// When the heartbeat was refreshed during the launch, and for which task.
    beats: Vec<(i64, String)>,
    releases: Vec<String>,
    stop_at: Option<i64>,
    /// Exit this long after SIGTERM; `None` ignores SIGTERM.
    term_exit_ms: Option<i64>,
    /// Live pids and their start ticks, all on boot `boot-1`.
    pids: std::collections::HashMap<u32, u64>,
    /// What the launch writes to `$RUN/events.jsonl` when it starts.
    events: String,
    /// Harnesses whose sign-in check fails.
    unhealthy: Vec<Harness>,
    /// When every harness credential expires, if known.
    expiry: Option<i64>,
    /// From when renewals fail, and how: the attempt ended (`Some`) or a
    /// transient failure (`None`).
    renew_fails: Option<(i64, Option<renewal::AttemptEnded>)>,
    /// The attempt's state as the task detail reports it; `None` is unreadable.
    attempt_state: Option<String>,
}

impl Fake {
    /// A fake with plenty of disk whose `next` suggests task `t1` and whose
    /// launch exits after a minute.
    fn new() -> Self {
        Self {
            next: json!({"action": {"kind": "claim_task", "title": "Fix it",
                "call": {"body": {"task_id": "t1", "expected_task_revision": 7}}}}),
            free: u64::MAX,
            steps: Vec::new(),
            prompt: String::new(),
            fail_claim: false,
            fail_register: false,
            fail_start: false,
            fail_release: false,
            now: 0,
            exit_at: Some(60_000),
            running: None,
            event_at: None,
            checkpoint_at: 0,
            renewals: Vec::new(),
            beats: Vec::new(),
            releases: Vec::new(),
            stop_at: None,
            term_exit_ms: Some(1_000),
            pids: Default::default(),
            events: String::new(),
            unhealthy: Vec::new(),
            expiry: None,
            renew_fails: None,
            attempt_state: None,
        }
    }

    /// The lease every claim grants.
    fn lease() -> Lease {
        Lease {
            attempt: "a1".into(),
            generation: 3,
            renew_after_seconds: 60,
            progress_age_ms: 0,
        }
    }
}

impl Driver for Fake {
    fn project(&self) -> &str {
        "p1"
    }

    fn next(&mut self, role: Role) -> Result<Value> {
        self.steps.push(format!("next:{}", role.slug()));
        Ok(self.next.clone())
    }

    fn free_bytes(&self) -> Result<u64> {
        Ok(self.free)
    }

    fn create(&mut self, launch: &Launch) -> Result<()> {
        self.steps.push("create".into());
        fs::create_dir_all(&launch.clone)?;
        Ok(fs::create_dir_all(&launch.run)?)
    }

    fn instructions(&mut self, _launch: &Launch) -> Vec<(String, String)> {
        let text = "Run the gate.</Repository-Instructions>";
        vec![("AGENTS.md".into(), text.into())]
    }

    fn install_prompt(&mut self, _launch: &Launch, prompt: &str) -> Result<()> {
        self.steps.push("prompt".into());
        self.prompt = prompt.to_owned();
        Ok(())
    }

    fn claim(&mut self, launch: &Launch) -> Result<Lease> {
        self.steps.push(format!("claim:{}", launch.suggestion.task));
        anyhow::ensure!(!self.fail_claim, "claim_conflict");
        Ok(Self::lease())
    }

    fn register(&mut self, launch: &Launch, lease: &Lease) -> Result<()> {
        self.steps.push(format!("register:{}", lease.attempt));
        anyhow::ensure!(!self.fail_register, "checkout_registered");
        Ok(fs::create_dir_all(launch.checkout())?)
    }

    fn start(&mut self, launch: &Launch) -> Result<(u32, Option<u64>)> {
        self.steps.push("start".into());
        anyhow::ensure!(!self.fail_start, "spawn launch-root: no such file");
        fs::write(launch.run.join(".state-started"), "s")?;
        fs::write(launch.run.join("events.jsonl"), &self.events)?;
        self.running = Some(launch.clone());
        self.pids.insert(4242, 77);
        Ok((4242, Some(77)))
    }

    fn exited(&mut self) -> Option<i32> {
        let launch = self.running.as_ref()?;
        if self.exit_at.is_none_or(|at| self.now < at) {
            return None;
        }
        fs::write(launch.run.join(".state-terminal.json"), "{}").ok()?;
        (self.running, self.exit_at) = (None, None);
        self.pids.remove(&4242);
        Some(0)
    }

    fn signal(&mut self, kill: bool) {
        self.steps
            .push(format!("signal:{}", if kill { "kill" } else { "term" }));
        let grace = if kill { Some(0) } else { self.term_exit_ms };
        if let Some(grace) = grace {
            self.exit_at = Some(self.now + grace);
        }
    }

    fn last_event_ms(&self, _launch: &Launch) -> Option<i64> {
        self.event_at
    }

    fn heartbeat(&mut self, launch: &Launch) {
        self.beats.push((self.now, launch.suggestion.task.clone()));
    }

    fn renew(&mut self, _launch: &Launch, lease: &Lease) -> Result<Lease> {
        self.renewals.push(self.now);
        if let Some((from, ended)) = &self.renew_fails
            && self.now >= *from
        {
            return Err(ended
                .clone()
                .map_or_else(|| anyhow::anyhow!("transport_failure"), Into::into));
        }
        Ok(Lease {
            progress_age_ms: self.now - self.checkpoint_at,
            ..lease.clone()
        })
    }

    fn attempt_state(&mut self, _launch: &Launch, _lease: &Lease) -> Option<String> {
        self.attempt_state.clone()
    }

    fn release(&mut self, _launch: &Launch, lease: &Lease, summary: &str) -> Result<()> {
        self.steps.push("release".into());
        anyhow::ensure!(!self.fail_release, "service unavailable");
        self.releases.push(format!("{}: {summary}", lease.attempt));
        Ok(())
    }

    fn boot_id(&self) -> String {
        "boot-1".into()
    }

    fn may_be_alive(&self, launch: &LaunchRecord) -> bool {
        let ticks = |pid| self.pids.get(&pid).copied();
        record::may_be_alive(launch, "boot-1", ticks, self.now)
    }

    fn has_marker(&self, launch: &Launch, name: &str) -> bool {
        launch.run.join(name).is_file()
    }

    fn discard(&mut self, launch: &Launch) -> Result<()> {
        for path in [&launch.clone, &launch.run] {
            match fs::remove_dir_all(path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn now_ms(&self) -> i64 {
        self.now
    }

    fn pause(&mut self, duration: Duration) {
        self.now += i64::try_from(duration.as_millis()).unwrap();
    }

    fn stopping(&self) -> bool {
        self.stop_at.is_some_and(|at| self.now >= at)
    }

    fn harness_status(&mut self, harness: Harness) -> Result<()> {
        anyhow::ensure!(!self.unhealthy.contains(&harness), "exit status 1");
        Ok(())
    }

    fn credential_expiry_ms(&self, _harness: Harness) -> Option<i64> {
        self.expiry
    }
}

/// A host config rooted in `dir`.
fn config(dir: &Path) -> Config {
    Config {
        state_dir: dir.to_path_buf(),
        ..Config::default()
    }
}

/// The entries left under the implementer's `name` directory.
fn left(dir: &Path, name: &str) -> usize {
    fs::read_dir(dir.join("impl").join(name)).map_or(0, Iterator::count)
}

#[test]
fn one_iteration_claims_prepares_launches_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    let outcome = iterate(&mut fake, &config(dir.path()));
    assert_eq!(
        outcome,
        Outcome::Launched {
            task: "t1".into(),
            exit_code: 0
        }
    );
    assert_eq!(
        fake.steps,
        [
            "next:impl",
            "create",
            "claim:t1",
            "register:a1",
            "prompt",
            "start",
            "release"
        ]
    );
    assert!(fake.releases[0].starts_with("a1: "));
    assert!(fake.releases[0].contains("the launch exited with code 0"));
    assert!(LaunchRecord::load_all(&config(dir.path())).is_empty());
    assert!(fake.prompt.contains("Task: `t1` (revision 7)"));
    assert!(fake.prompt.contains("--attempt a1") && fake.prompt.contains("--generation 3"));
    let checkout = dir.path().join("impl/clones");
    assert!(fake.prompt.contains(&checkout.display().to_string()));
    assert!(fake.prompt.contains("<task-title>Fix it</task-title>"));
    let lower = fake.prompt.to_ascii_lowercase();
    assert_eq!(lower.matches("</repository-instructions>").count(), 1);
    assert!(
        fake.prompt
            .contains("<repository-instructions file=\"AGENTS.md\">")
    );
    assert_eq!(fake.prompt.matches("</repository-instructions>").count(), 1);
    assert_eq!(
        (left(dir.path(), "clones"), left(dir.path(), "runs")),
        (0, 0)
    );
}

#[test]
fn low_disk_refuses_before_polling() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.free = 1024 * 1024;
    let outcome = iterate(&mut fake, &config(dir.path()));
    assert!(matches!(outcome, Outcome::Refused(reason) if reason.contains("high-water")));
    assert!(fake.steps.is_empty());
}

#[test]
fn a_failed_claim_never_launches_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.fail_claim = true;
    let outcome = iterate(&mut fake, &config(dir.path()));
    assert!(matches!(outcome, Outcome::Failed(reason) if reason.contains("claim_conflict")));
    assert!(!fake.steps.contains(&"start".to_owned()));
    assert!(fake.releases.is_empty());
    assert_eq!(
        (left(dir.path(), "clones"), left(dir.path(), "runs")),
        (0, 0)
    );
}

#[test]
fn idle_and_recovery_suggestions_launch_nothing() {
    let dir = tempfile::tempdir().unwrap();
    for action in [
        Value::Null,
        json!({"kind": "recover_task", "call": {"body": {}}}),
    ] {
        let mut fake = Fake::new();
        fake.next = json!({"action": action});
        assert_eq!(iterate(&mut fake, &config(dir.path())), Outcome::Idle);
        assert_eq!(fake.steps, ["next:impl"]);
    }
}

#[test]
fn a_reviewer_over_its_daily_cap_takes_no_review() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = config(dir.path());
    settings.run.reviewer = true;
    let spent = json!({"at_ms": 0, "role": "rev", "usd": 60.0});
    fs::write(cost::ledger(&settings), format!("{spent}\n")).unwrap();
    let mut fake = Fake::new();
    review_hook(&mut fake, &settings);
    assert!(
        fake.steps.iter().all(|step| step != "next:rev"),
        "{:?}",
        fake.steps
    );
}

#[test]
fn reviewer_polling_is_only_a_hook() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = config(dir.path());
    settings.run.reviewer = true;
    let mut fake = Fake::new();
    iterate(&mut fake, &settings);
    assert_eq!(&fake.steps[..2], ["next:rev", "next:impl"]);
}

#[test]
fn a_started_run_without_a_terminal_record_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    let launch = planned(dir.path());
    fs::create_dir_all(&launch.clone).unwrap();
    fs::create_dir_all(&launch.run).unwrap();
    fs::write(launch.run.join(".state-started"), "s").unwrap();
    assert!(!remove_finished(&mut fake, &launch).unwrap());
    assert!(launch.clone.exists() && launch.run.exists());
    fs::write(launch.run.join(".state-terminal.json"), "{}").unwrap();
    assert!(remove_finished(&mut fake, &launch).unwrap());
    assert!(!launch.clone.exists() && !launch.run.exists());
}

#[test]
fn run_once_writes_a_heartbeat() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.next = json!({"action": null});
    let settings = config(dir.path());
    run(&mut fake, &settings, true).unwrap();
    let beat: Value =
        serde_json::from_slice(&fs::read(dir.path().join("heartbeat.json")).unwrap()).unwrap();
    assert_eq!(
        (beat["iteration"].as_u64(), beat["outcome"].as_str()),
        (Some(1), Some("Idle"))
    );
}

#[test]
fn a_long_implementer_launch_keeps_the_heartbeat_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.exit_at = Some(minutes(20));
    fake.event_at = Some(minutes(20));
    iterate(&mut fake, &config(dir.path()));
    let times: Vec<i64> = fake.beats.iter().map(|(at, _)| *at).collect();
    assert_eq!(times.first(), Some(&0), "{times:?}");
    assert!(times.len() >= 40, "{times:?}");
    assert!(
        times.windows(2).all(|w| w[1] - w[0] <= minutes(1)),
        "{times:?}"
    );
    assert!(
        minutes(20) - times.last().unwrap() <= minutes(1),
        "{times:?}"
    );
    assert!(fake.beats.iter().all(|(_, task)| task == "t1"));
}

#[test]
fn a_launch_beat_keeps_the_last_poll_and_the_next_poll_clears_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("heartbeat.json");
    let read = || -> Value { serde_json::from_slice(&fs::read(&path).unwrap()).unwrap() };
    beat_busy(&path, "reviewer", "t9").unwrap();
    let busy = read();
    assert_eq!(
        (busy["iteration"].as_u64(), busy["outcome"].as_str()),
        (Some(0), Some("Idle"))
    );
    assert_eq!(busy["launch"], json!({"role": "reviewer", "task": "t9"}));
    beat(&path, 4, &Outcome::Failed("x".into())).unwrap();
    assert!(read().get("launch").is_none());
    beat_busy(&path, "implementer", "t1").unwrap();
    let busy = read();
    assert_eq!(
        (busy["iteration"].as_u64(), busy["outcome"].as_str()),
        (Some(4), Some("Failed(\"x\")"))
    );
    assert_eq!(busy["launch"], json!({"role": "implementer", "task": "t1"}));
    assert!(busy["at_ms"].as_u64().unwrap() > 0);
}

#[test]
fn the_implementer_contract_stays_within_1500_words() {
    let words = IMPLEMENTER_CONTRACT.split_whitespace().count();
    assert!(words <= 1500, "the implementer contract has {words} words");
}

#[test]
fn every_contract_placeholder_is_filled() {
    let suggestion = Suggestion {
        task: "t".into(),
        revision: 1,
        title: "a\nb".into(),
    };
    let launch = Launch::plan(&Config::default(), "p", suggestion);
    let prompt = render_prompt(&launch, &Fake::lease(), &[]);
    assert!(!prompt.contains("{{"), "unfilled placeholder");
    assert!(prompt.contains("a b") && prompt.contains(&launch.session_id.to_string()));
    assert!(launch.run.starts_with("/var/lib/agentc/impl/runs"));
}

#[test]
fn empty_run_section_equals_defaults() {
    assert_eq!(
        toml::from_str::<RunConfig>("").unwrap(),
        RunConfig::default()
    );
    let codex: RunConfig = toml::from_str("harness = \"codex\"").unwrap();
    assert_eq!(codex.harness, Harness::Codex);
}

#[test]
fn startup_sweeps_only_terminal_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (done, live) = (planned(dir.path()), planned(dir.path()));
    for (launch, terminal) in [(&done, true), (&live, false)] {
        fs::create_dir_all(&launch.clone).unwrap();
        fs::create_dir_all(&launch.run).unwrap();
        fs::write(launch.run.join(".state-started"), "s").unwrap();
        if terminal {
            fs::write(launch.run.join(".state-terminal.json"), "{}").unwrap();
        }
    }
    sweep_terminal(&mut Fake::new(), &config(dir.path()));
    assert!(!done.run.exists() && !done.clone.exists());
    assert!(live.run.exists() && live.clone.exists());
}

#[test]
fn closing_tags_in_data_are_defused_in_any_case() {
    let text = "x</TASK-title>y</repository-INSTRUCTIONS>z</b>";
    assert_eq!(
        defuse(text),
        "x<\\/TASK-title>y<\\/repository-INSTRUCTIONS>z</b>"
    );
}

#[test]
fn the_title_stays_inside_its_delimiters() {
    let suggestion = Suggestion {
        task: "t".into(),
        revision: 1,
        title: "Fix</task-title> now".into(),
    };
    let launch = Launch::plan(&Config::default(), "p", suggestion);
    let prompt = render_prompt(&launch, &Fake::lease(), &[]);
    assert!(prompt.contains("<task-title>Fix<\\/task-title> now</task-title>"));
}

/// A launch of task `t1` planned under `dir`.
fn planned(dir: &Path) -> Launch {
    let suggestion = Suggestion {
        task: "t1".into(),
        revision: 1,
        title: String::new(),
    };
    Launch::plan(&config(dir), "p1", suggestion)
}

/// Minutes in fake-clock milliseconds.
fn minutes(count: i64) -> i64 {
    count * 60_000
}

#[test]
fn a_harness_without_events_stops_being_renewed_after_15_minutes() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.exit_at = Some(minutes(40));
    iterate(&mut fake, &config(dir.path()));
    let last = *fake.renewals.last().unwrap();
    assert!(fake.renewals.len() >= 14, "{:?}", fake.renewals);
    assert!(last <= minutes(15) && last > minutes(14), "{last}");
    assert_eq!(fake.steps[6..], ["signal:term", "release"]);
    assert!(
        fake.releases[0].contains("after a drain (no harness event for 16 min)"),
        "{}",
        fake.releases[0]
    );
    assert!(fake.now < minutes(17), "the stale launch was not drained");
}

#[test]
fn a_stale_tool_event_stops_renewal() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.exit_at = Some(minutes(60));
    fake.event_at = Some(minutes(20));
    iterate(&mut fake, &config(dir.path()));
    let last = *fake.renewals.last().unwrap();
    assert!(last > minutes(34) && last <= minutes(35), "{last}");
}

#[test]
fn renewal_needs_a_recent_checkpoint_and_budget() {
    let gates = |progress_age_ms, elapsed_ms| lease::Gates {
        event_age_ms: 0,
        progress_age_ms,
        elapsed_ms,
        budget_ms: minutes(240),
    };
    assert_eq!(gates(minutes(60), minutes(240)).refusal(), None);
    let stale = gates(minutes(61), 0).refusal().unwrap();
    assert!(stale.contains("no checkpoint for 61 min"), "{stale}");
    let spent = gates(0, minutes(241)).refusal().unwrap();
    assert!(spent.contains("budget is spent after 241 min"), "{spent}");
}

#[test]
fn an_old_checkpoint_reported_by_the_service_stops_renewal() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.exit_at = Some(minutes(30));
    fake.checkpoint_at = -minutes(58);
    fake.event_at = Some(i64::MAX / 2);
    iterate(&mut fake, &config(dir.path()));
    assert_eq!(fake.renewals, [minutes(1), minutes(2)]);
}

#[test]
fn a_claim_followed_by_a_failed_launch_releases() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.fail_start = true;
    let outcome = iterate(&mut fake, &config(dir.path()));
    assert!(matches!(outcome, Outcome::Failed(reason) if reason.contains("no such file")));
    assert_eq!(
        fake.steps[2..],
        ["claim:t1", "register:a1", "prompt", "start", "release"]
    );
    assert!(fake.releases[0].contains("the launch failed"));
    assert_eq!(
        (left(dir.path(), "clones"), left(dir.path(), "runs")),
        (0, 0)
    );
    assert!(LaunchRecord::load_all(&config(dir.path())).is_empty());
}

/// A record, as a killed loop leaves it, of a launch of pid 4242 started at
/// tick 77 on `boot`, with its clone and started run.
fn crashed(dir: &Path, boot: &str) -> LaunchRecord {
    let launch = planned(dir);
    fs::create_dir_all(&launch.clone).unwrap();
    fs::create_dir_all(&launch.run).unwrap();
    fs::write(launch.run.join(".state-started"), "s").unwrap();
    let mut record = LaunchRecord::new(&launch, &Fake::lease(), boot.into(), 0);
    (record.pid, record.start_ticks) = (Some(4242), Some(77));
    record.save(&config(dir)).unwrap();
    record
}

#[test]
fn a_launch_alive_on_this_boot_is_never_respawned_and_released_once_dead() {
    let dir = tempfile::tempdir().unwrap();
    let settings = config(dir.path());
    let record = crashed(dir.path(), "boot-1");
    let mut fake = Fake::new();
    fake.pids.insert(4242, 77);
    let outcome = iterate(&mut fake, &settings);
    assert!(matches!(outcome, Outcome::Refused(reason) if reason.contains("may still run")));
    assert!(fake.steps.is_empty() && fake.releases.is_empty());
    assert!(record.launch(&settings, "p1").run.exists());
    fake.pids.insert(4242, 78);
    iterate(&mut fake, &settings);
    assert!(fake.releases[0].starts_with("a1: "));
    assert!(fake.releases[0].contains("no supervisor watched it"));
    assert!(!record.launch(&settings, "p1").run.exists());
    assert!(LaunchRecord::load(&settings, &record.session_id).is_none());
    assert_eq!(fake.steps[1..3], ["next:impl", "create"]);
}

#[test]
fn a_launch_from_an_earlier_boot_is_released() {
    let dir = tempfile::tempdir().unwrap();
    crashed(dir.path(), "boot-0");
    let mut fake = Fake::new();
    fake.pids.insert(4242, 77);
    fake.next = json!({"action": null});
    assert_eq!(iterate(&mut fake, &config(dir.path())), Outcome::Idle);
    assert_eq!(fake.steps, ["release", "next:impl"]);
}

#[test]
fn liveness_needs_this_boot_and_the_same_process_start() {
    let (pid, boot) = (std::process::id(), record::boot_id());
    let alive =
        |record: &LaunchRecord, now| record::may_be_alive(record, &boot, record::start_ticks, now);
    let mut record = LaunchRecord::new(&planned(Path::new("/x")), &Fake::lease(), boot.clone(), 0);
    assert!(alive(&record, record::NO_PID_GRACE_MS));
    assert!(!alive(&record, record::NO_PID_GRACE_MS + 1));
    (record.pid, record.start_ticks) = (Some(pid), record::start_ticks(pid));
    assert!(record.start_ticks.is_some());
    assert!(alive(&record, i64::MAX));
    record.start_ticks = record.start_ticks.map(|t| t + 1);
    assert!(!alive(&record, 0));
    record.boot_id = "another-boot".into();
    assert!(!alive(&record, 0));
}

#[test]
fn a_record_without_a_pid_is_released_after_the_grace_period() {
    let dir = tempfile::tempdir().unwrap();
    let settings = config(dir.path());
    let mut record = crashed(dir.path(), "boot-1");
    (record.pid, record.start_ticks) = (None, None);
    record.save(&settings).unwrap();
    let mut fake = Fake::new();
    fake.next = json!({"action": null});
    let outcome = iterate(&mut fake, &settings);
    assert!(matches!(outcome, Outcome::Refused(reason) if reason.contains("may still run")));
    fake.now = record::NO_PID_GRACE_MS + 1;
    assert_eq!(iterate(&mut fake, &settings), Outcome::Idle);
    assert_eq!(fake.steps, ["release", "next:impl"]);
    assert!(LaunchRecord::load_all(&settings).is_empty());
}

#[test]
fn releases_are_retried_a_bounded_number_of_times_until_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let settings = config(dir.path());
    crashed(dir.path(), "boot-0");
    let mut fake = Fake::new();
    (fake.fail_release, fake.next) = (true, json!({"action": null}));
    for _ in 0..8 {
        assert_eq!(iterate(&mut fake, &settings), Outcome::Idle);
    }
    let tries = fake.steps.iter().filter(|step| *step == "release").count();
    assert_eq!(tries, record::MAX_RELEASE_TRIES as usize);
    assert_eq!(LaunchRecord::load_all(&settings).len(), 1);
    fake.fail_release = false;
    run(&mut fake, &settings, true).unwrap();
    assert!(LaunchRecord::load_all(&settings).is_empty());
}

#[test]
fn sigterm_drains_the_launch_releases_and_ends_the_loop() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.exit_at = None;
    fake.stop_at = Some(minutes(5));
    run(&mut fake, &config(dir.path()), false).unwrap();
    assert_eq!(fake.steps[5..], ["start", "signal:term", "release"]);
    assert!(fake.releases[0].contains("after a drain (the host supervisor stopped)"));
    assert_eq!(
        fake.steps.iter().filter(|s| s.starts_with("next")).count(),
        1
    );
}

#[test]
fn a_launch_that_ignores_sigterm_is_killed_after_the_drain() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    (fake.exit_at, fake.term_exit_ms) = (None, None);
    fake.stop_at = Some(minutes(5));
    iterate(&mut fake, &config(dir.path()));
    assert_eq!(fake.steps[6..], ["signal:term", "signal:kill", "release"]);
    assert!(fake.now >= minutes(5) + 30_000);
}

#[test]
fn a_failed_registration_releases_without_launching() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.fail_register = true;
    let outcome = iterate(&mut fake, &config(dir.path()));
    assert!(matches!(outcome, Outcome::Failed(reason) if reason.contains("register the checkout")));
    assert_eq!(fake.steps[2..], ["claim:t1", "register:a1", "release"]);
    assert!(fake.releases[0].contains("checkout_registered"));
    assert_eq!(
        (left(dir.path(), "clones"), left(dir.path(), "runs")),
        (0, 0)
    );
}

#[test]
fn clones_get_the_default_commit_identity() {
    let run = RunConfig::default();
    assert_eq!(run.git_name, "agentc implementer");
    assert_eq!(run.git_email, "agentc-impl@agentc.invalid");
    let custom: RunConfig = toml::from_str("git_email = \"bot@example.com\"").unwrap();
    assert_eq!(custom.git_email, "bot@example.com");
}

#[test]
fn a_stop_before_the_claim_claims_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.stop_at = Some(0);
    let outcome = iterate(&mut fake, &config(dir.path()));
    assert!(matches!(outcome, Outcome::Failed(reason) if reason.contains("stopping")));
    assert!(fake.releases.is_empty());
}

#[test]
fn a_failed_release_keeps_the_launch_for_the_next_poll() {
    let dir = tempfile::tempdir().unwrap();
    let settings = config(dir.path());
    let mut fake = Fake::new();
    fake.fail_release = true;
    iterate(&mut fake, &settings);
    let [record] = &LaunchRecord::load_all(&settings)[..] else {
        panic!("the launch record is gone");
    };
    assert!(!record.released && record.launch(&settings, "p1").run.exists());
    (fake.fail_release, fake.next) = (false, json!({"action": null}));
    assert_eq!(iterate(&mut fake, &settings), Outcome::Idle);
    assert!(fake.releases[0].contains("no supervisor watched it"));
    assert!(LaunchRecord::load_all(&settings).is_empty());
    assert_eq!(
        (left(dir.path(), "clones"), left(dir.path(), "runs")),
        (0, 0)
    );
}

#[path = "health_tests.rs"]
mod health_tests;
#[path = "renewal_tests.rs"]
mod renewal_tests;
