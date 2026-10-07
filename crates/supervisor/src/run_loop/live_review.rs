//! The live [`ReviewDriver`] (plan §2.2-6b, §2.3 "Reviewers"). Root claims,
//! decides and releases reviews with the reviewer principal's write
//! credential, kept in the root-only `<state_dir>/verdict/home` that no
//! launch can read. Each reviewer launch runs as the reviewer account in its
//! own clone at the candidate revision, gets no coordinator credential, and
//! is removed with its `$RUN` once its final output has been read.
use super::binding::{self, Binding};
use super::live::{self, mirror};
use super::review::{self, Review, ReviewClaim, ReviewDriver, Strikes};
use super::rooted;
use crate::clone;
use crate::config::Config;
use crate::profile::{Harness, Role, run_files};
use crate::push_helper::accounts::{self, Account};
use anyhow::{Context, Result, ensure};
use coordinator_client::CoordinatorClient;
use coordinator_core::workflow::ReviewInput;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long a reviewer launch may run before it is killed and its review
/// released (well inside the one-hour activity lease, which is not renewed).
const MAX_REVIEW: Duration = Duration::from_secs(45 * 60);
/// The largest final output read back from a launch.
const MAX_OUTPUT: u64 = 64 * 1024 * 1024;

/// Claims, runs and decides reviews for one host.
pub struct LiveReviewer {
    config: Config,
    config_arg: Vec<String>,
    runtime: tokio::runtime::Runtime,
    client: CoordinatorClient,
    project: String,
    /// Whether coordinator calls may use plain HTTP to loopback.
    insecure: bool,
    account: Account,
    strikes: Strikes,
}

impl LiveReviewer {
    /// Connects with the reviewer principal's credential under the loop's
    /// `binding` and resolves the reviewer account.
    pub fn new(
        config: &Config,
        config_arg: &[String],
        binding: &Binding,
        insecure: bool,
    ) -> Result<Self> {
        let home = verdict_home(config);
        require_root_only(&home)?;
        let path = home.join("credentials.toml");
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        remove_stale_credentials(&home, binding)?;
        place_named_credential(&home, binding, &text)?;
        let origin = &binding.service_url;
        Ok(Self {
            config: config.clone(),
            config_arg: config_arg.to_vec(),
            runtime: tokio::runtime::Runtime::new()?,
            client: crate::shadow::client_from(&text, &path, origin, insecure)?,
            project: binding.project_id.clone(),
            insecure,
            account: Account::lookup(Role::Reviewer.user(config))?,
            strikes: Strikes::default(),
        })
    }

    /// `GET path` with the reviewer principal; the envelope's `data`.
    fn get(&self, path: &str) -> Result<Value> {
        let path = format!("/api/v1/projects/{}/{path}", self.project);
        self.runtime
            .block_on(crate::shadow::get_data(&self.client, &path, &[]))
    }

    /// Runs the coordinator CLI as root in the review's own session with the
    /// reviewer principal's home, `input` on standard input; its JSON output.
    fn cli(&self, session: Uuid, args: &[String], input: &[u8]) -> Result<Value> {
        let mut command = Command::new(self.config.bin_dir.join("agent-coordinator"));
        command
            .args(args)
            .current_dir(verdict_home(&self.config))
            .env_clear();
        command.envs(verdict_env(&self.config, session, self.insecure));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().context("run agent-coordinator")?;
        child.stdin.take().context("stdin")?.write_all(input)?;
        let output = child.wait_with_output()?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        ensure!(
            output.status.success(),
            "{} failed: {}",
            args.join(" "),
            stderr.trim()
        );
        serde_json::from_slice(&output.stdout).context("the CLI printed no JSON")
    }

    /// Clones the mirror into a root-owned checkout the CLI fetches the
    /// candidate into while it verifies it before claiming.
    fn checkout(&self, dest: &Path) -> Result<()> {
        let origin = clone::git_output(
            &mirror(&self.config),
            &["config", "--get", "remote.origin.url"],
        )?;
        let source = mirror(&self.config).display().to_string();
        let parent = dest.parent().context("checkout parent")?;
        std::fs::create_dir_all(parent)?;
        clone::git_output(
            parent,
            &[
                "clone",
                "--quiet",
                "--",
                &source,
                &dest.display().to_string(),
            ],
        )?;
        clone::set_origin(dest, &origin)
    }

    /// Runs `agentc-supervisor subcommand flags` as the reviewer account.
    fn as_reviewer(&self, subcommand: &str, flags: Vec<String>) -> Result<()> {
        let mut args = self.config_arg.clone();
        args.push(subcommand.into());
        args.extend(flags);
        let mut command = Command::new(crate::relay::program(&self.config));
        command
            .args(&args)
            .current_dir("/")
            .env_clear()
            .envs(reviewer_env(&self.config));
        accounts::run_as(&mut command, &self.account);
        let output = command
            .stdin(Stdio::null())
            .output()
            .context("run agentc-supervisor")?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        ensure!(
            output.status.success(),
            "{subcommand} failed: {}",
            stderr.trim()
        );
        Ok(())
    }

    /// The candidate revision, fetched into the mirror under a per-review
    /// branch the reviewer's clone can see; a submission without one is
    /// reviewed at the mirror's branch head.
    fn candidate(&self, claim: &ReviewClaim) -> Result<String> {
        let mirror = mirror(&self.config);
        let (Some(reference), Some(revision)) = (
            claim.submission["candidate_ref"].as_str(),
            claim.submission["candidate_revision"].as_str(),
        ) else {
            let head = format!("refs/heads/{}", self.config.run.branch);
            return clone::git_output(&mirror, &["rev-parse", "--verify", &head]);
        };
        let refspec = format!("+{reference}:{}", review_branch(claim));
        clone::git_output(
            &mirror,
            &["fetch", "--no-tags", "--quiet", "origin", &refspec],
        )?;
        let commit = format!("{}^{{commit}}", review_branch(claim));
        let fetched = clone::git_output(&mirror, &["rev-parse", "--verify", &commit])?;
        ensure!(
            fetched == revision,
            "the candidate ref moved from {revision} to {fetched}"
        );
        Ok(fetched)
    }

    /// Clones the candidate, prepares `$RUN`, writes the prompt and runs
    /// `launch-root`; the launch's final output.
    fn launch(&mut self, review: &Review, claim: &ReviewClaim, paths: &Paths) -> Result<String> {
        let revision = self.candidate(claim)?;
        let origin = clone::git_output(
            &mirror(&self.config),
            &["config", "--get", "remote.origin.url"],
        )?;
        self.as_reviewer(
            "clone",
            vec![
                format!("--url={}", mirror(&self.config).display()),
                format!("--revision={revision}"),
                format!("--dest={}", paths.clone.display()),
                format!("--origin-url={origin}"),
            ],
        )?;
        self.as_reviewer("prepare", self.spec_flags(paths, claim))?;
        let prompt = review::render_prompt(&self.project, review, claim, &self.instructions(claim));
        let (uid, gid) = (self.account.uid, self.account.gid);
        let relative = paths.relative(&paths.run)?.join(run_files::PROMPT);
        rooted::write(&paths.role, &relative, prompt.as_bytes(), uid, gid)?;
        self.run_launch(paths, claim)?;
        self.final_output(paths)
    }

    /// The instruction files at the submission's base revision, which the
    /// candidate cannot have changed.
    fn instructions(&self, claim: &ReviewClaim) -> Vec<(String, String)> {
        let base = claim.submission["base_revision"]
            .as_str()
            .unwrap_or(&self.config.run.branch);
        let mirror = mirror(&self.config);
        let read =
            |name: &&str| Some(((*name).to_owned(), live::instruction(&mirror, base, name)?));
        super::INSTRUCTION_FILES.iter().filter_map(read).collect()
    }

    /// Runs `launch-root` for the review until it exits, killing its process
    /// group after [`MAX_REVIEW`] or on a stop request.
    fn run_launch(&self, paths: &Paths, claim: &ReviewClaim) -> Result<()> {
        use std::os::unix::process::CommandExt;
        let mut args = self.config_arg.clone();
        args.push("launch-root".into());
        args.extend(self.spec_flags(paths, claim));
        let mut command = Command::new(crate::relay::program(&self.config));
        command.args(&args).stdin(Stdio::null()).process_group(0);
        let mut child = command.spawn().context("spawn launch-root")?;
        let started = Instant::now();
        while child.try_wait()?.is_none() {
            if started.elapsed() > MAX_REVIEW || live::stop_requested() {
                kill_group(child.id());
                child.wait()?;
                anyhow::bail!("the reviewer launch was stopped before it finished");
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        Ok(())
    }

    /// Codex's last-message file or Claude's event stream, read as root
    /// without following the reviewer's symlinks.
    fn final_output(&self, paths: &Paths) -> Result<String> {
        let name = match self.config.run.harness {
            Harness::Codex => run_files::LAST_MESSAGE,
            Harness::Claude => "events.jsonl",
        };
        let relative = paths.relative(&paths.run)?.join(name);
        let bytes = rooted::read(&paths.role, &relative, self.account.uid, MAX_OUTPUT)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// `prepare` and `launch-root` flags for a reviewer launch.
    fn spec_flags(&self, paths: &Paths, claim: &ReviewClaim) -> Vec<String> {
        let run = &self.config.run;
        vec![
            "--role=reviewer".into(),
            format!("--harness={:?}", run.harness).to_lowercase(),
            format!("--clone={}", paths.clone.display()),
            format!("--run={}", paths.run.display()),
            format!("--model={}", run.model),
            format!("--effort={}", run.effort),
            format!("--project={}", self.project),
            format!("--session-id={}", claim.session),
        ]
    }

    /// Removes the review's clone, `$RUN` and mirror branch.
    fn discard(&self, paths: &Paths, claim: &ReviewClaim) {
        for path in [&paths.clone, &paths.run] {
            let removed = paths
                .relative(path)
                .and_then(|p| rooted::remove_tree(&paths.role, &p));
            if let Err(error) = removed {
                eprintln!("agentc-supervisor run: review cleanup: {error:#}");
            }
        }
        let branch = review_branch(claim);
        let _ = clone::git_output(&mirror(&self.config), &["update-ref", "-d", &branch]);
    }

    /// `reviews <subcommand>` arguments naming the claimed attempt.
    fn activity_args(
        subcommand: &str,
        activity: &str,
        attempt: &str,
        generation: u64,
    ) -> Vec<String> {
        vec![
            "--json".into(),
            "reviews".into(),
            subcommand.into(),
            format!("--activity={activity}"),
            format!("--attempt={attempt}"),
            format!("--generation={generation}"),
        ]
    }

    /// Releases `attempt` of `activity` in `session` with a handoff `summary`.
    fn release_attempt(
        &self,
        session: Uuid,
        activity: &str,
        attempt: (&str, u64),
        summary: &str,
    ) -> Result<()> {
        let mut args = Self::activity_args("release", activity, attempt.0, attempt.1);
        args.push("--input=-".into());
        let body = serde_json::to_vec(&json!({"summary": summary}))?;
        self.cli(session, &args, &body).map(drop)
    }
}

impl ReviewDriver for LiveReviewer {
    fn next_review(&mut self) -> Result<Value> {
        let call = crate::shadow::fetch_next(&self.client, &self.project, "reviewer");
        self.runtime.block_on(call)
    }

    /// Reads the submission and criteria, then connects a fresh session and
    /// claims through a root-owned checkout the CLI verifies the candidate in.
    fn claim_review(&mut self, review: &Review) -> Result<ReviewClaim> {
        let workflow = self.get(&format!("tasks/{}/workflow", review.subject))?;
        let submission = current_submission(review, &workflow)?;
        let task = self.get(&format!("tasks/{}", review.subject))?;
        let session = Uuid::new_v4();
        let checkout = verdict_home(&self.config)
            .join("checkouts")
            .join(session.to_string());
        let claimed = self
            .checkout(&checkout)
            .and_then(|()| self.claim_in(review, session, &checkout));
        let _ = std::fs::remove_dir_all(&checkout);
        let lookup = || self.get(&format!("workflow-activities/{}", review.activity));
        let summary = "agentc-supervisor could not read its claim; the review is queued again.";
        let release = |attempt: &str, generation| {
            self.release_attempt(session, &review.activity, (attempt, generation), summary)
        };
        let (attempt, generation) = held_attempt(&claimed?, lookup, release)?;
        Ok(ReviewClaim {
            attempt,
            generation,
            session,
            criteria: review::strings(task["acceptance_criteria"].as_array().map_or(&[], |v| v)),
            submission,
        })
    }

    fn run_reviewer(&mut self, review: &Review, claim: &ReviewClaim) -> Result<String> {
        let paths = Paths::new(&self.config, claim.session);
        let output = self.launch(review, claim, &paths);
        self.discard(&paths, claim);
        output
    }

    /// Posts the decision; the CLI adds the generation and submission.
    fn decide(&mut self, review: &Review, claim: &ReviewClaim, input: &ReviewInput) -> Result<()> {
        let mut body = serde_json::to_value(input)?;
        if let Some(fields) = body.as_object_mut() {
            fields.remove("generation");
            fields.remove("submission_id");
        }
        let mut args =
            Self::activity_args("decide", &review.activity, &claim.attempt, claim.generation);
        args.extend([
            format!("--submission={}", review.submission),
            "--input=-".into(),
        ]);
        self.cli(claim.session, &args, &serde_json::to_vec(&body)?)
            .map(drop)
    }

    fn release_review(
        &mut self,
        review: &Review,
        claim: &ReviewClaim,
        summary: &str,
    ) -> Result<()> {
        let attempt = (claim.attempt.as_str(), claim.generation);
        self.release_attempt(claim.session, &review.activity, attempt, summary)
    }

    fn strikes(&mut self) -> &mut Strikes {
        &mut self.strikes
    }
}

/// The subject's current submission, refused unless it is the one `next`
/// offered (checked before the claim, so nothing is held on a mismatch).
fn current_submission(review: &Review, workflow: &Value) -> Result<Value> {
    let submission = &workflow["submission"];
    ensure!(
        submission["id"].as_str() == Some(review.submission.as_str()),
        "the subject's submission is no longer {}",
        review.submission
    );
    Ok(submission.clone())
}

/// The attempt and generation a claim reply names. A reply that names none
/// may still have claimed: the activity's current attempt is then released,
/// so the review is never held for its whole lease, and the claim fails.
fn held_attempt(
    reply: &Value,
    activity: impl FnOnce() -> Result<Value>,
    release: impl FnOnce(&str, u64) -> Result<()>,
) -> Result<(String, u64)> {
    if let Some(found) = attempt_of(&reply["data"]["attempt"]) {
        return Ok(found);
    }
    let activity = activity()?;
    let record = activity.get("activity").unwrap_or(&activity);
    let (attempt, generation) = attempt_of(&record["current_attempt"])
        .context("neither the claim reply nor the activity names an attempt")?;
    release(&attempt, generation)?;
    anyhow::bail!("the claim reply named no attempt; attempt {attempt} was released")
}

/// The `id` and `generation` of an attempt object.
fn attempt_of(attempt: &Value) -> Option<(String, u64)> {
    Some((
        attempt["id"].as_str()?.to_owned(),
        attempt["generation"].as_u64()?,
    ))
}

impl LiveReviewer {
    /// Connects `session` and claims the review with its pinned revisions.
    fn claim_in(&self, review: &Review, session: Uuid, checkout: &Path) -> Result<Value> {
        let harness = format!(
            "--harness=agentc-supervisor-reviewer-{:?}",
            self.config.run.harness
        );
        self.cli(
            session,
            &["--json".into(), "connect".into(), harness.to_lowercase()],
            b"",
        )?;
        self.cli(session, &claim_args(review, checkout), b"")
    }
}

/// `reviews claim` arguments for `review` with its pinned revisions, asserting
/// the supervisor's `distinct_launch` independence so the service checks it.
fn claim_args(review: &Review, checkout: &Path) -> Vec<String> {
    vec![
        "--json".into(),
        "reviews".into(),
        "claim".into(),
        format!("--activity={}", review.activity),
        format!("--submission={}", review.submission),
        format!(
            "--project-policy-revision={}",
            review.project_policy_revision
        ),
        format!(
            "--workflow-policy-revision={}",
            review.workflow_policy_revision
        ),
        format!("--candidate-checkout={}", checkout.display()),
        format!("--review-independence={}", review::INDEPENDENCE),
    ]
}

/// Where one review launch lives below the reviewer's role directory.
struct Paths {
    role: PathBuf,
    clone: PathBuf,
    run: PathBuf,
}

impl Paths {
    /// The clone and `$RUN` of the launch in `session`.
    fn new(config: &Config, session: Uuid) -> Self {
        let role = config.state_dir.join(Role::Reviewer.slug());
        Self {
            clone: role.join("clones").join(session.to_string()),
            run: role.join("runs").join(session.to_string()),
            role,
        }
    }

    /// `path` relative to the role directory, for the `rooted` helpers.
    fn relative(&self, path: &Path) -> Result<PathBuf> {
        Ok(path.strip_prefix(&self.role)?.to_owned())
    }
}

/// The reviewer principal's coordinator home, `<state_dir>/verdict/home`.
fn verdict_home(config: &Config) -> PathBuf {
    config.state_dir.join("verdict").join("home")
}

/// The mirror branch a review's candidate is fetched into.
fn review_branch(claim: &ReviewClaim) -> String {
    format!("refs/heads/agentc-review/{}", claim.session)
}

/// Refuses a verdict home that is not a root-owned directory closed to
/// everyone else, since it holds the reviewer principal's write credential.
fn require_root_only(home: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta =
        std::fs::symlink_metadata(home).with_context(|| format!("stat {}", home.display()))?;
    let private = meta.is_dir() && meta.uid() == 0 && meta.mode() & 0o077 == 0;
    ensure!(
        private,
        "{} must be a root-owned mode 0700 directory",
        home.display()
    );
    Ok(())
}

/// Copies the reviewer principal's credential to the binding's
/// `project_name` directory in the root-only verdict `home` (mode 0700
/// directories, a 0600 file), where its CLI commands look for it.
fn place_named_credential(home: &Path, binding: &Binding, text: &str) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if binding.project_name.is_none() {
        return Ok(());
    }
    let path = home.join(binding.credentials());
    let directory = path.parent().context("credential directory")?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(file.write_all(text.as_bytes())?)
}

/// Removes the verdict credential copies a `[run.binding]` change left in
/// the root-only `home`: every `<name>/` holding `config/credentials.toml`
/// whose name is not the binding's `project_name` (all of them when it has
/// none). Paths are opened beneath `home` without following symlinks, so a
/// symlinked entry is neither recognised nor followed.
fn remove_stale_credentials(home: &Path, binding: &Binding) -> Result<()> {
    for entry in std::fs::read_dir(home).with_context(|| format!("list {}", home.display()))? {
        let name = entry?.file_name();
        if is_stale_copy(home, &name, binding) {
            // `remove_tree` opens the entry's parent beneath its base, so the
            // base is `home`'s parent and the entry's parent `home` itself.
            let (Some(base), Some(leaf)) = (home.parent(), home.file_name()) else {
                anyhow::bail!("{} has no parent directory", home.display());
            };
            rooted::remove_tree(base, &Path::new(leaf).join(&name))?;
            eprintln!(
                "agentc-supervisor: removed the stale verdict credential copy in {}",
                home.join(&name).display()
            );
        }
    }
    Ok(())
}

/// Whether `name` in `home` is a project-named credential copy other than
/// the binding's own. Hidden entries and the claim `checkouts` never are.
fn is_stale_copy(home: &Path, name: &std::ffi::OsStr, binding: &Binding) -> bool {
    let current = binding.project_name.as_deref().map(std::ffi::OsStr::new);
    let hidden = name.as_encoded_bytes().starts_with(b".");
    let credential = Path::new(name).join("config").join("credentials.toml");
    Some(name) != current && !hidden && name != "checkouts" && rooted::is_regular(home, &credential)
}

/// The environment of root's reviewer-principal CLI commands; `insecure`
/// adds the loopback flag.
fn verdict_env(config: &Config, session: Uuid, insecure: bool) -> Vec<(&'static str, String)> {
    let home = verdict_home(config).display().to_string();
    let mut env = vec![
        (
            "PATH",
            format!("{}:/usr/bin:/bin", config.bin_dir.display()),
        ),
        ("HOME", home.clone()),
        ("LANG", "C.UTF-8".into()),
        ("AGENT_COORDINATOR_HOME", home),
        ("AGENT_COORDINATOR_SESSION", session.to_string()),
        (
            binding::REPO_CONFIG_ENV,
            binding::installed_path(config).display().to_string(),
        ),
    ];
    if insecure {
        env.push((binding::INSECURE_ENV, "true".into()));
    }
    env
}

/// The environment of the reviewer account's `clone` and `prepare`.
fn reviewer_env(config: &Config) -> Vec<(&'static str, String)> {
    let home = config.state_dir.join(Role::Reviewer.slug()).join("home");
    vec![
        (
            "PATH",
            format!("{}:/usr/bin:/bin", config.bin_dir.display()),
        ),
        ("HOME", home.display().to_string()),
        ("LANG", "C.UTF-8".into()),
    ]
}

/// Kills the process group led by `pid`.
fn kill_group(pid: u32) {
    if let Ok(group) = libc::pid_t::try_from(pid) {
        // SAFETY: kill takes plain integers and touches no memory.
        unsafe { libc::kill(-group, libc::SIGKILL) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_verdict_credential_never_reaches_reviewer_commands() {
        let config = Config::default();
        let session = Uuid::nil();
        let verdict = verdict_env(&config, session, false);
        assert!(verdict.contains(&(
            "AGENT_COORDINATOR_HOME",
            "/var/lib/agentc/verdict/home".into()
        )));
        assert!(
            verdict
                .iter()
                .all(|(name, _)| *name != binding::INSECURE_ENV)
        );
        let insecure = verdict_env(&config, session, true);
        assert!(insecure.contains(&(binding::INSECURE_ENV, "true".into())));
        let reviewer = reviewer_env(&config);
        assert!(reviewer.iter().all(|(_, value)| !value.contains("verdict")));
        assert!(
            reviewer
                .iter()
                .all(|(name, _)| !name.starts_with("AGENT_COORDINATOR"))
        );
    }

    #[test]
    fn a_claim_reply_without_an_attempt_releases_the_held_review() {
        let reply = json!({"data": {"attempt": {"id": "a1", "generation": 2}}});
        let never = || -> Result<Value> { panic!("no lookup needed") };
        let found = held_attempt(&reply, never, |_, _| panic!("no release")).unwrap();
        assert_eq!(found, ("a1".to_owned(), 2));
        let activity = json!({"activity": {"current_attempt": {"id": "a9", "generation": 5}}});
        let mut released = None;
        let error = held_attempt(
            &json!({"data": {}}),
            || Ok(activity),
            |a, g| {
                released = Some((a.to_owned(), g));
                Ok(())
            },
        );
        assert!(error.is_err());
        assert_eq!(released, Some(("a9".to_owned(), 5)));
        let none = held_attempt(&json!({}), || Ok(json!({})), |_, _| panic!("nothing held"));
        assert!(none.is_err());
    }

    #[test]
    fn a_superseded_submission_is_refused_before_the_claim() {
        let review = Review {
            activity: "r1".into(),
            subject: "t1".into(),
            submission: "s1".into(),
            title: String::new(),
            project_policy_revision: 1,
            workflow_policy_revision: 0,
        };
        let current = json!({"submission": {"id": "s1", "ac_amendment": null}});
        assert_eq!(current_submission(&review, &current).unwrap()["id"], "s1");
        let moved = json!({"submission": {"id": "s2"}});
        assert!(current_submission(&review, &moved).is_err());
        assert!(current_submission(&review, &json!({"submission": null})).is_err());
    }

    #[test]
    fn the_verdict_credential_is_copied_to_the_project_named_directory() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let mut binding = Binding {
            service_url: "http://127.0.0.1:18080".into(),
            project_id: "p".into(),
            project_name: None,
        };
        place_named_credential(home.path(), &binding, "t").unwrap();
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
        binding.project_name = Some("Staging".into());
        place_named_credential(home.path(), &binding, "t").unwrap();
        let path = home.path().join("Staging/config/credentials.toml");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "t");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn stale_verdict_credential_copies_are_removed_without_following_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(dir.path()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mut binding = Binding {
            service_url: "http://127.0.0.1:18080".into(),
            project_id: "p".into(),
            project_name: None,
        };
        for name in ["Old", "Current"] {
            binding.project_name = Some(name.into());
            place_named_credential(&home, &binding, "t").unwrap();
        }
        std::fs::create_dir_all(outside.path().join("config")).unwrap();
        std::fs::write(outside.path().join("config/credentials.toml"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), home.join("Linked")).unwrap();
        std::fs::create_dir_all(home.join("checkouts/s")).unwrap();
        std::fs::write(home.join("credentials.toml"), "t").unwrap();
        remove_stale_credentials(&home, &binding).unwrap();
        assert!(!home.join("Old").exists(), "stale copy kept");
        assert!(home.join("Current/config/credentials.toml").is_file());
        assert!(
            home.join("Linked").is_symlink()
                && outside.path().join("config/credentials.toml").is_file()
        );
        assert!(home.join("checkouts/s").is_dir() && home.join("credentials.toml").is_file());
        binding.project_name = None;
        remove_stale_credentials(&home, &binding).unwrap();
        assert!(!home.join("Current").exists(), "unbound copy kept");
    }

    #[test]
    fn the_review_claim_asserts_distinct_launch() {
        let review = Review {
            activity: "r1".into(),
            subject: "t1".into(),
            submission: "s1".into(),
            title: String::new(),
            project_policy_revision: 1,
            workflow_policy_revision: 0,
        };
        let args = claim_args(&review, Path::new("/clone"));
        assert!(args.contains(&"--review-independence=distinct_launch".to_owned()));
        assert!(args.contains(&"--activity=r1".to_owned()));
    }

    #[test]
    fn a_verdict_home_open_to_others_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(require_root_only(dir.path()).is_err());
        assert!(require_root_only(&dir.path().join("missing")).is_err());
    }
}
