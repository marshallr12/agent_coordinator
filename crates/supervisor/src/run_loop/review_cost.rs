//! Reviewer launch cost (autonomy plan §2.3 Cost, audit item 15). A reviewer
//! launch has no implementer-style [`super::record::LaunchRecord`], so the
//! live reviewer writes a root-owned record, `<state_dir>/reviews/<session>.json`,
//! before it spawns the launch and removes it once the launch's cost is in
//! the ledger `<state_dir>/costs.jsonl` (see [`super::cost::settle_review`])
//! and its run is gone. A record that outlives the supervisor that wrote it
//! belongs to a launch that ended while nobody watched: [`recover`] settles
//! its cost from the events it left, and the ledger holds at most one
//! reviewer row per session, so a retry or a second recovery never counts
//! it twice.
use super::cost;
use super::health::Vendor;
use crate::config::Config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use uuid::Uuid;

/// How long a record may belong to a launch the supervisor still waits for:
/// the live reviewer kills a launch after 45 minutes, plus a grace period.
pub const LIVE_MS: i64 = 50 * 60 * 1000;

/// One reviewer launch, as persisted before it is spawned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewLaunch {
    pub session: Uuid,
    pub project: String,
    /// The task under review.
    pub task: String,
    pub activity: String,
    pub attempt: String,
    pub vendor: Vendor,
    pub boot_id: String,
    /// When the record was written (ms since the epoch).
    pub recorded_ms: i64,
}

/// The root-owned directory of reviewer launch records, `<state_dir>/reviews`.
pub fn dir(config: &Config) -> PathBuf {
    config.state_dir.join("reviews")
}

impl ReviewLaunch {
    /// `<state_dir>/reviews/<session>.json`.
    fn path(config: &Config, session: &Uuid) -> PathBuf {
        dir(config).join(format!("{session}.json"))
    }

    /// Writes the record atomically (a mode 0600 temporary file, then rename).
    pub fn save(&self, config: &Config) -> Result<()> {
        let path = Self::path(config, &self.session);
        fs::create_dir_all(dir(config)).context("create the review record directory")?;
        let temp = path.with_extension("tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        std::io::Write::write_all(&mut options.open(&temp)?, &serde_json::to_vec(self)?)?;
        fs::rename(&temp, &path).with_context(|| format!("replace {}", path.display()))
    }

    /// Every readable record; unreadable ones are logged and left alone.
    pub fn load_all(config: &Config) -> Vec<Self> {
        let Ok(entries) = fs::read_dir(dir(config)) else {
            return Vec::new();
        };
        let paths = entries.flatten().map(|entry| entry.path());
        let records = paths.filter(|path| path.extension().is_some_and(|e| e == "json"));
        let parse = |path: PathBuf| {
            let record = serde_json::from_slice(&fs::read(&path).ok()?).ok();
            record.or_else(|| {
                eprintln!("agentc-supervisor run: unreadable {}", path.display());
                None
            })
        };
        records.filter_map(parse).collect()
    }

    /// Removes the record of `session`.
    pub fn remove(config: &Config, session: &Uuid) {
        match fs::remove_file(Self::path(config, session)) {
            Err(error) if error.kind() != ErrorKind::NotFound => {
                eprintln!("agentc-supervisor run: remove review record {session}: {error}");
            }
            _ => {}
        }
    }

    /// Whether the launch may still run under a supervisor that waits for
    /// it: it was recorded on this boot, no more than [`LIVE_MS`] ago.
    fn may_be_waited_for(&self, boot: &str, now: i64) -> bool {
        let same_boot = boot.is_empty() || self.boot_id.is_empty() || self.boot_id == boot;
        same_boot && now.saturating_sub(self.recorded_ms) <= LIVE_MS
    }
}

/// Settles the launch's cost from `events` (`None`: it wrote none, so it
/// spent nothing) and returns whether that is done; when it is not (the
/// ledger could not be written) the caller keeps the record and the run.
pub fn settle(config: &Config, launch: &ReviewLaunch, events: Option<&[u8]>, now: i64) -> bool {
    events.is_none_or(|events| cost::settle_review(config, launch, events, now))
}

/// Settles every reviewer launch an earlier supervisor left recorded and
/// that cannot still be waited for: reads its events with `events` (`None`
/// when its run is gone), records its cost once, then has `discard` remove
/// its clone and run and drops the record.
pub fn recover(
    config: &Config,
    boot: &str,
    now: i64,
    events: impl Fn(&ReviewLaunch) -> Option<Vec<u8>>,
    discard: impl Fn(&ReviewLaunch),
) {
    for launch in ReviewLaunch::load_all(config) {
        if launch.may_be_waited_for(boot, now) {
            continue;
        }
        if settle(config, &launch, events(&launch).as_deref(), now) {
            discard(&launch);
            ReviewLaunch::remove(config, &launch.session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Harness, Role};
    use serde_json::{Value, json};
    use std::cell::Cell;

    /// A reviewer launch recorded at `recorded_ms` on boot `boot-1`.
    fn launch(session: Uuid, recorded_ms: i64) -> ReviewLaunch {
        ReviewLaunch {
            session,
            project: "p".into(),
            task: "t1".into(),
            activity: "r1".into(),
            attempt: "a1".into(),
            vendor: Vendor {
                harness: Harness::Claude,
                model: "claude-opus-5-5".into(),
                effort: "high".into(),
            },
            boot_id: "boot-1".into(),
            recorded_ms,
        }
    }

    fn config(dir: &std::path::Path) -> Config {
        Config {
            state_dir: dir.to_path_buf(),
            ..Config::default()
        }
    }

    /// Claude's final `result` event for a launch that cost `usd`.
    fn result(usd: f64) -> Vec<u8> {
        let usage = json!({"input_tokens": 10, "cache_creation_input_tokens": 5,
            "cache_read_input_tokens": 20, "output_tokens": 7});
        json!({"type": "result", "total_cost_usd": usd, "usage": usage})
            .to_string()
            .into_bytes()
    }

    /// Every ledger row.
    fn rows(config: &Config) -> Vec<Value> {
        let text = fs::read_to_string(cost::ledger(config)).unwrap_or_default();
        text.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn a_finished_launch_appends_one_reviewer_row_with_its_task_session_tokens_and_dollars() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let launch = launch(Uuid::new_v4(), 0);
        assert!(settle(&config, &launch, Some(&result(1.25)), 99));
        let [row] = &rows(&config)[..] else {
            panic!("{:?}", rows(&config));
        };
        assert_eq!(row["role"], Role::Reviewer.slug());
        assert_eq!(row["task"], "t1");
        assert_eq!(row["session"], launch.session.to_string());
        assert_eq!(row["attempt"], "a1");
        assert_eq!(
            (
                row["input_tokens"].as_u64(),
                row["cached_input_tokens"].as_u64()
            ),
            (Some(15), Some(20))
        );
        assert_eq!(
            (row["output_tokens"].as_u64(), row["usd"].as_f64()),
            (Some(7), Some(1.25))
        );
        assert_eq!(row["at_ms"], 99);
    }

    /// Claude `assistant` events as a launch killed before its result leaves
    /// them: one message of 1 000 000 output tokens, reported twice.
    fn killed() -> Vec<u8> {
        let usage = json!({"input_tokens": 10, "cache_creation_input_tokens": 5,
            "cache_read_input_tokens": 20, "output_tokens": 1_000_000});
        let event = json!({"type": "assistant", "message": {"id": "m1", "usage": usage}});
        format!("{event}\n{event}").into_bytes()
    }

    #[test]
    fn a_killed_reviewer_launch_is_estimated_from_its_assistant_events() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        assert!(settle(
            &config,
            &launch(Uuid::new_v4(), 0),
            Some(&killed()),
            5
        ));
        let [row] = &rows(&config)[..] else {
            panic!("{:?}", rows(&config));
        };
        assert_eq!(row["role"], Role::Reviewer.slug());
        assert_eq!(
            (
                row["input_tokens"].as_u64(),
                row["cached_input_tokens"].as_u64()
            ),
            (Some(15), Some(20))
        );
        assert_eq!(
            (row["output_tokens"].as_u64(), row["usd"].as_f64()),
            (Some(1_000_000), Some(20.0))
        );
        assert_eq!(row["usd_estimated"], true);
    }

    #[test]
    fn a_result_costed_row_is_not_marked_estimated() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        assert!(settle(
            &config,
            &launch(Uuid::new_v4(), 0),
            Some(&result(1.0)),
            5
        ));
        assert!(rows(&config)[0].get("usd_estimated").is_none());
    }

    #[test]
    fn a_killed_reviewers_estimate_counts_toward_its_daily_cap() {
        use super::super::health::{self, DAY_MS};
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        config.health.reviewer_daily_usd = 2.0;
        let now = 10 * DAY_MS;
        assert!(settle(
            &config,
            &launch(Uuid::new_v4(), 0),
            Some(&killed()),
            now
        ));
        let refusal = health::capped(&config, Role::Reviewer, now + 1).unwrap_err();
        assert!(refusal.contains("rev spent $20.00"), "{refusal}");
    }

    #[test]
    fn settling_the_same_launch_twice_counts_it_once() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let launch = launch(Uuid::new_v4(), 0);
        assert!(settle(&config, &launch, Some(&result(1.0)), 1));
        assert!(settle(&config, &launch, Some(&result(1.0)), 2));
        let other = self::launch(Uuid::new_v4(), 0);
        assert!(settle(&config, &other, Some(&result(2.0)), 3));
        assert_eq!(rows(&config).len(), 2);
    }

    #[test]
    fn a_launch_that_wrote_no_events_adds_no_row() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        assert!(settle(&config, &launch(Uuid::new_v4(), 0), None, 1));
        assert!(rows(&config).is_empty());
    }

    #[test]
    fn a_launch_recovered_after_a_crash_is_not_counted_twice() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let crashed = launch(Uuid::new_v4(), 0);
        crashed.save(&config).unwrap();
        let discarded = Cell::new(0);
        let recover_once = |now| {
            recover(
                &config,
                "boot-2",
                now,
                |_| Some(result(3.0)),
                |_| discarded.set(discarded.get() + 1),
            );
        };
        recover_once(10);
        assert_eq!(rows(&config).len(), 1);
        assert_eq!(discarded.get(), 1);
        assert!(ReviewLaunch::load_all(&config).is_empty());
        // The supervisor died after the ledger append but before the record
        // went: the record comes back and is settled again without a row.
        crashed.save(&config).unwrap();
        recover_once(20);
        assert_eq!(rows(&config).len(), 1);
        assert!(ReviewLaunch::load_all(&config).is_empty());
        // The live path settled it first; recovery then adds nothing either.
        let live = launch(Uuid::new_v4(), 0);
        assert!(settle(&config, &live, Some(&result(1.0)), 30));
        live.save(&config).unwrap();
        recover_once(40);
        assert_eq!(rows(&config).len(), 2);
    }

    #[test]
    fn a_launch_a_supervisor_may_still_wait_for_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        launch(Uuid::new_v4(), 1_000).save(&config).unwrap();
        recover(
            &config,
            "boot-1",
            1_000 + LIVE_MS,
            |_| Some(result(1.0)),
            |_| {},
        );
        assert!(rows(&config).is_empty());
        assert_eq!(ReviewLaunch::load_all(&config).len(), 1);
        // Past the live window, or on another boot, it is settled.
        recover(
            &config,
            "boot-1",
            1_001 + LIVE_MS,
            |_| Some(result(1.0)),
            |_| {},
        );
        assert_eq!(rows(&config).len(), 1);
        launch(Uuid::new_v4(), 1_000).save(&config).unwrap();
        recover(&config, "boot-2", 2_000, |_| Some(result(1.0)), |_| {});
        assert_eq!(rows(&config).len(), 2);
    }

    #[test]
    fn a_recovered_launch_whose_run_is_gone_leaves_no_record() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        launch(Uuid::new_v4(), 0).save(&config).unwrap();
        recover(&config, "boot-2", 5, |_| None, |_| {});
        assert!(rows(&config).is_empty());
        assert!(ReviewLaunch::load_all(&config).is_empty());
    }

    #[test]
    fn reviewer_spend_counts_toward_the_reviewers_daily_cap() {
        use super::super::health::{self, DAY_MS};
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        config.health.reviewer_daily_usd = 2.0;
        let now = 10 * DAY_MS;
        assert!(health::capped(&config, Role::Reviewer, now).is_ok());
        assert!(settle(
            &config,
            &launch(Uuid::new_v4(), 0),
            Some(&result(2.5)),
            now
        ));
        let refusal = health::capped(&config, Role::Reviewer, now + 1).unwrap_err();
        assert!(refusal.contains("rev spent $2.50"), "{refusal}");
        assert!(health::capped(&config, Role::Implementer, now + 1).is_ok());
        assert!(health::capped(&config, Role::Reviewer, now + DAY_MS + 1).is_ok());
    }
}
