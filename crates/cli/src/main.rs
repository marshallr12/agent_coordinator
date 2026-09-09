mod config;
mod state;

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
    /// Send a bounded API request. POST requests use durable mutation state.
    Request(RequestArgs),
    /// Retry the exact pending mutation and its saved idempotency key.
    Retry,
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
        Command::Request(args) => request(cli, &context, args).await,
        Command::Retry => retry(cli, &context).await,
    }
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
    body.insert("mode".into(), Value::String("work".into()));
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
    let pending_path = state.pending.as_ref().map(|pending| pending.path.clone());
    let response = send_saved(context, &path, &mut state).await?;
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
    state
        .set_pending(PendingMutation {
            key: Uuid::new_v4().to_string(),
            method,
            path: path.to_owned(),
            body,
            include_session_id,
        })
        .map_err(Failure::invalid)?;
    state::save(session_path, state).map_err(Failure::invalid)?;
    send_saved(context, session_path, state).await
}

async fn send_saved(
    context: &ContextData,
    session_path: &Path,
    state: &mut SessionState,
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
    if mutation_response_is_definitive(response.status) {
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
