//! Launch-time environment injection, not an MCP proxy or remote runner.
use std::{ffi::OsString, path::Path, process::Stdio};

use clap::Args;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::process::Command;

use crate::{
    Cli, ContextData, Failure, client_failure, config, load_required_state, require_success, state,
};

#[derive(Args)]
pub struct LaunchArgs {
    /// Absolute path to a trusted foreground MCP client, followed by its arguments. Configure
    /// its endpoint to this binding's exact /mcp URL and headers from the injected environment.
    #[arg(required = true, trailing_var_arg = true, num_args = 1..)]
    command: Vec<OsString>,
}

pub async fn launch(cli: &Cli, context: &ContextData, args: &LaunchArgs) -> Result<Value, Failure> {
    let (session_lock, path, saved) = load_required_state(cli, context)?;
    if saved.pending.is_some() {
        return Err(Failure::invalid(
            "Resolve the saved pending CLI mutation with retry before launching an MCP client.",
        ));
    }
    // A second client must use a different harness. This lock is separate from
    // the short-lived CLI state lock, so this client's native CLI remains usable.
    let _launcher_lock = state::lock(&path.with_extension("mcp-client.json"))
        .map_err(|_| Failure::temporary("Cannot acquire the MCP launcher lock. Another client may use this local harness; select another --session for an independent client."))?;
    let response = context
        .client
        .get(
            &format!("/api/v1/sessions/{}", saved.session.id),
            Some(&saved.session),
        )
        .await
        .map_err(client_failure)?;
    let response = require_success(response)?;
    let remote = &response["data"];
    if remote["session_id"].as_str() != Some(saved.session.id.as_str())
        || !remote.get("closed_at").is_some_and(Value::is_null)
    {
        return Err(Failure::invalid(
            "This harness session is closed or no longer matches. Connect a fresh --session first.",
        ));
    }
    let token =
        config::token(&context.origin, cli.allow_insecure_loopback).map_err(Failure::invalid)?;
    if hex::encode(Sha256::digest(token.as_bytes())) != saved.credential_digest {
        return Err(Failure::invalid(
            "The credential changed during launch. Reconnect using a fresh harness session.",
        ));
    }
    let mut child = child_command(&args.command, &context.origin, &token, &saved)?;
    child
        .env("AGENT_COORDINATOR_REPO_CONFIG", &context.binding_path)
        .env(
            "AGENT_COORDINATOR_ALLOW_INSECURE_LOOPBACK",
            if cli.allow_insecure_loopback {
                "true"
            } else {
                "false"
            },
        );
    // Never hold the native mutation journal lock while the client runs. Its
    // CLI subprocesses must use their ordinary short-lived locks and journals.
    drop(session_lock);
    let status = child.status().await.map_err(|_| {
        Failure::temporary("Could not launch or wait for the trusted local MCP client.")
    })?;
    if !status.success() {
        return Err(Failure::local(
            7,
            "mcp_client_failed",
            "The local MCP client exited unsuccessfully. Its session remains unchanged; inspect saved work before resuming.",
            false,
        ));
    }
    Ok(
        json!({"data":{"client_exited":true,"service_origin":context.origin,"local_session":saved.local_session,"session_id":saved.session.id,"authority_renewed":false}}),
    )
}

fn child_command(
    command: &[OsString],
    origin: &str,
    token: &str,
    saved: &state::SessionState,
) -> Result<Command, Failure> {
    let executable = command.first().ok_or_else(|| {
        Failure::invalid("Specify a trusted local MCP client executable after --.")
    })?;
    if !Path::new(executable).is_absolute() {
        return Err(Failure::invalid(
            "Use an absolute path to a trusted foreground MCP client executable; PATH lookup is disabled.",
        ));
    }
    let mut child = Command::new(executable);
    child
        .args(&command[1..])
        .env("AGENT_COORDINATOR_MCP_TOKEN", token)
        .env("AGENT_COORDINATOR_MCP_SESSION_ID", &saved.session.id)
        .env("AGENT_COORDINATOR_MCP_SESSION_PROOF", &saved.session.proof)
        .env("AGENT_COORDINATOR_MCP_URL", format!("{origin}/mcp"))
        .env("AGENT_COORDINATOR_MCP_PROJECT_ID", &saved.project_id)
        .env("AGENT_COORDINATOR_TOKEN", token)
        .env("AGENT_COORDINATOR_ORIGIN", origin)
        .env("AGENT_COORDINATOR_SESSION", &saved.local_session)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    // Do not print/debug this Command: its environment contains secret values.
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;
    use coordinator_client::SessionAuth;

    #[test]
    fn launcher_credentials_are_environment_only_and_use_exact_saved_identity() {
        let saved = state::SessionState::new(
            "https://example.test".into(),
            "project".into(),
            "local-harness".into(),
            "digest".into(),
            SessionAuth {
                id: "session-id".into(),
                proof: "fixture-proof".into(),
            },
            "workstation".into(),
            "harness".into(),
            vec![],
        );
        let command = child_command(
            &[
                std::env::current_exe().unwrap().into_os_string(),
                OsString::from("arg with spaces"),
            ],
            &saved.service_origin,
            "fixture-token",
            &saved,
        )
        .unwrap_or_else(|_| panic!("Command preparation failed"));
        let native = command.as_std();
        let args: Vec<_> = native.get_args().collect();
        assert_eq!(args, ["arg with spaces"]);
        let env: std::collections::BTreeMap<_, _> = native.get_envs().collect();
        for (key, expected) in [
            ("AGENT_COORDINATOR_MCP_TOKEN", "fixture-token"),
            ("AGENT_COORDINATOR_MCP_SESSION_ID", "session-id"),
            ("AGENT_COORDINATOR_MCP_SESSION_PROOF", "fixture-proof"),
            ("AGENT_COORDINATOR_MCP_URL", "https://example.test/mcp"),
            ("AGENT_COORDINATOR_SESSION", "local-harness"),
        ] {
            assert!(
                env.get(std::ffi::OsStr::new(key)).copied().flatten()
                    == Some(std::ffi::OsStr::new(expected))
            );
        }
    }

    #[test]
    fn launcher_lock_excludes_another_launcher_but_allows_native_journal_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        let launcher_path = path.with_extension("mcp-client.json");
        let launcher = state::lock(&launcher_path).unwrap();
        assert!(state::lock(&launcher_path).is_err());
        let native = state::lock(&path).unwrap();
        drop(native);
        drop(launcher);
        assert!(state::lock(&launcher_path).is_ok());
    }

    #[test]
    fn launcher_rejects_path_search_before_exposing_credentials() {
        let saved = state::SessionState::new(
            "https://example.test".into(),
            "project".into(),
            "local".into(),
            "digest".into(),
            SessionAuth {
                id: "session".into(),
                proof: "fixture-proof".into(),
            },
            "workstation".into(),
            "harness".into(),
            vec![],
        );
        for executable in ["trusted-client", "./trusted-client", ""] {
            assert!(
                child_command(
                    &[executable.into()],
                    &saved.service_origin,
                    "fixture-token",
                    &saved
                )
                .is_err()
            );
        }
    }
}
