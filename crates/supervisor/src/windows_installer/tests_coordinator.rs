//! The coordinator's part of `install`, `status`, `update` and `uninstall`
//! (phase 8, step 8a) against a host in memory. These run on every platform.
use super::access::{ADMINISTRATORS, AccessList, Entry, FULL, Kind, MODIFY, READ, SYSTEM, USERS};
use super::config_text::{self, Settings};
use super::control::RunState;
use super::coordinator;
use super::fake::{FakeHost, FakeServices};
use super::install::{Report, install_from, recorded_inputs, status_from};
use super::journal::Journal;
use super::model::{Desired, Inputs, Layout, Overrides, Sids};
use super::observed::{KeySource, PathState};
use super::plan::Action;
use super::release::Release;
use super::uninstall::uninstall;
use super::update::{Request, Run, update};

/// The remote the publication broker is given.
const REMOTE: &str = "https://github.com/example/project.git";
/// The coordinator's origin.
const URL: &str = "https://coordinator.example.test";
/// The project the supervisor claims from.
const PROJECT: &str = "project-1";
/// The credential file the owner downloaded.
const CREDENTIAL: &str = r"C:\Users\owner\Downloads\credentials.toml";
/// The CLI file the owner downloaded.
const CLI: &str = r"C:\Users\owner\Downloads\agent-coordinator.exe";
/// Where a new build runs from: outside the program directory.
const SOURCE: &str = r"C:\build\agentc-supervisor.exe";

/// The digest of the binary being installed.
fn binary() -> String {
    "a".repeat(64)
}

/// The digest of the CLI file the owner downloaded.
fn cli_digest() -> String {
    "c".repeat(64)
}

/// What a first install with a coordinator and no push App is told.
fn first_run() -> Overrides {
    Overrides {
        remote: Some(REMOTE.into()),
        coordinator: coordinator::Overrides {
            url: Some(URL.into()),
            project: Some(PROJECT.into()),
            credential_source: Some(CREDENTIAL.into()),
            cli_source: Some(CLI.into()),
            fixture: Some(vec![r"C:\tools\fixture.exe".into(), "--exit".into()]),
            off: false,
        },
        ..Overrides::default()
    }
}

/// A run that names only `coordinator`.
fn only(coordinator: coordinator::Overrides) -> Overrides {
    Overrides {
        coordinator,
        ..Overrides::default()
    }
}

/// A clean machine on which the owner's CLI file has its digest.
fn clean() -> FakeHost {
    let mut host = FakeHost::clean();
    host.digests.insert(CLI.into(), cli_digest());
    host
}

/// Runs `install` on `host` as `overrides` says.
fn install(host: &mut FakeHost, overrides: &Overrides) -> Report {
    let dir = tempfile::tempdir().unwrap();
    let journal = &mut Journal::at(dir.path().join("journal.jsonl"));
    install_from(host, Layout::default(), overrides, binary(), journal).unwrap()
}

/// A clean machine on which the first run installed a coordinator host.
fn installed() -> FakeHost {
    let mut host = clean();
    let report = install(&mut host, &first_run());
    assert!(report.clean(), "{report:?}");
    host
}

/// The inputs `host`'s manifest records.
fn recorded(host: &FakeHost) -> Inputs {
    recorded_inputs(host, &Layout::default()).unwrap().unwrap()
}

/// What `status` reports of `host`.
fn status(host: &FakeHost) -> Report {
    status_from(host, Layout::default(), binary()).unwrap()
}

/// The actions of the changes a report lists.
fn actions(changes: &[super::plan::Change]) -> Vec<Action> {
    changes.iter().map(|c| c.action.clone()).collect()
}

/// `access` with Users granted read, as a hand-made change would.
fn widened(access: &AccessList) -> AccessList {
    let mut wider = access.clone();
    wider.entries.insert(Entry {
        kind: Kind::Allow,
        sid: USERS.into(),
        mask: READ,
        inheritance: 0,
        inherited: false,
    });
    wider
}

/// The access list of what `host` has at `path`.
fn access_at<'a>(host: &'a mut FakeHost, path: &str) -> &'a mut AccessList {
    match host.paths.get_mut(path) {
        Some(PathState::Directory { access, .. } | PathState::File { access, .. }) => access,
        other => panic!("{path} is {other:?}"),
    }
}

#[test]
fn a_coordinator_install_copies_the_credential_and_cli_and_a_second_changes_nothing() {
    let mut host = clean();
    let report = install(&mut host, &first_run());
    assert!(report.clean(), "{report:?}");
    let layout = Layout::default();
    let done = actions(&report.applied);
    for path in [layout.coordinator_dir(), layout.coordinator_launch_root()] {
        assert!(done.contains(&Action::CreateDirectory { path }), "{done:?}");
    }
    assert!(done.contains(&Action::InstallCredential), "{done:?}");
    assert!(done.contains(&Action::InstallCli), "{done:?}");
    assert_eq!(host.digests[&layout.cli()], cli_digest());
    assert!(matches!(
        host.paths[&layout.credential()],
        PathState::File { .. }
    ));
    let inputs = recorded(&host);
    assert_eq!(inputs.coordinator_url, URL);
    assert_eq!(inputs.coordinator_project, PROJECT);
    assert!(inputs.coordinator_credential);
    assert_eq!(inputs.coordinator_cli_sha256, cli_digest());
    assert!(status(&host).clean(), "{:?}", status(&host));
    let before = host.clone();
    let again = install(&mut host, &Overrides::default());
    assert!(again.clean() && again.applied.is_empty(), "{again:?}");
    assert_eq!(host, before);
}

#[test]
fn the_coordinator_directory_and_credential_grant_the_supervisor_and_nobody_else() {
    let host = installed();
    let layout = Layout::default();
    let desired = Desired::new(layout.clone(), recorded(&host), binary(), None).unwrap();
    let sids = Sids::of(&layout);
    let directory = &desired.directory(&layout.coordinator_dir()).unwrap().access;
    let grants = [(SYSTEM, FULL), (ADMINISTRATORS, FULL)];
    let supervisor = (sids.supervisor.as_str(), MODIFY);
    assert_eq!(
        *directory,
        AccessList::directory(&[grants[0], grants[1], supervisor])
    );
    let credential = &desired.credential.as_ref().unwrap().access;
    let read = (sids.supervisor.as_str(), READ);
    assert_eq!(*credential, AccessList::file(&[grants[0], grants[1], read]));
    let launches = &desired
        .directory(&layout.coordinator_launch_root())
        .unwrap();
    let stand_in = desired.directory(&layout.state(r"runner\stand-in-launches"));
    assert_eq!(launches.access, stand_in.unwrap().access);
    assert_eq!(launches.owners, stand_in.unwrap().owners);
    let others = [&*sids.runner, &*sids.egress, &*sids.publication, USERS];
    for list in [directory, credential] {
        assert!(
            list.entries.iter().all(|e| !others.contains(&&*e.sid)),
            "{list:?}"
        );
        assert!(list.protected && list.entries.iter().all(|e| !e.inherited));
    }
    let plain = Inputs {
        remote: REMOTE.into(),
        ..Inputs::default()
    };
    let without = Desired::new(layout.clone(), plain, binary(), None).unwrap();
    assert!(without.credential.is_none() && without.cli.is_none());
    assert!(without.directory(&layout.coordinator_dir()).is_none());
}

#[test]
fn coordinator_inputs_go_together_over_https_and_never_with_the_stand_in() {
    let whole = recorded(&installed());
    let build = |inputs: Inputs| Desired::new(Layout::default(), inputs, binary(), None);
    build(whole.clone()).unwrap();
    let partial: [fn(&mut Inputs); 5] = [
        |i| i.coordinator_url.clear(),
        |i| i.coordinator_project.clear(),
        |i| i.coordinator_credential = false,
        |i| i.coordinator_cli_sha256.clear(),
        |i| i.coordinator_fixture.clear(),
    ];
    for strip in partial {
        let mut inputs = whole.clone();
        strip(&mut inputs);
        let error = format!("{:#}", build(inputs).unwrap_err());
        assert!(error.contains("go together"), "{error}");
    }
    let plain_http = Inputs {
        coordinator_url: "http://coordinator.example.test".into(),
        ..whole.clone()
    };
    let error = format!("{:#}", build(plain_http).unwrap_err());
    assert!(error.contains("https://"), "{error}");
    let with_stand_in = Inputs {
        stand_in_fixture: vec![r"C:\tools\fixture.exe".into()],
        ..whole.clone()
    };
    let error = format!("{:#}", build(with_stand_in).unwrap_err());
    assert!(error.contains("--no-stand-in"), "{error}");
    let off = only(coordinator::Overrides {
        off: true,
        ..coordinator::Overrides::default()
    });
    let cleared = whole.with(&off);
    assert!(!cleared.coordinator() && build(cleared.clone()).unwrap().cli.is_none());
    assert_eq!(
        cleared.manifest().unwrap().matches("coordinator").count(),
        0
    );
}

#[test]
fn status_reports_a_drifted_cli_and_only_a_cli_file_puts_it_right() {
    let mut host = installed();
    let layout = Layout::default();
    host.digests.insert(layout.cli(), "e".repeat(64));
    let report = status(&host);
    let wanted = format!(
        "has SHA-256 {}, not the recorded {}",
        "e".repeat(64),
        cli_digest()
    );
    assert!(
        report.refusals.iter().any(|r| r.contains(&wanted)),
        "{report:?}"
    );
    let before = host.clone();
    let refused = install(&mut host, &Overrides::default());
    assert!(
        !refused.clean() && refused.applied.is_empty(),
        "{refused:?}"
    );
    assert_eq!(host, before);
    let again = only(coordinator::Overrides {
        cli_source: Some(CLI.into()),
        ..coordinator::Overrides::default()
    });
    let repaired = install(&mut host, &again);
    assert_eq!(
        actions(&repaired.applied),
        [Action::InstallCli],
        "{repaired:?}"
    );
    assert!(status(&host).clean());
    host.digests.remove(&layout.cli());
    assert!(!status(&host).refusals.is_empty(), "a missing CLI is drift");
}

#[test]
fn status_reports_a_widened_access_list_and_install_sets_it_back() {
    let layout = Layout::default();
    for path in [layout.credential(), layout.coordinator_dir()] {
        let mut host = installed();
        let access = access_at(&mut host, &path);
        let wider = widened(access);
        *access = wider;
        let pending = actions(&status(&host).pending);
        let set = Action::SetAccessList { path: path.clone() };
        assert_eq!(pending, std::slice::from_ref(&set), "{path}");
        let report = install(&mut host, &Overrides::default());
        assert_eq!(actions(&report.applied), [set], "{report:?}");
        assert!(status(&host).clean(), "{path}");
    }
}

#[test]
fn a_missing_credential_needs_a_credential_file() {
    let mut host = installed();
    host.paths.remove(&Layout::default().credential());
    let report = status(&host);
    let missing = "no coordinator credential is installed";
    assert!(
        report.refusals.iter().any(|r| r.contains(missing)),
        "{report:?}"
    );
    let again = only(coordinator::Overrides {
        credential_source: Some(CREDENTIAL.into()),
        ..coordinator::Overrides::default()
    });
    let repaired = install(&mut host, &again);
    let done = actions(&repaired.applied);
    assert_eq!(done, [Action::InstallCredential], "{repaired:?}");
}

#[test]
fn files_that_cannot_be_used_are_refused_and_no_report_carries_the_credential() {
    let mut host = FakeHost::clean();
    let refused = install(&mut host, &first_run());
    assert!(
        refused.refusals[0].starts_with("the coordinator CLI file cannot be used"),
        "{refused:?}"
    );
    assert_eq!(host, FakeHost::clean());
    let mut host = clean();
    let denied = KeySource::Unreadable(format!("{CREDENTIAL}: access is denied"));
    host.sources.insert(CREDENTIAL.into(), denied);
    let before = host.clone();
    let refused = install(&mut host, &first_run());
    let unusable = "the coordinator credential file cannot be used";
    assert!(
        refused.refusals.iter().any(|r| r.starts_with(unusable)),
        "{refused:?}"
    );
    assert!(refused.applied.is_empty());
    assert_eq!(host, before);
    let mut host = clean();
    let report = serde_json::to_string(&install(&mut host, &first_run())).unwrap();
    assert!(report.contains("install_credential") && report.contains("never shown"));
    assert!(!report.contains("Downloads"), "{report}");
}

#[test]
fn the_coordinator_is_written_only_while_enabled() {
    let mut host = installed();
    let layout = Layout::default();
    let text = host.texts[&layout.config()].clone();
    let file: toml::Table = text.parse().unwrap();
    let table = file["windows_supervisor"]["coordinator"]
        .as_table()
        .unwrap();
    let keys: Vec<&str> = table.keys().map(String::as_str).collect();
    assert_eq!(keys, ["enabled", "fixture", "project_id", "service_url"]);
    let loaded = Settings::parse(&text).unwrap();
    loaded.validate().unwrap();
    assert!(loaded.supervisor.coordinator.enabled);
    assert_eq!(loaded.supervisor.coordinator.service_url, URL);
    let off = only(coordinator::Overrides {
        off: true,
        ..coordinator::Overrides::default()
    });
    let report = install(&mut host, &off);
    let done = actions(&report.applied);
    assert!(
        matches!(
            &done[..],
            [Action::WriteManifest, Action::WriteConfig { .. }]
        ),
        "{done:?}"
    );
    assert!(!host.texts[&layout.config()].contains("coordinator"));
    assert!(!host.texts[&layout.manifest()].contains("coordinator"));
}

#[test]
fn another_layout_writes_the_coordinators_paths_only_while_enabled() {
    let layout = Layout {
        program_dir: r"D:\test\program".into(),
        state_root: r"D:\test\state".into(),
        prefix: "agentc-t1".into(),
    };
    let inputs = recorded(&installed());
    let off = only(coordinator::Overrides {
        off: true,
        ..coordinator::Overrides::default()
    });
    let plain = Desired::new(layout.clone(), inputs.clone().with(&off), binary(), None);
    let plain = plain.unwrap().settings;
    let desired = Desired::new(layout, inputs, binary(), None).unwrap();
    let enabled = config_text::merged(None, &desired.settings).unwrap();
    for observed in [None, Some(enabled.as_str())] {
        let text = config_text::merged(observed, &plain).unwrap();
        assert!(!text.contains("coordinator"), "{text}");
    }
    let settings = &desired.settings.supervisor.coordinator;
    let path = std::path::PathBuf::from;
    assert_eq!(settings.cli, path(r"D:\test\program\agent-coordinator.exe"));
    assert_eq!(
        settings.directory,
        path(r"D:\test\state\supervisor\coordinator")
    );
    assert_eq!(
        settings.launch_root,
        path(r"D:\test\state\runner\fixture-launches")
    );
    assert_eq!(
        settings.ledger,
        path(r"D:\test\state\supervisor\ledger.jsonl")
    );
    assert_eq!(
        settings.credentials(),
        path(&desired.credential.unwrap().path)
    );
    let text = config_text::merged(None, &desired.settings).unwrap();
    assert_eq!(Settings::parse(&text).unwrap(), desired.settings);
    let file: toml::Table = text.parse().unwrap();
    let table = file["windows_supervisor"]["coordinator"]
        .as_table()
        .unwrap();
    for key in ["cli", "directory", "launch_root", "ledger"] {
        assert!(table.contains_key(key), "{key}: {table:?}");
    }
}

#[test]
fn uninstall_removes_the_cli_and_the_credential_and_notes_the_token() {
    let mut host = installed();
    let layout = Layout::default();
    let mut services = FakeServices::of(&host, &layout);
    let report = uninstall(&mut host, &mut services, &layout, SOURCE).unwrap();
    assert!(report.clean(), "{report:?}");
    assert!(!host.digests.contains_key(&layout.cli()));
    assert!(!host.paths.contains_key(&layout.credential()));
    assert!(!host.paths.contains_key(&layout.coordinator_dir()));
    let token = "its token stays valid";
    assert!(report.notes.iter().any(|n| n.contains(token)), "{report:?}");
}

#[test]
fn an_update_leaves_the_cli_and_credential_and_keeps_the_coordinator_configured() {
    let mut host = installed();
    let layout = Layout::default();
    let to = Release {
        sha256: "b".repeat(64),
        commit: "b".repeat(40),
    };
    host.digests.insert(SOURCE.into(), to.sha256.clone());
    let mut services = FakeServices::of(&host, &layout);
    services
        .states
        .values_mut()
        .for_each(|s| *s = RunState::Running);
    let request = Request {
        to: to.clone(),
        dirty: false,
        built_from: to.commit.clone(),
        retry: false,
    };
    let desired = Desired::new(layout.clone(), recorded(&host), to.sha256.clone(), None).unwrap();
    host.digests.insert(layout.cli(), "e".repeat(64));
    let mut run = Run {
        host: &mut host,
        services: &mut services,
        desired: &desired,
        source: SOURCE.into(),
    };
    let refused = update(&mut run, &request).unwrap();
    assert_eq!(refused.outcome, "refused", "{refused:?}");
    assert!(
        refused
            .refusals
            .iter()
            .any(|r| r.contains("coordinator CLI"))
    );
    run.host.copy(CLI, &layout.cli()).unwrap();
    let credential = run.host.path(&layout.credential()).unwrap();
    let report = update(&mut run, &request).unwrap();
    assert_eq!(report.outcome, "updated", "{report:?}");
    assert_eq!(host.digests[&layout.cli()], cli_digest());
    assert_eq!(host.paths[&layout.credential()], credential);
    let loaded = Settings::parse(&host.texts[&layout.config()]).unwrap();
    assert!(loaded.supervisor.coordinator.enabled);
}
