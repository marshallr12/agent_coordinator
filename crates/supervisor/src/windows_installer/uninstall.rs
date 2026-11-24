//! `windows-host uninstall` (phase 7, step 7b): stops and deletes the four
//! services, removes both trees, and removes what Windows kept for the
//! services' accounts.
//!
//! Every step looks at the host as it is, so a second run, or one after a
//! restart, does only what is left. Nothing is removed through a link and
//! no ownership is taken: what cannot be removed is reported. A tree that
//! holds anything the installer did not make there is refused before
//! anything changes, so a mistaken `--program-dir` removes nothing.
//!
//! The AppContainer profiles the launches made belong to the runner's
//! account, which made them, and live in its service profile; they go with
//! that profile. Windows keeps a service account's profile loaded after the
//! service stopped, often until a restart: such a profile is reported as
//! pending, and the next run removes it. That run needs no record: the four
//! accounts follow from the service names. WSL is never touched.
use super::control::{self, RunState, STOP_ORDER, Services};
use super::install::Machine;
use super::model::{BINARY, CONFIG, Inputs, Layout, Role, Sids};
use super::observed::{PathState, ProfileState};
use super::plan;
use anyhow::{Context, Result};
use serde::Serialize;

/// What the program directory may hold: the binary, the configuration, the
/// kept releases and a file an interrupted write left beside its place.
const PROGRAM_ENTRIES: [&str; 3] = [BINARY, CONFIG, "releases"];
/// What the state root may hold: the installer's and each service's
/// directories, and the key's.
const STATE_ENTRIES: [&str; 7] = [
    "installer",
    "runner",
    "supervisor",
    "stand-in",
    "egress",
    "publication",
    "publication-key",
];

/// What `uninstall` found and did.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct UninstallReport {
    /// Why nothing was changed.
    pub refusals: Vec<String>,
    /// What this run removed, in order.
    pub removed: Vec<String>,
    /// What is left for a later run, and why.
    pub pending: Vec<String>,
    /// Paths that could not be removed, with why.
    pub left: Vec<String>,
    /// The AppContainer profiles the runner's journal names, which go with
    /// the runner's service profile.
    pub launch_profiles: Vec<String>,
    /// What the owner must do elsewhere.
    pub notes: Vec<String>,
}

impl UninstallReport {
    /// Whether nothing of the host is left.
    pub fn clean(&self) -> bool {
        self.refusals.is_empty() && self.pending.is_empty() && self.left.is_empty()
    }
}

/// Removes the host laid out as `layout`. `source` is the running binary,
/// which may not be inside the program directory it removes.
pub fn uninstall(
    host: &mut dyn Machine,
    services: &mut dyn Services,
    layout: &Layout,
    source: &str,
) -> Result<UninstallReport> {
    let refusals = refusals(&*host, layout, source)?;
    if !refusals.is_empty() {
        return Ok(UninstallReport {
            refusals,
            ..UninstallReport::default()
        });
    }
    let mut report = UninstallReport {
        launch_profiles: launch_profiles(&*host, layout)?,
        notes: key_note(&*host, layout)?,
        ..UninstallReport::default()
    };
    remove_services(host, services, layout, &mut report)?;
    for root in [&layout.program_dir, &layout.state_root] {
        remove_root(host, root, &mut report)?;
    }
    remove_profiles(host, layout, &mut report)?;
    Ok(report)
}

/// Every reason to leave the host alone.
fn refusals(host: &dyn Machine, layout: &Layout, source: &str) -> Result<Vec<String>> {
    if !host.elevated()? {
        return Ok(vec![plan::Refusal::NotElevated.to_string()]);
    }
    let mut found: Vec<String> = super::update::source_refusal(layout, source)
        .into_iter()
        .collect();
    found.extend(tree_refusal(host, &layout.program_dir, &PROGRAM_ENTRIES)?);
    found.extend(tree_refusal(host, &layout.state_root, &STATE_ENTRIES)?);
    Ok(found)
}

/// Why the tree at `root` may not be removed: a link, a file, a directory
/// someone else owns, or one that holds what the installer did not make.
fn tree_refusal(host: &dyn Machine, root: &str, allowed: &[&str]) -> Result<Vec<String>> {
    let state = host.path(root)?;
    if let Some(refusal) = plan::directory_refusal(root, &[], &state) {
        return Ok(vec![refusal.to_string()]);
    }
    if state == PathState::Missing {
        return Ok(Vec::new());
    }
    let ours = |name: &String| {
        let base = name.strip_suffix(".new").unwrap_or(name);
        allowed.iter().any(|a| a.eq_ignore_ascii_case(base))
    };
    let foreign: Vec<String> = host
        .entries(root)?
        .into_iter()
        .filter(|n| !ours(n))
        .collect();
    Ok(match foreign.is_empty() {
        true => Vec::new(),
        false => vec![format!(
            "{root} holds what the installer did not make there ({}); remove or move it first",
            foreign.join(", ")
        )],
    })
}

/// The AppContainer profiles the runner's journal names: one entry per
/// launch, each naming its profile and a reviewer's candidate profile.
fn launch_profiles(host: &dyn Machine, layout: &Layout) -> Result<Vec<String>> {
    let runner = layout.state(Role::Runner.suffix());
    if !matches!(host.path(&runner)?, PathState::Directory { .. }) {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in host.entries(&runner)? {
        let Some(text) = entry
            .ends_with(".json")
            .then(|| host.text(&format!(r"{runner}\{entry}")))
        else {
            continue;
        };
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text?.unwrap_or_default()) {
            collect_profiles(&value, &mut names);
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// Every string a `profile` field holds anywhere in `value`.
fn collect_profiles(value: &serde_json::Value, names: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, inner) in map {
                match (key.as_str(), inner) {
                    ("profile", serde_json::Value::String(name)) => names.push(name.clone()),
                    _ => collect_profiles(inner, names),
                }
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|i| collect_profiles(i, names)),
        _ => {}
    }
}

/// The note about the push App's key, when the host had one: removing the
/// file does not remove the key from the App.
fn key_note(host: &dyn Machine, layout: &Layout) -> Result<Vec<String>> {
    let manifest = match host.path(&layout.installer_dir())? {
        PathState::Directory { .. } => host.text(&layout.manifest())?,
        _ => None,
    };
    let inputs = manifest
        .as_deref()
        .map(Inputs::from_manifest)
        .transpose()
        .ok()
        .flatten();
    let had_key =
        inputs.is_some_and(|i| i.push_key) || host.path(&layout.key())? != PathState::Missing;
    Ok(match had_key {
        true => vec![format!(
            "the push App's key file is removed with {}, but the key stays registered with the \
             push App until you delete it in the App's settings on GitHub",
            layout.state_root
        )],
        false => Vec::new(),
    })
}

/// Stops each registered service, the supervisor first, and deletes it.
fn remove_services(
    host: &mut dyn Machine,
    services: &mut dyn Services,
    layout: &Layout,
    report: &mut UninstallReport,
) -> Result<()> {
    let mut registered = Vec::new();
    for (_, name) in layout.named(&STOP_ORDER) {
        if host.service(&name)?.is_some() {
            registered.push(name);
        }
    }
    for name in &registered {
        if services.state(name)? != RunState::Missing {
            control::stop(services, name)
                .with_context(|| format!("stop {name} before deleting it"))?;
        }
    }
    for name in registered {
        host.delete_service(&name)?;
        report.removed.push(format!("service {name}"));
    }
    Ok(())
}

/// Removes the tree at `root`, reporting what could not be removed.
fn remove_root(host: &mut dyn Machine, root: &str, report: &mut UninstallReport) -> Result<()> {
    if host.path(root)? == PathState::Missing {
        return Ok(());
    }
    let left = host.remove_tree(root)?;
    match left.is_empty() {
        true => report.removed.push(root.to_owned()),
        false => report.left.extend(left),
    }
    Ok(())
}

/// Removes each service account's profile, or reports it pending while
/// Windows keeps it loaded.
fn remove_profiles(
    host: &mut dyn Machine,
    layout: &Layout,
    report: &mut UninstallReport,
) -> Result<()> {
    let sids = Sids::of(layout);
    for (role, name) in layout.named(&Role::ALL) {
        let sid = sids.get(role);
        match host.profile(sid)? {
            ProfileState::Absent => {}
            ProfileState::Unloaded => {
                host.delete_profile(sid)?;
                report
                    .removed
                    .push(format!("the profile of NT SERVICE\\{name} ({sid})"));
            }
            ProfileState::Loaded => report.pending.push(format!(
                "Windows keeps the profile of NT SERVICE\\{name} ({sid}) loaded; restart Windows \
                 and run uninstall again to remove it{}",
                if role == Role::Runner {
                    ", with the launches' AppContainer profiles"
                } else {
                    ""
                }
            )),
        }
    }
    Ok(())
}
