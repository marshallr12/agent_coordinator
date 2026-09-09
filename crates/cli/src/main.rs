mod config;
mod job_state;
mod state;
mod worktree;

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use coordinator_client::{ApiResponse, ClientError, CoordinatorClient, HttpMethod, SessionAuth};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use state::{OrientationVersion, PendingMutation, SessionState};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "agent-coordinator",
    version,
    about = "Native Agent Coordinator client"
)]
struct Cli {
    /// Repository binding. Defaults to .agent-coordinator.toml in this directory or a parent.
    #[arg(long, global = true)]
    repo_config: Option<PathBuf>,

    /// Local harness-session name. Set a different value for every independent harness.
    #[arg(long, global = true, env = "AGENT_COORDINATOR_SESSION")]
    session: Option<String>,

    /// Permit plain HTTP only when the service is on a loopback address.
    #[arg(long, global = true, env = "AGENT_COORDINATOR_ALLOW_INSECURE_LOOPBACK")]
    allow_insecure_loopback: bool,

    /// Emit the complete response as compact JSON instead of a human-readable summary.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create or resume this harness session and return project orientation. Never claims work.
    Connect(ConnectArgs),
    /// List or create projects.
    Projects {
        #[command(subcommand)]
        command: ProjectsCommand,
    },
    /// List or create tasks in the bound project.
    Tasks {
        #[command(subcommand)]
        command: TasksCommand,
    },
    /// Claim one explicit task or the next eligible task.
    Claim(ClaimArgs),
    /// Renew an active attempt lease.
    Renew(AttemptGenerationArgs),
    /// Save a checkpoint without implicitly renewing the lease.
    Checkpoint(AttemptInputArgs),
    /// Release an attempt without marking its task complete.
    Release(AttemptInputArgs),
    /// Prepare and register a separate clean Git worktree.
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommand,
    },
    /// List and reserve named shared resources.
    Resources {
        #[command(subcommand)]
        command: ResourcesCommand,
    },
    /// Inspect or release reservations held by attempts.
    Reservations {
        #[command(subcommand)]
        command: ReservationsCommand,
    },
    /// Run and inspect durable local jobs.
    Jobs {
        #[command(subcommand)]
        command: JobsCommand,
    },
    /// Record an explicit recovery inspection and disposition.
    Recovery {
        #[command(subcommand)]
        command: RecoveryCommand,
    },
    /// Send a bounded API request. POST requests use durable mutation state.
    Request(RequestArgs),
    /// Retry the exact pending mutation and its saved idempotency key.
    Retry,
    #[command(name = "__job-guardian", hide = true)]
    JobGuardian(JobGuardianArgs),
}

#[derive(Subcommand)]
enum WorktreeCommand {
    /// Create a new worktree and register it to the current attempt.
    Prepare(WorktreePrepareArgs),
}

#[derive(Args)]
struct WorktreePrepareArgs {
    #[arg(long)]
    attempt: String,
    #[arg(long)]
    generation: u64,
    /// Existing clean checkout of the configured repository.
    #[arg(long, default_value = ".")]
    source: PathBuf,
    /// New worktree directory. Its parent must already exist.
    #[arg(long)]
    path: PathBuf,
    /// New local branch for this attempt.
    #[arg(long)]
    branch: String,
    /// Commit-ish to resolve and check out exactly.
    #[arg(long)]
    base: String,
}

#[derive(Subcommand)]
enum ResourcesCommand {
    /// List resources configured by human administrators.
    List(ListArgs),
    /// Atomically reserve the complete resource set in a JSON file.
    Reserve(AttemptInputArgs),
    /// Release a reservation after all attached jobs are terminal.
    Release(ReservationReleaseArgs),
}

#[derive(Subcommand)]
enum ReservationsCommand {
    /// List reservations and their held or recovery-required state.
    List(ListArgs),
    /// Release a reservation after all attached jobs are terminal.
    Release(ReservationReleaseArgs),
}

#[derive(Args)]
struct ReservationReleaseArgs {
    #[arg(long)]
    reservation: String,
    #[arg(long)]
    generation: u64,
    #[arg(long)]
    reason: String,
}

#[derive(Subcommand)]
enum JobsCommand {
    /// List remotely registered jobs without starting anything.
    List(ListArgs),
    /// Inspect one remotely registered job without starting anything.
    Status(JobStatusArgs),
    /// Register and start one durable local producer.
    Run(JobRunArgs),
    /// Reattach reporting to an existing local producer; never launches one.
    Reconnect(JobLocalArgs),
    /// Inspect protected local job state; never launches one.
    Inspect(JobLocalArgs),
}

#[derive(Args)]
struct JobStatusArgs {
    #[arg(long)]
    job: String,
}

#[derive(Args)]
struct JobLocalArgs {
    #[arg(long)]
    job: String,
}

#[derive(Args)]
struct JobRunArgs {
    #[arg(long)]
    attempt: String,
    #[arg(long)]
    generation: u64,
    #[arg(long)]
    reservation: String,
    /// Prepared worktree registered for the attempt.
    #[arg(long)]
    checkout: PathBuf,
    /// JSON file containing label, program, argv, and optional environment/log limit.
    #[arg(long)]
    input: PathBuf,
    /// Maximum seconds of delegated attempt renewal; zero disables renewal.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u16).range(0..=3600))]
    renew_for_seconds: u16,
    /// Harness PID whose exact process lifetime bounds delegated renewal.
    #[arg(long)]
    watch_pid: Option<u32>,
}

#[derive(Args)]
struct JobGuardianArgs {
    #[arg(long)]
    state_file: PathBuf,
    #[arg(long, value_enum)]
    mode: GuardianModeArg,
}

#[derive(Clone, Copy, ValueEnum)]
enum GuardianModeArg {
    Run,
    Observe,
}

impl From<GuardianModeArg> for coordinator_local::GuardianMode {
    fn from(value: GuardianModeArg) -> Self {
        match value {
            GuardianModeArg::Run => Self::Run,
            GuardianModeArg::Observe => Self::Observe,
        }
    }
}

#[derive(Subcommand)]
enum RecoveryCommand {
    /// Inspect an attempt and its saved work/job evidence without changing it.
    Inspect(RecoveryInspectArgs),
    /// Record inspected saved-work/job evidence and its explicit disposition.
    Resolve(AttemptInputArgs),
}

#[derive(Args)]
struct RecoveryInspectArgs {
    #[arg(long)]
    attempt: String,
}

#[derive(Args)]
struct ConnectArgs {
    #[arg(long, default_value = "agent-coordinator-cli")]
    harness: String,
    #[arg(long)]
    workstation: Option<String>,
    #[arg(long = "capability")]
    capabilities: Vec<String>,
}

#[derive(Subcommand)]
enum ProjectsCommand {
    List(ListArgs),
    Create(InputArgs),
}

#[derive(Subcommand)]
enum TasksCommand {
    List(ListArgs),
    Create(InputArgs),
}

#[derive(Args)]
struct InputArgs {
    /// JSON file, or - for standard input.
    #[arg(long)]
    input: PathBuf,
}

#[derive(Args)]
struct ListArgs {
    /// Opaque cursor returned by the previous page.
    #[arg(long)]
    cursor: Option<String>,
    /// Requested page size (1 through 200).
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=200))]
    limit: Option<u16>,
}

#[derive(Args)]
struct ClaimArgs {
    /// Claim ordinary work or inspect an expired attempt for recovery.
    #[arg(long, value_enum, default_value = "work")]
    mode: ClaimMode,
    /// Claim the next eligible task.
    #[arg(long, conflicts_with = "task", required_unless_present = "task")]
    next: bool,
    /// Claim this task ID.
    #[arg(long, conflicts_with = "next", required_unless_present = "next")]
    task: Option<String>,
    /// Required current task revision when --task is used.
    #[arg(long, requires = "task")]
    revision: Option<u64>,
}

#[derive(Copy, Clone, ValueEnum)]
enum ClaimMode {
    Work,
    Recovery,
}

impl ClaimMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Recovery => "recovery",
        }
    }
}

#[derive(Args)]
struct AttemptGenerationArgs {
    #[arg(long)]
    attempt: String,
    #[arg(long)]
    generation: u64,
}

#[derive(Args)]
struct AttemptInputArgs {
    #[arg(long)]
    attempt: String,
    #[arg(long)]
    generation: u64,
    /// JSON file, or - for standard input.
    #[arg(long)]
    input: PathBuf,
}

#[derive(Copy, Clone, ValueEnum)]
enum RequestMethod {
    Get,
    Post,
    Patch,
}

#[derive(Args)]
struct RequestArgs {
    #[arg(long, value_enum)]
    method: RequestMethod,
    /// Absolute /api/v1 path. Query strings and redirects are refused.
    #[arg(long)]
    path: String,
    /// JSON file, or - for standard input. Required for POST and PATCH.
    #[arg(long)]
    input: Option<PathBuf>,
}

struct ContextData {
    binding: config::RepositoryBinding,
    origin: String,
    client: CoordinatorClient,
    credential_digest: String,
}

struct Failure {
    exit: u8,
    output: Value,
}

impl Failure {
    fn local(exit: u8, code: &str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            exit,
            output: json!({
                "error": {
                    "code": code,
                    "message": message.into(),
                    "details": {},
                    "next_actions": [],
                    "retryable": retryable
                }
            }),
        }
    }

    fn invalid(error: impl std::fmt::Display) -> Self {
        Self::local(2, "invalid_client_input", error.to_string(), false)
    }

    fn temporary(error: impl std::fmt::Display) -> Self {
        Self::local(7, "transport_failure", error.to_string(), true)
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let compact = cli.json;
    match run(&cli).await {
        Ok(output) => {
            if compact {
                print_json(&output, true);
            } else {
                print_human(&cli.command, &output);
            }
            ExitCode::SUCCESS
        }
        Err(failure) => {
            if compact {
                print_json(&failure.output, true);
            } else {
                print_human(&cli.command, &failure.output);
            }
            ExitCode::from(failure.exit)
        }
    }
}

fn print_json(value: &Value, compact: bool) {
    let rendered = if compact {
        serde_json::to_string(value)
    } else {
        serde_json::to_string_pretty(value)
    };
    match rendered {
        Ok(rendered) => println!("{rendered}"),
        Err(_) => println!(
            "{{\"error\":{{\"code\":\"output_failure\",\"message\":\"could not encode JSON output\"}}}}"
        ),
    }
}

fn print_human(command: &Command, value: &Value) {
    if let Some(error) = value.get("error") {
        let code = error.get("code").and_then(Value::as_str).unwrap_or("error");
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("request failed");
        println!("Error ({code}): {message}");
        if let Some(help_path) = error.pointer("/details/help_path").and_then(Value::as_str) {
            println!("Help: {help_path}");
        }
        if let Some(help) = error.pointer("/details/service_help/data") {
            println!("Setup:");
            print_json(help, false);
        }
        if let Some(actions) = error.get("next_actions").and_then(Value::as_array) {
            for action in actions {
                if let Some(name) = action.get("action").and_then(Value::as_str) {
                    println!("Next: {name}");
                }
            }
        }
        return;
    }

    match command {
        Command::Projects {
            command: ProjectsCommand::List(_),
        } => print_project_table(value),
        Command::Tasks {
            command: TasksCommand::List(_),
        } => print_task_table(value),
        Command::Connect(_) => print_connection(value),
        Command::Claim(_) => print_claim(value),
        _ => {
            println!("Result:");
            print_json(value.get("data").unwrap_or(value), false);
        }
    }
}

fn print_project_table(value: &Value) {
    let items = value
        .pointer("/data/items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if items.is_empty() {
        println!("No projects.");
    } else {
        println!("ID\tNAME\tTARGET BRANCH\tREPOSITORY");
        for item in items {
            println!(
                "{}\t{}\t{}\t{}",
                field(item, "id"),
                field(item, "name"),
                field(item, "target_branch"),
                field(item, "repository_url")
            );
        }
    }
    print_next_cursor(value);
}

fn print_task_table(value: &Value) {
    let items = value
        .pointer("/data/items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if items.is_empty() {
        println!("No tasks.");
    } else {
        println!("ID\tPRIORITY\tLIFECYCLE\tWORK\tTITLE");
        for item in items {
            println!(
                "{}\t{}\t{}\t{}\t{}",
                field(item, "id"),
                field(item, "priority"),
                field(item, "lifecycle"),
                field(item, "work_status"),
                field(item, "title")
            );
        }
    }
    print_next_cursor(value);
}

fn print_next_cursor(value: &Value) {
    if let Some(cursor) = value.pointer("/data/next_cursor").and_then(Value::as_str) {
        println!("Next cursor: {cursor}");
    }
}

fn print_connection(value: &Value) {
    let data = value.get("data").unwrap_or(value);
    let resumed = data
        .get("resumed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    println!(
        "Session: {} ({})",
        field(data, "local_session"),
        if resumed { "resumed" } else { "created" }
    );
    let orientation = data.get("orientation").unwrap_or(&Value::Null);
    if let Some(project) = orientation.get("project") {
        println!(
            "Project: {} ({})",
            field(project, "name"),
            field(project, "id")
        );
    }
    println!("Policy revision: {}", field(orientation, "policy_revision"));
    println!(
        "Instruction version: {}",
        field(orientation, "instruction_version")
    );
    if let Some(instructions) = orientation.get("instructions").and_then(Value::as_str) {
        println!("\nInstructions:\n{instructions}");
    }
    if orientation
        .get("instructions_complete")
        .and_then(Value::as_bool)
        == Some(false)
    {
        println!("\nInstructions are incomplete; follow the continuation steps before claiming.");
    }
    if let Some(candidates) = orientation.get("candidates").and_then(Value::as_array)
        && !candidates.is_empty()
    {
        println!("\nCandidate tasks:");
        for candidate in candidates {
            let task = candidate.get("task").unwrap_or(candidate);
            println!("{}\t{}", field(task, "id"), field(task, "title"));
        }
    }
    if let Some(active) = orientation.get("active_attempts").and_then(Value::as_array)
        && !active.is_empty()
    {
        println!("\nActive attempts:");
        for attempt in active {
            println!(
                "{}\ttask {}\tgeneration {}\texpires {}",
                field(attempt, "id"),
                field(attempt, "task_id"),
                field(attempt, "generation"),
                field(attempt, "expires_at")
            );
        }
    }
}

fn print_claim(value: &Value) {
    let claim = value.pointer("/data/claim").unwrap_or(&Value::Null);
    if claim.is_null() {
        println!("No eligible task was claimed.");
        if let Some(reasons) = value.pointer("/data/reasons").and_then(Value::as_array) {
            for reason in reasons {
                println!("Reason: {}", scalar(reason));
            }
        }
        return;
    }
    let task = claim.get("task").unwrap_or(&Value::Null);
    let attempt = claim.get("attempt").unwrap_or(&Value::Null);
    println!(
        "Claimed task: {} — {}",
        field(task, "id"),
        field(task, "title")
    );
    println!(
        "Attempt: {} (generation {}, expires {})",
        field(attempt, "id"),
        field(attempt, "generation"),
        field(attempt, "expires_at")
    );
    if let Some(authority) = value.pointer("/data/current_authority") {
        println!("Current authority:");
        print_json(authority, false);
    }
}

fn field(value: &Value, name: &str) -> String {
    value.get(name).map(scalar).unwrap_or_else(|| "-".into())
}

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "-".into(),
        Value::String(value) => value.replace(['\t', '\n', '\r'], " "),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| "?".into()),
    }
}

async fn run(cli: &Cli) -> std::result::Result<Value, Failure> {
    if let Command::JobGuardian(args) = &cli.command {
        let outcome = coordinator_local::run_guardian(&args.state_file, args.mode.into())
            .await
            .map_err(Failure::temporary)?;
        return serde_json::to_value(outcome).map_err(Failure::invalid);
    }
    let context = build_context(cli).await?;
    match &cli.command {
        Command::Connect(args) => connect(cli, &context, args).await,
        Command::Projects { command } => match command {
            ProjectsCommand::List(args) => finish(
                context
                    .client
                    .get_query("/api/v1/projects", &list_query(args), None)
                    .await,
            ),
            ProjectsCommand::Create(input) => {
                let body = read_json(&input.input).map_err(Failure::invalid)?;
                mutate(cli, &context, "/api/v1/projects", body, true).await
            }
        },
        Command::Tasks { command } => {
            let path = format!("/api/v1/projects/{}/tasks", context.binding.project_id);
            match command {
                TasksCommand::List(args) => finish(
                    context
                        .client
                        .get_query(&path, &list_query(args), None)
                        .await,
                ),
                TasksCommand::Create(input) => {
                    let body = read_json(&input.input).map_err(Failure::invalid)?;
                    mutate(cli, &context, &path, body, true).await
                }
            }
        }
        Command::Claim(args) => claim(cli, &context, args).await,
        Command::Renew(args) => {
            let path = attempt_path(&context, &args.attempt, "renew").map_err(Failure::invalid)?;
            mutate(
                cli,
                &context,
                &path,
                json!({"generation": args.generation}),
                true,
            )
            .await
        }
        Command::Checkpoint(args) => {
            let body =
                input_with_generation(&args.input, args.generation).map_err(Failure::invalid)?;
            let path =
                attempt_path(&context, &args.attempt, "checkpoints").map_err(Failure::invalid)?;
            mutate(cli, &context, &path, body, true).await
        }
        Command::Release(args) => {
            let body =
                input_with_generation(&args.input, args.generation).map_err(Failure::invalid)?;
            let path =
                attempt_path(&context, &args.attempt, "release").map_err(Failure::invalid)?;
            mutate(cli, &context, &path, body, true).await
        }
        Command::Worktree { command } => match command {
            WorktreeCommand::Prepare(args) => prepare_worktree(cli, &context, args).await,
        },
        Command::Resources { command } => match command {
            ResourcesCommand::List(args) => finish(
                context
                    .client
                    .get_query("/api/v1/resources", &list_query(args), None)
                    .await,
            ),
            ResourcesCommand::Reserve(args) => {
                let body = input_with_generation(&args.input, args.generation)
                    .map_err(Failure::invalid)?;
                let path = attempt_path(&context, &args.attempt, "reservations")
                    .map_err(Failure::invalid)?;
                mutate(cli, &context, &path, body, true).await
            }
            ResourcesCommand::Release(args) => release_reservation(cli, &context, args).await,
        },
        Command::Reservations { command } => match command {
            ReservationsCommand::List(args) => {
                let path = format!(
                    "/api/v1/projects/{}/reservations",
                    context.binding.project_id
                );
                let optional = optional_state(cli, &context)?;
                let session = optional.as_ref().map(|(_, _, state)| &state.session);
                finish(
                    context
                        .client
                        .get_query(&path, &list_query(args), session)
                        .await,
                )
            }
            ReservationsCommand::Release(args) => release_reservation(cli, &context, args).await,
        },
        Command::Jobs { command } => jobs_command(cli, &context, command).await,
        Command::Recovery { command } => match command {
            RecoveryCommand::Inspect(args) => {
                validate_segment("attempt", &args.attempt).map_err(Failure::invalid)?;
                let path = format!(
                    "/api/v1/projects/{}/attempts/{}",
                    context.binding.project_id, args.attempt
                );
                let optional = optional_state(cli, &context)?;
                let session = optional.as_ref().map(|(_, _, state)| &state.session);
                finish(context.client.get(&path, session).await)
            }
            RecoveryCommand::Resolve(args) => {
                let body = input_with_generation(&args.input, args.generation)
                    .map_err(Failure::invalid)?;
                let path = attempt_path(&context, &args.attempt, "recovery-resolution")
                    .map_err(Failure::invalid)?;
                mutate(cli, &context, &path, body, true).await
            }
        },
        Command::Request(args) => request(cli, &context, args).await,
        Command::Retry => retry(cli, &context).await,
        Command::JobGuardian(_) => unreachable!("guardian handled before loading credentials"),
    }
}

async fn release_reservation(
    cli: &Cli,
    context: &ContextData,
    args: &ReservationReleaseArgs,
) -> std::result::Result<Value, Failure> {
    validate_segment("reservation", &args.reservation).map_err(Failure::invalid)?;
    if args.reason.trim().is_empty() {
        return Err(Failure::invalid("--reason must not be empty"));
    }
    let path = format!(
        "/api/v1/projects/{}/reservations/{}/release",
        context.binding.project_id, args.reservation
    );
    mutate(
        cli,
        context,
        &path,
        json!({"generation": args.generation, "reason": args.reason}),
        true,
    )
    .await
}

async fn prepare_worktree(
    cli: &Cli,
    context: &ContextData,
    args: &WorktreePrepareArgs,
) -> std::result::Result<Value, Failure> {
    validate_segment("attempt", &args.attempt).map_err(Failure::invalid)?;
    let (_session_lock, session_path, mut state) = load_required_state(cli, context)?;
    let attempt_path = format!(
        "/api/v1/projects/{}/attempts/{}",
        context.binding.project_id, args.attempt
    );
    let attempt_response = context
        .client
        .get(&attempt_path, Some(&state.session))
        .await
        .map_err(client_failure)?;
    let attempt_body = require_success(attempt_response)?;
    require_current_work_attempt(&attempt_body, args.generation)?;
    let project = bound_project(context, Some(&state.session)).await?;
    let repository_url = project
        .get("repository_url")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::temporary("project response omitted repository_url"))?;

    let prepared = worktree::prepare(worktree::PrepareRequest {
        service_origin: &context.origin,
        project_id: &context.binding.project_id,
        attempt_id: &args.attempt,
        generation: args.generation,
        repository_url,
        source: &args.source,
        destination: &args.path,
        branch: &args.branch,
        base: &args.base,
    })
    .map_err(Failure::invalid)?;
    let identity = prepared
        .git_dir_identity
        .as_ref()
        .and_then(|path| path.to_str())
        .ok_or_else(|| Failure::invalid("resolved Git directory is not valid UTF-8"))?;
    let local_path = prepared
        .destination
        .to_str()
        .ok_or_else(|| Failure::invalid("worktree path is not valid UTF-8"))?;
    let body = json!({
        "generation": args.generation,
        "workstation_id": state.workstation_id,
        "identity": identity,
        "path": local_path,
        "branch": prepared.branch,
        "base_revision": prepared.base_revision,
        "clean": true
    });
    let register_path = format!(
        "/api/v1/projects/{}/attempts/{}/checkout",
        context.binding.project_id, args.attempt
    );
    if let Some(existing) = attempt_body.pointer("/data/checkout") {
        let matches = [
            "workstation_id",
            "identity",
            "path",
            "branch",
            "base_revision",
        ]
        .iter()
        .all(|field| existing.get(field) == body.get(field));
        if !matches {
            return Err(Failure::local(
                5,
                "checkout_registered",
                "the attempt already has a different registered checkout",
                false,
            ));
        }
        if let Some(pending) = &state.pending {
            if pending.method != HttpMethod::Post
                || pending.path != register_path
                || pending.body != body
                || !pending.include_session_id
            {
                return Err(Failure::invalid(format!(
                    "an earlier mutation is unresolved ({}); run `agent-coordinator retry`",
                    pending.path
                )));
            }
            state.pending = None;
            state::save(&session_path, &state).map_err(Failure::invalid)?;
        }
        return Ok(json!({
            "data": {
                "worktree": prepared,
                "registration": existing,
                "reconciled": true
            }
        }));
    }
    let response = persist_and_send(
        context,
        &session_path,
        &mut state,
        HttpMethod::Post,
        &register_path,
        body,
        true,
    )
    .await?;
    let remote = require_success(response)?;
    Ok(json!({
        "data": {
            "worktree": prepared,
            "registration": data(&remote)
        },
        "response": remote
    }))
}

fn require_current_work_attempt(body: &Value, generation: u64) -> std::result::Result<(), Failure> {
    let data = body.get("data").unwrap_or(body);
    let attempt = data.get("attempt").unwrap_or(data);
    if attempt.get("generation").and_then(Value::as_u64) != Some(generation) {
        return Err(Failure::local(
            5,
            "stale_generation",
            "the attempt generation no longer matches",
            false,
        ));
    }
    if attempt.get("state").and_then(Value::as_str) != Some("active")
        || attempt.get("mode").and_then(Value::as_str) != Some("work")
        || data.get("authority_valid").and_then(Value::as_bool) != Some(true)
    {
        return Err(Failure::local(
            5,
            "ownership_not_current",
            "the attempt is not a current owned work attempt; no local Git state was changed",
            false,
        ));
    }
    Ok(())
}

async fn bound_project(
    context: &ContextData,
    session: Option<&SessionAuth>,
) -> std::result::Result<Value, Failure> {
    let mut cursor: Option<String> = None;
    loop {
        let mut query = vec![("limit", "200".to_owned())];
        if let Some(value) = &cursor {
            query.push(("cursor", value.clone()));
        }
        let response = context
            .client
            .get_query("/api/v1/projects", &query, session)
            .await
            .map_err(client_failure)?;
        let body = require_success(response)?;
        let page = body.get("data").unwrap_or(&body);
        if let Some(project) = page
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| {
                items.iter().find(|item| {
                    item.get("id").and_then(Value::as_str) == Some(&context.binding.project_id)
                })
            })
        {
            return Ok(project.clone());
        }
        cursor = page
            .get("next_cursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            return Err(Failure::local(
                5,
                "project_binding_missing",
                "the repository's bound project is not available from this service",
                false,
            ));
        }
    }
}

async fn jobs_command(
    cli: &Cli,
    context: &ContextData,
    command: &JobsCommand,
) -> std::result::Result<Value, Failure> {
    match command {
        JobsCommand::List(args) => {
            let path = format!("/api/v1/projects/{}/jobs", context.binding.project_id);
            let optional = optional_state(cli, context)?;
            let session = optional.as_ref().map(|(_, _, state)| &state.session);
            finish(
                context
                    .client
                    .get_query(&path, &list_query(args), session)
                    .await,
            )
        }
        JobsCommand::Status(args) => remote_job(context, cli, &args.job).await,
        JobsCommand::Run(args) => run_job(cli, context, args).await,
        JobsCommand::Reconnect(args) => {
            validate_segment("job", &args.job).map_err(Failure::invalid)?;
            let intent = job_state::load_by_job(&args.job).map_err(Failure::invalid)?;
            validate_local_job_binding(context, &intent)?;
            let summary =
                coordinator_local::inspect_job(&intent.state_file).map_err(Failure::invalid)?;
            let executable = env::current_exe().map_err(Failure::invalid)?;
            coordinator_local::start_guardian(
                &executable,
                &intent.state_file,
                coordinator_local::GuardianMode::Observe,
            )
            .map_err(Failure::temporary)?;
            Ok(json!({
                "data": {
                    "job": summary,
                    "guardian_started": true,
                    "mode": "observe"
                }
            }))
        }
        JobsCommand::Inspect(args) => {
            validate_segment("job", &args.job).map_err(Failure::invalid)?;
            let intent = job_state::load_by_job(&args.job).map_err(Failure::invalid)?;
            validate_local_job_binding(context, &intent)?;
            let summary =
                coordinator_local::inspect_job(&intent.state_file).map_err(Failure::invalid)?;
            serde_json::to_value(json!({"data":{"job":summary}})).map_err(Failure::invalid)
        }
    }
}

fn validate_local_job_binding(
    context: &ContextData,
    intent: &job_state::RunIntent,
) -> std::result::Result<(), Failure> {
    if intent.service_origin != context.origin || intent.project_id != context.binding.project_id {
        return Err(Failure::invalid(
            "local job state does not belong to the repository's bound project and service",
        ));
    }
    Ok(())
}

async fn remote_job(
    context: &ContextData,
    cli: &Cli,
    job_id: &str,
) -> std::result::Result<Value, Failure> {
    validate_segment("job", job_id).map_err(Failure::invalid)?;
    let path = format!(
        "/api/v1/projects/{}/jobs/{job_id}",
        context.binding.project_id
    );
    let optional = optional_state(cli, context)?;
    let session = optional.as_ref().map(|(_, _, state)| &state.session);
    finish(context.client.get(&path, session).await)
}

async fn run_job(
    cli: &Cli,
    context: &ContextData,
    args: &JobRunArgs,
) -> std::result::Result<Value, Failure> {
    validate_segment("attempt", &args.attempt).map_err(Failure::invalid)?;
    validate_segment("reservation", &args.reservation).map_err(Failure::invalid)?;
    if (args.renew_for_seconds > 0) != args.watch_pid.is_some() {
        return Err(Failure::invalid(
            "--renew-for-seconds and --watch-pid must be supplied together",
        ));
    }
    let (_session_lock, session_path, mut session) = load_required_state(cli, context)?;
    if let Some(pending) = &session.pending {
        return Err(Failure::invalid(format!(
            "an earlier mutation is unresolved ({}); run `agent-coordinator retry` before starting or resuming a job",
            pending.path
        )));
    }
    let attempt_path = format!(
        "/api/v1/projects/{}/attempts/{}",
        context.binding.project_id, args.attempt
    );
    let attempt_response = context
        .client
        .get(&attempt_path, Some(&session.session))
        .await
        .map_err(client_failure)?;
    let attempt_body = require_success(attempt_response)?;
    require_current_work_attempt(&attempt_body, args.generation)?;

    let prepared =
        worktree::load_for_attempt(&context.origin, &context.binding.project_id, &args.attempt)
            .map_err(Failure::invalid)?;
    let requested_checkout = std::fs::canonicalize(&args.checkout).map_err(Failure::invalid)?;
    if requested_checkout != prepared.destination {
        return Err(Failure::invalid(
            "--checkout does not match the saved worktree for this attempt",
        ));
    }
    if prepared.generation != args.generation {
        return Err(Failure::local(
            5,
            "stale_generation",
            "the prepared worktree belongs to a different attempt generation",
            false,
        ));
    }
    let checkout = attempt_body
        .pointer("/data/checkout")
        .or_else(|| attempt_body.get("checkout"))
        .ok_or_else(|| {
            Failure::local(
                5,
                "checkout_not_registered",
                "register the prepared worktree before starting a job",
                false,
            )
        })?;
    if checkout.get("path").and_then(Value::as_str) != prepared.destination.to_str() {
        return Err(Failure::local(
            5,
            "checkout_mismatch",
            "the attempt's registered checkout does not match the prepared local worktree",
            false,
        ));
    }
    let (source_revision, source_tree) =
        worktree::current_snapshot(&prepared).map_err(Failure::invalid)?;
    let input = job_state::read_program(&args.input).map_err(Failure::invalid)?;
    let harness = match args.watch_pid {
        Some(pid) => Some(
            coordinator_local::capture_process_identity(pid)
                .map_err(Failure::invalid)?
                .ok_or_else(|| {
                    Failure::invalid("--watch-pid is not the exact identity of a running process")
                })?,
        ),
        None => None,
    };
    let mut intent = job_state::load_or_create(job_state::NewRunIntent {
        service_origin: &context.origin,
        project_id: &context.binding.project_id,
        session_id: &session.session.id,
        attempt_id: &args.attempt,
        generation: args.generation,
        reservation_id: &args.reservation,
        checkout: &prepared.destination,
        source_revision: &source_revision,
        source_tree: &source_tree,
        input: input.clone(),
        renew_for_seconds: args.renew_for_seconds,
        watch_pid: args.watch_pid,
        job_id: Uuid::new_v4().to_string(),
        producer_id: Uuid::new_v4().to_string(),
        runner_instance_id: Uuid::new_v4().to_string(),
        reporter_id: Uuid::new_v4().to_string(),
        reporter_proof: random_secret().map_err(Failure::invalid)?,
    })
    .map_err(Failure::invalid)?
    .0;

    let summary = if intent.initialized {
        if !intent.state_file.is_file() {
            return Err(Failure::local(
                5,
                "local_job_state_missing",
                "the durable job journal is missing; the producer may already have run, so it will not be launched again",
                false,
            ));
        }
        coordinator_local::inspect_job(&intent.state_file).map_err(Failure::invalid)?
    } else {
        let initialized =
            coordinator_local::initialize_launch_state(coordinator_local::InitializeJob {
                state_file: intent.state_file.clone(),
                identities: coordinator_local::JobIdentities {
                    job_id: intent.job_id.clone(),
                    producer_id: intent.producer_id.clone(),
                    runner_instance_id: intent.runner_instance_id.clone(),
                    reporter_id: intent.reporter_id.clone(),
                },
                command: coordinator_local::CommandSpec {
                    program: input.program.clone(),
                    args: input.argv.clone(),
                    working_directory: prepared.destination.clone(),
                    environment: input.environment.clone(),
                },
                source: coordinator_local::SourceSnapshot {
                    checkout: prepared.destination.clone(),
                    revision: source_revision.clone(),
                    tree: source_tree.clone(),
                },
                log_limit_bytes: input.log_limit_bytes,
                harness,
            })
            .map_err(Failure::invalid)?;
        job_state::mark_initialized(&mut intent).map_err(Failure::invalid)?;
        initialized
    };

    let remote = if summary.registered {
        reconcile_remote_job(context, &session.session, &intent).await?
    } else {
        register_job(context, &session_path, &mut session, &intent).await?
    };
    let summary = coordinator_local::inspect_job(&intent.state_file).map_err(Failure::invalid)?;
    if !summary.registered {
        return Err(Failure::local(
            5,
            "job_registration_incomplete",
            "the local job is not durably bound to its scoped reporter",
            true,
        ));
    }
    let executable = env::current_exe().map_err(Failure::invalid)?;
    coordinator_local::start_guardian(
        &executable,
        &intent.state_file,
        coordinator_local::GuardianMode::Run,
    )
    .map_err(Failure::temporary)?;
    Ok(json!({
        "data": {
            "job": summary,
            "remote_job": data(&remote),
            "guardian_started": true,
            "mode": "run"
        },
        "response": remote
    }))
}

async fn register_job(
    context: &ContextData,
    session_path: &Path,
    session: &mut SessionState,
    intent: &job_state::RunIntent,
) -> std::result::Result<Value, Failure> {
    let path = attempt_path(context, &intent.attempt_id, "jobs").map_err(Failure::invalid)?;
    let body = json!({
        "generation": intent.generation,
        "job_id": intent.job_id,
        "producer_id": intent.producer_id,
        "runner_instance_id": intent.runner_instance_id,
        "workstation_id": session.workstation_id,
        "label": intent.input.label,
        "source_revision": intent.source_revision,
        "source_tree": intent.source_tree,
        "reservation_id": intent.reservation_id,
        "reporter_id": intent.reporter_id,
        "reporter_proof": intent.reporter_proof,
        "renew_for_seconds": intent.renew_for_seconds
    });
    let desired = PendingMutation {
        key: Uuid::new_v4().to_string(),
        method: HttpMethod::Post,
        path,
        body,
        include_session_id: true,
    };
    match &session.pending {
        Some(existing)
            if existing.method == desired.method
                && existing.path == desired.path
                && existing.body == desired.body
                && existing.include_session_id == desired.include_session_id => {}
        _ => {
            session.set_pending(desired).map_err(Failure::invalid)?;
            state::save(session_path, session).map_err(Failure::invalid)?;
        }
    }
    let response = send_saved_policy(context, session_path, session, false).await?;
    let remote = require_success(response)?;
    record_job_registration(context, intent, &remote)?;
    reconcile_remote_job(context, &session.session, intent).await?;
    session.pending = None;
    state::save(session_path, session).map_err(Failure::invalid)?;
    Ok(remote)
}

fn record_job_registration(
    context: &ContextData,
    intent: &job_state::RunIntent,
    response: &Value,
) -> std::result::Result<(), Failure> {
    let data = response.get("data").unwrap_or(response);
    let reporter = data.get("reporter").ok_or_else(|| {
        Failure::temporary("job registration response omitted reporter authority")
    })?;
    if reporter.get("id").and_then(Value::as_str) != Some(&intent.reporter_id) {
        return Err(Failure::temporary(
            "job registration response named a different reporter",
        ));
    }
    let renewal = if intent.renew_for_seconds == 0 {
        None
    } else {
        Some(coordinator_local::RenewalConfig {
            generation: intent.generation as i64,
            renew_after_seconds: data
                .get("renew_after_seconds")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    Failure::temporary("job registration response omitted renew_after_seconds")
                })?,
            renew_until: reporter
                .get("renew_until")
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::temporary("job registration response omitted renew_until"))?
                .to_owned(),
        })
    };
    let bearer = format!("acr_{}.{}", intent.reporter_id, intent.reporter_proof);
    coordinator_local::record_registration(
        &intent.state_file,
        coordinator_local::ReporterRegistration::new(
            &context.origin,
            context.client.origin().starts_with("http://"),
            bearer,
            renewal,
        )
        .map_err(Failure::invalid)?,
    )
    .map_err(Failure::invalid)?;
    Ok(())
}

async fn reconcile_remote_job(
    context: &ContextData,
    session: &SessionAuth,
    intent: &job_state::RunIntent,
) -> std::result::Result<Value, Failure> {
    let path = format!(
        "/api/v1/projects/{}/jobs/{}",
        context.binding.project_id, intent.job_id
    );
    let response = context
        .client
        .get(&path, Some(session))
        .await
        .map_err(client_failure)?;
    let body = require_success(response)?;
    let data = body.get("data").unwrap_or(&body);
    let job = data.get("job").unwrap_or(data);
    for (name, expected) in [
        ("id", intent.job_id.as_str()),
        ("producer_id", intent.producer_id.as_str()),
        ("source_revision", intent.source_revision.as_str()),
        ("source_tree", intent.source_tree.as_str()),
        ("reservation_id", intent.reservation_id.as_str()),
    ] {
        if job.get(name).and_then(Value::as_str) != Some(expected) {
            return Err(Failure::local(
                5,
                "job_identity_mismatch",
                format!("remote job does not match saved local {name}"),
                false,
            ));
        }
    }
    Ok(body)
}

async fn build_context(cli: &Cli) -> std::result::Result<ContextData, Failure> {
    let (_, binding) = config::binding(cli.repo_config.as_deref()).map_err(Failure::invalid)?;
    validate_segment("project_id", &binding.project_id).map_err(Failure::invalid)?;
    let origin =
        coordinator_client::normalize_origin(&binding.service_url, cli.allow_insecure_loopback)
            .map_err(client_failure)?;

    let token = match config::token(&origin, cli.allow_insecure_loopback) {
        Ok(token) => token,
        Err(error) => {
            let help = CoordinatorClient::unauthenticated(
                &binding.service_url,
                cli.allow_insecure_loopback,
            )
            .map_err(client_failure)?
            .get("/api/v1/help/authentication", None)
            .await;
            let help = help.ok().map(|response| response.body);
            return Err(Failure {
                exit: 3,
                output: json!({
                    "error": {
                        "code": "authentication_required",
                        "message": error.to_string(),
                        "details": {
                            "help_path": "/api/v1/help/authentication",
                            "service_help": help
                        },
                        "next_actions": [{"action": "show_operator_setup_help"}],
                        "retryable": false
                    }
                }),
            });
        }
    };
    let credential_digest = hex::encode(Sha256::digest(token.as_bytes()));
    let client = CoordinatorClient::new(&binding.service_url, token, cli.allow_insecure_loopback)
        .map_err(client_failure)?;
    Ok(ContextData {
        binding,
        origin,
        client,
        credential_digest,
    })
}

async fn connect(
    cli: &Cli,
    context: &ContextData,
    args: &ConnectArgs,
) -> std::result::Result<Value, Failure> {
    let local_session = required_session(cli)?;
    if args.harness.trim().is_empty() {
        return Err(Failure::invalid("--harness must not be empty"));
    }
    let session_path = state::path_for(&context.origin, &context.binding.project_id, local_session)
        .map_err(Failure::invalid)?;
    let _session_lock = state::lock(&session_path).map_err(Failure::temporary)?;
    let existing = state::load(&session_path).map_err(Failure::invalid)?;
    let resumed = existing.is_some();
    let mut state = match existing {
        Some(state) => state,
        None => {
            let workstation = args.workstation.clone().unwrap_or_else(default_workstation);
            let session = SessionAuth {
                id: Uuid::new_v4().to_string(),
                proof: random_secret().map_err(Failure::invalid)?,
            };
            SessionState::new(
                context.origin.clone(),
                context.binding.project_id.clone(),
                local_session.to_owned(),
                context.credential_digest.clone(),
                session,
                workstation,
                args.harness.clone(),
                args.capabilities.clone(),
            )
        }
    };
    validate_state(context, local_session, &state)?;

    if let Some(pending) = &state.pending {
        if pending.path != "/api/v1/sessions" {
            return Err(Failure::invalid(format!(
                "an earlier mutation is unresolved ({}); run `agent-coordinator retry --session {local_session}` before connect",
                pending.path
            )));
        }
        let response = send_saved(context, &session_path, &mut state).await?;
        require_success(response)?;
    } else if !resumed {
        let body = json!({
            "session_id": state.session.id,
            "workstation_id": state.workstation_id,
            "harness": state.harness,
            "capabilities": state.capabilities,
        });
        let pending = PendingMutation {
            key: Uuid::new_v4().to_string(),
            method: HttpMethod::Post,
            path: "/api/v1/sessions".into(),
            body,
            include_session_id: false,
        };
        state.set_pending(pending).map_err(Failure::invalid)?;
        state::save(&session_path, &state).map_err(Failure::invalid)?;
        let response = send_saved(context, &session_path, &mut state).await?;
        require_success(response)?;
    }

    let session_response = context
        .client
        .get(
            &format!("/api/v1/sessions/{}", state.session.id),
            Some(&state.session),
        )
        .await
        .map_err(client_failure)?;
    let session_body = require_success(session_response)?;
    let orientation_response = context
        .client
        .get(
            &format!(
                "/api/v1/projects/{}/orientation",
                context.binding.project_id
            ),
            Some(&state.session),
        )
        .await
        .map_err(client_failure)?;
    let orientation_body = require_success(orientation_response)?;
    state.orientation = parse_orientation(&orientation_body);
    state::save(&session_path, &state).map_err(Failure::invalid)?;

    Ok(json!({
        "data": {
            "resumed": resumed,
            "local_session": local_session,
            "session": data(&session_body),
            "orientation": data(&orientation_body)
        },
        "responses": {
            "session": session_body,
            "orientation": orientation_body
        }
    }))
}

async fn claim(
    cli: &Cli,
    context: &ContextData,
    args: &ClaimArgs,
) -> std::result::Result<Value, Failure> {
    if args.task.is_some() && args.revision.is_none() {
        return Err(Failure::invalid("--revision is required with --task"));
    }
    let (_session_lock, session_path, mut state) = load_required_state(cli, context)?;
    let orientation = state.orientation.clone().ok_or_else(|| {
        Failure::invalid("run connect before claiming so current instructions can be returned")
    })?;
    if !orientation.instructions_complete {
        return Err(Failure::invalid(
            "orientation is incomplete; reconnect and read all required instruction sections before claiming",
        ));
    }

    if state.acknowledged.as_ref().map(|version| {
        (
            version.policy_revision,
            &version.instruction_version,
            &version.sections,
        )
    }) != Some((
        orientation.policy_revision,
        &orientation.instruction_version,
        &orientation.sections,
    )) {
        let acknowledge_path = format!(
            "/api/v1/sessions/{}/instruction-acknowledgments",
            state.session.id
        );
        let acknowledge_body = json!({
            "project_id": context.binding.project_id,
            "policy_revision": orientation.policy_revision,
            "instruction_version": orientation.instruction_version,
            "sections": orientation.sections,
        });
        let response = persist_and_send(
            context,
            &session_path,
            &mut state,
            HttpMethod::Post,
            &acknowledge_path,
            acknowledge_body,
            true,
        )
        .await?;
        require_success(response)?;
        state.acknowledged = Some(orientation.clone());
        state::save(&session_path, &state).map_err(Failure::invalid)?;
    }

    let mut body = Map::new();
    body.insert("mode".into(), Value::String(args.mode.as_str().to_owned()));
    body.insert(
        "policy_revision".into(),
        Value::Number(orientation.policy_revision.into()),
    );
    body.insert(
        "instruction_version".into(),
        Value::String(orientation.instruction_version),
    );
    if let Some(task) = &args.task {
        body.insert("task_id".into(), Value::String(task.clone()));
        body.insert(
            "expected_task_revision".into(),
            Value::Number(args.revision.expect("validated above").into()),
        );
    }
    let path = format!("/api/v1/projects/{}/claims", context.binding.project_id);
    let response = persist_and_send(
        context,
        &session_path,
        &mut state,
        HttpMethod::Post,
        &path,
        Value::Object(body),
        true,
    )
    .await?;
    require_success(response)
}

async fn request(
    cli: &Cli,
    context: &ContextData,
    args: &RequestArgs,
) -> std::result::Result<Value, Failure> {
    match args.method {
        RequestMethod::Get => {
            if args.input.is_some() {
                return Err(Failure::invalid("--input is not accepted for GET requests"));
            }
            let optional = optional_state(cli, context)?;
            let session = optional.as_ref().map(|(_, _, state)| &state.session);
            finish(context.client.get(&args.path, session).await)
        }
        RequestMethod::Post => {
            let input = args
                .input
                .as_ref()
                .ok_or_else(|| Failure::invalid("--input is required for POST requests"))?;
            let body = read_json(input).map_err(Failure::invalid)?;
            mutate(cli, context, &args.path, body, true).await
        }
        RequestMethod::Patch => {
            let input = args
                .input
                .as_ref()
                .ok_or_else(|| Failure::invalid("--input is required for PATCH requests"))?;
            let body = read_json(input).map_err(Failure::invalid)?;
            mutate_method(cli, context, HttpMethod::Patch, &args.path, body, true).await
        }
    }
}

async fn retry(cli: &Cli, context: &ContextData) -> std::result::Result<Value, Failure> {
    let (_session_lock, path, mut state) = load_required_state(cli, context)?;
    if state.pending.is_none() {
        return Err(Failure::invalid(
            "this harness session has no pending mutation",
        ));
    }
    let pending = state.pending.clone().expect("checked above");
    let pending_path = Some(pending.path.clone());
    let job_intent = if is_job_registration(&pending.path) {
        let job_id = pending
            .body
            .get("job_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Failure::invalid("saved job registration omitted job_id"))?;
        Some(job_state::load_by_job(job_id).map_err(Failure::invalid)?)
    } else {
        None
    };
    let response = send_saved_policy(context, &path, &mut state, job_intent.is_none()).await?;
    if response.is_success()
        && let Some(intent) = &job_intent
    {
        record_job_registration(context, intent, &response.body)?;
        reconcile_remote_job(context, &state.session, intent).await?;
        state.pending = None;
        state::save(&path, &state).map_err(Failure::invalid)?;
    }
    if response.is_success()
        && pending_path
            .as_deref()
            .is_some_and(|path| path.ends_with("/instruction-acknowledgments"))
    {
        state.acknowledged = state.orientation.clone();
        state::save(&path, &state).map_err(Failure::invalid)?;
    }
    require_success(response)
}

fn is_job_registration(path: &str) -> bool {
    path.starts_with("/api/v1/projects/") && path.contains("/attempts/") && path.ends_with("/jobs")
}

async fn mutate(
    cli: &Cli,
    context: &ContextData,
    path: &str,
    body: Value,
    include_session_id: bool,
) -> std::result::Result<Value, Failure> {
    mutate_method(
        cli,
        context,
        HttpMethod::Post,
        path,
        body,
        include_session_id,
    )
    .await
}

async fn mutate_method(
    cli: &Cli,
    context: &ContextData,
    method: HttpMethod,
    path: &str,
    body: Value,
    include_session_id: bool,
) -> std::result::Result<Value, Failure> {
    let (_session_lock, session_path, mut state) = load_required_state(cli, context)?;
    let response = persist_and_send(
        context,
        &session_path,
        &mut state,
        method,
        path,
        body,
        include_session_id,
    )
    .await?;
    require_success(response)
}

async fn persist_and_send(
    context: &ContextData,
    session_path: &Path,
    state: &mut SessionState,
    method: HttpMethod,
    path: &str,
    body: Value,
    include_session_id: bool,
) -> std::result::Result<ApiResponse, Failure> {
    let desired = PendingMutation {
        key: Uuid::new_v4().to_string(),
        method,
        path: path.to_owned(),
        body,
        include_session_id,
    };
    match &state.pending {
        Some(existing)
            if existing.method == desired.method
                && existing.path == desired.path
                && existing.body == desired.body
                && existing.include_session_id == desired.include_session_id => {}
        _ => {
            state.set_pending(desired).map_err(Failure::invalid)?;
            state::save(session_path, state).map_err(Failure::invalid)?;
        }
    }
    send_saved(context, session_path, state).await
}

async fn send_saved(
    context: &ContextData,
    session_path: &Path,
    state: &mut SessionState,
) -> std::result::Result<ApiResponse, Failure> {
    send_saved_policy(context, session_path, state, true).await
}

async fn send_saved_policy(
    context: &ContextData,
    session_path: &Path,
    state: &mut SessionState,
    clear_success: bool,
) -> std::result::Result<ApiResponse, Failure> {
    let pending = state
        .pending
        .clone()
        .ok_or_else(|| Failure::invalid("no pending mutation is saved"))?;
    let response = context
        .client
        .request(
            pending.method,
            &pending.path,
            Some(&pending.body),
            Some(&pending.key),
            Some(&state.session),
            pending.include_session_id,
        )
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            // The pending request was saved before I/O and deliberately remains
            // intact because the service may have committed before connection loss.
            return Err(client_failure(error));
        }
    };
    if mutation_response_is_definitive(response.status) && (clear_success || !response.is_success())
    {
        state.pending = None;
        state::save(session_path, state).map_err(Failure::invalid)?;
    }
    Ok(response)
}

fn mutation_response_is_definitive(status: u16) -> bool {
    !(status == 429 || (300..400).contains(&status) || status >= 500)
}

fn load_required_state(
    cli: &Cli,
    context: &ContextData,
) -> std::result::Result<(state::SessionLock, PathBuf, SessionState), Failure> {
    let local_session = required_session(cli)?;
    let path = state::path_for(&context.origin, &context.binding.project_id, local_session)
        .map_err(Failure::invalid)?;
    let lock = state::lock(&path).map_err(Failure::temporary)?;
    let state = state::load(&path)
        .map_err(Failure::invalid)?
        .ok_or_else(|| {
            Failure::invalid(format!(
                "no local state exists for session {local_session}; run connect first"
            ))
        })?;
    validate_state(context, local_session, &state)?;
    Ok((lock, path, state))
}

fn optional_state(
    cli: &Cli,
    context: &ContextData,
) -> std::result::Result<Option<(state::SessionLock, PathBuf, SessionState)>, Failure> {
    if cli.session.is_none() {
        return Ok(None);
    }
    load_required_state(cli, context).map(Some)
}

fn validate_state(
    context: &ContextData,
    local_session: &str,
    state: &SessionState,
) -> std::result::Result<(), Failure> {
    if state.service_origin != context.origin
        || state.project_id != context.binding.project_id
        || state.local_session != local_session
        || state.credential_digest != context.credential_digest
    {
        return Err(Failure::invalid(
            "local session state does not match the repository binding and requested session",
        ));
    }
    Ok(())
}

fn required_session(cli: &Cli) -> std::result::Result<&str, Failure> {
    let session = cli.session.as_deref().ok_or_else(|| {
        Failure::invalid(
            "--session (or AGENT_COORDINATOR_SESSION) is required so independent harnesses never share ownership",
        )
    })?;
    if session.trim().is_empty() {
        return Err(Failure::invalid("--session must not be empty"));
    }
    Ok(session)
}

fn parse_orientation(body: &Value) -> Option<OrientationVersion> {
    let data = body.get("data")?;
    Some(OrientationVersion {
        policy_revision: data.get("policy_revision")?.as_u64()?,
        instruction_version: data.get("instruction_version")?.as_str()?.to_owned(),
        sections: data
            .get("required_sections")?
            .as_array()?
            .iter()
            .map(|value| value.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?,
        instructions_complete: data.get("instructions_complete")?.as_bool()?,
    })
}

fn data(value: &Value) -> Value {
    value.get("data").cloned().unwrap_or_else(|| value.clone())
}

fn require_success(response: ApiResponse) -> std::result::Result<Value, Failure> {
    if response.is_success() {
        Ok(response.body)
    } else {
        Err(Failure {
            exit: exit_for_status(response.status),
            output: response.body,
        })
    }
}

fn finish(
    response: std::result::Result<ApiResponse, ClientError>,
) -> std::result::Result<Value, Failure> {
    require_success(response.map_err(client_failure)?)
}

fn exit_for_status(status: u16) -> u8 {
    match status {
        401 => 3,
        403 => 4,
        409 => 5,
        422 => 6,
        429 | 500..=599 | 300..=399 => 7,
        _ => 2,
    }
}

fn client_failure(error: ClientError) -> Failure {
    match error {
        ClientError::Transport(_) | ClientError::InvalidResponse(_) => Failure::temporary(error),
        _ => Failure::invalid(error),
    }
}

fn read_json(path: &Path) -> Result<Value> {
    let mut input = String::new();
    if path == Path::new("-") {
        use std::io::Read;
        std::io::stdin()
            .read_to_string(&mut input)
            .context("read JSON from standard input")?;
    } else {
        input = std::fs::read_to_string(path)
            .with_context(|| format!("read JSON input {}", path.display()))?;
    }
    let value: Value = serde_json::from_str(&input).context("parse JSON input")?;
    if !value.is_object() {
        bail!("JSON input must be an object");
    }
    Ok(value)
}

fn input_with_generation(path: &Path, generation: u64) -> Result<Value> {
    let mut body = read_json(path)?;
    let object = body
        .as_object_mut()
        .ok_or_else(|| anyhow!("JSON input must be an object"))?;
    if let Some(input_generation) = object.get("generation")
        && input_generation.as_u64() != Some(generation)
    {
        bail!("generation in --input does not match --generation");
    }
    object.insert("generation".into(), Value::Number(generation.into()));
    Ok(body)
}

fn attempt_path(context: &ContextData, attempt: &str, operation: &str) -> Result<String> {
    validate_segment("attempt", attempt)?;
    Ok(format!(
        "/api/v1/projects/{}/attempts/{attempt}/{operation}",
        context.binding.project_id
    ))
}

fn validate_segment(name: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('?')
        || value.contains('#')
    {
        bail!("{name} must be one non-empty URL path segment");
    }
    Ok(())
}

fn list_query(args: &ListArgs) -> Vec<(&str, String)> {
    let mut query = Vec::new();
    if let Some(cursor) = &args.cursor {
        query.push(("cursor", cursor.clone()));
    }
    if let Some(limit) = args.limit {
        query.push(("limit", limit.to_string()));
    }
    query
}

fn default_workstation() -> String {
    env::var("HOSTNAME")
        .or_else(|_| env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown-workstation".into())
}

fn random_secret() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| anyhow!("generate session proof: {error}"))?;
    Ok(hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_service_responses_keep_pending_mutation() {
        assert!(!mutation_response_is_definitive(302));
        assert!(!mutation_response_is_definitive(429));
        assert!(!mutation_response_is_definitive(503));
        assert!(mutation_response_is_definitive(201));
        assert!(mutation_response_is_definitive(409));
    }

    #[test]
    fn status_codes_follow_cli_contract() {
        assert_eq!(exit_for_status(401), 3);
        assert_eq!(exit_for_status(403), 4);
        assert_eq!(exit_for_status(409), 5);
        assert_eq!(exit_for_status(422), 6);
        assert_eq!(exit_for_status(503), 7);
    }
}
