//! The installer's model, plan and apply loop against a host in memory.
//! These run on every platform.
use super::access::{AccessList, Entry, FULL, Kind};
use super::config_text::{self, Settings};
use super::fake::{FakeHost, other_start};
use super::install::{install, install_from, observe, status, status_from};
use super::journal::Journal;
use super::model::{Desired, Inputs, Layout, Overrides, RecoveryAction, StartType};
use super::observed::{KeySource, PathState};
use super::plan::{Action, Change, plan};

/// A user who is none of the system, administrators or a service.
const SOMEONE: &str = "S-1-5-21-1-2-3-1001";
/// Everyone.
const EVERYONE: &str = "S-1-1-0";

/// The owner's inputs for a host with a push App.
fn inputs() -> Inputs {
    Inputs {
        remote: "https://github.com/example/project.git".into(),
        app_id: 7,
        installation_id: 9,
        push_key: true,
        ..Inputs::default()
    }
}

/// The default layout's desired state for `inputs`, with a key file given.
fn desired_for(inputs: Inputs) -> Desired {
    let source = inputs
        .push_key
        .then(|| r"C:\Users\owner\key.pem".to_owned());
    Desired::new(Layout::default(), inputs, "ab".repeat(32), source).unwrap()
}

/// The desired state of the usual test host.
fn desired() -> Desired {
    desired_for(inputs())
}

/// A journal in a directory that exists.
fn journal(dir: &tempfile::TempDir) -> Journal {
    Journal::at(dir.path().join("journal.jsonl"))
}

/// A host on which `desired` was installed.
fn installed(desired: &Desired) -> FakeHost {
    let (mut host, dir) = (FakeHost::clean(), tempfile::tempdir().unwrap());
    let report = install(&mut host, desired, &mut journal(&dir)).unwrap();
    assert!(report.clean(), "{report:?}");
    host
}

/// The changes `host` needs to become `desired`.
fn changes(host: &FakeHost, desired: &Desired) -> Vec<Change> {
    plan(desired, &observe(host, desired).unwrap()).unwrap()
}

/// The actions `host` needs.
fn actions(host: &FakeHost, desired: &Desired) -> Vec<Action> {
    changes(host, desired)
        .into_iter()
        .map(|c| c.action)
        .collect()
}

/// The refusals `host` earns, as text.
fn refusals(host: &FakeHost, desired: &Desired) -> Vec<String> {
    let refused = plan(desired, &observe(host, desired).unwrap()).unwrap_err();
    refused.iter().map(ToString::to_string).collect()
}

/// A change to the service `name`.
fn on(name: &str, make: fn(String) -> Action) -> Vec<Action> {
    vec![make(name.to_owned())]
}

#[test]
fn a_clean_host_gets_the_whole_plan_in_order() {
    let desired = desired();
    let planned = actions(&FakeHost::clean(), &desired);
    let directories = desired.directories.len();
    assert_eq!(directories, 11);
    for (spec, action) in desired.directories.iter().zip(&planned) {
        assert_eq!(
            *action,
            Action::CreateDirectory {
                path: spec.path.clone()
            }
        );
    }
    let files = &planned[directories..directories + 4];
    assert_eq!(files[0], Action::InstallBinary);
    assert_eq!(files[1], Action::WriteManifest);
    assert!(matches!(files[2], Action::WriteConfig { .. }));
    assert_eq!(files[3], Action::InstallKey);
    let services = &planned[directories + 4..];
    assert_eq!(services.len(), 16, "{services:?}");
    for (spec, four) in desired.services.iter().zip(services.chunks(4)) {
        let service = spec.name.clone();
        assert_eq!(
            four[0],
            Action::CreateService {
                service: service.clone()
            }
        );
        assert_eq!(
            four[1],
            Action::SetRecovery {
                service: service.clone()
            }
        );
        assert_eq!(
            four[2],
            Action::SetPreshutdown {
                service: service.clone()
            }
        );
        assert_eq!(four[3], Action::SetPrivileges { service });
    }
}

#[test]
fn the_services_are_the_four_roles_stopped_under_their_own_accounts() {
    let desired = desired();
    let names: Vec<&str> = desired.services.iter().map(|s| s.name.as_str()).collect();
    let expected = [
        "agentc-egress",
        "agentc-publication",
        "agentc-runner",
        "agentc-supervisor",
    ];
    assert_eq!(names, expected);
    let runner = desired.service("agentc-runner").unwrap();
    assert_eq!(
        runner.bin_path,
        r#""C:\Program Files\agentc\agentc-supervisor.exe" --config "C:\Program Files\agentc\supervisor.toml" windows-service"#
    );
    assert_eq!(runner.account, r"NT SERVICE\agentc-runner");
    assert!(runner.dependencies.is_empty());
    let supervisor = desired.service("agentc-supervisor").unwrap();
    assert!(
        supervisor
            .bin_path
            .ends_with("windows-service --role supervisor")
    );
    assert_eq!(supervisor.dependencies, ["agentc-runner"]);
    for service in &desired.services {
        assert_eq!(service.start, StartType::Demand);
        assert_eq!(service.preshutdown_ms, 120_000);
        let restart = RecoveryAction::Restart(60_000);
        assert_eq!(
            service.recovery.actions,
            [restart, restart, RecoveryAction::None]
        );
        assert_eq!(service.recovery.reset_seconds, 86_400);
        assert_eq!(
            service.privileges,
            ["SeChangeNotifyPrivilege", "SeImpersonatePrivilege"]
        );
    }
}

#[test]
fn an_installed_host_needs_nothing_and_a_second_install_changes_nothing() {
    let desired = desired();
    let mut host = installed(&desired);
    assert_eq!(actions(&host, &desired), []);
    let before = host.clone();
    let dir = tempfile::tempdir().unwrap();
    let report = install(&mut host, &desired, &mut journal(&dir)).unwrap();
    assert!(report.clean() && report.applied.is_empty(), "{report:?}");
    assert_eq!(host, before);
    assert!(
        !dir.path().join("journal.jsonl").exists(),
        "nothing to journal"
    );
}

#[test]
fn each_drifted_service_setting_gets_exactly_its_repair() {
    let desired = desired();
    let name = "agentc-supervisor";
    type Drift = fn(&mut FakeHost);
    let cases: [(Drift, Vec<Action>); 8] = [
        (
            |h| h.spec("agentc-supervisor").bin_path.push_str(" --extra"),
            on(name, |service| Action::SetBinPath { service }),
        ),
        (
            |h| {
                let s = h.spec("agentc-supervisor");
                s.start = other_start(s.start)
            },
            on(name, |service| Action::SetStartType { service }),
        ),
        (
            |h| h.spec("agentc-supervisor").dependencies.clear(),
            on(name, |service| Action::SetDependencies { service }),
        ),
        (
            |h| {
                h.spec("agentc-supervisor")
                    .recovery
                    .actions
                    .pop()
                    .map(drop)
                    .unwrap()
            },
            on(name, |service| Action::SetRecovery { service }),
        ),
        (
            |h| h.spec("agentc-supervisor").recovery.command = "calc.exe".into(),
            on(name, |service| Action::SetRecovery { service }),
        ),
        (
            |h| h.spec("agentc-supervisor").preshutdown_ms = 10_000,
            on(name, |service| Action::SetPreshutdown { service }),
        ),
        (
            |h| {
                h.spec("agentc-supervisor")
                    .privileges
                    .push("SeDebugPrivilege".into())
            },
            on(name, |service| Action::SetPrivileges { service }),
        ),
        (
            |h| h.spec("agentc-supervisor").account = "LocalSystem".into(),
            on(name, |service| Action::SetAccount { service }),
        ),
    ];
    for (drift, repair) in cases {
        let mut host = installed(&desired);
        drift(&mut host);
        assert_eq!(actions(&host, &desired), repair);
        repaired(host, &desired);
    }
}

/// Installs again on a drifted `host` and checks nothing is left to do.
fn repaired(mut host: FakeHost, desired: &Desired) {
    let dir = tempfile::tempdir().unwrap();
    let report = install(&mut host, desired, &mut journal(&dir)).unwrap();
    assert!(report.clean(), "{report:?}");
    assert_eq!(report.applied.len(), 1, "{report:?}");
    assert_eq!(actions(&host, desired), []);
}

#[test]
fn a_missing_directory_is_made_again() {
    let desired = desired();
    let mut host = installed(&desired);
    let path = desired.layout.state(r"stand-in\queue");
    host.paths.remove(&path);
    assert_eq!(actions(&host, &desired), [Action::CreateDirectory { path }]);
    repaired(host, &desired);
}

/// `access` with a grant of full control to everyone added.
fn widened(access: &AccessList) -> AccessList {
    let mut wide = access.clone();
    let inheritance = wide.entries.first().map_or(0, |e| e.inheritance);
    wide.entries.insert(Entry {
        kind: Kind::Allow,
        sid: EVERYONE.into(),
        mask: FULL,
        inheritance,
        inherited: false,
    });
    wide
}

#[test]
fn a_widened_access_list_is_set_back() {
    let desired = desired();
    let path = desired.layout.state("publication");
    let mut host = installed(&desired);
    let PathState::Directory { access, .. } = host.paths.get_mut(&path).unwrap() else {
        panic!("not a directory");
    };
    *access = widened(access);
    assert_eq!(actions(&host, &desired), [Action::SetAccessList { path }]);
    repaired(host, &desired);
}

#[test]
fn a_list_that_inherits_or_orders_differently_is_judged_by_its_entries() {
    let desired = desired();
    let path = desired.layout.state_root.clone();
    let mut host = installed(&desired);
    let PathState::Directory { access, .. } = host.paths.get_mut(&path).unwrap() else {
        panic!("not a directory");
    };
    access.protected = false;
    assert_eq!(actions(&host, &desired), [Action::SetAccessList { path }]);
}

#[test]
fn a_key_whose_access_list_was_widened_is_set_back_without_a_key_file() {
    let desired = desired();
    let mut host = installed(&desired);
    let again = Desired::new(Layout::default(), inputs(), "ab".repeat(32), None).unwrap();
    assert_eq!(
        actions(&host, &again),
        [],
        "no key file is needed once the key is installed"
    );
    let path = desired.layout.key();
    let PathState::File { access, .. } = host.paths.get_mut(&path).unwrap() else {
        panic!("not a file");
    };
    *access = widened(access);
    assert_eq!(actions(&host, &again), [Action::SetAccessList { path }]);
    repaired(host, &again);
}

#[test]
fn a_different_key_file_replaces_the_installed_key() {
    let desired = desired();
    let mut host = installed(&desired);
    host.key_source = KeySource::Readable { installed: false };
    assert_eq!(actions(&host, &desired), [Action::InstallKey]);
}

#[test]
fn a_changed_configuration_entry_is_named_and_put_back() {
    let desired = desired();
    let mut host = installed(&desired);
    let path = desired.layout.config();
    let text = host.texts[&path].replace("example/project", "example/other");
    host.texts.insert(path, text);
    let entries = vec!["windows_publication.remote".to_owned()];
    assert_eq!(actions(&host, &desired), [Action::WriteConfig { entries }]);
    repaired(host, &desired);
}

#[test]
fn a_removed_configuration_entry_is_put_back() {
    let desired = desired();
    let mut host = installed(&desired);
    let path = desired.layout.config();
    let kept = |line: &&str| !line.starts_with("client_sid");
    let text: Vec<&str> = host.texts[&path].lines().filter(kept).collect();
    host.texts.insert(path, text.join("\n"));
    let entries = vec!["windows_service.client_sid".to_owned()];
    assert_eq!(actions(&host, &desired), [Action::WriteConfig { entries }]);
}

#[test]
fn the_owners_own_entries_survive_a_rewrite() {
    let desired = desired();
    let mut host = installed(&desired);
    let path = desired.layout.config();
    host.texts
        .insert(path.clone(), owner_edited(&host.texts[&path]));
    repaired(host.clone(), &desired);
    let dir = tempfile::tempdir().unwrap();
    install(&mut host, &desired, &mut journal(&dir)).unwrap();
    let written: toml::Table = host.texts[&path].parse().unwrap();
    assert_eq!(written["run"]["poll_seconds"].as_integer(), Some(10));
    assert_eq!(
        written["windows_service"]["stop_grace_seconds"].as_integer(),
        Some(45)
    );
    let remote = written["windows_publication"]["remote"].as_str();
    assert_eq!(remote, Some("https://github.com/example/project.git"));
}

/// An installed configuration after the owner tuned two unmanaged entries
/// and changed a managed one.
fn owner_edited(text: &str) -> String {
    let mut file: toml::Table = text.parse().unwrap();
    let mut run = toml::Table::new();
    run.insert("poll_seconds".into(), 10.into());
    file.insert("run".into(), run.into());
    let service = file["windows_service"].as_table_mut().unwrap();
    service.insert("stop_grace_seconds".into(), 45.into());
    let publication = file["windows_publication"].as_table_mut().unwrap();
    publication.insert(
        "remote".into(),
        "https://github.com/example/other.git".into(),
    );
    toml::to_string(&file).unwrap()
}

#[test]
fn the_generated_configuration_loads_and_carries_only_what_has_no_default() {
    let desired = desired();
    let text = config_text::merged(None, &desired.settings).unwrap();
    let loaded = Settings::parse(&text).unwrap();
    assert_eq!(loaded, desired.settings);
    loaded.validate().unwrap();
    let file: toml::Table = text.parse().unwrap();
    let keys =
        |table: &str| -> Vec<String> { file[table].as_table().unwrap().keys().cloned().collect() };
    assert_eq!(
        keys("windows_service"),
        [
            "client_sid",
            "egress_broker_sid",
            "egress_control_pipe",
            "publication_broker_sid",
            "publication_control_pipe"
        ]
    );
    assert_eq!(
        keys("windows_supervisor"),
        ["publication_broker_sid", "runner_sid"]
    );
    assert_eq!(keys("windows_egress"), ["runner_sid"]);
    assert_eq!(
        keys("windows_publication"),
        [
            "app_id",
            "installation_id",
            "remote",
            "runner_sid",
            "supervisor_sid"
        ]
    );
    assert_eq!(file.len(), 4);
}

#[test]
fn each_service_is_configured_with_the_others_sids_and_nobody_elses() {
    let settings = desired().settings;
    let sid = super::service_sid::service_sid;
    let (egress, publication) = (sid("agentc-egress"), sid("agentc-publication"));
    let (runner, supervisor) = (sid("agentc-runner"), sid("agentc-supervisor"));
    assert_eq!(settings.runner.client_sid, supervisor);
    assert_eq!(settings.runner.egress_broker_sid, egress);
    assert_eq!(settings.runner.publication_broker_sid, publication);
    assert_eq!(settings.supervisor.runner_sid, runner);
    assert_eq!(settings.supervisor.publication_broker_sid, publication);
    assert_eq!(settings.egress.runner_sid, runner);
    assert_eq!(settings.publication.runner_sid, runner);
    assert_eq!(settings.publication.supervisor_sid, supervisor);
}

#[test]
fn the_stand_in_is_written_only_when_a_fixture_is_given() {
    let fixture = vec![r"C:\tools\fixture.exe".to_owned(), "brokers".to_owned()];
    let with = desired_for(Inputs {
        stand_in_fixture: fixture.clone(),
        ..inputs()
    });
    assert!(with.settings.supervisor.stand_in.enabled);
    let text = config_text::merged(None, &with.settings).unwrap();
    let file: toml::Table = text.parse().unwrap();
    let stand_in = file["windows_supervisor"]["stand_in"].as_table().unwrap();
    assert_eq!(stand_in.len(), 2, "{stand_in:?}");
    assert_eq!(stand_in["enabled"].as_bool(), Some(true));
    let mut host = installed(&with);
    let without = desired();
    let changed = actions(&host, &without);
    assert!(
        matches!(
            &changed[..],
            [Action::WriteManifest, Action::WriteConfig { .. }]
        ),
        "{changed:?}"
    );
    let dir = tempfile::tempdir().unwrap();
    install(&mut host, &without, &mut journal(&dir)).unwrap();
    assert!(!host.texts[&without.layout.config()].contains("stand_in"));
}

#[test]
fn another_layout_writes_its_names_directories_and_pipes() {
    let layout = Layout {
        program_dir: r"D:\test\program".into(),
        state_root: r"D:\test\state".into(),
        prefix: "agentc-t1".into(),
    };
    let desired = Desired::new(layout, inputs(), "ab".repeat(32), None).unwrap();
    let settings = &desired.settings;
    assert_eq!(settings.runner.name, "agentc-t1-runner");
    assert_eq!(settings.runner.pipe_name, "agentc-t1-runner");
    assert_eq!(settings.supervisor.runner_pipe, "agentc-t1-runner");
    assert_eq!(
        settings.egress.control_pipe,
        settings.runner.egress_control_pipe
    );
    assert_eq!(
        settings.publication.authority_pipe,
        settings.supervisor.authority_pipe
    );
    let key = std::path::PathBuf::from(r"D:\test\state\publication-key\push-app.pem");
    assert_eq!(settings.publication.private_key, key);
    assert_eq!(settings.egress.must_not_open, [key]);
    let text = config_text::merged(None, settings).unwrap();
    assert_eq!(Settings::parse(&text).unwrap(), *settings);
    assert!(
        desired.services[0]
            .bin_path
            .starts_with(r#""D:\test\program\agentc-supervisor.exe""#)
    );
}

#[test]
fn a_session_that_is_not_elevated_is_refused_and_nothing_is_read() {
    let desired = desired();
    let mut host = installed(&desired);
    host.elevated = false;
    let seen = observe(&host, &desired).unwrap();
    assert!(seen.paths.is_empty() && seen.services.is_empty() && seen.config.is_none());
    assert_eq!(refusals(&host, &desired).len(), 1);
    assert!(refusals(&host, &desired)[0].contains("not elevated"));
    assert!(status(&host, &desired).unwrap().tokens.is_empty());
}

/// Installs on `host`, which must be refused with nothing changed, and
/// returns the refusals.
fn refused(mut host: FakeHost, desired: &Desired) -> Vec<String> {
    let (before, dir) = (host.clone(), tempfile::tempdir().unwrap());
    let report = install(&mut host, desired, &mut journal(&dir)).unwrap();
    assert!(!report.clean() && report.applied.is_empty(), "{report:?}");
    assert_eq!(host, before, "a refused host must not change");
    assert!(!dir.path().join("journal.jsonl").exists());
    assert_eq!(status(&host, desired).unwrap().refusals, report.refusals);
    report.refusals
}

#[test]
fn a_root_that_is_a_link_is_refused_with_nothing_changed() {
    let desired = desired();
    for root in [&desired.layout.state_root, &desired.layout.program_dir] {
        let mut host = FakeHost::clean();
        host.paths.insert(root.clone(), PathState::Link);
        let said = refused(host, &desired);
        assert_eq!(said, [format!("{root} is a link or reparse point")]);
    }
}

#[test]
fn nothing_below_a_refused_directory_is_looked_at() {
    let desired = desired();
    let mut host = installed(&desired);
    host.paths
        .insert(desired.layout.state_root.clone(), PathState::Link);
    let seen = observe(&host, &desired).unwrap();
    let below = |path: &&String| path.starts_with(&format!(r"{}\", desired.layout.state_root));
    assert_eq!(seen.paths.keys().filter(below).count(), 0);
    assert_eq!(seen.manifest, None);
    assert_eq!(refusals(&host, &desired).len(), 1);
}

#[test]
fn a_directory_someone_else_owns_is_refused_with_nothing_changed() {
    let desired = desired();
    for path in [
        desired.layout.state_root.clone(),
        desired.layout.program_dir.clone(),
        desired.layout.state("runner"),
    ] {
        let mut host = installed(&desired);
        let PathState::Directory { owner, .. } = host.paths.get_mut(&path).unwrap() else {
            panic!("not a directory");
        };
        *owner = SOMEONE.into();
        let said = refused(host, &desired);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0].starts_with(&format!("{path} is owned by {SOMEONE}")),
            "{said:?}"
        );
    }
}

#[test]
fn a_service_may_own_its_own_directory_and_no_other() {
    let desired = desired();
    let sid = super::service_sid::service_sid;
    let mut host = installed(&desired);
    let own = |host: &mut FakeHost, rest: &str, sid: String| {
        let path = desired.layout.state(rest);
        let PathState::Directory { owner, .. } = host.paths.get_mut(&path).unwrap() else {
            panic!("not a directory");
        };
        *owner = sid;
    };
    own(&mut host, "egress", sid("agentc-egress"));
    assert_eq!(actions(&host, &desired), []);
    own(&mut host, "publication", sid("agentc-egress"));
    assert_eq!(refusals(&host, &desired).len(), 1);
}

#[test]
fn a_file_where_a_directory_belongs_is_refused() {
    let desired = desired();
    let mut host = FakeHost::clean();
    let access = AccessList::file(&[]);
    let file = PathState::File {
        owner: SOMEONE.into(),
        access,
    };
    host.paths.insert(desired.layout.state_root.clone(), file);
    let said = refused(host, &desired);
    assert!(said[0].ends_with("is not a directory"), "{said:?}");
}

#[test]
fn a_key_file_that_cannot_be_read_is_refused_with_nothing_changed() {
    let desired = desired();
    let mut host = FakeHost::clean();
    host.key_source = KeySource::Unreadable(r"C:\Users\owner\key.pem: access is denied".into());
    let said = refused(host, &desired);
    assert_eq!(
        said,
        [r"the key file cannot be used: C:\Users\owner\key.pem: access is denied"]
    );
    let mut installed_host = installed(&desired);
    installed_host.key_source = KeySource::Unreadable("gone".into());
    assert_eq!(
        refused(installed_host, &desired).len(),
        1,
        "even with a key installed"
    );
}

#[test]
fn a_wanted_key_with_no_key_file_is_refused_on_a_clean_host() {
    let without = Desired::new(Layout::default(), inputs(), "ab".repeat(32), None).unwrap();
    let said = refused(FakeHost::clean(), &without);
    assert!(said[0].contains("no --key-source was given"), "{said:?}");
}

#[test]
fn a_configuration_that_does_not_load_is_left_alone() {
    let desired = desired();
    let mut host = installed(&desired);
    host.texts.insert(
        desired.layout.config(),
        "[windows_service]\nclient_sidd = 1\n".into(),
    );
    let said = refused(host, &desired);
    assert!(
        said[0].starts_with("the configuration cannot be loaded"),
        "{said:?}"
    );
}

#[test]
fn a_different_binary_is_not_put_under_running_services() {
    let desired = desired();
    let mut host = installed(&desired);
    host.binary = Some("cd".repeat(32));
    assert_eq!(
        actions(&host, &desired),
        [Action::InstallBinary],
        "all stopped: replaced"
    );
    host.services.get_mut("agentc-runner").unwrap().process = Some(41);
    let said = refused(host, &desired);
    assert!(said[0].contains("agentc-runner run from it"), "{said:?}");
}

#[test]
fn a_failure_part_way_is_journaled_and_the_next_run_completes_it() {
    let desired = desired();
    let dir = tempfile::tempdir().unwrap();
    let mut host = FakeHost::clean();
    host.fail_at = Some(13);
    let failed = install(&mut host, &desired, &mut journal(&dir)).unwrap_err();
    let failed = format!("{failed:#}");
    assert!(
        failed.contains("failed after 13 changes were applied"),
        "{failed}"
    );
    assert_eq!(host.applied, 13);
    let stages = stages(&dir);
    assert_eq!(stages.iter().filter(|s| *s == "applied").count(), 13);
    assert_eq!(stages.last().map(String::as_str), Some("failed"));
    let left = actions(&host, &desired).len();
    let report = install(&mut host, &desired, &mut journal(&dir)).unwrap();
    assert!(report.clean(), "{report:?}");
    assert_eq!(report.applied.len(), left);
    assert_eq!(left + 13, actions(&FakeHost::clean(), &desired).len());
    assert_eq!(actions(&host, &desired), []);
}

/// The stage of every line of the journal in `dir`.
fn stages(dir: &tempfile::TempDir) -> Vec<String> {
    let text = std::fs::read_to_string(dir.path().join("journal.jsonl")).unwrap();
    let stage = |line: &str| -> String {
        let line: serde_json::Value = serde_json::from_str(line).unwrap();
        line["stage"].as_str().unwrap().to_owned()
    };
    text.lines().map(stage).collect()
}

#[test]
fn a_change_that_does_not_take_effect_ends_the_run() {
    /// A host whose binary never changes.
    struct Stuck(FakeHost);
    impl super::install::Host for Stuck {
        fn elevated(&self) -> anyhow::Result<bool> {
            self.0.elevated()
        }
        fn path(&self, path: &str) -> anyhow::Result<PathState> {
            self.0.path(path)
        }
        fn text(&self, path: &str) -> anyhow::Result<Option<String>> {
            self.0.text(path)
        }
        fn sha256(&self, _: &str) -> anyhow::Result<Option<String>> {
            Ok(None)
        }
        fn service(&self, name: &str) -> anyhow::Result<Option<super::observed::ServiceState>> {
            self.0.service(name)
        }
        fn key_source(&self, source: &str, installed: Option<&str>) -> KeySource {
            self.0.key_source(source, installed)
        }
        fn privileges(&self, process: u32) -> anyhow::Result<Vec<String>> {
            self.0.privileges(process)
        }
        fn apply(&mut self, desired: &Desired, action: &Action) -> anyhow::Result<()> {
            self.0.apply(desired, action)
        }
    }
    let (desired, dir) = (desired(), tempfile::tempdir().unwrap());
    let error = install(&mut Stuck(FakeHost::clean()), &desired, &mut journal(&dir)).unwrap_err();
    assert!(
        format!("{error:#}").contains("the host still needs it"),
        "{error:#}"
    );
}

#[test]
fn a_stopped_service_is_not_inventoried_and_a_running_one_is_compared() {
    let desired = desired();
    let mut host = installed(&desired);
    let report = status(&host, &desired).unwrap();
    assert!(
        report.tokens.iter().all(|t| t.state == "stopped"),
        "{report:?}"
    );
    assert!(
        report.clean(),
        "a stopped service is no finding, and no pass either"
    );
    host.services.get_mut("agentc-egress").unwrap().process = Some(7);
    host.services.get_mut("agentc-runner").unwrap().process = Some(8);
    host.services.get_mut("agentc-supervisor").unwrap().process = Some(9);
    let may = ["SeChangeNotifyPrivilege", "SeImpersonatePrivilege"].map(String::from);
    host.tokens.insert(7, may.to_vec());
    host.tokens
        .insert(8, vec![may[0].clone(), "SeCreateGlobalPrivilege".into()]);
    let report = status(&host, &desired).unwrap();
    assert!(!report.clean() && report.pending.is_empty());
    let by_name = |name: &str| report.tokens.iter().find(|t| t.service == name).unwrap();
    assert_eq!(by_name("agentc-egress").state, "inventoried");
    assert!(by_name("agentc-egress").unexpected.is_empty());
    assert_eq!(
        by_name("agentc-runner").unexpected,
        ["SeCreateGlobalPrivilege"]
    );
    assert_eq!(by_name("agentc-runner").missing, ["SeImpersonatePrivilege"]);
    assert_eq!(by_name("agentc-supervisor").state, "not_inventoried");
    assert_eq!(by_name("agentc-publication").state, "stopped");
}

#[test]
fn status_changes_nothing_and_lists_what_install_would_do() {
    let desired = desired();
    let host = FakeHost::clean();
    let report = status(&host, &desired).unwrap();
    assert_eq!(host, FakeHost::clean());
    assert!(!report.clean() && report.applied.is_empty());
    assert_eq!(report.pending, changes(&host, &desired));
    assert!(report.tokens.iter().all(|t| t.state == "not_registered"));
}

#[test]
fn inputs_and_layouts_that_no_host_could_run_are_refused() {
    let build = |inputs: Inputs| Desired::new(Layout::default(), inputs, String::new(), None);
    assert!(build(Inputs::default()).is_err(), "no remote");
    assert!(
        build(Inputs {
            push_key: false,
            ..inputs()
        })
        .is_err(),
        "an App without its key"
    );
    assert!(
        build(Inputs {
            app_id: 0,
            installation_id: 0,
            ..inputs()
        })
        .is_err()
    );
    assert!(
        build(Inputs {
            start: StartType::Disabled,
            ..inputs()
        })
        .is_err()
    );
    let plain = Inputs {
        app_id: 0,
        installation_id: 0,
        push_key: false,
        ..inputs()
    };
    assert!(build(plain.clone()).unwrap().key.is_none());
    for bad in [
        r"relative\dir",
        r"C:\a\..\b",
        r"\\server\share",
        r#"C:\a"b"#,
        "C:/a",
        r"C:\a\",
    ] {
        let layout = Layout {
            state_root: bad.into(),
            ..Layout::default()
        };
        assert!(
            Desired::new(layout, plain.clone(), String::new(), None).is_err(),
            "{bad}"
        );
    }
    let layout = Layout {
        prefix: "agentc x".into(),
        ..Layout::default()
    };
    assert!(Desired::new(layout, plain, String::new(), None).is_err());
}

#[test]
fn the_manifest_round_trips_and_refuses_what_it_does_not_know() {
    let text = inputs().manifest().unwrap();
    assert_eq!(Inputs::from_manifest(&text).unwrap(), inputs());
    assert!(Inputs::from_manifest(r#"{"remote":"x","surprise":1}"#).is_err());
}

#[test]
fn no_report_or_change_carries_anything_of_the_key() {
    // The model holds a path to the key file and never its bytes, so no
    // value the installer prints can carry them. The native test scans the
    // real command's output, journal and files for a synthetic key.
    let desired = desired();
    let host = FakeHost::clean();
    let report = serde_json::to_string(&status(&host, &desired).unwrap()).unwrap();
    assert!(
        report.contains("install_key") && report.contains("never shown"),
        "{report}"
    );
    assert!(!report.contains("key.pem"), "{report}");
}

/// What a first install is told on its command line.
fn first_run() -> Overrides {
    Overrides {
        remote: Some("https://github.com/example/project.git".into()),
        app: Some((7, 9)),
        key_source: Some(r"C:\Users\owner\key.pem".into()),
        ..Overrides::default()
    }
}

#[test]
fn a_later_run_needs_no_arguments_and_status_reads_the_manifest() {
    let (mut host, dir) = (FakeHost::clean(), tempfile::tempdir().unwrap());
    let digest = || "ab".repeat(32);
    let none = status_from(&host, Layout::default(), digest()).unwrap();
    assert!(
        none.refusals[0].starts_with("nothing is installed"),
        "{none:?}"
    );
    let first = install_from(
        &mut host,
        Layout::default(),
        &first_run(),
        digest(),
        &mut journal(&dir),
    );
    assert!(first.unwrap().clean());
    assert_eq!(
        Inputs::from_manifest(&host.texts[&Layout::default().manifest()]).unwrap(),
        inputs()
    );
    assert!(
        status_from(&host, Layout::default(), digest())
            .unwrap()
            .clean()
    );
    let before = host.clone();
    let again = install_from(
        &mut host,
        Layout::default(),
        &Overrides::default(),
        digest(),
        &mut journal(&dir),
    );
    let again = again.unwrap();
    assert!(again.clean() && again.applied.is_empty(), "{again:?}");
    assert_eq!(host, before);
}

#[test]
fn an_argument_changes_only_what_it_names() {
    let (mut host, dir) = (FakeHost::clean(), tempfile::tempdir().unwrap());
    let digest = || "ab".repeat(32);
    install_from(
        &mut host,
        Layout::default(),
        &first_run(),
        digest(),
        &mut journal(&dir),
    )
    .unwrap();
    let auto = Overrides {
        start: Some(StartType::Auto),
        ..Overrides::default()
    };
    let report = install_from(
        &mut host,
        Layout::default(),
        &auto,
        digest(),
        &mut journal(&dir),
    )
    .unwrap();
    let done: Vec<&Action> = report.applied.iter().map(|c| &c.action).collect();
    assert_eq!(done.len(), 5, "{done:?}");
    assert_eq!(*done[0], Action::WriteManifest);
    assert!(
        done[1..]
            .iter()
            .all(|a| matches!(a, Action::SetStartType { .. })),
        "{done:?}"
    );
    let recorded = Inputs::from_manifest(&host.texts[&Layout::default().manifest()]).unwrap();
    assert_eq!(
        recorded,
        Inputs {
            start: StartType::Auto,
            ..inputs()
        }
    );
}

#[test]
fn the_manifest_is_not_read_without_elevation_or_through_a_link() {
    let (mut host, dir) = (FakeHost::clean(), tempfile::tempdir().unwrap());
    let digest = || "ab".repeat(32);
    install_from(
        &mut host,
        Layout::default(),
        &first_run(),
        digest(),
        &mut journal(&dir),
    )
    .unwrap();
    let mut plain = host.clone();
    plain.elevated = false;
    let refused = status_from(&plain, Layout::default(), digest()).unwrap();
    assert!(refused.refusals[0].contains("not elevated"), "{refused:?}");
    host.paths
        .insert(Layout::default().installer_dir(), PathState::Link);
    let before = host.clone();
    let refused = install_from(
        &mut host,
        Layout::default(),
        &Overrides::default(),
        digest(),
        &mut journal(&dir),
    );
    let refused = refused.unwrap();
    assert!(
        refused.refusals[0].ends_with("is a link or reparse point"),
        "{refused:?}"
    );
    assert_eq!(host, before);
}

#[test]
fn a_first_install_without_a_remote_is_an_error_that_changes_nothing() {
    let (mut host, dir) = (FakeHost::clean(), tempfile::tempdir().unwrap());
    let error = install_from(
        &mut host,
        Layout::default(),
        &Overrides::default(),
        String::new(),
        &mut journal(&dir),
    );
    assert!(format!("{:#}", error.unwrap_err()).contains("--remote"));
    assert_eq!(host, FakeHost::clean());
}

#[test]
fn the_installation_chapter_shows_the_file_a_default_install_writes() {
    let chapter = include_str!("../../../../book/src/docs/windows-installation.md");
    let start = chapter.find("```toml").expect("a TOML sample") + "```toml".len();
    let sample = &chapter[start..start + chapter[start..].find("```").unwrap()];
    let shown: toml::Table = sample.parse().unwrap();
    let documented = Inputs {
        remote: "https://github.com/OWNER/REPO.git".into(),
        app_id: 123_456,
        installation_id: 7_654_321,
        push_key: true,
        ..Inputs::default()
    };
    let written = config_text::merged(None, &desired_for(documented).settings).unwrap();
    assert_eq!(
        written.lines().take_while(|l| l.starts_with('#')).count(),
        3
    );
    assert_eq!(shown, written.parse::<toml::Table>().unwrap());
}
