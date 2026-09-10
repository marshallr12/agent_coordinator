//! Bounded host-local payload retention that preserves authority and provenance.

use crate::state::AppState;
use anyhow::{Context, ensure};
use coordinator_core::timestamp;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use uuid::Uuid;

pub const DEFAULT_MAINTENANCE_BATCH_SIZE: usize = 500;
pub const DEFAULT_MAINTENANCE_MAX_BATCHES: usize = 20;
pub const MAX_MAINTENANCE_BATCH_SIZE: usize = 1_000;
pub const MAX_MAINTENANCE_BATCHES: usize = 100;
pub const RECEIPT_RESULT_RETENTION_DAYS: i64 = 30;
const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy)]
pub struct MaintenanceOptions {
    pub batch_size: usize,
    pub max_batches: usize,
}

impl Default for MaintenanceOptions {
    fn default() -> Self {
        Self {
            batch_size: DEFAULT_MAINTENANCE_BATCH_SIZE,
            max_batches: DEFAULT_MAINTENANCE_MAX_BATCHES,
        }
    }
}

/// Compacts old replay and redundant heartbeat payloads in short writer
/// transactions. This is a host-local operation, not an authenticated API.
pub async fn run_maintenance(
    state: &AppState,
    options: MaintenanceOptions,
) -> anyhow::Result<Value> {
    validate_options(options)?;
    let run_id = Uuid::new_v4().to_string();
    let (started_at, cutoff_at) = begin_run(state, &run_id, options).await?;
    let mut receipts = 0_u64;
    let mut observations = 0_u64;
    let mut batches = 0_usize;
    let mut artifact_cleanup_passes = 0_usize;
    let mut receipts_remaining = false;
    let mut observations_remaining = false;

    for batch in 1..=options.max_batches {
        let result = run_batch(state, &run_id, cutoff_at, options, batch).await?;
        batches = batch;
        receipts = receipts.saturating_add(result.receipts);
        observations = observations.saturating_add(result.observations);
        receipts_remaining = result.receipts_remaining;
        observations_remaining = result.observations_remaining;
        crate::artifacts::reconcile_store(state)
            .await
            .context("bounded artifact cleanup failed")?;
        artifact_cleanup_passes = artifact_cleanup_passes.saturating_add(1);
        if !receipts_remaining && !observations_remaining {
            break;
        }
    }
    let completed_at = complete_run(
        state,
        &run_id,
        receipts,
        observations,
        batches,
        receipts_remaining,
        observations_remaining,
    )
    .await?;
    Ok(json!({
        "run_id":run_id,
        "state":"complete",
        "started_at":timestamp(started_at),
        "completed_at":timestamp(completed_at),
        "receipt_result_cutoff":timestamp(cutoff_at),
        "receipt_results_compacted":receipts,
        "observation_payloads_compacted":observations,
        "artifact_cleanup_passes":artifact_cleanup_passes,
        "batches":batches,
        "remaining":{
            "receipt_results":receipts_remaining,
            "observation_payloads":observations_remaining,
        },
        "limits":{
            "batch_size":options.batch_size,
            "max_batches":options.max_batches,
            "receipt_result_retention_days":RECEIPT_RESULT_RETENTION_DAYS,
        }
    }))
}

fn validate_options(options: MaintenanceOptions) -> anyhow::Result<()> {
    ensure!(
        (1..=MAX_MAINTENANCE_BATCH_SIZE).contains(&options.batch_size),
        "maintenance batch_size must be between 1 and 1000"
    );
    ensure!(
        (1..=MAX_MAINTENANCE_BATCHES).contains(&options.max_batches),
        "maintenance max_batches must be between 1 and 100"
    );
    Ok(())
}

async fn begin_run(
    state: &AppState,
    run_id: &str,
    options: MaintenanceOptions,
) -> anyhow::Result<(i64, i64)> {
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let sample = state.sample_clock(&mut tx).await?;
    if sample.incident_detected {
        tx.commit().await?;
        anyhow::bail!(
            "clock_reconciliation_required: maintenance stopped after detecting host clock rollback"
        );
    }
    require_safe_clock_for_write(&sample)?;
    let cutoff = sample
        .now
        .saturating_sub(RECEIPT_RESULT_RETENTION_DAYS.saturating_mul(DAY_MS));
    sqlx::query(
        "INSERT INTO maintenance_runs(id,cutoff_at,started_at,batch_size,max_batches) \
         VALUES(?,?,?,?,?)",
    )
    .bind(run_id)
    .bind(cutoff)
    .bind(sample.now)
    .bind(i64::try_from(options.batch_size).context("maintenance batch_size overflowed")?)
    .bind(i64::try_from(options.max_batches).context("maintenance max_batches overflowed")?)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((sample.now, cutoff))
}

struct BatchResult {
    receipts: u64,
    observations: u64,
    receipts_remaining: bool,
    observations_remaining: bool,
}

async fn run_batch(
    state: &AppState,
    run_id: &str,
    cutoff_at: i64,
    options: MaintenanceOptions,
    batch: usize,
) -> anyhow::Result<BatchResult> {
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let sample = state.sample_clock(&mut tx).await?;
    if sample.incident_detected {
        tx.commit().await?;
        anyhow::bail!(
            "clock_reconciliation_required: maintenance stopped after detecting host clock rollback"
        );
    }
    require_safe_clock_for_write(&sample)?;
    let receipts = compact_receipts(
        &mut tx,
        cutoff_at,
        sample.now,
        i64::try_from(options.batch_size).context("maintenance batch_size overflowed")?,
    )
    .await?;
    let observations = compact_observations(
        &mut tx,
        cutoff_at,
        sample.now,
        i64::try_from(options.batch_size).context("maintenance batch_size overflowed")?,
    )
    .await?;
    let receipts_remaining = has_receipts(&mut tx, cutoff_at).await?;
    let observations_remaining = has_observations(&mut tx, cutoff_at).await?;
    let updated = sqlx::query(
        "UPDATE maintenance_runs SET batches=?,receipt_results_compacted=receipt_results_compacted+?, \
         observation_payloads_compacted=observation_payloads_compacted+? WHERE id=? AND state='running'",
    )
    .bind(i64::try_from(batch).context("maintenance batch number overflowed")?)
    .bind(i64::try_from(receipts).context("receipt compaction count overflowed")?)
    .bind(i64::try_from(observations).context("observation compaction count overflowed")?)
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    ensure!(
        updated.rows_affected() == 1,
        "maintenance run state changed unexpectedly"
    );
    tx.commit().await?;
    Ok(BatchResult {
        receipts,
        observations,
        receipts_remaining,
        observations_remaining,
    })
}

async fn complete_run(
    state: &AppState,
    run_id: &str,
    receipts: u64,
    observations: u64,
    batches: usize,
    receipts_remaining: bool,
    observations_remaining: bool,
) -> anyhow::Result<i64> {
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let sample = state.sample_clock(&mut tx).await?;
    if sample.incident_detected {
        tx.commit().await?;
        anyhow::bail!(
            "clock_reconciliation_required: maintenance stopped after detecting host clock rollback"
        );
    }
    require_safe_clock_for_write(&sample)?;
    let updated = sqlx::query(
        "UPDATE maintenance_runs SET state='complete',completed_at=?,batches=?, \
         receipt_results_compacted=?,observation_payloads_compacted=?, \
         receipts_remaining=?,observations_remaining=? WHERE id=? AND state='running'",
    )
    .bind(sample.now)
    .bind(i64::try_from(batches).context("maintenance batch count overflowed")?)
    .bind(i64::try_from(receipts).context("receipt compaction count overflowed")?)
    .bind(i64::try_from(observations).context("observation compaction count overflowed")?)
    .bind(receipts_remaining)
    .bind(observations_remaining)
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    ensure!(
        updated.rows_affected() == 1,
        "maintenance run state changed unexpectedly"
    );
    tx.commit().await?;
    Ok(sample.now)
}

fn require_safe_clock_for_write(sample: &crate::state::ClockSample) -> anyhow::Result<()> {
    ensure!(
        !sample.incident_active,
        "clock_reconciliation_required: reconcile the host clock before storage maintenance"
    );
    Ok(())
}

async fn compact_receipts(
    tx: &mut Transaction<'static, Sqlite>,
    cutoff_at: i64,
    now: i64,
    limit: i64,
) -> anyhow::Result<u64> {
    let result = sqlx::query(
        "UPDATE mutation_receipts SET result_json='null',compacted_at=? WHERE rowid IN ( \
           SELECT rowid FROM mutation_receipts WHERE compacted_at IS NULL AND created_at<? \
           ORDER BY created_at,principal_id,operation,key LIMIT ? \
         )",
    )
    .bind(now)
    .bind(cutoff_at)
    .bind(limit)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected())
}

async fn has_receipts(
    tx: &mut Transaction<'static, Sqlite>,
    cutoff_at: i64,
) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS(SELECT 1 FROM mutation_receipts WHERE compacted_at IS NULL AND created_at<?)",
    )
    .bind(cutoff_at)
    .fetch_one(&mut **tx)
    .await?
        != 0)
}

async fn compact_observations(
    tx: &mut Transaction<'static, Sqlite>,
    cutoff_at: i64,
    now: i64,
    limit: i64,
) -> anyhow::Result<u64> {
    let result = sqlx::query(
        "UPDATE job_observations SET summary='',payload_compacted_at=? WHERE rowid IN ( \
           SELECT current.rowid FROM job_observations current \
           WHERE current.payload_compacted_at IS NULL AND current.observed_at<? \
             AND current.state='running' AND current.summary<>'' \
             AND current.sequence<>(SELECT min(first.sequence) FROM job_observations first WHERE first.reporter_id=current.reporter_id) \
             AND current.sequence<>(SELECT max(last.sequence) FROM job_observations last WHERE last.reporter_id=current.reporter_id) \
             AND EXISTS(SELECT 1 FROM job_observations previous \
               WHERE previous.reporter_id=current.reporter_id \
                 AND previous.sequence=(SELECT max(p.sequence) FROM job_observations p WHERE p.reporter_id=current.reporter_id AND p.sequence<current.sequence) \
                 AND previous.state=current.state AND previous.pid IS current.pid \
                 AND previous.process_started_at IS current.process_started_at \
                 AND previous.exit_code IS current.exit_code \
                 AND previous.inputs_unchanged IS current.inputs_unchanged \
                 AND previous.summary=current.summary) \
             AND EXISTS(SELECT 1 FROM job_observations following \
               WHERE following.reporter_id=current.reporter_id \
                 AND following.sequence=(SELECT min(n.sequence) FROM job_observations n WHERE n.reporter_id=current.reporter_id AND n.sequence>current.sequence) \
                 AND following.state=current.state AND following.pid IS current.pid \
                 AND following.process_started_at IS current.process_started_at \
                 AND following.exit_code IS current.exit_code \
                 AND following.inputs_unchanged IS current.inputs_unchanged \
                 AND following.summary=current.summary) \
           ORDER BY current.observed_at,current.reporter_id,current.sequence LIMIT ? \
         )",
    )
    .bind(now)
    .bind(cutoff_at)
    .bind(limit)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected())
}

async fn has_observations(
    tx: &mut Transaction<'static, Sqlite>,
    cutoff_at: i64,
) -> anyhow::Result<bool> {
    let row = sqlx::query(
        "SELECT current.reporter_id,current.sequence FROM job_observations current \
         WHERE current.payload_compacted_at IS NULL AND current.observed_at<? \
           AND current.state='running' AND current.summary<>'' \
           AND current.sequence<>(SELECT min(first.sequence) FROM job_observations first WHERE first.reporter_id=current.reporter_id) \
           AND current.sequence<>(SELECT max(last.sequence) FROM job_observations last WHERE last.reporter_id=current.reporter_id) \
           AND EXISTS(SELECT 1 FROM job_observations previous \
             WHERE previous.reporter_id=current.reporter_id \
               AND previous.sequence=(SELECT max(p.sequence) FROM job_observations p WHERE p.reporter_id=current.reporter_id AND p.sequence<current.sequence) \
               AND previous.state=current.state AND previous.pid IS current.pid \
               AND previous.process_started_at IS current.process_started_at \
               AND previous.exit_code IS current.exit_code \
               AND previous.inputs_unchanged IS current.inputs_unchanged \
               AND previous.summary=current.summary) \
           AND EXISTS(SELECT 1 FROM job_observations following \
             WHERE following.reporter_id=current.reporter_id \
               AND following.sequence=(SELECT min(n.sequence) FROM job_observations n WHERE n.reporter_id=current.reporter_id AND n.sequence>current.sequence) \
               AND following.state=current.state AND following.pid IS current.pid \
               AND following.process_started_at IS current.process_started_at \
               AND following.exit_code IS current.exit_code \
               AND following.inputs_unchanged IS current.inputs_unchanged \
               AND following.summary=current.summary) LIMIT 1",
    )
    .bind(cutoff_at)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.is_some())
}
