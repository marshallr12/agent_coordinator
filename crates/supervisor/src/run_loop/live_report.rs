//! The live side of [`super::report`]: sends cost-row batches with the
//! implementer's coordinator credential, which any write credential of an
//! agent principal may use to report. A failed pass is not retried for
//! [`RETRY_AFTER_MS`], so a coordinator that is down costs one request per
//! interval, not one per poll.
use super::report::{self, Delivery, Summary};
use crate::config::Config;
use crate::profile::Role;
use coordinator_client::CoordinatorClient;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// How long after a pass that left rows unsent the next one waits.
pub(super) const RETRY_AFTER_MS: i64 = 10 * 60 * 1000;

/// How long after the caps were reported they are reported again, so a
/// coordinator restored from an older backup learns them again.
pub(super) const CAPS_EVERY_MS: i64 = 60 * 60 * 1000;

/// This host's name as the coordinator records it (the kernel's hostname,
/// which is what `e2e-canary.py` reports too).
pub(super) fn host_name() -> String {
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
    match name.trim() {
        "" => "unknown".into(),
        name => name.chars().take(253).collect(),
    }
}

/// How the coordinator's answer to a batch is acted on: stored on a 200; a
/// 400 or 422 refuses the rows themselves; a 404 coded `project_not_found`
/// says the rows' project does not exist; a 401 or 403 means this credential
/// cannot report, which every project shares; anything else (including any
/// other 404, which an older coordinator without the route also answers) is
/// retried.
fn delivery(status: u16, body: &Value) -> Delivery {
    let data = &body["data"];
    let code = body["error"]["code"].as_str().unwrap_or("unknown");
    match status {
        200 => Delivery::Stored(
            data["accepted"].as_u64().unwrap_or(0),
            data["duplicates"].as_u64().unwrap_or(0),
        ),
        400 | 422 => Delivery::Rejected(format!("HTTP {status} {code}")),
        401 | 403 => Delivery::Retry {
            stop: true,
            reason: format!("HTTP {status} {code}"),
        },
        404 if code == "project_not_found" => Delivery::MissingProject(format!(
            "HTTP {status} {code}: the coordinator has no project {}",
            body["error"]["details"]["project_id"]
                .as_str()
                .unwrap_or("with this id")
        )),
        404 => Delivery::Retry {
            stop: false,
            reason: format!("HTTP {status} {code}"),
        },
        _ => Delivery::Retry {
            stop: true,
            reason: format!("HTTP {status} {code}"),
        },
    }
}

/// Sends every unsent ledger row; `default_project` is the project of rows
/// that name none. Never fails: what could not be sent stays unsent.
pub(super) fn pass(
    config: &Config,
    runtime: &tokio::runtime::Runtime,
    client: &CoordinatorClient,
    default_project: &str,
) -> Summary {
    let mut send = |project: &str, body: &Value, key: &str| {
        let path = format!("/api/v1/projects/{project}/telemetry/cost-rows");
        match runtime.block_on(client.mutate(&path, body, key, None)) {
            Ok(response) => delivery(response.status, &response.body),
            Err(error) => Delivery::Retry {
                stop: true,
                reason: error.to_string(),
            },
        }
    };
    report::flush(config, &host_name(), Some(default_project), &mut send)
}

/// The body of a caps report: this host's daily caps per role.
fn caps_body(config: &Config, host: &str) -> Value {
    serde_json::json!({
        "host": host,
        "impl_daily_usd": config.health.cap(Role::Implementer),
        "rev_daily_usd": config.health.cap(Role::Reviewer),
    })
}

/// The Idempotency-Key of a caps report: the same caps reuse it.
fn caps_key(project: &str, body: &Value) -> String {
    let digest = Sha256::digest(format!("{project}\n{body}"));
    format!("caps-{}", &hex::encode(digest)[..40])
}

/// Tells the coordinator the daily caps this host enforces, once per project.
/// Returns false when any report failed, so the caller tries again later. A
/// coordinator that predates the route answers 404 and counts as a failure.
pub(super) fn caps_pass(
    config: &Config,
    runtime: &tokio::runtime::Runtime,
    client: &CoordinatorClient,
    projects: &[&str],
) -> bool {
    let body = caps_body(config, &host_name());
    let mut sent = true;
    for project in projects {
        let path = format!("/api/v1/projects/{project}/telemetry/caps");
        let key = caps_key(project, &body);
        match runtime.block_on(client.mutate(&path, &body, &key, None)) {
            Ok(response) if response.status == 200 => {}
            Ok(response) => {
                sent = false;
                eprintln!(
                    "agentc-supervisor run: report caps: HTTP {}",
                    response.status
                );
            }
            Err(error) => {
                sent = false;
                eprintln!("agentc-supervisor run: report caps: {error}");
            }
        }
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn caps_report_both_roles_and_key_on_their_content() {
        let mut config = Config::default();
        config.health.implementer_daily_usd = 12.5;
        config.health.reviewer_daily_usd = 0.0;
        let body = caps_body(&config, "h");
        assert_eq!(body["host"], "h");
        assert_eq!(body["impl_daily_usd"], 12.5);
        assert_eq!(body["rev_daily_usd"], 0.0);
        assert_eq!(caps_key("p", &body), caps_key("p", &body));
        assert_ne!(caps_key("p", &body), caps_key("q", &body));
        config.health.reviewer_daily_usd = 5.0;
        assert_ne!(
            caps_key("p", &body),
            caps_key("p", &caps_body(&config, "h"))
        );
    }

    #[test]
    fn answers_map_to_deliveries() {
        let stored = json!({"data": {"accepted": 2, "duplicates": 1}});
        assert_eq!(delivery(200, &stored), Delivery::Stored(2, 1));
        let refused = json!({"error": {"code": "invalid_request"}});
        assert!(matches!(delivery(400, &refused), Delivery::Rejected(_)));
        let missing = json!({"error": {"code": "record_not_found"}});
        assert!(matches!(
            delivery(404, &missing),
            Delivery::Retry { stop: false, .. }
        ));
        // An older coordinator's 404 may not even be a JSON error.
        assert!(matches!(
            delivery(404, &json!(null)),
            Delivery::Retry { stop: false, .. }
        ));
        let gone = json!({"error": {"code": "project_not_found",
            "details": {"project_id": "p1"}}});
        match delivery(404, &gone) {
            Delivery::MissingProject(reason) => assert!(reason.contains("p1"), "{reason}"),
            other => panic!("{other:?}"),
        }
        for status in [401, 403, 409, 429, 500, 502] {
            assert!(matches!(
                delivery(status, &json!({})),
                Delivery::Retry { stop: true, .. }
            ));
        }
    }
}
