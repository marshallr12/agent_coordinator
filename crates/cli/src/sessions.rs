//! `sessions list`: the open agent sessions connected to the bound project,
//! so an owner can see who is still working before a test or cutover.
//! (`session`, singular, manages this harness's own local state instead.)
use super::*;

/// Shown when the service lists no session in the window.
const EMPTY: &str = "No agent sessions are connected to this project in this window.";

#[derive(Subcommand)]
pub(crate) enum SessionsCommand {
    /// List open agent sessions bound to this project, most recently active first.
    List(SessionsListArgs),
}

#[derive(Args)]
pub(crate) struct SessionsListArgs {
    /// Only sessions active in the last N hours (service default 24; 0 lists
    /// every open session). Sessions holding an attempt are always listed.
    #[arg(long)]
    active_within_hours: Option<u32>,
}

/// Runs a `sessions` subcommand against the bound project.
pub(crate) async fn run(
    cli: &Cli,
    context: &ContextData,
    command: &SessionsCommand,
) -> std::result::Result<Value, Failure> {
    let SessionsCommand::List(args) = command;
    let query: Vec<(&str, String)> = args
        .active_within_hours
        .map(|hours| ("active_within_hours", hours.to_string()))
        .into_iter()
        .collect();
    let path = operator::project_path(context, "sessions");
    operator::get(cli, context, &path, &query).await
}

/// Prints the session list as a tab-separated table.
pub(crate) fn print_table(value: &Value) {
    for line in table_lines(value) {
        println!("{line}");
    }
}

/// The table's lines: a header and one row per session, the empty-window
/// message when there is none, and a note when the list was truncated.
fn table_lines(value: &Value) -> Vec<String> {
    let data = value.get("data").unwrap_or(value);
    let items = data
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut lines = if items.is_empty() {
        vec![EMPTY.to_owned()]
    } else {
        let header = "SESSION\tPRINCIPAL\tAGENT\tWORKSTATION\tSTARTED\tLAST ACTIVITY\tHELD";
        std::iter::once(header.to_owned())
            .chain(items.iter().map(row))
            .collect()
    };
    if data.get("truncated").and_then(Value::as_bool) == Some(true) {
        lines.push("Truncated: more sessions match; narrow --active-within-hours.".into());
    }
    lines
}

/// One session's table row.
fn row(item: &Value) -> String {
    [
        field(item, "session_id"),
        item.pointer("/principal/name")
            .map(scalar)
            .unwrap_or_else(|| "-".into()),
        agent(item),
        field(item, "workstation_id"),
        field(item, "started_at"),
        field(item, "last_activity_at"),
        held(item),
    ]
    .join("\t")
}

/// The harness, followed by the subagent name for a subagent session.
fn agent(item: &Value) -> String {
    let harness = field(item, "harness");
    match item.pointer("/subagent/name").and_then(Value::as_str) {
        Some(name) => format!("{harness} / subagent {name}"),
        None => harness,
    }
}

/// The held-attempt count, then each held task id with its activity kind.
fn held(item: &Value) -> String {
    let attempts = item
        .get("held_attempts")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if attempts.is_empty() {
        return "0".into();
    }
    let tasks: Vec<String> = attempts.iter().map(held_task).collect();
    format!("{}: {}", attempts.len(), tasks.join(", "))
}

/// One held attempt as `task-id`, plus its activity kind in parentheses and
/// ` (lease expired)` when its lease lapsed while the attempt stayed active.
fn held_task(attempt: &Value) -> String {
    let mut task = field(attempt, "task_id");
    if let Some(kind) = attempt.get("activity_kind").and_then(Value::as_str) {
        task.push_str(&format!(" ({kind})"));
    }
    if attempt.get("lease_expired").and_then(Value::as_bool) == Some(true) {
        task.push_str(" (lease expired)");
    }
    task
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listed subagent session holding a review and a plain idle session.
    fn listing(truncated: bool) -> Value {
        json!({"data":{"truncated":truncated,"items":[
            {"session_id":"s1","principal":{"name":"builder"},"harness":"claude-code",
             "subagent":{"name":"reviewer-1"},"workstation_id":"ws1",
             "started_at":"2026-10-05T10:00:00.000Z","last_activity_at":"2026-10-05T11:00:00.000Z",
             "held_attempts":[{"task_id":"t-review","activity_kind":"agent_review"},{"task_id":"t-work","activity_kind":null,"lease_expired":true}]},
            {"session_id":"s2","principal":{"name":"idle"},"harness":"agent-coordinator-cli",
             "subagent":null,"workstation_id":"ws2","started_at":"a","last_activity_at":"b","held_attempts":[]}
        ]}})
    }

    #[test]
    fn rows_name_the_agent_and_held_work() {
        let lines = table_lines(&listing(false));
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("SESSION\tPRINCIPAL\tAGENT"));
        assert_eq!(
            lines[1],
            "s1\tbuilder\tclaude-code / subagent reviewer-1\tws1\t2026-10-05T10:00:00.000Z\t2026-10-05T11:00:00.000Z\t2: t-review (agent_review), t-work (lease expired)"
        );
        assert_eq!(lines[2], "s2\tidle\tagent-coordinator-cli\tws2\ta\tb\t0");
    }

    #[test]
    fn empty_and_truncated_listings_say_so() {
        assert_eq!(
            table_lines(&json!({"data":{"items":[],"truncated":false}})),
            vec![EMPTY]
        );
        let lines = table_lines(&listing(true));
        assert!(lines.last().unwrap().starts_with("Truncated:"));
    }
}
