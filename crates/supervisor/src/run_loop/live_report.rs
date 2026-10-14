//! The live side of [`super::report`]: sends cost-row batches with the
//! implementer's coordinator credential, which any write credential of an
//! agent principal may use to report. A failed pass is not retried for
//! [`RETRY_AFTER_MS`], so a coordinator that is down costs one request per
//! interval, not one per poll.
use super::report::{self, Delivery, Summary};
use crate::config::Config;
use coordinator_client::CoordinatorClient;
use serde_json::Value;

/// How long after a pass that left rows unsent the next one waits.
pub(super) const RETRY_AFTER_MS: i64 = 10 * 60 * 1000;

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
/// 400 or 422 refuses the rows themselves; a 401 or 403 means this
/// credential cannot report, which every project shares; anything else
/// (including a 404, which an older coordinator also answers) is retried.
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        for status in [401, 403, 409, 429, 500, 502] {
            assert!(matches!(
                delivery(status, &json!({})),
                Delivery::Retry { stop: true, .. }
            ));
        }
    }
}
