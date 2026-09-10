//! Named shared-record commands use the same durable mutation journal as work commands.
use super::*;

#[derive(Args)]
pub(crate) struct ContextArgs {
    /// Plain text search, not an FTS expression.
    #[arg(long)]
    query: String,
    /// Include records explicitly shared by other projects.
    #[arg(long)]
    include_shared: bool,
    #[arg(long)]
    budget: Option<u32>,
    #[arg(long)]
    task_id: Option<String>,
    #[arg(long)]
    component: Option<String>,
    #[arg(long)]
    environment: Option<String>,
    #[arg(long)]
    version: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=100))]
    limit: Option<u16>,
}

#[derive(Args)]
pub(crate) struct RecordArgs {
    #[arg(long)]
    id: String,
}

#[derive(Args)]
pub(crate) struct RecordInput {
    #[arg(long)]
    id: String,
    /// Exact JSON request including the expected revision, or - for stdin.
    #[arg(long)]
    input: PathBuf,
}

#[derive(Subcommand)]
pub(crate) enum KnowledgeCommand {
    List(ListArgs),
    Show(RecordArgs),
    Create(InputArgs),
    Edit(RecordInput),
    Feedback(RecordInput),
}

#[derive(Subcommand)]
pub(crate) enum DecisionsCommand {
    List(ListArgs),
    Show(RecordArgs),
    Create(InputArgs),
    Answer(RecordInput),
    Reopen(RecordInput),
}

#[derive(Subcommand)]
pub(crate) enum ArtifactsCommand {
    List(ListArgs),
    Show(RecordArgs),
    Link(InputArgs),
    Reserve(InputArgs),
    Upload(UploadArgs),
    Download(DownloadArgs),
    Retention(RecordInput),
    Delete(RecordInput),
}

#[derive(Args)]
pub(crate) struct UploadArgs {
    #[arg(long)]
    id: String,
    #[arg(long)]
    file: PathBuf,
}

#[derive(Args)]
pub(crate) struct DownloadArgs {
    #[arg(long)]
    id: String,
    #[arg(long)]
    output: PathBuf,
}

#[derive(Subcommand)]
pub(crate) enum ImportsCommand {
    Preview(InputArgs),
    Show(RecordArgs),
    Apply(RecordInput),
}

fn path(context: &ContextData, collection: &str, id: Option<&str>, action: &str) -> Result<String> {
    let mut path = format!(
        "/api/v1/projects/{}/{collection}",
        context.binding.project_id
    );
    if let Some(id) = id {
        validate_segment("record id", id)?;
        path.push('/');
        path.push_str(id);
    }
    if !action.is_empty() {
        path.push('/');
        path.push_str(action);
    }
    Ok(path)
}

async fn get(
    cli: &Cli,
    context: &ContextData,
    path: String,
    query: &[(&str, String)],
) -> std::result::Result<Value, Failure> {
    let optional = optional_state(cli, context)?;
    let session = optional.as_ref().map(|(_, _, state)| &state.session);
    finish(context.client.get_query(&path, query, session).await)
}

async fn write(
    cli: &Cli,
    context: &ContextData,
    path: String,
    input: &Path,
    method: HttpMethod,
) -> std::result::Result<Value, Failure> {
    let body = read_json(input).map_err(Failure::invalid)?;
    mutate_method(cli, context, method, &path, body, true).await
}

pub(crate) async fn context(
    cli: &Cli,
    context: &ContextData,
    args: &ContextArgs,
) -> std::result::Result<Value, Failure> {
    let mut query = vec![
        ("q", args.query.clone()),
        ("include_shared", args.include_shared.to_string()),
    ];
    for (key, value) in [
        ("task_id", &args.task_id),
        ("component", &args.component),
        ("environment", &args.environment),
        ("version", &args.version),
    ] {
        if let Some(value) = value {
            query.push((key, value.clone()));
        }
    }
    if let Some(value) = args.budget {
        query.push(("budget", value.to_string()));
    }
    if let Some(value) = args.limit {
        query.push(("limit", value.to_string()));
    }
    get(
        cli,
        context,
        path(context, "context", None, "").map_err(Failure::invalid)?,
        &query,
    )
    .await
}

pub(crate) async fn knowledge(
    cli: &Cli,
    context: &ContextData,
    command: &KnowledgeCommand,
) -> std::result::Result<Value, Failure> {
    let base = |id, action| path(context, "knowledge", id, action).map_err(Failure::invalid);
    match command {
        KnowledgeCommand::List(args) => get(cli, context, base(None, "")?, &list_query(args)).await,
        KnowledgeCommand::Show(args) => get(cli, context, base(Some(&args.id), "")?, &[]).await,
        KnowledgeCommand::Create(args) => {
            write(cli, context, base(None, "")?, &args.input, HttpMethod::Post).await
        }
        KnowledgeCommand::Edit(args) => {
            write(
                cli,
                context,
                base(Some(&args.id), "")?,
                &args.input,
                HttpMethod::Patch,
            )
            .await
        }
        KnowledgeCommand::Feedback(args) => {
            write(
                cli,
                context,
                base(Some(&args.id), "feedback")?,
                &args.input,
                HttpMethod::Post,
            )
            .await
        }
    }
}

pub(crate) async fn decisions(
    cli: &Cli,
    context: &ContextData,
    command: &DecisionsCommand,
) -> std::result::Result<Value, Failure> {
    let base = |id, action| path(context, "decisions", id, action).map_err(Failure::invalid);
    match command {
        DecisionsCommand::List(args) => get(cli, context, base(None, "")?, &list_query(args)).await,
        DecisionsCommand::Show(args) => get(cli, context, base(Some(&args.id), "")?, &[]).await,
        DecisionsCommand::Create(args) => {
            write(cli, context, base(None, "")?, &args.input, HttpMethod::Post).await
        }
        DecisionsCommand::Answer(args) => {
            write(
                cli,
                context,
                base(Some(&args.id), "answer")?,
                &args.input,
                HttpMethod::Post,
            )
            .await
        }
        DecisionsCommand::Reopen(args) => {
            write(
                cli,
                context,
                base(Some(&args.id), "reopen")?,
                &args.input,
                HttpMethod::Post,
            )
            .await
        }
    }
}

pub(crate) async fn artifacts(
    cli: &Cli,
    context: &ContextData,
    command: &ArtifactsCommand,
) -> std::result::Result<Value, Failure> {
    let base = |id, action| path(context, "artifacts", id, action).map_err(Failure::invalid);
    match command {
        ArtifactsCommand::List(args) => get(cli, context, base(None, "")?, &list_query(args)).await,
        ArtifactsCommand::Show(args) => get(cli, context, base(Some(&args.id), "")?, &[]).await,
        ArtifactsCommand::Link(args) => {
            write(cli, context, base(None, "")?, &args.input, HttpMethod::Post).await
        }
        ArtifactsCommand::Reserve(args) => {
            write(
                cli,
                context,
                base(None, "uploads")?,
                &args.input,
                HttpMethod::Post,
            )
            .await
        }
        ArtifactsCommand::Upload(args) => {
            let (_lock, _, session) = load_required_state(cli, context)?;
            let transfer = artifact_transfer::TransferContext {
                client: &context.client,
                service_origin: &context.origin,
                project_id: &context.binding.project_id,
                local_session: &session.local_session,
                session: &session.session,
            };
            let response = artifact_transfer::upload(&transfer, &args.id, &args.file)
                .await
                .map_err(transfer_failure)?;
            require_success(response)
        }
        ArtifactsCommand::Download(args) => {
            let (_lock, _, session) = load_required_state(cli, context)?;
            let transfer = artifact_transfer::TransferContext {
                client: &context.client,
                service_origin: &context.origin,
                project_id: &context.binding.project_id,
                local_session: &session.local_session,
                session: &session.session,
            };
            match artifact_transfer::download(&transfer, &args.id, &args.output)
                .await
                .map_err(transfer_failure)?
            {
                coordinator_client::DownloadResponse::Downloaded(receipt) => {
                    Ok(json!({"data":{"download":receipt}}))
                }
                coordinator_client::DownloadResponse::Api(response) => require_success(response),
            }
        }
        ArtifactsCommand::Retention(args) => {
            write(
                cli,
                context,
                base(Some(&args.id), "retention")?,
                &args.input,
                HttpMethod::Post,
            )
            .await
        }
        ArtifactsCommand::Delete(args) => {
            write(
                cli,
                context,
                base(Some(&args.id), "delete")?,
                &args.input,
                HttpMethod::Post,
            )
            .await
        }
    }
}

pub(crate) async fn imports(
    cli: &Cli,
    context: &ContextData,
    command: &ImportsCommand,
) -> std::result::Result<Value, Failure> {
    let base = |id, action| path(context, "imports", id, action).map_err(Failure::invalid);
    match command {
        ImportsCommand::Preview(args) => {
            write(
                cli,
                context,
                base(None, "preview")?,
                &args.input,
                HttpMethod::Post,
            )
            .await
        }
        ImportsCommand::Show(args) => get(cli, context, base(Some(&args.id), "")?, &[]).await,
        ImportsCommand::Apply(args) => {
            write(
                cli,
                context,
                base(Some(&args.id), "apply")?,
                &args.input,
                HttpMethod::Post,
            )
            .await
        }
    }
}

pub(crate) async fn export(
    cli: &Cli,
    context: &ContextData,
    args: &ListArgs,
) -> std::result::Result<Value, Failure> {
    get(
        cli,
        context,
        path(context, "exports", None, "").map_err(Failure::invalid)?,
        &list_query(args),
    )
    .await
}

pub(crate) fn print_records(value: &Value) {
    let data = value.get("data").unwrap_or(value);
    let Some(items) = data.get("items").and_then(Value::as_array) else {
        print_json(data, false);
        return;
    };
    if let Some(rules) = data.pointer("/policy/rules").and_then(Value::as_str) {
        println!("Binding project rules:\n{rules}\n");
    }
    if data.get("instructions_complete").and_then(Value::as_bool) == Some(false) {
        println!(
            "Instructions are incomplete. Increase --budget before relying on this context packet."
        );
    }
    if items.is_empty() {
        println!("No matching records.");
    }
    for item in items {
        let record = item.get("record").unwrap_or(item);
        let title = ["title", "question", "display_name", "filename"]
            .into_iter()
            .find_map(|key| record.get(key).and_then(Value::as_str))
            .unwrap_or("Record");
        println!("{}\t{title}", field(record, "id"));
        for key in ["status", "availability", "body", "snippet", "applicability"] {
            if let Some(text) = record.get(key).and_then(Value::as_str)
                && !text.is_empty()
            {
                println!("  {key}: {text}");
            }
        }
    }
    if data.get("truncated").and_then(Value::as_bool) == Some(true) {
        println!("Results were bounded; narrow the query or request the next page.");
    }
    print_next_cursor(value);
}

fn transfer_failure(error: anyhow::Error) -> Failure {
    if artifact_transfer::failure_is_temporary(&error) {
        Failure::temporary(error)
    } else {
        Failure::invalid(error)
    }
}
