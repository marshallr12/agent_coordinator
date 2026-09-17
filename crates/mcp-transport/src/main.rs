//! Stdio adapter for the coordinator's stateless JSON Streamable HTTP endpoint.
//! Credentials never come from model-visible arguments or enter diagnostics.
mod journal;
mod private;
use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use journal::Journal;
use reqwest::{
    Client, Url,
    header::{HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
const MAX_BYTES: usize = 4 * 1024 * 1024;
const STATUS: &str = "coordinator_transport_status";
const RETRY: &str = "coordinator_transport_retry";

#[derive(Parser)]
#[command(
    about = "Durable stdio MCP adapter; supply coordinator credentials through protected environment mappings"
)]
struct Args {
    /// Dedicated absolute private directory outside the repository; parent must exist.
    #[arg(long, env = "AGENT_COORDINATOR_MCP_STATE_DIR")]
    state_dir: PathBuf,
    /// Only for disposable local tests. Never permits remote plaintext.
    #[arg(
        long,
        env = "AGENT_COORDINATOR_ALLOW_INSECURE_LOOPBACK",
        default_value_t = false
    )]
    allow_insecure_loopback: bool,
}

struct Adapter {
    http: Client,
    url: Url,
    journal: Journal,
    tools: BTreeMap<String, Value>,
    protocol: String,
    public_identity: Value,
}

fn env(name: &str) -> Result<String> {
    let value =
        std::env::var(name).context("required protected MCP environment mapping missing")?;
    ensure!(!value.is_empty(), "empty protected MCP environment mapping");
    Ok(value)
}

fn endpoint(value: &str, allow_loopback: bool) -> Result<Url> {
    let url = Url::parse(value).context("invalid MCP endpoint")?;
    let loopback = url.host_str().is_some_and(|h| {
        h == "localhost"
            || h.parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
            || h == "[::1]"
    });
    ensure!(
        (url.scheme() == "https" || (allow_loopback && url.scheme() == "http" && loopback))
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/mcp"
            && url.query().is_none()
            && url.fragment().is_none(),
        "MCP endpoint must be the trusted HTTPS origin plus /mcp"
    );
    Ok(url)
}

impl Adapter {
    fn new(args: Args) -> Result<Self> {
        let url = endpoint(
            &env("AGENT_COORDINATOR_MCP_URL")?,
            args.allow_insecure_loopback,
        )?;
        let token = env("AGENT_COORDINATOR_MCP_TOKEN")?;
        let session = env("AGENT_COORDINATOR_MCP_SESSION_ID")?;
        let proof = env("AGENT_COORDINATOR_MCP_SESSION_PROOF")?;
        let project = env("AGENT_COORDINATOR_MCP_PROJECT_ID")?;
        let journal = Journal::open(
            &args.state_dir,
            journal::binding(&[url.as_str(), &session, &token, &proof, &project]),
        )?;
        let public_identity = json!({"session_id":session,"project_id":project,"service_url":url.origin().ascii_serialization()});
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("authorization", format!("Bearer {token}")),
            ("x-coordinator-session", session),
            ("x-coordinator-session-proof", proof),
        ] {
            let mut value =
                HeaderValue::from_str(&value).context("invalid protected header mapping")?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        headers.insert(
            "accept",
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        let http = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            url,
            journal,
            tools: BTreeMap::new(),
            protocol: "2025-11-25".into(),
            public_identity,
        })
    }

    async fn remote(&self, method: &str, params: Value) -> Result<Value> {
        let id = uuid::Uuid::new_v4().to_string();
        let response = self
            .http
            .post(self.url.clone())
            .header("mcp-protocol-version", &self.protocol)
            .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .send()
            .await
            .context("upstream unavailable; saved mutation remains pending")?;
        ensure!(
            response.status().is_success(),
            "upstream rejected request; saved mutation remains pending"
        );
        ensure!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.split(';').next() == Some("application/json")),
            "upstream did not return JSON"
        );
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= MAX_BYTES,
                "upstream response limit exceeded"
            );
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).context("invalid upstream response")?;
        ensure!(
            value["jsonrpc"] == "2.0"
                && value["id"] == id
                && (value.get("result").is_some() ^ value.get("error").is_some()),
            "upstream response identity or envelope mismatch"
        );
        Ok(value)
    }

    async fn catalog(&mut self) -> Result<Value> {
        let value = self.remote("tools/list", json!({})).await?;
        ensure!(value.get("error").is_none(), "upstream discovery failed");
        ensure!(
            value["result"].get("nextCursor").is_none(),
            "paginated catalog unsupported; mutations disabled"
        );
        let tools = value["result"]["tools"]
            .as_array()
            .context("invalid upstream catalog")?;
        let mut catalog = BTreeMap::new();
        for tool in tools {
            let name = tool["name"].as_str().context("invalid tool name")?;
            ensure!(
                name != STATUS
                    && name != RETRY
                    && catalog.insert(name.to_owned(), tool.clone()).is_none(),
                "duplicate or reserved tool name"
            );
        }
        self.tools = catalog;
        let mut result = value["result"].clone();
        let tools = result["tools"].as_array_mut().unwrap();
        tools.push(local_tool(STATUS,"Inspect durable adapter capability and whether a mutation is pending. Does not grant or renew ownership.",true));
        tools.push(local_tool(RETRY,"Replay the exact pending mutation, or the most recently completed request if no mutation is pending, after uncertainty. Inspect current ownership separately; receipt replay does not renew a lease.",false));
        Ok(result)
    }

    async fn dispatch(&mut self, params: Value) -> Result<Value> {
        let value = self.remote("tools/call", params.clone()).await?;
        // JSON-RPC and tool errors may follow a committed effect. Conservatively
        // preserve intent; reads and exact retries remain available.
        if value.get("error").is_none()
            && (value["result"]["isError"] != true || rejected_claim(&params, &value["result"]))
        {
            ensure!(value["result"]["content"].is_array(), "invalid tool result");
            self.journal.complete()?;
        }
        if let Some(error) = value.get("error") {
            return Ok(
                json!({"isError":true,"content":[{"type":"text","text":"Upstream protocol error; mutation remains pending. Use coordinator_transport_retry."}],"structuredContent":{"error":error,"pending":true}}),
            );
        }
        Ok(value["result"].clone())
    }

    async fn handle(&mut self, method: &str, params: Value) -> Result<Value> {
        match method {
            "initialize" => {
                let remote = self.remote(method, params).await?;
                ensure!(
                    remote.get("error").is_none(),
                    "upstream initialization failed"
                );
                let mut result = remote["result"].clone();
                self.protocol = result["protocolVersion"]
                    .as_str()
                    .context("missing protocol version")?
                    .to_owned();
                let instructions = result["instructions"].as_str().unwrap_or("").to_owned();
                result["instructions"] = json!(format!(
                    "{instructions}\n\nThis connection uses agent-coordinator-mcp with a private durable journal. Call coordinator_transport_status before mutations; its configured_identity supplies the non-secret session_id and project_id for registration. If session_get reports an unregistered session, use that configured session_id with coordinator_session_register; never invent a different ID. Supply a unique idempotency_key; the adapter saves the exact arguments before sending. After uncertainty use coordinator_transport_retry, never a new key. Reads remain available. A replayed receipt is not proof of current ownership. Do not share this configured session with another host or transport writer."
                ));
                Ok(result)
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                ensure!(
                    params.get("cursor").is_none(),
                    "catalog cursors unsupported"
                );
                self.catalog().await
            }
            "tools/call" => {
                let name = params["name"].as_str().context("missing tool name")?;
                if name == STATUS || name == RETRY {
                    ensure!(
                        params["arguments"].as_object().is_none_or(|a| a.is_empty()),
                        "local tools take no arguments"
                    );
                    if name == STATUS {
                        let mut status = self.journal.status();
                        status["configured_identity"] = self.public_identity.clone();
                        return Ok(structured(status));
                    }
                    let mut pending = self
                        .journal
                        .retry_request()
                        .context("no saved mutation to retry")?;
                    self.journal.prepare(&mut pending)?;
                    return self.dispatch(pending).await;
                }
                if self.tools.is_empty() {
                    self.catalog().await?;
                }
                let tool = self
                    .tools
                    .get(name)
                    .context("tool not in trusted catalog")?;
                if !argument_envelope_valid(
                    &tool["inputSchema"],
                    params.get("arguments").unwrap_or(&json!({})),
                ) {
                    let mut result = structured(
                        json!({"error":{"code":"invalid_tool_arguments","message":"Correct the tool arguments using the advertised inputSchema. This request was rejected before dispatch.","allowed_arguments":tool["inputSchema"]["properties"].as_object().map(|p|p.keys().collect::<Vec<_>>()),"required_arguments":tool["inputSchema"]["required"]},"dispatched":false,"journal_pending":self.journal.pending().is_some()}),
                    );
                    result["isError"] = json!(true);
                    return Ok(result);
                }
                if tool["annotations"]["readOnlyHint"] == true {
                    let value = self.remote(method, params).await?;
                    ensure!(value.get("error").is_none(), "upstream read failed");
                    return Ok(value["result"].clone());
                }
                ensure!(
                    tool["inputSchema"]["properties"]
                        .get("idempotency_key")
                        .is_some(),
                    "tool lacks a supported durable mutation contract"
                );
                let mut params = params;
                self.journal.prepare(&mut params)?;
                self.dispatch(params).await
            }
            _ => bail!("unsupported MCP method"),
        }
    }
}

// Check the closed outer tool argument envelope before journaling. The service
// remains authoritative for typed body validation and current policy/authority.
fn argument_envelope_valid(schema: &Value, arguments: &Value) -> bool {
    let Some(args) = arguments.as_object() else {
        return false;
    };
    let Some(properties) = schema["properties"].as_object() else {
        return false;
    };
    if schema["additionalProperties"] == false
        && args.keys().any(|key| !properties.contains_key(key))
    {
        return false;
    }
    schema["required"].as_array().is_none_or(|required| {
        required
            .iter()
            .all(|key| key.as_str().is_some_and(|key| args.contains_key(key)))
    })
}

// These claim rejections occur after receipt lookup and before any claim write.
// Never generalize this to isError, retryable=false, or an HTTP status class.
fn rejected_claim(params: &Value, result: &Value) -> bool {
    params["name"] == "coordinator_claim"
        && result["isError"] == true
        && result["structuredContent"]["request_id"].is_string()
        && matches!(
            result["structuredContent"]["error"]["code"].as_str(),
            Some(
                "claim_conflict" | "revision_conflict" | "policy_changed" | "instructions_required"
            )
        )
}

fn local_tool(name: &str, description: &str, read_only: bool) -> Value {
    json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":{},"additionalProperties":false},"annotations":{"readOnlyHint":read_only}})
}
fn structured(value: Value) -> Value {
    json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value,"isError":false})
}

#[tokio::main]
async fn main() {
    // Never print error chains: HTTP errors can contain configured URLs or data.
    if run().await.is_err() {
        eprintln!(
            "MCP adapter stopped: configuration, private journal, or stdio failure. Retain the journal and reconcile with the same configured identity; no capability is available from this process."
        );
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let mut adapter = Adapter::new(Args::parse())?;
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    loop {
        let mut line = Vec::new();
        let count = (&mut input)
            .take((MAX_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)
            .await?;
        if count == 0 {
            break;
        }
        ensure!(line.len() <= MAX_BYTES, "stdio request too large");
        let request: Value = serde_json::from_slice(&line)?;
        ensure!(
            request["jsonrpc"] == "2.0" && request.is_object(),
            "invalid JSON-RPC request"
        );
        let method = request["method"].as_str().context("missing method")?;
        // Notifications never cause network mutations; cancellation cannot erase
        // a pending intent. The stateless upstream requires no initialization notification.
        let Some(id) = request.get("id") else {
            continue;
        };
        let result = adapter
            .handle(method, request.get("params").cloned().unwrap_or(json!({})))
            .await;
        let reply = match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(_) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"Adapter refused or could not confirm request. Inspect coordinator_transport_status; if pending, use coordinator_transport_retry with the same configured session. No ownership or renewal is implied."}})
            }
        };
        output.write_all(&serde_json::to_vec(&reply)?).await?;
        output.write_all(b"\n").await?;
        output.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn argument_errors_are_detected_before_journaling() {
        let schema = json!({"properties":{"body":{},"idempotency_key":{},"session":{}},"required":["body","idempotency_key"],"additionalProperties":false});
        let mut args = json!({"project":"wrong-level","body":{"project_id":"p"},"idempotency_key":"unique-key-123456"});
        assert!(!argument_envelope_valid(&schema, &args));
        args.as_object_mut().unwrap().remove("project");
        assert!(argument_envelope_valid(&schema, &args));
        assert!(!argument_envelope_valid(&schema, &json!({})));
    }
    #[test]
    fn endpoint_never_accepts_remote_plaintext_or_credential_url() {
        for value in [
            "http://example.org/mcp",
            "https://user:secret@example.org/mcp",
            "https://example.org/mcp?token=x",
            "https://example.org/other",
        ] {
            assert!(endpoint(value, true).is_err());
        }
        assert!(endpoint("http://127.0.0.1:1234/mcp", true).is_ok());
        assert!(endpoint("http://127.0.0.1:1234/mcp", false).is_err());
    }
}
