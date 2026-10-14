//! `integrator-reports list` and `show`: the integrator's findings (a failing
//! target, a flaky check, a privilege gate) that otherwise reach only the
//! manager and the daily digest. Read-only: resolving a report is a human
//! manager action, so no command here writes.
use super::*;

/// Shown when the service lists no report.
const EMPTY: &str = "No integrator reports.";

#[derive(Subcommand)]
pub(crate) enum IntegratorReportsCommand {
    /// List the bound project's integrator reports, newest first.
    List(ReportsListArgs),
    /// Read one integrator report (complete JSON with --json).
    Show(operator::RecordArgs),
}

#[derive(Args)]
pub(crate) struct ReportsListArgs {
    /// Only reports that no human has resolved yet.
    #[arg(long)]
    open: bool,
    /// Only reports about this task.
    #[arg(long)]
    task: Option<String>,
    /// Requested page size (1 through 1000; service default 200).
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=1000))]
    limit: Option<u16>,
    /// Report ID that ends the previous page (its `Next before`); lists the reports recorded before it.
    #[arg(long)]
    before: Option<String>,
}

/// Runs an `integrator-reports` subcommand against the bound project.
pub(crate) async fn run(
    cli: &Cli,
    context: &ContextData,
    command: &IntegratorReportsCommand,
) -> std::result::Result<Value, Failure> {
    match command {
        IntegratorReportsCommand::List(args) => {
            let path = operator::project_path(context, "integrator/reports");
            let query = list_query(args).map_err(Failure::invalid)?;
            operator::get(cli, context, &path, &query).await
        }
        IntegratorReportsCommand::Show(args) => {
            validate_segment("report id", &args.id).map_err(Failure::invalid)?;
            let path = operator::project_path(context, &format!("integrator/reports/{}", args.id));
            operator::get(cli, context, &path, &[]).await
        }
    }
}

/// The list route's query for `args`; ids must be single path segments.
fn list_query(args: &ReportsListArgs) -> Result<Vec<(&'static str, String)>> {
    let mut query = Vec::new();
    if args.open {
        query.push(("open", "true".to_owned()));
    }
    if let Some(task) = &args.task {
        validate_segment("task", task)?;
        query.push(("task_id", task.clone()));
    }
    if let Some(limit) = args.limit {
        query.push(("limit", limit.to_string()));
    }
    if let Some(before) = &args.before {
        validate_segment("before", before)?;
        query.push(("before", before.clone()));
    }
    Ok(query)
}

/// Prints the report list as a tab-separated table.
pub(crate) fn print_table(value: &Value) {
    for line in table_lines(value) {
        println!("{line}");
    }
}

/// Prints one report's summary, then its complete record.
pub(crate) fn print_detail(value: &Value) {
    let report = value.get("data").unwrap_or(value);
    for line in summary_lines(report) {
        println!("{line}");
    }
    println!("\nResult:");
    print_json(report, false);
}

/// The table's lines: a header and one row per report, then the cursor of
/// the next page when there is one.
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
        let header = "ID\tKIND\tTASK\tREQUIRES HUMAN\tCREATED\tRESOLVED\tVERDICT / REASON";
        std::iter::once(header.to_owned())
            .chain(items.iter().map(row))
            .collect()
    };
    if let Some(before) = data.get("next_before").and_then(Value::as_str) {
        lines.push(format!("Next before: {before}"));
    }
    lines
}

/// One report's table row.
fn row(report: &Value) -> String {
    [
        field(report, "id"),
        field(report, "kind"),
        field(report, "task_id"),
        field(report, "requires_human"),
        field(report, "created_at"),
        resolved(report),
        verdict(report),
    ]
    .join("\t")
}

/// `open`, or when and how a human resolved the report.
fn resolved(report: &Value) -> String {
    match report.get("resolved_at").and_then(Value::as_str) {
        None => "open".into(),
        Some(at) => match report.get("decision").and_then(Value::as_str) {
            Some(decision) => format!("{at} ({decision})"),
            None => at.to_owned(),
        },
    }
}

/// The verdict and reason the integrator recorded in the report's details.
fn verdict(report: &Value) -> String {
    let details = report.get("details").unwrap_or(&Value::Null);
    let parts: Vec<String> = ["verdict", "reason"]
        .iter()
        .filter_map(|name| details.get(name).filter(|v| !v.is_null()))
        .map(scalar)
        .collect();
    if parts.is_empty() {
        "-".into()
    } else {
        parts.join(": ")
    }
}

/// The lines that summarize one report above its complete record.
fn summary_lines(report: &Value) -> Vec<String> {
    let mut lines = vec![
        format!(
            "Report: {} — {}",
            field(report, "id"),
            field(report, "kind")
        ),
        format!("Task: {}", field(report, "task_id")),
        format!("Requires human: {}", field(report, "requires_human")),
        format!("Created: {}", field(report, "created_at")),
        format!("Resolved: {}", resolved(report)),
        format!("Verdict / reason: {}", verdict(report)),
    ];
    if let Some(note) = report.get("resolution_note").and_then(Value::as_str) {
        lines.push(format!("Resolution note: {}", scalar(&json!(note))));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An open fix_target report and a resolved privilege_gate report.
    fn listing(next: Value) -> Value {
        json!({"data":{"project_id":"p","next_before":next,"items":[
            {"id":"r2","kind":"fix_target","task_id":"t1","requires_human":true,
             "created_at":"2026-10-13T10:00:00.000Z","resolved_at":null,"decision":null,
             "details":{"verdict":"target_failing","reason":"audit failed"}},
            {"id":"r1","kind":"privilege_gate","task_id":null,"requires_human":true,
             "created_at":"2026-10-12T10:00:00.000Z","resolved_at":"2026-10-12T11:00:00.000Z",
             "decision":"allow","details":{}}
        ]}})
    }

    #[test]
    fn rows_show_state_and_verdict() {
        let lines = table_lines(&listing(json!("r1")));
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("ID\tKIND\tTASK"));
        assert_eq!(
            lines[1],
            "r2\tfix_target\tt1\ttrue\t2026-10-13T10:00:00.000Z\topen\ttarget_failing: audit failed"
        );
        assert_eq!(
            lines[2],
            "r1\tprivilege_gate\t-\ttrue\t2026-10-12T10:00:00.000Z\t2026-10-12T11:00:00.000Z (allow)\t-"
        );
        assert_eq!(lines[3], "Next before: r1");
    }

    #[test]
    fn last_and_empty_pages_have_no_cursor() {
        assert_eq!(table_lines(&listing(Value::Null)).len(), 3);
        assert_eq!(
            table_lines(&json!({"data":{"items":[],"next_before":null}})),
            vec![EMPTY]
        );
    }

    #[test]
    fn summary_names_the_verdict() {
        let lines = summary_lines(&listing(Value::Null)["data"]["items"][0]);
        assert_eq!(lines[0], "Report: r2 — fix_target");
        assert!(lines.contains(&"Resolved: open".to_owned()));
        assert!(lines.contains(&"Verdict / reason: target_failing: audit failed".to_owned()));
    }

    #[test]
    fn query_carries_every_filter() {
        let args = ReportsListArgs {
            open: true,
            task: Some("t1".into()),
            limit: Some(5),
            before: Some("r9".into()),
        };
        assert_eq!(
            list_query(&args).unwrap(),
            [
                ("open", "true".to_owned()),
                ("task_id", "t1".to_owned()),
                ("limit", "5".to_owned()),
                ("before", "r9".to_owned())
            ]
        );
        let bad = ReportsListArgs {
            open: false,
            task: Some("a/b".into()),
            limit: None,
            before: None,
        };
        assert!(list_query(&bad).is_err());
    }
}
