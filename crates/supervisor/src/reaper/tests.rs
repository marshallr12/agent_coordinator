use super::*;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

/// Set in a re-executed copy of this test binary: the scenario it plays.
const SCENARIO: &str = "AGENTC_REAPER_TEST_SCENARIO";
/// Set in a re-executed copy that ptrace-attaches to the given pid and sleeps.
const TRACE: &str = "AGENTC_REAPER_TEST_TRACE";
/// How long a re-executed scenario may run before it counts as hung.
const SCENARIO_DEADLINE: Duration = Duration::from_secs(60);

#[test]
fn harness_processes_cannot_gain_privileges() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "grep NoNewPrivs: /proc/self/status"]);
    forbid_new_privileges(&mut command);
    let output = command.output().unwrap();
    let status = String::from_utf8(output.stdout).unwrap();
    assert_eq!(status.split_whitespace().nth(1), Some("1"), "{status}");
}

#[test]
fn parent_field_survives_names_with_spaces_and_parentheses() {
    assert_eq!(parent_of("42 (a) b) (c) S 7 42 42 0"), Some("7"));
    assert_eq!(parent_of("42 no-parenthesis"), None);
}

/// Subreaping and killing every descendant is process-wide, so each
/// scenario runs in a re-executed copy of this test binary that has no other
/// children, under a deadline so a stalled reaper fails instead of hanging.
#[test]
fn leftovers_are_killed_in_a_fresh_launch_process() {
    if let Some(pid) = std::env::var_os(TRACE) {
        return trace_forever(pid.to_str().unwrap().parse().unwrap());
    }
    match std::env::var(SCENARIO).as_deref() {
        Ok("detached") => detached_and_orphaned_leftovers(),
        Ok("chain") => deep_chain(),
        Ok("traced") => traced_leftover(),
        Ok("reaped") => reaped_cleans_up_after_success_and_failure(),
        Ok("zombie") => exited_leftovers_are_told_apart_from_killed_ones(),
        _ => ["detached", "chain", "traced", "reaped", "zombie"]
            .into_iter()
            .for_each(run_scenario),
    }
}

/// Re-runs this test as `scenario` and requires it to pass in time.
fn run_scenario(scenario: &str) {
    let mut child = reexec(&[(SCENARIO, scenario)])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > SCENARIO_DEADLINE {
            child.kill().unwrap();
            panic!("scenario {scenario} hung");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "scenario {scenario}: {stdout}");
    assert!(stdout.contains("1 passed"), "scenario {scenario}: {stdout}");
}

/// This test binary, filtered to this one test, with `env` set.
fn reexec(env: &[(&str, &str)]) -> Command {
    let name = "reaper::tests::leftovers_are_killed_in_a_fresh_launch_process";
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([name, "--exact", "--test-threads=1", "--nocapture"]);
    command.envs(env.iter().copied()).stderr(Stdio::null());
    command
}

/// A setsid daemon whose parent already exited and a still-running child
/// with an orphaned grandchild must all be adopted and killed.
fn detached_and_orphaned_leftovers() {
    adopt_orphans().unwrap();
    let daemon = detached_daemon();
    let (mut running, grandchild) = child_with_orphaned_grandchild();
    let me = std::process::id().to_string();
    let stat = std::fs::read_to_string(format!("/proc/{daemon}/stat")).unwrap();
    assert_eq!(
        parent_of(&stat),
        Some(me.as_str()),
        "daemon was not adopted"
    );
    assert!(kill_leftovers().unwrap().len() >= 3);
    assert_gone(&[daemon, grandchild]);
    assert!(running.try_wait().is_err(), "running child was not reaped");
}

/// A linear chain 100 deep dies completely in a handful of rounds, which
/// only killing the whole tree at once achieves.
fn deep_chain() {
    adopt_orphans().unwrap();
    let script = "[ \"$D\" -le 1 ] && { echo $$; exec sleep 300; }; \
                  D=$((D-1)) /bin/sh -c \"$0\" \"$0\" & wait";
    let mut chain = Command::new("/bin/sh")
        .args(["-c", script, script])
        .env("D", "100")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let leaf = first_line_pid(&mut chain);
    let (found, rounds) = kill_rounds().unwrap();
    assert!(found.len() >= 100, "found {}", found.len());
    assert!(rounds < 20, "took {rounds} rounds");
    assert_gone(&[leaf]);
}

/// A leftover held by a ptrace tracer that never waits for it must not stall
/// the reaper, whichever of the two has the lower pid.
fn traced_leftover() {
    adopt_orphans().unwrap();
    let mut tracee = Command::new("/bin/sleep").arg("300").spawn().unwrap();
    let pid = tracee.id().to_string();
    // The tracer must not hold the scenario's stdout, or a leaked tracer
    // would stall `run_scenario` past its deadline.
    let mut tracer = reexec(&[(TRACE, &pid)])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let attached = wait_for_tracer(tracee.id());
    let scope = std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope");
    assert!(
        attached || scope.is_ok_and(|s| s.trim() != "0"),
        "tracer never attached"
    );
    assert!(kill_leftovers().unwrap().len() >= 2);
    assert_gone(&[tracee.id(), tracer.id()]);
    assert!(
        tracee.try_wait().is_err() && tracer.try_wait().is_err(),
        "not reaped"
    );
}

/// `reaped` makes the launch a subreaper and kills a detached leftover both
/// through the cleanup it hands the body and after a body that fails.
fn reaped_cleans_up_after_success_and_failure() {
    let mut daemon = 0;
    reaped(|cleanup| {
        daemon = detached_daemon();
        cleanup();
        Ok(())
    })
    .unwrap();
    assert_gone(&[daemon]);
    let mut refused = 0;
    let result = reaped::<()>(|_| {
        refused = detached_daemon();
        anyhow::bail!("preflight refused the launch")
    });
    assert!(result.is_err());
    assert_gone(&[refused]);
}

/// An exited, unreaped child (as a Bubblewrap sandbox's PID-namespace init
/// is left) is reported as exited and reaped; a running one as killed.
fn exited_leftovers_are_told_apart_from_killed_ones() {
    adopt_orphans().unwrap();
    let mut exited = Command::new("/bin/true").spawn().unwrap();
    wait_for_zombie(exited.id());
    let mut running = Command::new("/bin/sleep").arg("300").spawn().unwrap();
    let mut found = kill_leftovers().unwrap();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    let named = |name: &str, exited| Leftover {
        name: name.into(),
        exited,
    };
    assert_eq!(found, [named("sleep", false), named("true", true)]);
    assert_gone(&[exited.id(), running.id()]);
    assert!(
        exited.try_wait().is_err() && running.try_wait().is_err(),
        "not reaped"
    );
}

/// Waits up to five seconds for `pid` to become a zombie.
fn wait_for_zombie(pid: u32) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        if stat
            .rsplit_once(')')
            .unwrap()
            .1
            .trim_start()
            .starts_with('Z')
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{pid} never exited");
}

#[test]
fn the_report_names_killed_and_reaped_leftovers_separately() {
    let named = |name: &str, exited| Leftover {
        name: name.into(),
        exited,
    };
    assert_eq!(report(&[]), None);
    let found = [
        named("bwrap", true),
        named("node", false),
        named("bwrap", true),
    ];
    assert_eq!(
        report(&found).unwrap(),
        "agentc-supervisor: killed 1 leftover launch processes (node); \
         reaped 2 exited launch processes (bwrap x2)"
    );
    let line = report(&found[..1]).unwrap();
    assert_eq!(
        line,
        "agentc-supervisor: reaped 1 exited launch processes (bwrap)"
    );
}

#[test]
fn control_characters_in_names_are_escaped_onto_one_line() {
    let named = |name: &str| Leftover {
        name: name.into(),
        exited: false,
    };
    let found = [
        named("evil\nagentc-supervisor: forged"),
        named("esc\x1b[2J\r\t\0"),
        named("back\\slash\u{85}\u{2028}"),
        named("plain"),
    ];
    let line = report(&found).unwrap();
    assert_eq!(
        line,
        "agentc-supervisor: killed 4 leftover launch processes (\
         back\\\\slash\\u{85}\\u{2028}, esc\\x1b[2J\\r\\t\\x00, \
         evil\\nagentc-supervisor: forged, plain)"
    );
    assert!(!line.chars().any(char::is_control));
}

/// Attaches to `pid` with ptrace and then sleeps without ever waiting on it.
fn trace_forever(pid: libc::pid_t) {
    // SAFETY: PTRACE_ATTACH takes only a pid; the address and data are null.
    unsafe { libc::ptrace(libc::PTRACE_ATTACH, pid, 0usize, 0usize) };
    std::thread::sleep(Duration::from_secs(300));
}

/// Whether `pid` reports a tracer within five seconds.
fn wait_for_tracer(pid: u32) -> bool {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        if status
            .lines()
            .any(|l| l.starts_with("TracerPid:") && !l.ends_with("\t0"))
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Starts `sleep` in its own session from a shell that exits at once;
/// returns the sleeper's pid.
fn detached_daemon() -> u32 {
    let output = Command::new("/bin/sh")
        .args([
            "-c",
            "setsid sleep 300 </dev/null >/dev/null 2>&1 & echo $!",
        ])
        .output()
        .unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Starts a sleeping child whose subshell orphaned a sleeping grandchild;
/// returns the child and the grandchild's pid.
fn child_with_orphaned_grandchild() -> (Child, u32) {
    let mut running = Command::new("/bin/sh")
        .args(["-c", "(sleep 300 & echo $!); exec sleep 300"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let grandchild = first_line_pid(&mut running);
    (running, grandchild)
}

/// The pid `child` printed on its first stdout line.
fn first_line_pid(child: &mut Child) -> u32 {
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    line.trim().parse().unwrap()
}

/// Requires every pid to be gone and nothing left below this process.
fn assert_gone(pids: &[u32]) {
    for pid in pids {
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "{pid} survived"
        );
    }
    assert!(descendants().unwrap().is_empty(), "descendants survived");
}
