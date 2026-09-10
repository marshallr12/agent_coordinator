//! Named task and policy commands share the ordinary durable mutation journal.
use super::*;

#[derive(Args)]
pub(crate) struct RecordArgs {
    #[arg(long)]
    pub(crate) id: String,
}

#[derive(Args)]
pub(crate) struct RecordInput {
    #[arg(long)]
    pub(crate) id: String,
    /// Complete JSON request with its expected revision, or - for stdin.
    #[arg(long)]
    pub(crate) input: PathBuf,
}

#[derive(Subcommand)]
pub(crate) enum PolicyCommand {
    /// Read the current project rules and permission settings in full.
    Show,
    /// Submit a revision-checked policy update, if this principal is authorized.
    Edit(InputArgs),
    /// Read preserved policy revisions and their provenance.
    History(ListArgs),
}

pub(crate) fn project_path(context: &ContextData, suffix: &str) -> String {
    format!("/api/v1/projects/{}/{}", context.binding.project_id, suffix)
}

pub(crate) fn task_path(context: &ContextData, id: &str, action: &str) -> Result<String> {
    validate_segment("task id", id)?;
    let mut path = project_path(context, &format!("tasks/{id}"));
    if !action.is_empty() {
        path.push('/');
        path.push_str(action);
    }
    Ok(path)
}

pub(crate) async fn get(
    cli: &Cli,
    context: &ContextData,
    path: &str,
    query: &[(&str, String)],
) -> std::result::Result<Value, Failure> {
    let optional = optional_state(cli, context)?;
    let session = optional.as_ref().map(|(_, _, state)| &state.session);
    finish(context.client.get_query(path, query, session).await)
}

pub(crate) async fn task_show(
    cli: &Cli,
    context: &ContextData,
    args: &RecordArgs,
) -> std::result::Result<Value, Failure> {
    get(
        cli,
        context,
        &task_path(context, &args.id, "").map_err(Failure::invalid)?,
        &[],
    )
    .await
}

pub(crate) async fn task_write(
    cli: &Cli,
    context: &ContextData,
    args: &RecordInput,
    action: &str,
    method: HttpMethod,
) -> std::result::Result<Value, Failure> {
    let path = task_path(context, &args.id, action).map_err(Failure::invalid)?;
    let body = read_json(&args.input).map_err(Failure::invalid)?;
    mutate_method(cli, context, method, &path, body, true).await
}

pub(crate) async fn policy(
    cli: &Cli,
    context: &ContextData,
    command: &PolicyCommand,
) -> std::result::Result<Value, Failure> {
    match command {
        PolicyCommand::Show => {
            let mut result = get(cli, context, &project_path(context, "orientation"), &[]).await?;
            let project = result.pointer("/data/project").cloned().ok_or_else(|| {
                Failure::temporary(anyhow!("orientation omitted its project policy"))
            })?;
            result["data"] = project;
            Ok(result)
        }
        PolicyCommand::Edit(args) => {
            let body = read_json(&args.input).map_err(Failure::invalid)?;
            mutate_method(
                cli,
                context,
                HttpMethod::Patch,
                &project_path(context, "policy"),
                body,
                true,
            )
            .await
        }
        PolicyCommand::History(args) => {
            get(
                cli,
                context,
                &project_path(context, "policy/history"),
                &list_query(args),
            )
            .await
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum ObjectivesCommand {
    /// List objectives in this project.
    List(ListArgs),
    /// Read an objective and its children.
    Show(ObjectiveShowArgs),
    /// Create a general task grouping with its own acceptance criteria.
    Create(InputArgs),
    /// Replace child membership before objective work begins.
    Children(RecordInput),
}

#[derive(Args)]
pub(crate) struct ObjectiveShowArgs {
    #[arg(long)]
    id: String,
    #[command(flatten)]
    page: ListArgs,
}

#[derive(Args)]
pub(crate) struct HistoryArgs {
    #[arg(long)]
    id: String,
    #[arg(long, value_parser = ["attempts", "checkpoints", "checkouts", "jobs", "job_observations", "resources", "artifacts", "submissions", "reviews", "integrations", "task_revisions", "events"])]
    kind: String,
    #[command(flatten)]
    page: ListArgs,
}

pub(crate) async fn history(
    cli: &Cli,
    context: &ContextData,
    args: &HistoryArgs,
) -> std::result::Result<Value, Failure> {
    let mut query = list_query(&args.page);
    query.push(("kind", args.kind.clone()));
    get(
        cli,
        context,
        &task_path(context, &args.id, "history").map_err(Failure::invalid)?,
        &query,
    )
    .await
}

pub(crate) async fn objectives(
    cli: &Cli,
    context: &ContextData,
    command: &ObjectivesCommand,
) -> std::result::Result<Value, Failure> {
    let path = project_path(context, "objectives");
    match command {
        ObjectivesCommand::List(args) => get(cli, context, &path, &list_query(args)).await,
        ObjectivesCommand::Show(args) => {
            validate_segment("objective id", &args.id).map_err(Failure::invalid)?;
            get(
                cli,
                context,
                &format!("{path}/{}", args.id),
                &list_query(&args.page),
            )
            .await
        }
        ObjectivesCommand::Create(args) => {
            let body = read_json(&args.input).map_err(Failure::invalid)?;
            mutate(cli, context, &path, body, true).await
        }
        ObjectivesCommand::Children(args) => {
            validate_segment("objective id", &args.id).map_err(Failure::invalid)?;
            let body = read_json(&args.input).map_err(Failure::invalid)?;
            mutate_method(
                cli,
                context,
                HttpMethod::Patch,
                &format!("{path}/{}/children", args.id),
                body,
                true,
            )
            .await
        }
    }
}
