//! `update`, `rollback` and `uninstall` (phase 7, step 7b) against a host
//! and a Service Control Manager in memory. These run on every platform.
use super::control::RunState;
use super::fake::{FakeHost, FakeServices};
use super::install::install;
use super::journal::Journal;
use super::model::{Desired, Inputs, Layout, Role, Sids};
use super::observed::{PathState, ProfileState};
use super::release::{Release, Releases};
use super::uninstall::uninstall;
use super::update::{self, Record, Request, Run, Stage, advance, rollback, save, update};

/// The remote the installed configuration names.
const REMOTE: &str = "https://github.com/example/project.git";
/// Where the new build runs from: outside the program directory.
const SOURCE: &str = r"C:\build\agentc-supervisor.exe";

/// A release whose digest and commit are made of `digit`.
fn release(digit: char) -> Release {
    Release {
        sha256: digit.to_string().repeat(64),
        commit: digit.to_string().repeat(40),
    }
}

/// The installed release.
fn old() -> Release {
    release('a')
}

/// The release the update installs.
fn new() -> Release {
    release('b')
}

/// The model of the host as the release `sha256` builds it.
fn model(sha256: &str) -> Desired {
    let inputs = Inputs {
        remote: REMOTE.into(),
        ..Inputs::default()
    };
    let mut desired = Desired::new(Layout::default(), inputs, sha256.into(), None).unwrap();
    desired.commit = if sha256 == old().sha256 {
        old().commit
    } else {
        new().commit
    };
    desired
}

/// A host with the old release installed and its four services running,
/// and the configuration's remote changed by hand, so the old and the new
/// release's configurations differ.
fn installed() -> (FakeHost, FakeServices) {
    let mut host = FakeHost::clean();
    let dir = tempfile::tempdir().unwrap();
    let report = install(
        &mut host,
        &model(&old().sha256),
        &mut Journal::at(dir.path().join("j")),
    )
    .unwrap();
    assert!(report.clean(), "{report:?}");
    let config = Layout::default().config();
    let edited = host.texts[&config].replace(REMOTE, "https://example.invalid/old.git");
    host.texts.insert(config, edited);
    host.digests.insert(SOURCE.into(), new().sha256);
    let mut services = FakeServices::of(&host, &Layout::default());
    for state in services.states.values_mut() {
        *state = RunState::Running;
    }
    (host, services)
}

/// What the owner asks for to install the new release.
fn request() -> Request {
    Request {
        to: new(),
        dirty: false,
        built_from: new().commit,
        retry: false,
    }
}

/// Runs `update` as the new release would.
fn run_update(
    host: &mut FakeHost,
    services: &mut FakeServices,
    request: &Request,
) -> update::UpdateReport {
    let desired = model(&request.to.sha256);
    let mut run = Run {
        host,
        services,
        desired: &desired,
        source: SOURCE.into(),
    };
    update(&mut run, request).unwrap()
}

/// Runs `rollback` as the installed release would, from a copy outside the
/// program directory.
fn run_rollback(host: &mut FakeHost, services: &mut FakeServices) -> update::UpdateReport {
    let desired = model(&new().sha256);
    let mut run = Run {
        host,
        services,
        desired: &desired,
        source: SOURCE.into(),
    };
    rollback(&mut run).unwrap()
}

/// The release record.
fn releases(host: &FakeHost) -> Releases {
    Releases::parse(&host.texts[&Layout::default().releases()]).unwrap()
}

/// Fails unless `release`'s binary and configuration are in place, nothing
/// is open, and the four services run.
fn fully(host: &FakeHost, services: &FakeServices, release: &Release, config: &str) {
    let layout = Layout::default();
    assert_eq!(host.digests[&layout.binary()], release.sha256, "binary");
    assert_eq!(host.texts[&layout.config()], config, "configuration");
    assert!(
        !host.texts.contains_key(&layout.update_record()),
        "an open record"
    );
    assert_eq!(services.running().len(), 4, "{:?}", services.states);
    assert_eq!(releases(host).current.as_ref(), Some(release));
}

/// The configuration the new release writes over `old_config`.
fn new_config(old_config: &str) -> String {
    super::config_text::merged(Some(old_config), &model(&new().sha256).settings).unwrap()
}

#[test]
fn an_update_keeps_the_old_release_stops_switches_and_starts_in_order() {
    let (mut host, mut services) = installed();
    let before = host.texts[&Layout::default().config()].clone();
    let report = run_update(&mut host, &mut services, &request());
    assert_eq!(report.outcome, "updated", "{report:?}");
    fully(&host, &services, &new(), &new_config(&before));
    let layout = Layout::default();
    assert_eq!(
        host.digests[&layout.kept_binary(&old().sha256)],
        old().sha256
    );
    assert_eq!(host.texts[&layout.kept_config(&old().sha256)], before);
    assert_eq!(releases(&host).previous, Some(old()));
    let order: Vec<&str> = services.calls.iter().map(String::as_str).collect();
    assert_eq!(
        order,
        [
            "stop agentc-supervisor",
            "stop agentc-runner",
            "stop agentc-egress",
            "stop agentc-publication",
            "start agentc-egress",
            "start agentc-publication",
            "start agentc-runner",
            "start agentc-supervisor",
        ]
    );
}

#[test]
fn a_failed_check_puts_the_old_release_back_running_and_remembers_the_new_one() {
    let (mut host, mut services) = installed();
    let before = host.texts[&Layout::default().config()].clone();
    services.silent_starts = 1;
    let report = run_update(&mut host, &mut services, &request());
    assert_eq!(report.outcome, "reverted", "{report:?}");
    assert!(
        report
            .reason
            .as_deref()
            .unwrap()
            .contains("gained no start line"),
        "{report:?}"
    );
    fully(&host, &services, &old(), &before);
    let recorded = releases(&host);
    assert!(recorded.rejected(&new().sha256));
    assert_eq!(recorded.previous, None, "nothing else was kept before");
    assert!(
        !host
            .paths
            .contains_key(&Layout::default().kept(&new().sha256)),
        "pruned"
    );
}

#[test]
fn a_rolled_back_release_is_refused_until_it_is_asked_for_again() {
    let (mut host, mut services) = installed();
    services.silent_starts = 1;
    run_update(&mut host, &mut services, &request());
    let refused = run_update(&mut host, &mut services, &request());
    assert_eq!(refused.outcome, "refused");
    assert!(refused.refusals[0].contains("--retry"), "{refused:?}");
    let retry = Request {
        retry: true,
        ..request()
    };
    let report = run_update(&mut host, &mut services, &retry);
    assert_eq!(report.outcome, "updated", "{report:?}");
    assert!(!releases(&host).rejected(&new().sha256));
}

/// Runs an update to its `stages`-th saved stage and then, when `acted`,
/// does the next stage's work without saving it: a kill in either window.
fn killed(
    host: &mut FakeHost,
    services: &mut FakeServices,
    stages: usize,
    acted: bool,
) -> Option<Stage> {
    let desired = model(&new().sha256);
    let mut run = Run {
        host,
        services,
        desired: &desired,
        source: SOURCE.into(),
    };
    let mut record = begun(&mut run);
    for _ in 0..stages {
        let next = advance(&mut run, &record).unwrap()?;
        save(&mut run, &next).unwrap();
        record = next;
    }
    // Work done past the last stage finishes the record.
    if acted && matches!(advance(&mut run, &record), Ok(None)) {
        return None;
    }
    Some(record.stage)
}

/// The record a new update writes before it changes anything.
fn begun(run: &mut Run) -> Record {
    let record = Record {
        kind: update::Kind::Update,
        from: old(),
        to: new(),
        running_before: Role::ALL.map(|r| Layout::default().service(r)).into(),
        stage: Stage::Begun,
        retained: false,
        switched: false,
        offsets: Default::default(),
        retry: false,
        reason: None,
    };
    save(run, &record).unwrap();
    record
}

/// A power loss: every service is stopped when the machine comes back.
fn power_loss(services: &mut FakeServices) {
    for state in services.states.values_mut() {
        *state = RunState::Stopped {
            exit: 0,
            specific: 0,
        };
    }
}

#[test]
fn an_update_killed_in_either_window_of_any_stage_ends_old_or_new_never_mixed() {
    for (stages, acted, lost) in cases(12) {
        let (mut host, mut services) = installed();
        let before = host.texts[&Layout::default().config()].clone();
        let at = killed(&mut host, &mut services, stages, acted);
        // A power loss after a finished update only stops the services, as
        // any restart does; it matters while a record is open.
        if lost && at.is_some() {
            power_loss(&mut services);
        }
        // The next run is any `update`: it finishes or undoes the record.
        let report = run_update(&mut host, &mut services, &request());
        let new_wins = at.is_none_or(|stage| stage >= Stage::Checked);
        let case = format!("{stages} stages, acted {acted}, lost {lost}, at {at:?}: {report:?}");
        assert!(report.succeeded() || report.outcome == "reverted", "{case}");
        if new_wins {
            fully(&host, &services, &new(), &new_config(&before));
        } else {
            fully(&host, &services, &old(), &before);
            let rejected = releases(&host).rejected(&new().sha256);
            assert_eq!(rejected, at.is_some_and(|s| s >= Stage::Switched), "{case}");
        }
    }
}

/// Every number of stages below `limit`, with the kill before or after the
/// next stage's work, and with or without a power loss after it.
fn cases(limit: usize) -> Vec<(usize, bool, bool)> {
    let flags = [false, true];
    (0..limit)
        .flat_map(|s| {
            flags
                .into_iter()
                .flat_map(move |a| flags.map(move |l| (s, a, l)))
        })
        .collect()
}

/// Runs an update through its start, turns it to undoing itself as a failed
/// check would, and kills the undo after `stages` saved stages, before or
/// after the next one's work.
fn killed_undoing(
    host: &mut FakeHost,
    services: &mut FakeServices,
    stages: usize,
    acted: bool,
) -> bool {
    let desired = model(&new().sha256);
    let mut run = Run {
        host,
        services,
        desired: &desired,
        source: SOURCE.into(),
    };
    let mut record = begun(&mut run);
    while record.stage < Stage::Started {
        record = advance(&mut run, &record).unwrap().unwrap();
        save(&mut run, &record).unwrap();
    }
    record = Record {
        stage: Stage::Reverting,
        reason: Some("a check failed".into()),
        ..record
    };
    save(&mut run, &record).unwrap();
    for _ in 0..stages {
        let Some(next) = advance(&mut run, &record).unwrap() else {
            return false;
        };
        save(&mut run, &next).unwrap();
        record = next;
    }
    !(acted && matches!(advance(&mut run, &record), Ok(None)))
}

#[test]
fn an_undo_killed_in_either_window_of_any_stage_ends_with_the_old_release_running() {
    for (stages, acted, lost) in cases(8) {
        let (mut host, mut services) = installed();
        let before = host.texts[&Layout::default().config()].clone();
        let open = killed_undoing(&mut host, &mut services, stages, acted);
        if lost && open {
            power_loss(&mut services);
        }
        let report = run_update(&mut host, &mut services, &request());
        let case = format!("{stages} undo stages, acted {acted}, lost {lost}: {report:?}");
        assert!(matches!(report.outcome, "reverted" | "refused"), "{case}");
        fully(&host, &services, &old(), &before);
        assert!(releases(&host).rejected(&new().sha256), "{case}");
    }
}

#[test]
fn a_runner_that_stops_with_launches_unconfirmed_stops_the_update_before_the_switch() {
    let (mut host, mut services) = installed();
    let before = host.texts[&Layout::default().config()].clone();
    services.unclean = vec!["agentc-runner".into()];
    let report = run_update(&mut host, &mut services, &request());
    assert_eq!(report.outcome, "reverted", "{report:?}");
    assert!(
        report
            .reason
            .as_deref()
            .unwrap()
            .contains("a launch may still run"),
        "{report:?}"
    );
    assert!(!report.stages.contains(&Stage::Switched), "{report:?}");
    fully(&host, &services, &old(), &before);
    assert!(
        !releases(&host).rejected(&new().sha256),
        "it was never put in place"
    );
}

#[test]
fn services_that_did_not_run_are_checked_and_stopped_again() {
    let (mut host, mut services) = installed();
    for state in services.states.values_mut() {
        *state = RunState::Stopped {
            exit: 0,
            specific: 0,
        };
    }
    let report = run_update(&mut host, &mut services, &request());
    assert_eq!(report.outcome, "updated", "{report:?}");
    assert!(
        services
            .calls
            .iter()
            .filter(|c| c.starts_with("start"))
            .count()
            == 4
    );
    assert!(services.running().is_empty(), "{:?}", services.states);
}

#[test]
fn rollback_returns_to_the_kept_release_and_then_has_nothing_to_return_to() {
    let (mut host, mut services) = installed();
    let before = host.texts[&Layout::default().config()].clone();
    run_update(&mut host, &mut services, &request());
    let report = run_rollback(&mut host, &mut services);
    assert_eq!(report.outcome, "returned", "{report:?}");
    fully(&host, &services, &old(), &before);
    let recorded = releases(&host);
    assert!(recorded.rejected(&new().sha256));
    assert_eq!(recorded.previous, None);
    let again = run_rollback(&mut host, &mut services);
    assert_eq!(again.outcome, "refused");
    assert!(
        again.refusals[0].contains("no previous release"),
        "{again:?}"
    );
}

#[test]
fn an_update_is_refused_unless_this_binary_is_the_release_named() {
    let cases: [(&str, Request); 4] = [
        (
            "SHA-256",
            Request {
                to: Release {
                    sha256: "c".repeat(64),
                    ..new()
                },
                ..request()
            },
        ),
        (
            "built from",
            Request {
                built_from: "d".repeat(40),
                ..request()
            },
        ),
        (
            "uncommitted",
            Request {
                dirty: true,
                ..request()
            },
        ),
        (
            "not a full source commit",
            Request {
                to: Release {
                    commit: "b".into(),
                    ..new()
                },
                ..request()
            },
        ),
    ];
    for (said, request) in cases {
        let (mut host, mut services) = installed();
        let before = (host.clone(), services.states.clone());
        let report = run_update(&mut host, &mut services, &request);
        assert_eq!(report.outcome, "refused", "{said}: {report:?}");
        assert!(
            report.refusals.iter().any(|r| r.contains(said)),
            "{said}: {report:?}"
        );
        assert_eq!((host, services.states), before, "{said}: nothing changes");
    }
}

#[test]
fn an_update_from_the_program_directory_or_onto_no_host_is_refused() {
    let (mut host, mut services) = installed();
    let desired = model(&new().sha256);
    let inside = Layout::default().binary();
    let mut run = Run {
        host: &mut host,
        services: &mut services,
        desired: &desired,
        source: inside,
    };
    let report = update(&mut run, &request()).unwrap();
    assert!(
        report.refusals.iter().any(|r| r.contains("inside")),
        "{report:?}"
    );
    let (mut bare, mut none) = (
        FakeHost::clean(),
        FakeServices::of(&FakeHost::clean(), &Layout::default()),
    );
    let report = run_update(&mut bare, &mut none, &request());
    assert!(
        report.refusals[0].contains("nothing is installed"),
        "{report:?}"
    );
}

#[test]
fn an_update_to_the_installed_release_changes_nothing() {
    let (mut host, mut services) = installed();
    let same = Request {
        to: old(),
        built_from: old().commit,
        ..request()
    };
    host.digests.insert(SOURCE.into(), old().sha256);
    let before = host.clone();
    let report = run_update(&mut host, &mut services, &same);
    assert_eq!(report.outcome, "unchanged", "{report:?}");
    assert_eq!(host, before);
    assert!(services.calls.is_empty());
}

/// A host whose runner journal names two launch profiles.
fn with_launches(host: &mut FakeHost) {
    let journal = Layout::default().state(r"runner\launch-1.json");
    let entry = r#"{"stage":"ended","profile":"agentc.standin.1","candidate":{"profile":"agentc.candidate.1"}}"#;
    host.texts.insert(journal, entry.into());
}

/// Every path of the two trees the fake host still holds.
fn tree_paths(host: &FakeHost) -> Vec<String> {
    let layout = Layout::default();
    let roots = [layout.program_dir.clone(), layout.state_root.clone()];
    let all = host
        .paths
        .keys()
        .chain(host.texts.keys())
        .chain(host.digests.keys());
    all.filter(|p| roots.iter().any(|r| p.starts_with(r.as_str())))
        .cloned()
        .collect()
}

#[test]
fn uninstall_removes_the_services_trees_and_profiles_and_a_loaded_profile_waits() {
    let (mut host, mut services) = installed();
    with_launches(&mut host);
    let sids = Sids::of(&Layout::default());
    host.profiles
        .insert(sids.runner.clone(), ProfileState::Loaded);
    host.profiles
        .insert(sids.egress.clone(), ProfileState::Unloaded);
    let report = uninstall(&mut host, &mut services, &Layout::default(), SOURCE).unwrap();
    assert!(report.refusals.is_empty(), "{report:?}");
    assert!(
        host.services.is_empty() && tree_paths(&host).is_empty(),
        "{report:?}"
    );
    assert_eq!(
        report.launch_profiles,
        ["agentc.candidate.1", "agentc.standin.1"]
    );
    assert_eq!(report.pending.len(), 1, "{report:?}");
    assert!(report.pending[0].contains(&sids.runner) && !report.clean());
    assert_eq!(host.profiles.get(&sids.egress), None, "deleted");
    host.profiles
        .insert(sids.runner.clone(), ProfileState::Unloaded);
    let again = uninstall(&mut host, &mut services, &Layout::default(), SOURCE).unwrap();
    assert!(again.clean(), "{again:?}");
    assert_eq!(
        again.removed.len(),
        1,
        "only the runner's profile is left: {again:?}"
    );
    let third = uninstall(&mut host, &mut services, &Layout::default(), SOURCE).unwrap();
    assert!(third.clean() && third.removed.is_empty(), "{third:?}");
}

#[test]
fn uninstall_stops_each_running_service_before_deleting_it() {
    let (mut host, mut services) = installed();
    uninstall(&mut host, &mut services, &Layout::default(), SOURCE).unwrap();
    let stops = services
        .calls
        .iter()
        .filter(|c| c.starts_with("stop"))
        .count();
    assert_eq!(stops, 4, "{:?}", services.calls);
}

#[test]
fn a_link_in_a_tree_is_removed_and_what_it_points_to_is_kept() {
    let (mut host, mut services) = installed();
    let link = Layout::default().state(r"runner\planted");
    host.paths.insert(link.clone(), PathState::Link);
    host.texts
        .insert(r"C:\Users\owner\keep.txt".into(), "kept".into());
    let report = uninstall(&mut host, &mut services, &Layout::default(), SOURCE).unwrap();
    assert!(report.left.is_empty(), "{report:?}");
    assert!(!host.paths.contains_key(&link));
    assert_eq!(host.texts[r"C:\Users\owner\keep.txt"], "kept");
}

#[test]
fn a_file_that_cannot_be_removed_is_reported_and_its_tree_kept() {
    let (mut host, mut services) = installed();
    let stuck = Layout::default().state(r"supervisor\service.log");
    host.texts.insert(stuck.clone(), "busy".into());
    host.stuck.push(stuck.clone());
    let report = uninstall(&mut host, &mut services, &Layout::default(), SOURCE).unwrap();
    assert!(
        report.left.iter().any(|l| l.starts_with(&stuck)),
        "{report:?}"
    );
    assert!(!report.clean());
}

#[test]
fn uninstall_refuses_a_tree_that_holds_what_the_installer_did_not_make() {
    let (mut host, mut services) = installed();
    let other = Layout::default().program("notes.txt");
    host.texts.insert(other, "the owner's".into());
    let before = host.clone();
    let report = uninstall(&mut host, &mut services, &Layout::default(), SOURCE).unwrap();
    assert!(report.refusals[0].contains("notes.txt"), "{report:?}");
    assert_eq!(host, before);
    assert!(services.calls.is_empty());
}

#[test]
fn uninstall_refuses_a_root_that_is_a_link_and_a_binary_inside_the_program_directory() {
    let (mut host, mut services) = installed();
    host.paths
        .insert(Layout::default().state_root.clone(), PathState::Link);
    let report = uninstall(&mut host, &mut services, &Layout::default(), SOURCE).unwrap();
    assert!(report.refusals[0].contains("link"), "{report:?}");
    let (mut host, mut services) = installed();
    let inside = Layout::default().binary();
    let report = uninstall(&mut host, &mut services, &Layout::default(), &inside).unwrap();
    assert!(report.refusals[0].contains("inside"), "{report:?}");
}

#[test]
fn uninstall_of_a_host_with_a_push_key_says_the_key_stays_with_the_app() {
    let (mut host, mut services) = installed();
    let manifest = Layout::default().manifest();
    let with_key = host.texts[&manifest].replace("\"push_key\": false", "\"push_key\": true");
    host.texts.insert(manifest, with_key);
    let report = uninstall(&mut host, &mut services, &Layout::default(), SOURCE).unwrap();
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("stays registered with the push App")),
        "{report:?}"
    );
}
