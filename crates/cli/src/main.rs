mod config;
mod job_state;
mod state;
mod worktree;

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

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
    /// Inspect the human-managed required-check roster.
    Checks {
        #[command(subcommand)]
        command: ChecksCommand,
    },
    /// Submit immutable implementation or general-work evidence.
    Submissions {
        #[command(subcommand)]
        command: SubmissionsCommand,
    },
    /// List, claim, and decide review activities.
    Reviews {
        #[command(subcommand)]
        command: ReviewsCommand,
    },
    /// List, claim, publish, and finish integration activities.
    Integrations {
        #[command(subcommand)]
        command: IntegrationsCommand,
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

#[derive(Subcommand)]
enum ChecksCommand {
    /// Show the exact required identity, version, and environment tuples.
    List,
}

#[derive(Subcommand)]
enum SubmissionsCommand {
    /// Submit a clean committed code candidate from the attempt's prepared checkout.
    Code(CodeSubmissionArgs),
    /// Submit acceptance evidence for a general task without Git fields.
    General(GeneralSubmissionArgs),
}

#[derive(Subcommand)]
enum ReviewsCommand {
    /// List review activities for one subject task without claiming them.
    List(WorkflowListArgs),
    /// Inspect one review activity without claiming it.
    Status(ActivityArgs),
    /// Claim one exact review activity and immutable submission.
    Claim(ActivityClaimArgs),
    /// Renew a claimed review activity.
    Renew(ActivityAttemptArgs),
    /// Release a claimed review activity with a JSON handoff.
    Release(ActivityAttemptInputArgs),
    /// Record an immutable agent review decision.
    Decide(ReviewDecisionArgs),
}

#[derive(Subcommand)]
enum IntegrationsCommand {
    /// List integration activities for one subject task without claiming them.
    List(WorkflowListArgs),
    /// Inspect one integration activity without claiming it.
    Status(ActivityArgs),
    /// Claim one exact approved integration activity and immutable submission.
    Claim(ActivityClaimArgs),
    /// Renew a claimed integration activity.
    Renew(ActivityAttemptArgs),
    /// Release a claimed integration activity with a JSON handoff.
    Release(ActivityAttemptInputArgs),
    /// Prepare an exact local integration result and register publication intent.
    Prepare(IntegrationPrepareArgs),
    /// Publish the prepared result with a fresh authority check and Git CAS.
    Publish(IntegrationOperationArgs),
    /// Observe the remote target after an interrupted or uncertain publication.
    Reconcile(IntegrationOperationArgs),
    /// Register exact publication and check-job receipts and finish integration.
    Finish(IntegrationFinishArgs),
}

#[derive(Args)]
struct CodeSubmissionArgs {
    #[arg(long)]
    attempt: String,
    #[arg(long)]
    generation: u64,
    #[arg(long)]
    task_revision: u64,
    #[arg(long)]
    project_policy_revision: u64,
    #[arg(long)]
    workflow_policy_revision: u64,
    /// Prepared worktree registered for this attempt.
    #[arg(long)]
    checkout: PathBuf,
    /// JSON evidence without kind or Git identity fields.
    #[arg(long)]
    input: PathBuf,
}

#[derive(Args)]
struct GeneralSubmissionArgs {
    #[arg(long)]
    attempt: String,
    #[arg(long)]
    generation: u64,
    #[arg(long)]
    task_revision: u64,
    #[arg(long)]
    project_policy_revision: u64,
    /// JSON summary, acceptance evidence, and handoff without Git fields.
    #[arg(long)]
    input: PathBuf,
}

#[derive(Args)]
struct WorkflowListArgs {
    /// Subject task whose current workflow activities should be listed.
    #[arg(long)]
    task: String,
}

#[derive(Args)]
struct ActivityArgs {
    #[arg(long)]
    activity: String,
}

#[derive(Args)]
struct ActivityClaimArgs {
    #[arg(long)]
    activity: String,
    #[arg(long)]
    submission: String,
    #[arg(long)]
    project_policy_revision: u64,
    #[arg(long)]
    workflow_policy_revision: u64,
}

#[derive(Args)]
struct ActivityAttemptArgs {
    #[arg(long)]
    activity: String,
    #[arg(long)]
    attempt: String,
    #[arg(long)]
    generation: u64,
}

#[derive(Args)]
struct ActivityAttemptInputArgs {
    #[command(flatten)]
    authority: ActivityAttemptArgs,
    /// JSON handoff, or - for standard input.
    #[arg(long)]
    input: PathBuf,
}

#[derive(Args)]
struct ReviewDecisionArgs {
    #[command(flatten)]
    authority: ActivityAttemptArgs,
    #[arg(long)]
    submission: String,
    /// JSON decision, summary, and findings, or - for standard input.
    #[arg(long)]
    input: PathBuf,
}

#[derive(Args)]
struct IntegrationPrepareArgs {
    #[command(flatten)]
    authority: ActivityAttemptArgs,
    #[arg(long)]
    submission: String,
    /// Isolated checkout registered for the integration attempt.
    #[arg(long)]
    checkout: PathBuf,
    /// Exact full remote target revision expected before publication.
    #[arg(long)]
    expected_target: String,
    /// Exact full candidate revision from the immutable submission.
    #[arg(long)]
    candidate: String,
}

#[derive(Args)]
struct IntegrationOperationArgs {
    #[arg(long)]
    activity: String,
    #[arg(long)]
    attempt: String,
    #[arg(long)]
    generation: u64,
}

#[derive(Args)]
struct IntegrationFinishArgs {
    #[command(flatten)]
    authority: IntegrationOperationArgs,
    /// JSON summary and check_job_ids, or - for standard input.
    #[arg(long)]
    input: PathBuf,
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
    /// Integration activity whose prepared result is checked by this job.
    #[arg(long)]
    activity: Option<String>,
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
        Command::Checks { command } => match command {
            ChecksCommand::List => {
                let path = format!(
                    "/api/v1/projects/{}/workflow-policy",
                    context.binding.project_id
                );
                let optional = optional_state(cli, &context)?;
                let session = optional.as_ref().map(|(_, _, state)| &state.session);
                finish(context.client.get(&path, session).await)
            }
        },
        Command::Submissions { command } => submissions_command(cli, &context, command).await,
        Command::Reviews { command } => reviews_command(cli, &context, command).await,
        Command::Integrations { command } => integrations_command(cli, &context, command).await,
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

async fn submissions_command(
    cli: &Cli,
    context: &ContextData,
    command: &SubmissionsCommand,
) -> std::result::Result<Value, Failure> {
    match command {
        SubmissionsCommand::Code(args) => submit_code(cli, context, args).await,
        SubmissionsCommand::General(args) => {
            validate_segment("attempt", &args.attempt).map_err(Failure::invalid)?;
            let mut body = submission_evidence(&args.input).map_err(Failure::invalid)?;
            insert_fields(
                &mut body,
                [
                    ("generation", json!(args.generation)),
                    ("task_revision", json!(args.task_revision)),
                    (
                        "project_policy_revision",
                        json!(args.project_policy_revision),
                    ),
                    ("workflow_policy_revision", json!(0)),
                    ("kind", json!("general")),
                ],
            )
            .map_err(Failure::invalid)?;
            let path =
                attempt_path(context, &args.attempt, "submissions").map_err(Failure::invalid)?;
            mutate(cli, context, &path, body, true).await
        }
    }
}

async fn submit_code(
    cli: &Cli,
    context: &ContextData,
    args: &CodeSubmissionArgs,
) -> std::result::Result<Value, Failure> {
    validate_segment("attempt", &args.attempt).map_err(Failure::invalid)?;
    let (_session_lock, session_path, mut session) = load_required_state(cli, context)?;
    let detail_path = format!(
        "/api/v1/projects/{}/attempts/{}",
        context.binding.project_id, args.attempt
    );
    let attempt = finish(
        context
            .client
            .get(&detail_path, Some(&session.session))
            .await,
    )?;
    require_current_work_attempt(&attempt, args.generation)?;
    let prepared =
        worktree::load_for_attempt(&context.origin, &context.binding.project_id, &args.attempt)
            .map_err(Failure::invalid)?;
    if prepared.generation != args.generation {
        return Err(Failure::local(
            5,
            "stale_generation",
            "the prepared worktree belongs to a different attempt generation",
            false,
        ));
    }
    let checkout = std::fs::canonicalize(&args.checkout).map_err(Failure::invalid)?;
    if checkout != prepared.destination {
        return Err(Failure::invalid(
            "--checkout does not match the saved worktree for this attempt",
        ));
    }
    let registered = registered_checkout(&attempt)?.ok_or_else(|| {
        Failure::local(
            5,
            "checkout_not_registered",
            "register the prepared worktree before submitting code",
            false,
        )
    })?;
    if registered.get("path").and_then(Value::as_str) != prepared.destination.to_str() {
        return Err(Failure::local(
            5,
            "checkout_mismatch",
            "the attempt's registered checkout does not match the prepared local worktree",
            false,
        ));
    }
    let (candidate_revision, candidate_tree) =
        worktree::current_snapshot(&prepared).map_err(Failure::invalid)?;
    let project = bound_project(context, Some(&session.session)).await?;
    let repository = project
        .get("repository_url")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::temporary("project response omitted repository_url"))?;
    if repository != prepared.repository_url {
        return Err(Failure::local(
            5,
            "repository_binding_changed",
            "the prepared worktree no longer matches the configured repository",
            false,
        ));
    }
    let mut body = submission_evidence(&args.input).map_err(Failure::invalid)?;
    insert_fields(
        &mut body,
        [
            ("generation", json!(args.generation)),
            ("task_revision", json!(args.task_revision)),
            (
                "project_policy_revision",
                json!(args.project_policy_revision),
            ),
            (
                "workflow_policy_revision",
                json!(args.workflow_policy_revision),
            ),
            ("kind", json!("code")),
            ("repository", json!(repository)),
            ("base_revision", json!(prepared.base_revision)),
            ("candidate_revision", json!(candidate_revision)),
            ("candidate_tree", json!(candidate_tree)),
        ],
    )
    .map_err(Failure::invalid)?;
    let path = attempt_path(context, &args.attempt, "submissions").map_err(Failure::invalid)?;
    let response = persist_and_send(
        context,
        &session_path,
        &mut session,
        HttpMethod::Post,
        &path,
        body,
        true,
    )
    .await?;
    require_success(response)
}

fn submission_evidence(path: &Path) -> Result<Value> {
    let body = read_json(path)?;
    reject_fields(
        &body,
        &[
            "generation",
            "task_revision",
            "project_policy_revision",
            "workflow_policy_revision",
            "kind",
            "repository",
            "base_revision",
            "candidate_revision",
            "candidate_tree",
        ],
    )?;
    Ok(body)
}

async fn reviews_command(
    cli: &Cli,
    context: &ContextData,
    command: &ReviewsCommand,
) -> std::result::Result<Value, Failure> {
    match command {
        ReviewsCommand::List(args) => workflow_list(cli, context, args, "review").await,
        ReviewsCommand::Status(args) => activity_status(cli, context, &args.activity).await,
        ReviewsCommand::Claim(args) => activity_claim(cli, context, args).await,
        ReviewsCommand::Renew(args) => activity_renew(cli, context, args, "review").await,
        ReviewsCommand::Release(args) => activity_release(cli, context, args, "review").await,
        ReviewsCommand::Decide(args) => {
            let mut body = read_json(&args.input).map_err(Failure::invalid)?;
            reject_fields(&body, &["generation", "submission_id"]).map_err(Failure::invalid)?;
            insert_fields(
                &mut body,
                [
                    ("generation", json!(args.authority.generation)),
                    ("submission_id", json!(args.submission)),
                ],
            )
            .map_err(Failure::invalid)?;
            activity_mutation(cli, context, &args.authority.activity, "review", body).await
        }
    }
}

async fn integrations_command(
    cli: &Cli,
    context: &ContextData,
    command: &IntegrationsCommand,
) -> std::result::Result<Value, Failure> {
    match command {
        IntegrationsCommand::List(args) => workflow_list(cli, context, args, "integration").await,
        IntegrationsCommand::Status(args) => activity_status(cli, context, &args.activity).await,
        IntegrationsCommand::Claim(args) => activity_claim(cli, context, args).await,
        IntegrationsCommand::Renew(args) => activity_renew(cli, context, args, "integration").await,
        IntegrationsCommand::Release(args) => {
            activity_release(cli, context, args, "integration").await
        }
        IntegrationsCommand::Prepare(args) => prepare_integration(cli, context, args).await,
        IntegrationsCommand::Publish(args) => publish_integration(cli, context, args).await,
        IntegrationsCommand::Reconcile(args) => reconcile_integration(cli, context, args).await,
        IntegrationsCommand::Finish(args) => finish_integration(cli, context, args).await,
    }
}

async fn prepare_integration(
    cli: &Cli,
    context: &ContextData,
    args: &IntegrationPrepareArgs,
) -> std::result::Result<Value, Failure> {
    validate_segment("activity", &args.authority.activity).map_err(Failure::invalid)?;
    validate_segment("attempt", &args.authority.attempt).map_err(Failure::invalid)?;
    validate_segment("submission", &args.submission).map_err(Failure::invalid)?;
    let (_session_lock, session_path, mut session) = load_required_state(cli, context)?;
    let publication_intent_path =
        activity_operation_path(context, &args.authority.activity, "publication-intent")?;
    if let Some(pending) = &session.pending
        && (pending.method != HttpMethod::Post
            || pending.path != publication_intent_path
            || !pending.include_session_id)
    {
        return Err(Failure::invalid(format!(
            "an earlier mutation is unresolved ({}); run `agent-coordinator retry` before changing local Git state",
            pending.path
        )));
    }
    let activity_body =
        remote_integration_context(context, &session.session, &args.authority.activity).await?;
    let submission =
        require_integration_activity(&activity_body, &args.authority, Some(&args.submission))?;
    if submission.get("candidate_revision").and_then(Value::as_str) != Some(&args.candidate) {
        return Err(Failure::local(
            5,
            "candidate_mismatch",
            "--candidate does not match the immutable submission",
            false,
        ));
    }
    let candidate_base = submission
        .get("base_revision")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::temporary("submission omitted base_revision"))?;
    require_fresh_attempt(context, &session.session, &args.authority).await?;
    let project = bound_project(context, Some(&session.session)).await?;
    let repository = project
        .get("repository_url")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::temporary("project response omitted repository_url"))?;
    let target_branch = project
        .get("target_branch")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::temporary("project response omitted target_branch"))?;
    if submission.get("repository").and_then(Value::as_str) != Some(repository)
        || submission.get("target_branch").and_then(Value::as_str) != Some(target_branch)
    {
        return Err(Failure::local(
            5,
            "submission_repository_mismatch",
            "the immutable submission does not match the configured repository target",
            false,
        ));
    }
    let prepared_worktree = worktree::load_for_attempt(
        &context.origin,
        &context.binding.project_id,
        &args.authority.attempt,
    )
    .map_err(Failure::invalid)?;
    if prepared_worktree.generation != args.authority.generation {
        return Err(Failure::local(
            5,
            "stale_generation",
            "the prepared worktree belongs to another attempt generation",
            false,
        ));
    }
    let checkout = std::fs::canonicalize(&args.checkout).map_err(Failure::invalid)?;
    if checkout != prepared_worktree.destination {
        return Err(Failure::invalid(
            "--checkout does not match the saved worktree for this integration attempt",
        ));
    }
    let state_file =
        integration_state_file(context, &args.authority.activity, &args.authority.attempt)
            .map_err(Failure::invalid)?;
    coordinator_local::git_workflow::prepare_integration(
        coordinator_local::git_workflow::PrepareIntegration {
            state_file: &state_file,
            checkout: &checkout,
            configured_remote: repository,
            target_branch,
            expected_target: &args.expected_target,
            candidate_base,
            candidate: &args.candidate,
        },
    )
    .map_err(Failure::invalid)?;
    let prepared =
        coordinator_local::git_workflow::materialize_prepared_result(&state_file, repository)
            .map_err(Failure::invalid)?;
    if submission.get("candidate_tree").and_then(Value::as_str)
        != Some(prepared.candidate_tree.as_str())
    {
        return Err(Failure::local(
            5,
            "candidate_tree_mismatch",
            "the local candidate tree does not match the immutable submission",
            false,
        ));
    }
    let result = prepared
        .result
        .as_deref()
        .ok_or_else(|| Failure::temporary("local integration preparation omitted result"))?;
    let result_tree = prepared
        .result_tree
        .as_deref()
        .ok_or_else(|| Failure::temporary("local integration preparation omitted result tree"))?;
    let body = json!({
        "generation": args.authority.generation,
        "submission_id": args.submission,
        "observed_target_revision": prepared.expected_target,
        "result_revision": result,
        "result_tree": result_tree
    });
    if let Some(existing) = activity_record(&activity_body)
        .get("intent")
        .filter(|value| !value.is_null())
    {
        require_matching_fields(
            existing,
            &body,
            &["observed_target_revision", "result_revision", "result_tree"],
        )
        .map_err(Failure::invalid)?;
        if let Some(pending) = &session.pending {
            if pending.body != body {
                return Err(Failure::invalid(
                    "saved publication-intent request does not match the local integration intent",
                ));
            }
            session.pending = None;
            state::save(&session_path, &session).map_err(Failure::invalid)?;
        }
        return Ok(json!({
            "data": {
                "integration": prepared,
                "publication_intent": existing,
                "reconciled": true
            }
        }));
    }
    let response = persist_and_send(
        context,
        &session_path,
        &mut session,
        HttpMethod::Post,
        &publication_intent_path,
        body,
        true,
    )
    .await?;
    let remote = require_success(response)?;
    Ok(json!({"data":{"integration":prepared,"publication_intent":data(&remote)}}))
}

async fn publish_integration(
    cli: &Cli,
    context: &ContextData,
    args: &IntegrationOperationArgs,
) -> std::result::Result<Value, Failure> {
    let (_session_lock, _, session) = load_required_state(cli, context)?;
    if let Some(pending) = &session.pending {
        return Err(Failure::invalid(format!(
            "an earlier mutation is unresolved ({}); run `agent-coordinator retry` before publishing",
            pending.path
        )));
    }
    let project = bound_project(context, Some(&session.session)).await?;
    let repository = project
        .get("repository_url")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::temporary("project response omitted repository_url"))?
        .to_owned();
    let state_file =
        integration_state_file(context, &args.activity, &args.attempt).map_err(Failure::invalid)?;
    let activity = args.activity.clone();
    let authority = ActivityAttemptArgs {
        activity: args.activity.clone(),
        attempt: args.attempt.clone(),
        generation: args.generation,
    };
    let session_auth = &session.session;
    let outcome = coordinator_local::git_workflow::publish_prepared(
        &state_file,
        &repository,
        |publication| async move {
            validate_publication_authority(
                context,
                session_auth,
                &activity,
                &authority,
                &publication,
            )
            .await
        },
    )
    .await
    .map_err(Failure::temporary)?;
    Ok(json!({"data":{"publication":outcome}}))
}

async fn reconcile_integration(
    cli: &Cli,
    context: &ContextData,
    args: &IntegrationOperationArgs,
) -> std::result::Result<Value, Failure> {
    let (_, _, session) = load_required_state(cli, context)?;
    let project = bound_project(context, Some(&session.session)).await?;
    let repository = project
        .get("repository_url")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::temporary("project response omitted repository_url"))?;
    let state_file =
        integration_state_file(context, &args.activity, &args.attempt).map_err(Failure::invalid)?;
    let outcome = coordinator_local::git_workflow::reconcile_publication(&state_file, repository)
        .map_err(Failure::temporary)?;
    Ok(json!({"data":{"publication":outcome}}))
}

async fn finish_integration(
    cli: &Cli,
    context: &ContextData,
    args: &IntegrationFinishArgs,
) -> std::result::Result<Value, Failure> {
    validate_segment("activity", &args.authority.activity).map_err(Failure::invalid)?;
    validate_segment("attempt", &args.authority.attempt).map_err(Failure::invalid)?;
    let supplied = read_json(&args.input).map_err(Failure::invalid)?;
    allow_fields(&supplied, &["submission_id", "check_job_ids", "summary"])
        .map_err(Failure::invalid)?;
    let expected_submission_id = supplied
        .get("submission_id")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| anyhow!("submission_id must be one string"))
        })
        .transpose()
        .map_err(Failure::invalid)?;
    if let Some(submission_id) = expected_submission_id {
        validate_segment("submission_id", submission_id).map_err(Failure::invalid)?;
    }
    let check_job_ids = supplied
        .get("check_job_ids")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
        .ok_or_else(|| Failure::invalid("finish input requires a non-empty check_job_ids array"))?
        .clone();
    if check_job_ids.iter().any(|id| {
        id.as_str()
            .is_none_or(|id| validate_segment("check job", id).is_err())
    }) {
        return Err(Failure::invalid(
            "every check_job_ids entry must be one job ID",
        ));
    }
    let summary = supplied
        .get("summary")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Failure::invalid("finish input requires a non-empty summary"))?
        .to_owned();
    let (_session_lock, session_path, mut session) = load_required_state(cli, context)?;
    let activity_authority = ActivityAttemptArgs {
        activity: args.authority.activity.clone(),
        attempt: args.authority.attempt.clone(),
        generation: args.authority.generation,
    };
    let activity_body =
        remote_integration_context(context, &session.session, &args.authority.activity).await?;
    let submission = require_integration_activity(&activity_body, &activity_authority, None)?;
    let submission_id = submission
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::temporary("workflow response omitted submission.id"))?
        .to_owned();
    validate_segment("submission_id", &submission_id).map_err(Failure::invalid)?;
    if expected_submission_id.is_some_and(|expected| expected != submission_id) {
        return Err(Failure::local(
            5,
            "submission_mismatch",
            "submission_id does not match the integration activity's immutable submission",
            false,
        ));
    }
    let project = bound_project(context, Some(&session.session)).await?;
    let repository = project
        .get("repository_url")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::temporary("project response omitted repository_url"))?;
    let state_file =
        integration_state_file(context, &args.authority.activity, &args.authority.attempt)
            .map_err(Failure::invalid)?;
    let publication =
        coordinator_local::git_workflow::reconcile_publication(&state_file, repository)
            .map_err(Failure::temporary)?;
    let coordinator_local::git_workflow::PublicationOutcome::Published { revision } = publication
    else {
        return Err(Failure::local(
            5,
            "publication_not_confirmed",
            "the configured remote does not confirm the exact prepared result",
            false,
        ));
    };
    let intent = coordinator_local::git_workflow::load_integration_intent(&state_file)
        .map_err(Failure::invalid)?;
    let result_tree = intent
        .result_tree
        .as_deref()
        .ok_or_else(|| Failure::temporary("integration intent omitted result_tree"))?;
    let result_body = json!({
        "generation": args.authority.generation,
        "submission_id": submission_id,
        "publication_state": "published",
        "observed_target_revision": intent.expected_target,
        "result_revision": revision,
        "result_tree": result_tree,
        "check_job_ids": check_job_ids,
        "summary": summary
    });
    let existing_result = activity_record(&activity_body)
        .get("result")
        .filter(|value| !value.is_null());
    if let Some(existing) = existing_result {
        require_matching_fields(
            existing,
            &result_body,
            &[
                "publication_state",
                "observed_target_revision",
                "result_revision",
                "result_tree",
                "check_job_ids",
                "summary",
            ],
        )
        .map_err(Failure::invalid)?;
        if let Some(pending) = &session.pending {
            let result_path =
                activity_operation_path(context, &args.authority.activity, "integration-result")?;
            if pending.path == result_path {
                if pending.method != HttpMethod::Post
                    || pending.body != result_body
                    || !pending.include_session_id
                {
                    return Err(Failure::invalid(
                        "saved integration-result request does not match the local result and selected checks",
                    ));
                }
                session.pending = None;
                state::save(&session_path, &session).map_err(Failure::invalid)?;
            }
        }
    } else {
        let response = persist_and_send(
            context,
            &session_path,
            &mut session,
            HttpMethod::Post,
            &activity_operation_path(context, &args.authority.activity, "integration-result")?,
            result_body,
            true,
        )
        .await?;
        require_success(response)?;
    }
    let observation = coordinator_local::git_workflow::observe_remote_target(
        &intent.checkout,
        repository,
        &intent.target_branch,
    )
    .map_err(Failure::temporary)?;
    if observation.revision.as_deref() != Some(&revision) {
        return Err(Failure::local(
            5,
            "published_target_changed",
            "the remote target moved before finalization",
            false,
        ));
    }
    let finalize_body = json!({
        "generation": args.authority.generation,
        "submission_id": submission_id,
        "observed_target_revision": revision,
        "observed_target_tree": result_tree
    });
    let response = persist_and_send(
        context,
        &session_path,
        &mut session,
        HttpMethod::Post,
        &activity_operation_path(context, &args.authority.activity, "finalize")?,
        finalize_body,
        true,
    )
    .await?;
    require_success(response)
}

async fn workflow_list(
    cli: &Cli,
    context: &ContextData,
    args: &WorkflowListArgs,
    activity_kind: &str,
) -> std::result::Result<Value, Failure> {
    validate_segment("task", &args.task).map_err(Failure::invalid)?;
    let (_, _, session) = load_required_state(cli, context)?;
    let path = format!(
        "/api/v1/projects/{}/tasks/{}/workflow",
        context.binding.project_id, args.task
    );
    let mut body = finish(context.client.get(&path, Some(&session.session)).await)?;
    if let Some(activities) = body
        .pointer_mut("/data/activities")
        .and_then(Value::as_array_mut)
    {
        activities.retain(|activity| {
            let kind = activity.get("kind").and_then(Value::as_str);
            match activity_kind {
                "review" => matches!(kind, Some("agent_review" | "human_review")),
                expected => kind == Some(expected),
            }
        });
    }
    Ok(body)
}

async fn activity_status(
    cli: &Cli,
    context: &ContextData,
    activity: &str,
) -> std::result::Result<Value, Failure> {
    validate_segment("activity", activity).map_err(Failure::invalid)?;
    let (_, _, session) = load_required_state(cli, context)?;
    let path = format!(
        "/api/v1/projects/{}/workflow-activities/{activity}",
        context.binding.project_id
    );
    finish(context.client.get(&path, Some(&session.session)).await)
}

async fn activity_claim(
    cli: &Cli,
    context: &ContextData,
    args: &ActivityClaimArgs,
) -> std::result::Result<Value, Failure> {
    activity_mutation(
        cli,
        context,
        &args.activity,
        "claim",
        json!({
            "expected_submission_id": args.submission,
            "expected_project_policy_revision": args.project_policy_revision,
            "expected_workflow_policy_revision": args.workflow_policy_revision
        }),
    )
    .await
}

async fn activity_renew(
    cli: &Cli,
    context: &ContextData,
    args: &ActivityAttemptArgs,
    expected_kind: &str,
) -> std::result::Result<Value, Failure> {
    require_activity_attempt(cli, context, args, expected_kind).await?;
    let path = attempt_path(context, &args.attempt, "renew").map_err(Failure::invalid)?;
    mutate(
        cli,
        context,
        &path,
        json!({"generation": args.generation}),
        true,
    )
    .await
}

async fn activity_release(
    cli: &Cli,
    context: &ContextData,
    args: &ActivityAttemptInputArgs,
    expected_kind: &str,
) -> std::result::Result<Value, Failure> {
    require_activity_attempt(cli, context, &args.authority, expected_kind).await?;
    let body =
        input_with_generation(&args.input, args.authority.generation).map_err(Failure::invalid)?;
    activity_mutation(cli, context, &args.authority.activity, "release", body).await
}

async fn require_activity_attempt(
    cli: &Cli,
    context: &ContextData,
    args: &ActivityAttemptArgs,
    expected_kind: &str,
) -> std::result::Result<Value, Failure> {
    let body = activity_status(cli, context, &args.activity).await?;
    let activity = body
        .pointer("/data/activity")
        .or_else(|| body.get("data"))
        .unwrap_or(&body);
    let kind = activity.get("kind").and_then(Value::as_str);
    let kind_matches = match expected_kind {
        "review" => matches!(kind, Some("agent_review" | "human_review")),
        expected => kind == Some(expected),
    };
    if !kind_matches {
        return Err(Failure::local(
            5,
            "activity_kind_mismatch",
            "the workflow activity has a different kind",
            false,
        ));
    }
    let attempt = activity
        .get("current_attempt")
        .filter(|value| !value.is_null())
        .ok_or_else(|| {
            Failure::local(
                5,
                "activity_not_owned",
                "the workflow activity has no current attempt",
                false,
            )
        })?;
    if attempt.get("id").and_then(Value::as_str) != Some(&args.attempt)
        || attempt.get("generation").and_then(Value::as_u64) != Some(args.generation)
    {
        return Err(Failure::local(
            5,
            "activity_attempt_mismatch",
            "the activity's current attempt or generation does not match",
            false,
        ));
    }
    Ok(body)
}

async fn activity_mutation(
    cli: &Cli,
    context: &ContextData,
    activity: &str,
    operation: &str,
    body: Value,
) -> std::result::Result<Value, Failure> {
    validate_segment("activity", activity).map_err(Failure::invalid)?;
    let path = format!(
        "/api/v1/projects/{}/workflow-activities/{activity}/{operation}",
        context.binding.project_id
    );
    mutate(cli, context, &path, body, true).await
}

async fn remote_activity(
    context: &ContextData,
    session: &SessionAuth,
    activity: &str,
) -> std::result::Result<Value, Failure> {
    validate_segment("activity", activity).map_err(Failure::invalid)?;
    let path = format!(
        "/api/v1/projects/{}/workflow-activities/{activity}",
        context.binding.project_id
    );
    finish(context.client.get(&path, Some(session)).await)
}

async fn remote_integration_context(
    context: &ContextData,
    session: &SessionAuth,
    activity_id: &str,
) -> std::result::Result<Value, Failure> {
    let detail = remote_activity(context, session, activity_id).await?;
    let activity = activity_record(&detail);
    let subject = activity
        .get("subject_task_id")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::temporary("integration activity omitted subject_task_id"))?;
    validate_segment("subject task", subject).map_err(Failure::invalid)?;
    let workflow_path = format!(
        "/api/v1/projects/{}/tasks/{subject}/workflow",
        context.binding.project_id
    );
    let workflow = finish(context.client.get(&workflow_path, Some(session)).await)?;
    let submission = workflow
        .pointer("/data/submission")
        .filter(|value| !value.is_null())
        .ok_or_else(|| Failure::temporary("subject workflow omitted its current submission"))?;
    Ok(json!({"data":{"activity":activity,"submission":submission}}))
}

fn activity_record(body: &Value) -> &Value {
    body.pointer("/data/activity")
        .or_else(|| body.get("data"))
        .unwrap_or(body)
}

fn submission_record(body: &Value) -> Option<&Value> {
    body.pointer("/data/submission")
        .or_else(|| activity_record(body).get("submission"))
        .filter(|value| !value.is_null())
}

fn require_integration_activity<'a>(
    body: &'a Value,
    authority: &ActivityAttemptArgs,
    submission_id: Option<&str>,
) -> std::result::Result<&'a Value, Failure> {
    let activity = activity_record(body);
    if activity.get("id").and_then(Value::as_str) != Some(&authority.activity)
        || activity.get("kind").and_then(Value::as_str) != Some("integration")
    {
        return Err(Failure::local(
            5,
            "integration_activity_mismatch",
            "the service response does not describe the requested integration activity",
            false,
        ));
    }
    if activity
        .pointer("/current_authority/valid")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(Failure::local(
            5,
            "integration_authority_not_current",
            "the integration activity does not grant current authority",
            false,
        ));
    }
    let attempt = activity
        .get("current_attempt")
        .filter(|value| !value.is_null())
        .ok_or_else(|| {
            Failure::local(
                5,
                "integration_not_owned",
                "the integration activity has no current attempt",
                false,
            )
        })?;
    if attempt.get("id").and_then(Value::as_str) != Some(&authority.attempt)
        || attempt.get("generation").and_then(Value::as_u64) != Some(authority.generation)
    {
        return Err(Failure::local(
            5,
            "integration_attempt_mismatch",
            "the current integration attempt or generation does not match",
            false,
        ));
    }
    let submission = submission_record(body)
        .ok_or_else(|| Failure::temporary("integration activity omitted its submission"))?;
    if submission_id.is_some_and(|id| submission.get("id").and_then(Value::as_str) != Some(id)) {
        return Err(Failure::local(
            5,
            "submission_mismatch",
            "the integration activity names a different immutable submission",
            false,
        ));
    }
    Ok(submission)
}

async fn require_fresh_attempt(
    context: &ContextData,
    session: &SessionAuth,
    authority: &ActivityAttemptArgs,
) -> std::result::Result<Value, Failure> {
    validate_segment("attempt", &authority.attempt).map_err(Failure::invalid)?;
    let path = format!(
        "/api/v1/projects/{}/attempts/{}",
        context.binding.project_id, authority.attempt
    );
    let body = finish(context.client.get(&path, Some(session)).await)?;
    require_current_work_attempt(&body, authority.generation)?;
    Ok(body)
}

async fn validate_publication_authority(
    context: &ContextData,
    session: &SessionAuth,
    activity_id: &str,
    authority: &ActivityAttemptArgs,
    publication: &coordinator_local::git_workflow::PublicationAuthorizationContext,
) -> Result<coordinator_local::git_workflow::FreshPublicationAuthority> {
    let authority_check_started = Instant::now();
    let attempt_path = format!(
        "/api/v1/projects/{}/attempts/{}",
        context.binding.project_id, authority.attempt
    );
    let attempt_response = context
        .client
        .get(&attempt_path, Some(session))
        .await
        .context("read current integration attempt")?;
    if !attempt_response.is_success() {
        bail!(
            "service refused fresh integration authority (HTTP {})",
            attempt_response.status
        );
    }
    let attempt_data = attempt_response
        .body
        .get("data")
        .unwrap_or(&attempt_response.body);
    let attempt = attempt_data.get("attempt").unwrap_or(attempt_data);
    ensure_attempt_authority(attempt_data, attempt, authority.generation)?;
    let lease_remaining_ms = attempt_data
        .get("lease_remaining_ms")
        .and_then(Value::as_u64)
        .context("attempt response omitted lease_remaining_ms")?;

    let activity_path = format!(
        "/api/v1/projects/{}/workflow-activities/{activity_id}",
        context.binding.project_id
    );
    let activity_response = context
        .client
        .get(&activity_path, Some(session))
        .await
        .context("read current integration activity")?;
    if !activity_response.is_success() {
        bail!(
            "service refused fresh publication authority (HTTP {})",
            activity_response.status
        );
    }
    let body = &activity_response.body;
    let activity = activity_record(body);
    if activity.get("id").and_then(Value::as_str) != Some(activity_id)
        || activity.get("kind").and_then(Value::as_str) != Some("integration")
    {
        bail!("fresh authority named a different integration activity");
    }
    let current = activity
        .get("current_attempt")
        .context("integration activity has no current attempt")?;
    if current.get("id").and_then(Value::as_str) != Some(&authority.attempt)
        || current.get("generation").and_then(Value::as_u64) != Some(authority.generation)
    {
        bail!("fresh authority named a different integration attempt");
    }
    if activity
        .pointer("/current_authority/valid")
        .and_then(Value::as_bool)
        != Some(true)
    {
        bail!("fresh integration activity authority is not valid");
    }
    if activity.get("publication_allowed").and_then(Value::as_bool) != Some(true) {
        bail!("the service has not authorized publication for the exact required checks");
    }
    let subject = activity
        .get("subject_task_id")
        .and_then(Value::as_str)
        .context("integration activity omitted subject task")?;
    let workflow_path = format!(
        "/api/v1/projects/{}/tasks/{subject}/workflow",
        context.binding.project_id
    );
    let workflow_response = context
        .client
        .get(&workflow_path, Some(session))
        .await
        .context("read current immutable submission")?;
    if !workflow_response.is_success() {
        bail!(
            "service refused current submission evidence (HTTP {})",
            workflow_response.status
        );
    }
    let submission = workflow_response
        .body
        .pointer("/data/submission")
        .context("subject workflow omitted submission")?;
    if submission.get("candidate_revision").and_then(Value::as_str)
        != Some(publication.candidate.as_str())
    {
        bail!("fresh authority named a different candidate");
    }
    if submission.get("target_branch").and_then(Value::as_str)
        != Some(publication.target_branch.as_str())
    {
        bail!("fresh authority named a different target branch");
    }
    let intent = activity
        .get("intent")
        .filter(|value| !value.is_null())
        .context("publication intent is not registered")?;
    for (name, expected) in [
        (
            "observed_target_revision",
            publication.expected_target.as_str(),
        ),
        ("result_revision", publication.result.as_str()),
        ("result_tree", publication.result_tree.as_str()),
    ] {
        if intent.get(name).and_then(Value::as_str) != Some(expected) {
            bail!("registered publication intent does not match local {name}");
        }
    }
    let conservative_remaining =
        Duration::from_millis(lease_remaining_ms).saturating_sub(authority_check_started.elapsed());
    coordinator_local::git_workflow::FreshPublicationAuthority::valid_for(conservative_remaining)
}

fn ensure_attempt_authority(data: &Value, attempt: &Value, generation: u64) -> Result<()> {
    if attempt.get("generation").and_then(Value::as_u64) != Some(generation)
        || attempt.get("state").and_then(Value::as_str) != Some("active")
        || attempt.get("mode").and_then(Value::as_str) != Some("work")
        || data.get("authority_valid").and_then(Value::as_bool) != Some(true)
    {
        bail!("integration attempt authority is no longer current");
    }
    Ok(())
}

fn activity_operation_path(
    context: &ContextData,
    activity: &str,
    operation: &str,
) -> std::result::Result<String, Failure> {
    validate_segment("activity", activity).map_err(Failure::invalid)?;
    Ok(format!(
        "/api/v1/projects/{}/workflow-activities/{activity}/{operation}",
        context.binding.project_id
    ))
}

fn integration_state_file(context: &ContextData, activity: &str, attempt: &str) -> Result<PathBuf> {
    validate_segment("activity", activity)?;
    validate_segment("attempt", attempt)?;
    let mut digest = Sha256::new();
    digest.update(context.origin.as_bytes());
    digest.update([0]);
    digest.update(context.binding.project_id.as_bytes());
    digest.update([0]);
    digest.update(activity.as_bytes());
    digest.update([0]);
    digest.update(attempt.as_bytes());
    Ok(config::coordinator_home()?
        .join("integrations")
        .join(format!("{}.json", hex::encode(digest.finalize()))))
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
    if let Some(existing) = registered_checkout(&attempt_body)? {
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

fn registered_checkout(body: &Value) -> std::result::Result<Option<&Value>, Failure> {
    match body
        .pointer("/data/checkout")
        .or_else(|| body.get("checkout"))
    {
        None | Some(Value::Null) => Ok(None),
        Some(value @ Value::Object(_)) => Ok(Some(value)),
        Some(_) => Err(Failure::temporary(
            "attempt response contained an invalid checkout record",
        )),
    }
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
    let checkout = registered_checkout(&attempt_body)?.ok_or_else(|| {
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
    let (source_revision, source_tree) = if let Some(activity) = &args.activity {
        validate_segment("activity", activity).map_err(Failure::invalid)?;
        let project = bound_project(context, Some(&session.session)).await?;
        let repository = project
            .get("repository_url")
            .and_then(Value::as_str)
            .ok_or_else(|| Failure::temporary("project response omitted repository_url"))?;
        let state_file =
            integration_state_file(context, activity, &args.attempt).map_err(Failure::invalid)?;
        let integration = coordinator_local::git_workflow::load_integration_intent(&state_file)
            .map_err(Failure::invalid)?;
        if integration.phase == coordinator_local::git_workflow::IntegrationPhase::PushIntent {
            return Err(Failure::local(
                5,
                "publication_uncertain",
                "reconcile the durable push intent before starting another required check",
                false,
            ));
        }
        if integration.checkout != prepared.destination {
            return Err(Failure::local(
                5,
                "integration_checkout_mismatch",
                "the integration intent belongs to a different checkout",
                false,
            ));
        }
        let snapshot = coordinator_local::git_workflow::capture_clean_snapshot(
            &prepared.destination,
            repository,
        )
        .map_err(Failure::invalid)?;
        if snapshot.git_dir != integration.git_dir
            || snapshot.common_git_dir != integration.common_git_dir
            || snapshot.remote != integration.remote
        {
            return Err(Failure::local(
                5,
                "integration_checkout_identity_changed",
                "the integration checkout or configured repository identity changed",
                false,
            ));
        }
        let result = integration
            .result
            .as_deref()
            .ok_or_else(|| Failure::invalid("integration result is not prepared"))?;
        let result_tree = integration
            .result_tree
            .as_deref()
            .ok_or_else(|| Failure::invalid("integration result tree is not prepared"))?;
        if snapshot.revision != result || snapshot.tree != result_tree {
            return Err(Failure::local(
                5,
                "integration_result_not_materialized",
                "the clean checkout is not at the exact prepared integration result",
                false,
            ));
        }
        let authority = ActivityAttemptArgs {
            activity: activity.clone(),
            attempt: args.attempt.clone(),
            generation: args.generation,
        };
        let detail = remote_integration_context(context, &session.session, activity).await?;
        require_integration_activity(&detail, &authority, None)?;
        (snapshot.revision, snapshot.tree)
    } else {
        worktree::current_snapshot(&prepared).map_err(Failure::invalid)?
    };
    let input = job_state::read_program(&args.input).map_err(Failure::invalid)?;
    if args.activity.is_some() != input.check_identity.is_some() {
        return Err(Failure::invalid(
            "--activity and the complete check identity/version/environment tuple must be supplied together",
        ));
    }
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
    let mut body = json!({
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
    if let (Some(identity), Some(version), Some(environment)) = (
        intent.input.check_identity.as_deref(),
        intent.input.check_version.as_deref(),
        intent.input.check_environment.as_deref(),
    ) {
        let object = body
            .as_object_mut()
            .expect("job registration body is an object");
        object.insert("check_identity".into(), json!(identity));
        object.insert("check_version".into(), json!(version));
        object.insert("check_environment".into(), json!(environment));
    }
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

fn reject_fields(body: &Value, names: &[&str]) -> Result<()> {
    let object = body
        .as_object()
        .ok_or_else(|| anyhow!("JSON input must be an object"))?;
    if let Some(name) = names.iter().find(|name| object.contains_key(**name)) {
        bail!("{name} is derived by the CLI and must be omitted from --input");
    }
    Ok(())
}

fn allow_fields(body: &Value, names: &[&str]) -> Result<()> {
    let object = body
        .as_object()
        .ok_or_else(|| anyhow!("JSON input must be an object"))?;
    if let Some(name) = object.keys().find(|name| !names.contains(&name.as_str())) {
        bail!("unexpected field {name} in --input");
    }
    Ok(())
}

fn require_matching_fields(actual: &Value, expected: &Value, names: &[&str]) -> Result<()> {
    for name in names {
        if actual.get(*name) != expected.get(*name) {
            bail!("recorded integration result does not match requested {name}");
        }
    }
    Ok(())
}

fn insert_fields<const N: usize>(body: &mut Value, fields: [(&str, Value); N]) -> Result<()> {
    let object = body
        .as_object_mut()
        .ok_or_else(|| anyhow!("JSON input must be an object"))?;
    for (name, value) in fields {
        object.insert(name.to_owned(), value);
    }
    Ok(())
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

    #[test]
    fn absent_checkout_accepts_the_service_null_envelope() {
        assert!(matches!(
            registered_checkout(&json!({"data":{"checkout":null}})),
            Ok(None)
        ));
        assert!(matches!(registered_checkout(&json!({"data":{}})), Ok(None)));
        assert!(matches!(
            registered_checkout(&json!({"data":{"checkout":{"path":"/tmp/worktree"}}})),
            Ok(Some(_))
        ));
        assert!(registered_checkout(&json!({"data":{"checkout":"invalid"}})).is_err());
    }
}
