//! Writes recheck authorization after obtaining SQLite's writer lock.
use crate::{
    auth::{Actor, Auth, digest},
    error::AppError,
    state::AppState,
};
use axum::http::HeaderMap;
use serde::Serialize;
use serde_json::Value;
use sqlx::{Row, Sqlite, Transaction};

pub struct Mutation {
    pub tx: Transaction<'static, Sqlite>,
    pub actor: Actor,
    pub now: i64,
    pub replay: Option<Value>,
    operation: String,
    key: String,
    fingerprint: String,
    authority_epoch: String,
}
impl Mutation {
    pub async fn begin<T: Serialize>(
        state: &AppState,
        auth: &Auth,
        headers: &HeaderMap,
        operation: &str,
        input: &T,
    ) -> Result<Self, AppError> {
        let key = mutation_key(headers)?;
        let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
        let clock = state.sample_clock(&mut tx).await?;
        if clock.incident_detected {
            tx.commit().await?;
            return Err(crate::state::clock_reconciliation_error());
        }
        let now = clock.now;
        let actor = auth.verify(&mut tx, now).await?;
        let authority_epoch = authority(&mut tx, operation, clock.incident_active).await?;
        // Session identity and proof verifier are included to reject key reuse across harnesses.
        let proof = headers
            .get("X-Coordinator-Session-Proof")
            .and_then(|v| v.to_str().ok())
            .map(digest);
        let fingerprint = if authority_epoch == "initial" {
            digest(&serde_json::to_string(&(input, &actor.session_id, proof))?)
        } else {
            digest(&serde_json::to_string(&(
                "mutation-v2",
                &authority_epoch,
                input,
                &actor.session_id,
                proof,
            ))?)
        };
        Self::load_receipt(tx, now, actor, operation, key, fingerprint, authority_epoch).await
    }

    /// Account creation may be retried after an uncertain response forced the
    /// administrator to sign in again. This one operation binds its receipt to
    /// the human administrator principal rather than one browser session. All
    /// current browser authentication and administrator authorization are still
    /// rechecked after acquiring SQLite's writer lock.
    pub async fn begin_human_admin_account_creation<T: Serialize>(
        state: &AppState,
        auth: &Auth,
        headers: &HeaderMap,
        input: &T,
    ) -> Result<Self, AppError> {
        auth.require_browser()?;
        let operation = "POST /api/v1/admin/operators";
        let key = mutation_key(headers)?;
        let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
        let clock = state.sample_clock(&mut tx).await?;
        if clock.incident_detected {
            tx.commit().await?;
            return Err(crate::state::clock_reconciliation_error());
        }
        let now = clock.now;
        let actor = auth.verify(&mut tx, now).await?;
        crate::auth::admin(&actor)?;
        let authority_epoch = authority(&mut tx, operation, clock.incident_active).await?;
        let fingerprint = if authority_epoch == "initial" {
            digest(&serde_json::to_string(&(
                "human-admin-account-creation-v1",
                input,
                &actor.id,
            ))?)
        } else {
            digest(&serde_json::to_string(&(
                "human-admin-account-creation-v2",
                &authority_epoch,
                input,
                &actor.id,
            ))?)
        };
        Self::load_receipt(tx, now, actor, operation, key, fingerprint, authority_epoch).await
    }

    pub async fn begin_reporter<T: Serialize>(
        state: &AppState,
        auth: &crate::jobs::ReporterAuth,
        headers: &HeaderMap,
        operation: &str,
        input: &T,
    ) -> Result<Self, AppError> {
        let key = mutation_key(headers)?;
        let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
        let clock = state.sample_clock(&mut tx).await?;
        if clock.incident_detected {
            tx.commit().await?;
            return Err(crate::state::clock_reconciliation_error());
        }
        let now = clock.now;
        let actor = auth.verify(&mut tx, now).await?;
        let authority_epoch = authority(&mut tx, operation, clock.incident_active).await?;
        let fingerprint = if authority_epoch == "initial" {
            digest(&serde_json::to_string(&("reporter-v1", auth.id(), input))?)
        } else {
            digest(&serde_json::to_string(&(
                "reporter-v2",
                &authority_epoch,
                auth.id(),
                input,
            ))?)
        };
        Self::load_receipt(tx, now, actor, operation, key, fingerprint, authority_epoch).await
    }

    async fn load_receipt(
        mut tx: Transaction<'static, Sqlite>,
        now: i64,
        actor: Actor,
        operation: &str,
        key: String,
        fingerprint: String,
        authority_epoch: String,
    ) -> Result<Self, AppError> {
        let previous = sqlx::query("SELECT fingerprint,result_json,created_at,authority_epoch FROM mutation_receipts WHERE principal_id=? AND operation=? AND key=?")
            .bind(&actor.id).bind(operation).bind(&key).fetch_optional(&mut *tx).await?;
        let replay = if let Some(row) = previous {
            if row.get::<String, _>("authority_epoch") != authority_epoch {
                return Err(AppError::conflict(
                    "request_from_previous_restore",
                    "This idempotency key belongs to authority from before the latest restore. Inspect the old result and use a new key for an intentional new operation.",
                ));
            }
            if row.get::<String, _>("fingerprint") != fingerprint {
                return Err(AppError::conflict(
                    "idempotency_conflict",
                    "This mutation key was already used with different input or session. Reconcile the original request.",
                ));
            }
            if now - row.get::<i64, _>("created_at") > 30 * 86_400_000 {
                return Err(AppError::conflict(
                    "idempotency_receipt_expired",
                    "The operation was already processed, but its replay window expired. Inspect its record before starting a new operation.",
                ));
            }
            Some(serde_json::from_str(&row.get::<String, _>("result_json"))?)
        } else {
            None
        };
        Ok(Self {
            tx,
            actor,
            now,
            replay,
            operation: operation.into(),
            key,
            fingerprint,
            authority_epoch,
        })
    }
    pub async fn finish(
        mut self,
        data: Value,
        project: Option<&str>,
        kind: &str,
        record_id: &str,
    ) -> Result<Value, AppError> {
        let encoded = serde_json::to_string(&data)?;
        sqlx::query("INSERT INTO mutation_receipts(principal_id,operation,key,fingerprint,result_json,created_at,authority_epoch) VALUES(?,?,?,?,?,?,?)")
            .bind(&self.actor.id).bind(&self.operation).bind(&self.key).bind(&self.fingerprint).bind(&encoded).bind(self.now).bind(&self.authority_epoch).execute(&mut *self.tx).await?;
        // Keep audit payloads small and never store credential-bearing response bodies here.
        sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) VALUES(?,?,?,?,?,?)")
            .bind(project).bind(&self.actor.id).bind(kind).bind(record_id).bind("{}").bind(self.now).execute(&mut *self.tx).await?;
        self.tx.commit().await?;
        Ok(data)
    }
}

async fn authority(
    tx: &mut Transaction<'static, Sqlite>,
    operation: &str,
    clock_incident_active: bool,
) -> Result<String, AppError> {
    let row = sqlx::query(
        "SELECT authority_epoch,coordination_state FROM service_state WHERE singleton=1",
    )
    .fetch_one(&mut **tx)
    .await?;
    if row.get::<String, _>("coordination_state") == "restore_reconciliation"
        && !allowed_during_restore(operation)
    {
        return Err(AppError::conflict(
            "restore_reconciliation_required",
            "The restored service is paused. Reconcile restored authority and external effects before changing coordination state.",
        ));
    }
    if clock_incident_active && !allowed_during_clock_reconciliation(operation) {
        return Err(crate::state::clock_reconciliation_error());
    }
    Ok(row.get("authority_epoch"))
}

fn allowed_during_restore(operation: &str) -> bool {
    operation == "POST /api/v1/auth/logout"
        || operation == "POST /api/v1/auth/password"
        || operation == "POST /api/v1/sessions"
        || (operation.starts_with("POST /api/v1/sessions/")
            && (operation.ends_with("/close")
                || operation.ends_with("/instruction-acknowledgments")))
        || operation == "POST /api/v1/admin/operators"
        || operation.starts_with("POST /api/v1/admin/operators/")
        || operation.starts_with("POST /api/v1/browser-sessions/")
        || operation.starts_with("POST /api/v1/admin/credentials/")
        || (operation.starts_with("POST /api/v1/admin/agents/")
            && operation.ends_with("/credentials"))
        || operation.starts_with("POST /api/v1/admin/restore/")
        || (operation.starts_with("POST /api/v1/projects/")
            && ((operation.contains("/reservations/") && operation.ends_with("/resolve"))
                || (operation.contains("/workflow-activities/")
                    && operation.ends_with("/publication-reconciliation"))))
}

fn allowed_during_clock_reconciliation(operation: &str) -> bool {
    operation == "POST /api/v1/auth/logout"
        || (operation.starts_with("POST /api/v1/sessions/") && operation.ends_with("/close"))
        || operation.starts_with("POST /api/v1/browser-sessions/")
        || operation == "POST /api/v1/admin/clock/reconcile"
        || operation.starts_with("POST /api/v1/admin/restore/")
        || (operation.starts_with("POST /api/v1/projects/")
            && ((operation.contains("/attempts/")
                && (operation.ends_with("/checkpoints") || operation.ends_with("/release")))
                || (operation.contains("/workflow-activities/")
                    && (operation.ends_with("/release")
                        || operation.ends_with("/integration-result")
                        || operation.ends_with("/publication-reconciliation")))
                || (operation.contains("/reservations/")
                    && (operation.ends_with("/release") || operation.ends_with("/resolve")))))
        || (operation.starts_with("POST /api/v1/reporters/")
            && operation.ends_with("/observations"))
}

fn mutation_key(headers: &HeaderMap) -> Result<String, AppError> {
    if headers.get_all("Idempotency-Key").iter().count() != 1 {
        return Err(AppError::bad_request(
            "Provide exactly one Idempotency-Key header.",
        ));
    }
    let key = headers.get("Idempotency-Key").and_then(|v|v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 128 && v.bytes().all(|b|b.is_ascii_graphic()))
        .ok_or_else(||AppError::bad_request("Provide an Idempotency-Key of 1–128 visible ASCII characters; persist it before sending."))?.to_owned();
    Ok(key)
}
