//! The egress and publication brokers as Windows services (phase 6, step
//! 6d). Each runs under its own virtual service account, so a broker's
//! account is the one Windows reports for the process, never a setting.
//!
//! The service starts, in this order: it validates its settings, secures
//! its state directory ([`state_directory::secure`]: made with an access
//! list for its own account, the system and administrators only, or, when
//! it exists, used only if one of them owns it and given that list again),
//! sends its diagnostics to `service.log` there, logs what its token holds
//! ([`token_inventory`]), builds its handler, creates its control pipe for
//! the runner's account and only then reports running. A failure at any
//! step stops the service; there is no broker without the control pipe's
//! access list.
//!
//! One thread serves the control pipe; each launch's pipe has threads of
//! its own ([`Broker`]). A stop or preshutdown request ends the control
//! loop within one [`TICK`]; dropping the broker then closes every launch,
//! which ends its sessions and removes its pipe, so a stopped broker serves
//! nothing and ongoing egress ends with it.
use super::authority_pipe::AuthorityRoute;
use super::broker::{Broker, Handler};
use super::broker_control::Control;
use super::egress_broker::{self, Egress};
use super::publication_broker::{self, Publication};
use super::scm::{self, Host};
use super::{sddl, state_directory, token, token_inventory};
use crate::config::Config;
use crate::service::broker_settings::{BrokerCore, PublicationSettings};
use crate::service::lifecycle::{Report, Signal};
use crate::service::quiet::Quiet;
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HANDLE;

/// How often the control loop looks at the service's life, and how long
/// it waits after the control pipe failed before trying it again.
pub const TICK: Duration = Duration::from_millis(250);
/// How long Windows may wait for the service to report that it started.
const START_HINT: Duration = Duration::from_secs(30);
/// How long Windows may wait for the broker's launches to close. A close
/// waits for a push's Git work under way; the service keeps no checkpoint
/// running meanwhile, so a longer close outlasts the hint.
const STOP_HINT: Duration = Duration::from_secs(60);

/// Runs the egress broker as the service `[windows_egress] name`, allowing
/// the Linux proxy's destinations. From a console it fails at once.
pub fn run_egress(config: Config) -> Result<()> {
    let settings = config.windows_egress.clone();
    let allow = config.egress_hosts();
    let name = settings.name.clone();
    scm::dispatch(&name, move |host| {
        let handler = || Ok(Arc::new(Egress::public(allow)) as Arc<dyn Handler>);
        serve(host, settings.core(), egress_broker::SERVICE, handler)
    })
}

/// Runs the publication broker as the service `[windows_publication]
/// name`. From a console it fails at once.
pub fn run_publication(config: Config) -> Result<()> {
    let settings = config.windows_publication.clone();
    let name = settings.name.clone();
    scm::dispatch(&name, move |host| {
        settings.validate()?;
        let handler = || publication(&settings);
        serve(host, settings.core(), publication_broker::SERVICE, handler)
    })
}

/// The publication handler: it asks the supervisor named in `settings`
/// and keeps each launch's private repository below the work root, which
/// is secured as the state directory is.
pub fn publication(settings: &PublicationSettings) -> Result<Arc<dyn Handler>> {
    let route = AuthorityRoute::new(&settings.authority_pipe, &settings.supervisor_sid)?;
    let work_root = settings.work_root();
    secure(&work_root)?;
    let publication = Publication::new(&settings.remote, work_root, Arc::new(route));
    Ok(Arc::new(publication))
}

/// Secures `directory` for this process's account.
fn secure(directory: &Path) -> Result<()> {
    let me = token::current_user_sid()?;
    state_directory::secure(directory, &me, &sddl::state_directory(&me)?)
}

/// The service's whole life. It returns true: closing the broker ends
/// every launch's sessions before the service reports stopped.
fn serve(
    host: &Host,
    core: BrokerCore<'_>,
    service: &str,
    handler: impl FnOnce() -> Result<Arc<dyn Handler>>,
) -> Result<bool> {
    host.starting(1, START_HINT)?;
    core.validate()?;
    secure(core.state_dir)?;
    scm::errors_to(&core.state_dir.join("service.log"))?;
    let mut control = open(core, service, handler, Some(host.wake()))?;
    host.report(&Report::Running)?;
    eprintln!("agentc-supervisor windows-service: {service} broker running");
    serve_until_stopped(host, &mut control, service);
    close(host, control, service)
}

/// Reports the service stopping and closes the broker, which ends every
/// launch's sessions and removes its pipe.
fn close(host: &Host, control: Control, service: &str) -> Result<bool> {
    let stopping = Report::Stopping {
        checkpoint: 1,
        wait_hint: STOP_HINT,
    };
    host.report(&stopping)?;
    drop(control);
    eprintln!("agentc-supervisor windows-service: {service} broker stopped");
    Ok(true)
}

/// Opens the broker `service` as this process's account: validates `core`,
/// secures its state directory, logs the token's inventory, builds the
/// handler and serves the control pipe to the runner's account. `wake`
/// interrupts the control pipe's waits. Everything the service does
/// before it reports running, except sending its diagnostics to its log.
pub fn open(
    core: BrokerCore<'_>,
    service: &str,
    handler: impl FnOnce() -> Result<Arc<dyn Handler>>,
    wake: Option<HANDLE>,
) -> Result<Control> {
    core.validate()?;
    secure(core.state_dir)?;
    log_inventory(service)?;
    let me = token::current_user_sid()?;
    let broker = Broker::new(service, &me, handler()?)?;
    Control::new(core.control_pipe, broker, core.runner_sid, wake)
}

/// Logs this process's token as one JSON line: its user, groups and
/// privileges as Windows reports them.
fn log_inventory(service: &str) -> Result<()> {
    let inventory = token_inventory::current()?;
    let line = serde_json::json!({"broker": service, "token": inventory});
    eprintln!("{line}");
    Ok(())
}

/// Serves the runner on the control pipe until a stop or preshutdown
/// request. A failure that repeats is logged once with a count, and the
/// loop waits a [`TICK`] after each one rather than spinning.
fn serve_until_stopped(host: &Host, control: &mut Control, service: &str) {
    let mut quiet = Quiet::default();
    loop {
        host.reset_wake();
        if ending(host.signal()) {
            return;
        }
        let stop = || ending(host.signal());
        match control.serve_one(TICK, &stop) {
            Ok(()) => quiet.clear("control"),
            Err(error) => {
                let text = format!("{service} control pipe: {error:#}");
                for line in quiet.lines("control", &text, Instant::now()) {
                    eprintln!("agentc-supervisor windows-service: {line}");
                }
                std::thread::sleep(TICK);
            }
        }
    }
}

/// Whether the service was asked to end.
fn ending(signal: &Signal) -> bool {
    signal.stop_requested() || signal.preshutdown_requested()
}
