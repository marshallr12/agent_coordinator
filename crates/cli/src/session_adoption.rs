//! Protected local session diagnostics, migration, and MCP adoption.
use std::path::PathBuf;

use clap::{Args, Subcommand};
use coordinator_client::SessionAuth;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    Cli, ContextData, Failure, client_failure, config, default_workstation, parse_orientation,
    require_success, required_session, state, validate_segment, validate_state,
};

#[derive(Subcommand)]
pub enum SessionCommand {
    /// Check local state paths, readability, lock-file access, and exclusive locking.
    Diagnose,
    /// Copy one quiescent session into the selected protected state directory.
    Migrate(MigrateArgs),
    /// Import the exact MCP session from protected environment variables. Makes no remote writes.
    /// Stop MCP writes and resolve their pending retries before adoption; serialize both transports afterward.
    AdoptMcp(AdoptArgs),
}

#[derive(Args)]
pub struct MigrateArgs {
    /// Existing protected state directory containing the session to copy.
    #[arg(long)]
    from_state_dir: PathBuf,
    /// Confirm all processes using the source session have stopped writing.
    #[arg(long, required = true)]
    session_writes_quiescent: bool,
}

#[derive(Args)]
pub struct AdoptArgs {
    /// Confirm MCP writes are stopped and all their pending requests are resolved.
    #[arg(long, required = true)]
    mcp_writes_quiescent: bool,
    /// Expected workstation identity; defaults to this machine's HOSTNAME or COMPUTERNAME.
    #[arg(long)]
    workstation: Option<String>,
}

struct Import {
    session: SessionAuth,
    token_digest: String,
}

fn read_import(
    context: &ContextData,
    get: impl Fn(&str) -> Option<String>,
) -> Result<Import, Failure> {
    let required = |name: &str| {
        get(name).filter(|v| !v.trim().is_empty()).ok_or_else(|| {
            Failure::invalid(format!(
                "{name} must be supplied in the protected environment"
            ))
        })
    };
    config::validate_mcp_url(&required("AGENT_COORDINATOR_MCP_URL")?, &context.origin)
        .map_err(Failure::invalid)?;
    if required("AGENT_COORDINATOR_MCP_PROJECT_ID")? != context.binding.project_id {
        return Err(Failure::invalid(
            "The MCP project does not match the repository binding.",
        ));
    }
    let token = required("AGENT_COORDINATOR_MCP_TOKEN")?;
    let token_digest = hex::encode(Sha256::digest(token.as_bytes()));
    if token_digest != context.credential_digest {
        return Err(Failure::invalid(
            "The native and MCP credentials differ; select the same protected credential.",
        ));
    }
    let id = required("AGENT_COORDINATOR_MCP_SESSION_ID")?;
    // Do not include untrusted environment values (including a misplaced proof) in diagnostics.
    validate_segment("session_id", &id)
        .map_err(|_| Failure::invalid("The MCP session ID is invalid."))?;
    Ok(Import {
        session: SessionAuth {
            id,
            proof: required("AGENT_COORDINATOR_MCP_SESSION_PROOF")?,
        },
        token_digest,
    })
}

fn verify_remote(
    remote: &Value,
    import: &Import,
    workstation: &str,
) -> Result<(String, Vec<String>), Failure> {
    if remote["session_id"].as_str() != Some(import.session.id.as_str())
        || !remote.get("closed_at").is_some_and(Value::is_null)
        || remote["workstation_id"].as_str() != Some(workstation)
    {
        return Err(Failure::invalid(
            "The MCP session is closed or its session/workstation identity does not match.",
        ));
    }
    let harness = remote["harness"]
        .as_str()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| Failure::invalid("The remote session has no harness identity."))?;
    let capabilities: Vec<String> = serde_json::from_value(remote["capabilities"].clone())
        .map_err(|_| Failure::invalid("The remote session capabilities are invalid."))?;
    Ok((harness.to_owned(), capabilities))
}

fn verify_existing(
    existing: &state::SessionState,
    incoming: &state::SessionState,
) -> Result<(), Failure> {
    if existing.pending.is_some() {
        return Err(Failure::invalid(
            "Resolve the local session's pending mutation with retry before adoption.",
        ));
    }
    if existing.session.id != incoming.session.id
        || existing.session.proof != incoming.session.proof
        || existing.workstation_id != incoming.workstation_id
        || existing.harness != incoming.harness
        || existing.capabilities != incoming.capabilities
        || existing.subagent != incoming.subagent
    {
        return Err(Failure::invalid(
            "This local session name already belongs to a different identity; choose an unused name.",
        ));
    }
    Ok(())
}

pub async fn run(
    cli: &Cli,
    context: &ContextData,
    command: &SessionCommand,
) -> Result<Value, Failure> {
    match command {
        SessionCommand::Diagnose => diagnose_local(cli),
        SessionCommand::Migrate(args) => migrate(cli, context, args),
        SessionCommand::AdoptMcp(args) => adopt_mcp(cli, context, args).await,
    }
}

pub fn diagnose_local(cli: &Cli) -> Result<Value, Failure> {
    let local_session = required_session(cli)?;
    let (_, binding) = config::binding(cli.repo_config.as_deref()).map_err(Failure::invalid)?;
    let origin =
        coordinator_client::normalize_origin(&binding.service_url, cli.allow_insecure_loopback)
            .map_err(crate::client_failure)?;
    let path = state::path_for(
        cli.state_dir.as_deref(),
        &origin,
        &binding.project_id,
        local_session,
    )
    .map_err(Failure::invalid)?;
    Ok(json!({"data": state::diagnose(&path)}))
}

fn migrate(cli: &Cli, context: &ContextData, args: &MigrateArgs) -> Result<Value, Failure> {
    if !args.session_writes_quiescent {
        return Err(Failure::invalid(
            "Confirm that all source-session writes are quiescent before migration.",
        ));
    }
    let local_session = required_session(cli)?;
    let source_path = state::path_for_directory(
        &args.from_state_dir,
        &context.origin,
        &context.binding.project_id,
        local_session,
    )
    .map_err(Failure::invalid)?;
    let destination_path = state::path_for(
        cli.state_dir.as_deref(),
        &context.origin,
        &context.binding.project_id,
        local_session,
    )
    .map_err(Failure::invalid)?;
    if source_path == destination_path {
        return Err(Failure::invalid(
            "source and destination session state paths are the same",
        ));
    }

    let _source_lock = state::lock(&source_path).map_err(Failure::state_access)?;
    let source = state::load(&source_path)
        .map_err(Failure::invalid)?
        .ok_or_else(|| Failure::invalid("the source session state does not exist"))?;
    validate_state(context, local_session, &source)?;
    if source.pending.is_some() {
        return Err(Failure::invalid(
            "Resolve the source session's pending mutation with retry before migration.",
        ));
    }

    let _destination_lock = state::lock(&destination_path).map_err(Failure::state_access)?;
    if state::load(&destination_path)
        .map_err(Failure::invalid)?
        .is_some()
    {
        return Err(Failure::invalid(
            "destination session state already exists; no file was overwritten",
        ));
    }
    state::save(&destination_path, &source).map_err(Failure::invalid)?;
    Ok(json!({
        "data": {
            "migrated": true,
            "local_session": local_session,
            "source_state_path": source_path,
            "destination_state_path": destination_path,
            "source_preserved": true,
            "destination_overwritten": false,
            "authority_renewed": false,
            "remote_writes": false,
            "next_action": "Use the destination --state-dir consistently and run `session diagnose` before resuming writes."
        }
    }))
}

async fn adopt_mcp(cli: &Cli, context: &ContextData, args: &AdoptArgs) -> Result<Value, Failure> {
    if !args.mcp_writes_quiescent {
        return Err(Failure::invalid(
            "Confirm that MCP writes are quiescent before adopting their session.",
        ));
    }
    let local_session = required_session(cli)?;
    let import = read_import(context, |name| std::env::var(name).ok())?;
    let workstation = args.workstation.clone().unwrap_or_else(default_workstation);
    if workstation.trim().is_empty()
        || (args.workstation.is_none() && workstation == "unknown-workstation")
    {
        return Err(Failure::invalid(
            "Supply --workstation with the existing MCP session's workstation identity.",
        ));
    }
    let path = state::path_for(
        cli.state_dir.as_deref(),
        &context.origin,
        &context.binding.project_id,
        local_session,
    )
    .map_err(Failure::invalid)?;
    let _lock = state::lock(&path).map_err(Failure::state_access)?;
    let existing = state::load(&path).map_err(Failure::invalid)?;
    if let Some(saved) = &existing {
        validate_state(context, local_session, saved)?;
        if saved.pending.is_some() {
            return Err(Failure::invalid(
                "Resolve the local session's pending mutation with retry before adoption.",
            ));
        }
    }
    // Both requests are reads. The service authenticates the proof and credential
    // together; a receipt must never be interpreted as renewed task ownership.
    let remote = require_success(
        context
            .client
            .get(
                &format!("/api/v1/sessions/{}", import.session.id),
                Some(&import.session),
            )
            .await
            .map_err(client_failure)?,
    )?;
    let (harness, capabilities) = verify_remote(&remote["data"], &import, &workstation)?;
    let orientation = require_success(
        context
            .client
            .get(
                &format!(
                    "/api/v1/projects/{}/orientation",
                    context.binding.project_id
                ),
                Some(&import.session),
            )
            .await
            .map_err(client_failure)?,
    )?;
    let mut incoming = state::SessionState::new(
        context.origin.clone(),
        context.binding.project_id.clone(),
        local_session.to_owned(),
        import.token_digest,
        import.session,
        workstation,
        harness,
        capabilities,
    );
    incoming.subagent = serde_json::from_value(remote["data"]["subagent"].clone())
        .map_err(|_| Failure::invalid("The remote subagent identity is invalid."))?;
    incoming.orientation = Some(parse_orientation(&orientation).ok_or_else(|| {
        Failure::invalid(
            "The service returned invalid project orientation; no session state was saved.",
        )
    })?);
    let already_adopted = existing.is_some();
    if let Some(saved) = existing {
        verify_existing(&saved, &incoming)?;
        // An identical retry must not erase orientation acknowledgements or touch
        // durable state, even if a process was interrupted after the initial save.
    } else {
        state::save(&path, &incoming).map_err(Failure::invalid)?;
    }
    Ok(
        json!({"data": {"adopted": true, "already_adopted": already_adopted,
        "service_origin": context.origin, "project_id": context.binding.project_id,
        "local_session": local_session, "session_id": incoming.session.id,
        "authority_renewed": false, "remote_writes": false,
        "orientation": orientation["data"],
        "next_action": "Inspect current task ownership and lease before native work; serialize MCP and CLI writes."}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use coordinator_client::{CoordinatorClient, HttpMethod};

    #[test]
    fn adoption_requires_explicit_quiescence_and_has_no_secret_arguments() {
        assert!(Cli::try_parse_from(["agent-coordinator", "session", "adopt-mcp"]).is_err());
        assert!(
            Cli::try_parse_from([
                "agent-coordinator",
                "--session",
                "local",
                "session",
                "adopt-mcp",
                "--mcp-writes-quiescent"
            ])
            .is_ok()
        );
        for option in ["--token", "--proof", "--session-proof"] {
            assert!(
                Cli::try_parse_from([
                    "agent-coordinator",
                    "session",
                    "adopt-mcp",
                    "--mcp-writes-quiescent",
                    option,
                    "fixture-secret"
                ])
                .is_err()
            );
        }
    }

    #[test]
    fn diagnostic_and_migration_commands_require_explicit_session_state() {
        assert!(
            Cli::try_parse_from([
                "agent-coordinator",
                "--session",
                "local",
                "session",
                "diagnose"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "agent-coordinator",
                "--session",
                "local",
                "session",
                "migrate",
                "--from-state-dir",
                "/private/source"
            ])
            .is_err()
        );
    }
    fn context() -> ContextData {
        ContextData {
            binding_path: "binding.toml".into(),
            binding: config::RepositoryBinding {
                service_url: "https://example.test".into(),
                project_id: "project".into(),
                project_name: None,
            },
            origin: "https://example.test".into(),
            client: CoordinatorClient::new("https://example.test", "fixture-token", false).unwrap(),
            credential_digest: hex::encode(Sha256::digest(b"fixture-token")),
        }
    }
    fn values() -> std::collections::BTreeMap<&'static str, String> {
        [
            ("AGENT_COORDINATOR_MCP_URL", "https://example.test/mcp"),
            ("AGENT_COORDINATOR_MCP_PROJECT_ID", "project"),
            ("AGENT_COORDINATOR_MCP_TOKEN", "fixture-token"),
            ("AGENT_COORDINATOR_MCP_SESSION_ID", "session-id"),
            ("AGENT_COORDINATOR_MCP_SESSION_PROOF", "fixture-proof"),
        ]
        .map(|(k, v)| (k, v.into()))
        .into()
    }
    #[test]
    fn import_rejects_missing_or_mismatched_binding_and_credentials_without_disclosure() {
        let context = context();
        for key in values().keys() {
            let mut env = values();
            env.remove(key);
            assert!(read_import(&context, |name| env.get(name).cloned()).is_err());
        }
        for key in [
            "AGENT_COORDINATOR_MCP_URL",
            "AGENT_COORDINATOR_MCP_PROJECT_ID",
            "AGENT_COORDINATOR_MCP_TOKEN",
            "AGENT_COORDINATOR_MCP_SESSION_ID",
        ] {
            let mut env = values();
            env.insert(key, "misplaced/secret".into());
            let failure = read_import(&context, |name| env.get(name).cloned())
                .err()
                .unwrap();
            assert!(!failure.output.to_string().contains("misplaced/secret"));
        }
    }
    #[test]
    fn remote_identity_and_existing_state_are_exact_and_pending_state_is_never_replaced() {
        let context = context();
        let env = values();
        let import = read_import(&context, |name| env.get(name).cloned())
            .ok()
            .unwrap();
        let remote = json!({"session_id":"session-id", "closed_at":null,"workstation_id":"machine","harness":"mcp","capabilities":["code"]});
        assert!(verify_remote(&remote, &import, "other-machine").is_err());
        let mut closed = remote.clone();
        closed["closed_at"] = json!("yesterday");
        assert!(verify_remote(&closed, &import, "machine").is_err());
        let (harness, capabilities) = verify_remote(&remote, &import, "machine").ok().unwrap();
        let incoming = state::SessionState::new(
            context.origin,
            "project".into(),
            "local".into(),
            import.token_digest,
            import.session,
            "machine".into(),
            harness,
            capabilities,
        );
        assert!(verify_existing(&incoming, &incoming).is_ok());
        let mut existing = incoming.clone();
        existing.session.proof = "other-proof".into();
        assert!(verify_existing(&existing, &incoming).is_err());
        let mut existing = incoming.clone();
        existing.pending = Some(state::PendingMutation {
            key: "saved".into(),
            method: HttpMethod::Post,
            path: "/api/v1/claims".into(),
            body: json!({}),
            include_session_id: true,
        });
        assert!(verify_existing(&existing, &incoming).is_err());
    }

    #[test]
    fn migration_copies_quiescent_state_without_removing_or_overwriting() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let context = context();
        let source_path = state::path_for_directory(
            source.path(),
            &context.origin,
            &context.binding.project_id,
            "local",
        )
        .unwrap();
        let saved = state::SessionState::new(
            context.origin.clone(),
            context.binding.project_id.clone(),
            "local".into(),
            context.credential_digest.clone(),
            SessionAuth {
                id: "session-id".into(),
                proof: "fixture-proof".into(),
            },
            "machine".into(),
            "harness".into(),
            vec!["code".into()],
        );
        state::save(&source_path, &saved).unwrap();

        let cli = Cli::try_parse_from([
            "agent-coordinator",
            "--session",
            "local",
            "--state-dir",
            destination.path().to_str().unwrap(),
            "session",
            "migrate",
            "--from-state-dir",
            source.path().to_str().unwrap(),
            "--session-writes-quiescent",
        ])
        .unwrap();
        let crate::Command::Session {
            command: SessionCommand::Migrate(args),
        } = &cli.command
        else {
            panic!("migration command was not parsed")
        };
        let result = migrate(&cli, &context, args).unwrap();
        let destination_path = state::path_for(
            cli.state_dir.as_deref(),
            &context.origin,
            &context.binding.project_id,
            "local",
        )
        .unwrap();
        assert!(source_path.is_file());
        assert!(destination_path.is_file());
        assert_eq!(result["data"]["source_preserved"], true);
        assert!(migrate(&cli, &context, args).is_err());
        assert!(source_path.is_file());
        assert!(destination_path.is_file());
    }
}
