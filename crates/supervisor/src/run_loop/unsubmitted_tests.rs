//! Launches that exit 0 without submitting: they are counted, the task (or
//! the host) is no longer claimed after a few, and the release handoff
//! carries the agent's final message with the launch's logs kept root-only.
use super::*;

/// A Claude event stream whose last message is `message`.
fn said(message: &str) -> String {
    let result = json!({"type": "result", "result": message, "total_cost_usd": 0.25});
    format!(
        "{}\n{result}\n",
        json!({"type": "system", "subtype": "init"})
    )
}

/// One poll of a fake whose launch exits by itself a minute after it starts.
fn poll(fake: &mut Fake, settings: &Config) -> Outcome {
    fake.exit_at = Some(fake.now + 60_000);
    iterate(fake, settings)
}

/// The claims `fake` has seen, one entry per launch.
fn claims(fake: &Fake) -> usize {
    fake.steps
        .iter()
        .filter(|s| s.starts_with("claim:"))
        .count()
}

#[test]
fn consecutive_exits_without_a_submission_stop_further_claims_of_the_task() {
    let dir = tempfile::tempdir().unwrap();
    let settings = config(dir.path());
    assert_eq!(settings.run.empty_exit_limit, 2);
    let mut fake = Fake::new();
    for _ in 0..2 {
        let outcome = poll(&mut fake, &settings);
        assert!(matches!(outcome, Outcome::Launched { .. }), "{outcome:?}");
    }
    assert_eq!(claims(&fake), 2);
    assert!(fake.releases[1].contains("Launch 2 in a row for this task"));
    assert!(fake.releases[1].contains("will not claim it again"));
    assert!(!fake.releases[0].contains("will not claim it again"));
    let outcome = poll(&mut fake, &settings);
    let Outcome::Refused(reason) = outcome else {
        panic!("{outcome:?}");
    };
    assert!(reason.contains("task t1 is held") && reason.contains("unsubmitted.json"));
    assert_eq!(claims(&fake), 2, "no third claim");
    // Another task is still claimed.
    fake.next = fake.suggest("t2");
    assert!(matches!(
        poll(&mut fake, &settings),
        Outcome::Launched { .. }
    ));
    assert_eq!(claims(&fake), 3);
    // Removing the file lets the operator release the task.
    fs::remove_file(unsubmitted::path(&settings)).unwrap();
    fake.next = fake.suggest("t1");
    assert!(matches!(
        poll(&mut fake, &settings),
        Outcome::Launched { .. }
    ));
}

#[test]
fn the_limit_is_configurable_and_zero_turns_it_off() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = config(dir.path());
    settings.run.empty_exit_limit = 3;
    settings.run.empty_exit_total_limit = 0;
    let mut fake = Fake::new();
    for _ in 0..3 {
        poll(&mut fake, &settings);
    }
    assert!(matches!(poll(&mut fake, &settings), Outcome::Refused(_)));
    assert_eq!(claims(&fake), 3);
    let mut off = config(dir.path());
    off.run.empty_exit_limit = 0;
    off.run.empty_exit_total_limit = 0;
    assert!(matches!(poll(&mut fake, &off), Outcome::Launched { .. }));
    let parsed: RunConfig =
        toml::from_str("empty_exit_limit = 4\nempty_exit_total_limit = 9").unwrap();
    assert_eq!(
        (parsed.empty_exit_limit, parsed.empty_exit_total_limit),
        (4, 9)
    );
}

#[test]
fn exits_across_tasks_stop_all_claims_at_the_total_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = config(dir.path());
    settings.run.empty_exit_total_limit = 3;
    let mut fake = Fake::new();
    for task in ["a", "b", "c"] {
        fake.next = fake.suggest(task);
        poll(&mut fake, &settings);
    }
    fake.next = fake.suggest("d");
    let outcome = poll(&mut fake, &settings);
    assert!(matches!(&outcome, Outcome::Refused(r) if r.contains("3 launches in a row")));
    assert_eq!(claims(&fake), 3);
    assert!(fake.releases[2].contains("will not claim it again"));
}

#[test]
fn a_launch_that_submits_clears_the_counts() {
    let dir = tempfile::tempdir().unwrap();
    let settings = config(dir.path());
    let mut fake = Fake::new();
    poll(&mut fake, &settings);
    assert_eq!(unsubmitted::Strikes::load(&settings).total, 1);
    fake.attempt_state = Some("submitted".into());
    poll(&mut fake, &settings);
    assert_eq!(
        fake.releases.len(),
        1,
        "the submitting launch is not released"
    );
    assert_eq!(unsubmitted::Strikes::load(&settings), Default::default());
    fake.attempt_state = None;
    for _ in 0..2 {
        poll(&mut fake, &settings);
    }
    assert_eq!(claims(&fake), 4, "the count started again");
}

#[test]
fn the_release_carries_the_final_message_and_the_logs_are_kept_root_only() {
    let dir = tempfile::tempdir().unwrap();
    let settings = config(dir.path());
    let mut fake = Fake::new();
    let long = format!("No working shell. {}", "x".repeat(5000));
    fake.events = said(&long);
    fake.stderr = "bash: fork: retry\n".into();
    let outcome = poll(&mut fake, &settings);
    assert!(matches!(outcome, Outcome::Launched { .. }), "{outcome:?}");
    let release = &fake.releases[0];
    assert!(release.contains("the launch exited with code 0"));
    assert!(release.contains("The agent's final message: \"No working shell. xxx"));
    assert!(release.contains('…'), "the message is truncated");
    assert!(release.chars().count() < 2000, "{}", release.len());
    // The run directory is gone; the logs are not.
    assert_eq!(left(dir.path(), "runs"), 0);
    let kept = unsubmitted::retained_dir(&settings);
    let sessions: Vec<_> = fs::read_dir(&kept).unwrap().flatten().collect();
    assert_eq!(sessions.len(), 1);
    let session = sessions[0].path();
    assert!(release.contains(&session.display().to_string()));
    let mut names: Vec<_> = fs::read_dir(&session)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["events.jsonl", "stderr.log"]);
    assert_eq!(
        fs::read_to_string(session.join("events.jsonl")).unwrap(),
        fake.events
    );
    assert_eq!(
        fs::read_to_string(session.join("stderr.log")).unwrap(),
        fake.stderr
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&session), 0o700);
        assert_eq!(mode(&kept), 0o700);
        assert_eq!(mode(&session.join("events.jsonl")), 0o600);
        assert_eq!(mode(&session.join("stderr.log")), 0o600);
        assert_eq!(mode(&unsubmitted::path(&settings)), 0o600);
    }
}

#[test]
fn a_launch_without_a_final_message_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    poll(&mut fake, &config(dir.path()));
    assert!(fake.releases[0].contains("The launch wrote no final message."));
}

#[test]
fn a_launch_that_never_ran_keeps_no_logs_and_counts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let settings = config(dir.path());
    let mut fake = Fake::new();
    fake.fail_start = true;
    for _ in 0..3 {
        poll(&mut fake, &settings);
    }
    assert_eq!(claims(&fake), 3);
    assert!(!unsubmitted::path(&settings).exists());
    assert!(!unsubmitted::retained_dir(&settings).exists());
}

#[test]
fn final_message_reads_claude_and_codex_streams() {
    let claude = format!(
        "{}\n{}\n",
        json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "first"}]}}),
        json!({"type": "assistant", "message": {"content": [
            {"type": "text", "text": "second"}, {"type": "tool_use", "name": "Bash"}]}}),
    );
    assert_eq!(
        unsubmitted::final_message(claude.as_bytes()).unwrap(),
        "second"
    );
    let codex = format!(
        "{}\n{}\nnot json\n",
        json!({"type": "item.completed", "item": {"type": "agent_message", "text": "a\nb"}}),
        json!({"type": "item.completed", "item": {"type": "command_execution", "text": "ls"}}),
    );
    assert_eq!(unsubmitted::final_message(codex.as_bytes()).unwrap(), "a b");
    assert_eq!(unsubmitted::final_message(b""), None);
}

#[test]
fn retained_logs_are_bounded_in_size_and_number() {
    let dir = tempfile::tempdir().unwrap();
    let settings = config(dir.path());
    let mut fake = Fake::new();
    let line = format!("{}\n", "y".repeat(999));
    fake.events = line.repeat(unsubmitted::MAX_RETAINED_BYTES / 1000 * 2);
    let mut open = settings.clone();
    open.run.empty_exit_limit = 0;
    open.run.empty_exit_total_limit = 0;
    for _ in 0..unsubmitted::KEEP_RETAINED + 3 {
        poll(&mut fake, &open);
    }
    let kept = fs::read_dir(unsubmitted::retained_dir(&open))
        .unwrap()
        .count();
    assert_eq!(kept, unsubmitted::KEEP_RETAINED);
    let newest = fs::read_dir(unsubmitted::retained_dir(&open))
        .unwrap()
        .flatten()
        .next()
        .unwrap();
    let events = fs::read(newest.path().join("events.jsonl")).unwrap();
    assert!(events.len() <= unsubmitted::MAX_RETAINED_BYTES);
    assert!(events.starts_with(b"yyy") && events.ends_with(b"\n"));
}
