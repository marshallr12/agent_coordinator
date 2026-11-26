//! The files `adopt` writes: the repository binding and the managed
//! instruction block in `AGENTS.md` and `CLAUDE.md`.
//!
//! Planning (pure, reads only) is separate from applying (atomic writes), so a
//! dry run and the confirmation prompt show exactly what a real run writes.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use super::text::{self, Merge};
use super::{Harnesses, failure};
use crate::Failure;

/// The binding file's name at the repository root.
pub const BINDING_FILE: &str = ".agent-coordinator.toml";
/// The instruction file read by Codex and other harnesses.
pub const AGENTS_FILE: &str = "AGENTS.md";
/// The instruction file read by Claude Code.
pub const CLAUDE_FILE: &str = "CLAUDE.md";
/// The single line that makes Claude Code read `AGENTS.md` (its file-import syntax).
pub const IMPORT_LINE: &str = "@AGENTS.md";
/// The permission bits given to a file `adopt` creates.
#[cfg(unix)]
const NEW_FILE_MODE: u32 = 0o644;

/// One file `adopt` manages, with its content before and after.
#[derive(Clone, Debug)]
pub struct FileChange {
    /// The instruction file's name as the person knows it.
    pub display: String,
    /// The file actually written; differs from the name when that is a symlink.
    pub target: PathBuf,
    /// Repository-relative path of `target`, which is what Git must be given.
    pub git_path: String,
    /// The bytes read while planning; `None` when the file does not exist yet.
    pub original: Option<Vec<u8>>,
    /// The bytes the file must hold afterwards.
    pub updated: Vec<u8>,
    /// What happens to the file, in a few words, for the plan.
    pub summary: String,
}

impl FileChange {
    /// True when applying this would alter the file on disk.
    pub fn is_change(&self) -> bool {
        self.original.as_deref() != Some(self.updated.as_slice())
    }
}

/// The instruction-file part of a plan: what changes, and what is left alone and why.
#[derive(Clone, Debug, Default)]
pub struct InstructionPlan {
    pub changes: Vec<FileChange>,
    /// `(file, reason)` for files that are deliberately not written.
    pub skipped: Vec<(String, String)>,
}

/// What a managed instruction file should contain after the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// Leave the file alone (or do not create it).
    Untouched,
    /// Contain the managed block.
    Block,
    /// Be the one-line import of `AGENTS.md`.
    Import,
}

/// An instruction file as found on disk.
enum Found {
    Absent,
    File(FoundFile),
    Refused(String),
}

/// An existing instruction file that adopt may write.
struct FoundFile {
    /// The file to write: the name itself, or the target of a symlink.
    target: PathBuf,
    /// The repository-relative path of `target`.
    git_path: String,
    bytes: Vec<u8>,
}

impl Found {
    /// True when something occupies the name, even if adopt will not write it.
    fn exists(&self) -> bool {
        !matches!(self, Found::Absent)
    }
}

/// Renders the binding file from validated values.
///
/// `project_name` is deliberately absent: it selects a project-specific
/// credential directory, and `adopt` reuses the shared credential.
pub fn render_binding(service_url: &str, project_id: &str) -> String {
    format!("service_url = \"{service_url}\"\nproject_id = \"{project_id}\"\n")
}

/// Plans the binding file for `project_id`; `replace` allows overwriting an existing one.
pub fn plan_binding(root: &Path, service_url: &str, project_id: &str, replace: bool) -> FileChange {
    let target = root.join(BINDING_FILE);
    let original = if replace {
        fs::read(&target).ok()
    } else {
        None
    };
    let summary = if original.is_some() { "replace" } else { "new" };
    FileChange {
        display: BINDING_FILE.to_owned(),
        target,
        git_path: BINDING_FILE.to_owned(),
        original,
        updated: render_binding(service_url, project_id).into_bytes(),
        summary: summary.to_owned(),
    }
}

/// Plans the instruction files for the selected harnesses, following the merge matrix.
pub fn plan_instructions(root: &Path, harnesses: Harnesses, force: bool) -> InstructionPlan {
    let agents = find(root, AGENTS_FILE);
    let claude = find(root, CLAUDE_FILE);
    let (agents_role, claude_role) = roles(&agents, &claude, harnesses);
    let mut plan = InstructionPlan::default();
    plan_file(&mut plan, root, AGENTS_FILE, agents, agents_role, force);
    plan_file(&mut plan, root, CLAUDE_FILE, claude, claude_role, force);
    plan
}

/// Applies the merge matrix: which file gets the block and which the import line.
fn roles(agents: &Found, claude: &Found, harnesses: Harnesses) -> (Role, Role) {
    let when = |wanted: bool, role: Role| if wanted { role } else { Role::Untouched };
    match (agents.exists(), claude.exists()) {
        (_, false) => (Role::Block, when(harnesses.claude, Role::Import)),
        (false, true) => (when(harnesses.codex, Role::Block), Role::Block),
        (true, true) => {
            let covered = same_target(agents, claude) || imports_agents(claude);
            (Role::Block, when(!covered, Role::Block))
        }
    }
}

/// True when both names resolve to one file (one is a symlink to the other).
fn same_target(agents: &Found, claude: &Found) -> bool {
    match (agents, claude) {
        (Found::File(a), Found::File(b)) => a.target == b.target,
        _ => false,
    }
}

/// True when the file already pulls in `AGENTS.md` with an import line.
fn imports_agents(found: &Found) -> bool {
    let Found::File(file) = found else {
        return false;
    };
    String::from_utf8_lossy(&file.bytes)
        .lines()
        .any(|line| line.starts_with(IMPORT_LINE))
}

/// Looks up one instruction file, resolving a symlink that stays inside the repository.
fn find(root: &Path, name: &str) -> Found {
    let path = root.join(name);
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return Found::Absent;
    };
    let (target, git_path) = if metadata.file_type().is_symlink() {
        match resolve_inside(root, &path) {
            Ok(resolved) => resolved,
            Err(reason) => return Found::Refused(reason),
        }
    } else {
        (path, name.to_owned())
    };
    match fs::read(&target) {
        Ok(bytes) => Found::File(FoundFile {
            target,
            git_path,
            bytes,
        }),
        Err(error) => Found::Refused(format!("it could not be read ({error})")),
    }
}

/// Resolves a symlink to its target and that target's repository-relative path.
///
/// Renaming a new file over a symlink would replace the link itself, so the
/// write goes to the target; a target outside the repository is refused.
fn resolve_inside(root: &Path, link: &Path) -> Result<(PathBuf, String), String> {
    let target = fs::canonicalize(link).map_err(|error| format!("its link is broken ({error})"))?;
    let root =
        fs::canonicalize(root).map_err(|error| format!("the root is unreadable ({error})"))?;
    let Ok(relative) = target.strip_prefix(&root) else {
        return Err(format!(
            "it is a symlink to {}, outside the repository",
            target.display()
        ));
    };
    let git_path = relative.to_string_lossy().replace('\\', "/");
    Ok((target, git_path))
}

/// Adds one file's change, or the reason it is skipped, to the plan.
fn plan_file(
    plan: &mut InstructionPlan,
    root: &Path,
    name: &str,
    found: Found,
    role: Role,
    force: bool,
) {
    if role == Role::Untouched {
        return;
    }
    let outcome = match found {
        Found::Absent => Ok(new_file(root, name, role)),
        Found::Refused(reason) => Err(reason),
        Found::File(file) => merged_file(name, file, force),
    };
    match outcome {
        Ok(change) => plan.changes.push(change),
        Err(reason) => plan.skipped.push((name.to_owned(), reason)),
    }
}

/// Plans a file that does not exist yet: the block, or the one-line import.
fn new_file(root: &Path, name: &str, role: Role) -> FileChange {
    let (content, summary) = match role {
        Role::Import => (
            format!("{IMPORT_LINE}\n"),
            format!("new: \"{IMPORT_LINE}\""),
        ),
        _ => (format!("{}\n", text::BLOCK), "new".to_owned()),
    };
    FileChange {
        display: name.to_owned(),
        target: root.join(name),
        git_path: name.to_owned(),
        original: None,
        updated: content.into_bytes(),
        summary,
    }
}

/// Plans the block for an existing file, or explains why the file is left alone.
fn merged_file(name: &str, file: FoundFile, force: bool) -> Result<FileChange, String> {
    let Some((content, shape)) = text::decode(&file.bytes) else {
        return Err("it is not UTF-8 text".to_owned());
    };
    let (merged, action) = merge_outcome(&content, force)?;
    let updated = text::encode(&merged, &shape);
    let current = updated == file.bytes;
    Ok(FileChange {
        display: name.to_owned(),
        target: file.target,
        git_path: file.git_path,
        original: Some(file.bytes),
        updated,
        summary: if current {
            "current".to_owned()
        } else {
            action
        },
    })
}

/// Places the block into the text; returns the new text and what was done, or why not.
fn merge_outcome(content: &str, force: bool) -> Result<(String, String), String> {
    match text::merge_block(content, force) {
        Merge::Inserted(merged) => Ok((merged, insert_summary())),
        Merge::Replaced(merged) => Ok((merged, "update: refresh the managed block".to_owned())),
        Merge::AlreadyMentions(line) => Err(format!(
            "it already has coordinator instructions: \"{line}\""
        )),
        Merge::Unterminated => Err("its begin marker has no end marker".to_owned()),
    }
}

/// Describes an insertion for the plan, e.g. "update: insert 15-line block at top".
fn insert_summary() -> String {
    format!(
        "update: insert {}-line block at top",
        text::BLOCK.lines().count()
    )
}

/// True when the file differs from its committed version by adopt's own text and nothing else.
///
/// The commit step asks this about a file an earlier, interrupted run wrote,
/// so it can be committed with the rest. A file that also carries a person's
/// uncommitted edits fails the test and is left for them to commit.
pub fn holds_only_adopt_text(change: &FileChange, committed: Option<&[u8]>) -> bool {
    let Ok(working) = fs::read(&change.target) else {
        return false;
    };
    let created = [format!("{}\n", text::BLOCK), format!("{IMPORT_LINE}\n")];
    match committed {
        None => created.iter().any(|content| content.as_bytes() == working),
        Some(committed) => with_block(committed).is_some_and(|expected| expected == working),
    }
}

/// Returns the committed bytes with the block placed, as adopt would have written them.
fn with_block(committed: &[u8]) -> Option<Vec<u8>> {
    let (content, shape) = text::decode(committed)?;
    match text::merge_block(&content, true) {
        Merge::Inserted(merged) | Merge::Replaced(merged) => Some(text::encode(&merged, &shape)),
        _ => None,
    }
}

/// Writes one planned change atomically; returns false when the file was already current.
///
/// A file that changed on disk since it was planned is left untouched and the
/// run stops with exit 5, so nothing the person just edited is overwritten.
pub fn apply(change: &FileChange) -> Result<bool, Failure> {
    if !change.is_change() {
        return Ok(false);
    }
    let directory = change.target.parent().unwrap_or(Path::new("."));
    let temporary = staged(directory, change).map_err(|error| write_failure(change, &error))?;
    match &change.original {
        None => create_exclusive(temporary, change)?,
        Some(original) => replace_unchanged(temporary, change, original)?,
    }
    sync_directory(directory);
    Ok(true)
}

/// Writes the new content to a synced temporary file beside the target.
fn staged(directory: &Path, change: &FileChange) -> std::io::Result<NamedTempFile> {
    let mut temporary = NamedTempFile::new_in(directory)?;
    temporary.write_all(&change.updated)?;
    keep_mode(temporary.path(), &change.target)?;
    temporary.as_file().sync_all()?;
    Ok(temporary)
}

/// Gives the staged file the target's mode, or an ordinary mode for a new file.
///
/// Temporary files are created private (0600); without this an adopted
/// `AGENTS.md` would silently lose its group and other read bits.
#[cfg(unix)]
fn keep_mode(temporary: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = match fs::metadata(target) {
        Ok(metadata) => metadata.permissions().mode() & 0o7777,
        Err(_) => NEW_FILE_MODE,
    };
    fs::set_permissions(temporary, fs::Permissions::from_mode(mode))
}

/// Windows files inherit their directory's access rules; nothing to copy.
#[cfg(not(unix))]
fn keep_mode(_temporary: &Path, _target: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Creates a new file, failing (exit 5) if anything appeared at its name meanwhile.
fn create_exclusive(temporary: NamedTempFile, change: &FileChange) -> Result<(), Failure> {
    temporary
        .persist_noclobber(&change.target)
        .map(|_| ())
        .map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                changed_failure(change)
            } else {
                write_failure(change, &error.error)
            }
        })
}

/// Replaces an existing file only if it still holds the bytes that were planned against.
fn replace_unchanged(
    temporary: NamedTempFile,
    change: &FileChange,
    original: &[u8],
) -> Result<(), Failure> {
    if fs::read(&change.target).ok().as_deref() != Some(original) {
        return Err(changed_failure(change));
    }
    temporary
        .persist(&change.target)
        .map(|_| ())
        .map_err(|error| write_failure(change, &error.error))
}

/// Flushes the directory entry so the rename survives a crash (best effort).
fn sync_directory(directory: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = fs::File::open(directory) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = directory;
}

/// The failure for a file that changed between planning and writing.
fn changed_failure(change: &FileChange) -> Failure {
    failure(
        5,
        "file_changed",
        format!(
            "{} changed while adopt was running, so it was left untouched. Run adopt again.",
            change.display
        ),
    )
}

/// The failure for a file that could not be written at all.
fn write_failure(change: &FileChange, error: &std::io::Error) -> Failure {
    failure(
        7,
        "write_failed",
        format!("could not write {}: {error}", change.display),
    )
}

/// How a harness will find the coordinator instructions after the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// The managed block is present and intact.
    Block,
    /// The file has its own coordinator instructions and was left alone.
    OwnInstructions,
    /// Nothing tells this harness about the coordinator.
    Missing,
}

/// Checks the instruction file a harness reads (`claude` reads `CLAUDE.md`, others `AGENTS.md`).
pub fn readiness(root: &Path, claude: bool) -> Readiness {
    let agents = read_text(root, AGENTS_FILE);
    if !claude {
        return text_readiness(agents.as_deref());
    }
    let own = read_text(root, CLAUDE_FILE);
    let imports = own
        .as_deref()
        .is_some_and(|content| content.lines().any(|line| line.starts_with(IMPORT_LINE)));
    match text_readiness(own.as_deref()) {
        Readiness::Missing if imports => text_readiness(agents.as_deref()),
        found => found,
    }
}

/// Classifies one file's text.
fn text_readiness(content: Option<&str>) -> Readiness {
    match content {
        Some(content) if text::has_intact_block(content) => Readiness::Block,
        Some(content) if content.contains(BINDING_FILE) => Readiness::OwnInstructions,
        _ => Readiness::Missing,
    }
}

/// Reads an instruction file as text with its line endings normalised.
fn read_text(root: &Path, name: &str) -> Option<String> {
    let bytes = fs::read(root.join(name)).ok()?;
    text::decode(&bytes).map(|(content, _)| content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adopt::testing::TestRepo;

    const BOTH: Harnesses = Harnesses {
        claude: true,
        codex: true,
    };
    const CLAUDE: Harnesses = Harnesses {
        claude: true,
        codex: false,
    };
    const CODEX: Harnesses = Harnesses {
        claude: false,
        codex: true,
    };

    /// Plans and applies the instruction files, returning the plan.
    fn adopt_files(repo: &TestRepo, harnesses: Harnesses) -> InstructionPlan {
        let plan = plan_instructions(repo.root(), harnesses, false);
        for change in &plan.changes {
            apply(change).unwrap();
        }
        plan
    }

    #[test]
    fn binding_is_rendered_from_the_two_values() {
        let rendered = render_binding(
            "https://agents.example",
            "00000000-0000-0000-0000-000000000001",
        );
        assert_eq!(
            rendered,
            "service_url = \"https://agents.example\"\nproject_id = \"00000000-0000-0000-0000-000000000001\"\n"
        );
        let parsed: crate::config::RepositoryBinding = toml::from_str(&rendered).unwrap();
        assert_eq!(parsed.project_name, None);
    }

    #[test]
    fn neither_file_creates_agents_and_a_claude_import() {
        let repo = TestRepo::new();
        adopt_files(&repo, BOTH);
        assert_eq!(repo.read(AGENTS_FILE), format!("{}\n", text::BLOCK));
        assert_eq!(repo.read(CLAUDE_FILE), "@AGENTS.md\n");
        let codex_only = TestRepo::new();
        adopt_files(&codex_only, CODEX);
        assert!(!codex_only.root().join(CLAUDE_FILE).exists());
    }

    #[test]
    fn agents_only_gets_the_block_and_claude_an_import() {
        let repo = TestRepo::new();
        repo.commit_file(AGENTS_FILE, "# Rules\n\nBe kind.\n");
        adopt_files(&repo, CLAUDE);
        let agents = repo.read(AGENTS_FILE);
        assert!(
            agents.starts_with("# Rules\n\n<!-- agent-coordinator:begin v1 -->\n"),
            "{agents}"
        );
        assert!(
            agents.ends_with("<!-- agent-coordinator:end -->\n\nBe kind.\n"),
            "{agents}"
        );
        assert_eq!(repo.read(CLAUDE_FILE), "@AGENTS.md\n");
    }

    #[test]
    fn claude_only_gets_the_block_and_codex_a_new_agents_file() {
        let repo = TestRepo::new();
        repo.commit_file(CLAUDE_FILE, "Project notes.\n");
        adopt_files(&repo, CLAUDE);
        assert!(repo.read(CLAUDE_FILE).contains(text::BLOCK));
        assert!(!repo.root().join(AGENTS_FILE).exists());
        let with_codex = TestRepo::new();
        with_codex.commit_file(CLAUDE_FILE, "Project notes.\n");
        adopt_files(&with_codex, BOTH);
        assert_eq!(with_codex.read(AGENTS_FILE), format!("{}\n", text::BLOCK));
        assert!(with_codex.read(CLAUDE_FILE).contains(text::BLOCK));
    }

    #[test]
    fn both_files_skip_a_claude_file_that_imports_agents() {
        let repo = TestRepo::new();
        repo.commit_file(AGENTS_FILE, "Rules.\n");
        repo.commit_file(CLAUDE_FILE, "@AGENTS.md\n\nClaude extras.\n");
        adopt_files(&repo, BOTH);
        assert!(repo.read(AGENTS_FILE).contains(text::BLOCK));
        assert_eq!(repo.read(CLAUDE_FILE), "@AGENTS.md\n\nClaude extras.\n");
        let separate = TestRepo::new();
        separate.commit_file(AGENTS_FILE, "Rules.\n");
        separate.commit_file(CLAUDE_FILE, "Claude rules.\n");
        adopt_files(&separate, BOTH);
        assert!(separate.read(CLAUDE_FILE).contains(text::BLOCK));
    }

    #[test]
    fn a_rerun_changes_nothing() {
        let repo = TestRepo::new();
        repo.commit_file(
            AGENTS_FILE,
            "---\ntitle: x\n---\n# Rules\nBody without blank line.\n",
        );
        adopt_files(&repo, BOTH);
        let first = (repo.read(AGENTS_FILE), repo.read(CLAUDE_FILE));
        let again = plan_instructions(repo.root(), BOTH, false);
        assert!(again.changes.iter().all(|change| !change.is_change()));
        assert!(
            again
                .changes
                .iter()
                .all(|change| change.summary == "current")
        );
        assert_eq!((repo.read(AGENTS_FILE), repo.read(CLAUDE_FILE)), first);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_repository_is_written_through() {
        let repo = TestRepo::new();
        repo.commit_file("docs/agents.md", "Shared rules.\n");
        std::os::unix::fs::symlink("docs/agents.md", repo.root().join(AGENTS_FILE)).unwrap();
        std::os::unix::fs::symlink("docs/agents.md", repo.root().join(CLAUDE_FILE)).unwrap();
        let plan = adopt_files(&repo, BOTH);
        assert_eq!(plan.changes.len(), 1, "one shared target is written once");
        assert!(repo.read("docs/agents.md").contains(text::BLOCK));
        let link = fs::symlink_metadata(repo.root().join(AGENTS_FILE)).unwrap();
        assert!(link.file_type().is_symlink(), "the link itself is kept");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_outside_the_repository_is_refused_with_a_reason() {
        let repo = TestRepo::new();
        let outside = tempfile::tempdir().unwrap();
        let elsewhere = outside.path().join("agents.md");
        fs::write(&elsewhere, "Not ours.\n").unwrap();
        std::os::unix::fs::symlink(&elsewhere, repo.root().join(AGENTS_FILE)).unwrap();
        let plan = adopt_files(&repo, CODEX);
        assert!(plan.changes.is_empty());
        assert!(
            plan.skipped[0].1.contains("outside the repository"),
            "{:?}",
            plan.skipped
        );
        assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "Not ours.\n");
    }

    #[test]
    fn existing_instructions_and_broken_markers_are_left_alone() {
        let repo = TestRepo::new();
        repo.commit_file(AGENTS_FILE, "Read `.agent-coordinator.toml` first.\n");
        let plan = adopt_files(&repo, CODEX);
        assert!(
            plan.skipped[0]
                .1
                .contains("already has coordinator instructions")
        );
        let forced = plan_instructions(repo.root(), CODEX, true);
        assert!(forced.changes[0].is_change());
        let broken = TestRepo::new();
        broken.commit_file(AGENTS_FILE, "<!-- agent-coordinator:begin v1 -->\nhalf\n");
        let plan = adopt_files(&broken, CODEX);
        assert!(
            plan.skipped[0].1.contains("no end marker"),
            "{:?}",
            plan.skipped
        );
        assert_eq!(
            broken.read(AGENTS_FILE),
            "<!-- agent-coordinator:begin v1 -->\nhalf\n"
        );
    }

    #[test]
    fn a_file_changed_after_planning_is_left_untouched() {
        let repo = TestRepo::new();
        repo.commit_file(AGENTS_FILE, "Rules.\n");
        let plan = plan_instructions(repo.root(), CODEX, false);
        repo.write(AGENTS_FILE, "Edited meanwhile.\n");
        let error = apply(&plan.changes[0]).unwrap_err();
        assert_eq!(error.exit, 5);
        assert_eq!(repo.read(AGENTS_FILE), "Edited meanwhile.\n");
    }

    #[test]
    fn a_new_file_never_replaces_one_that_appeared() {
        let repo = TestRepo::new();
        let change = plan_binding(repo.root(), "https://agents.example", "p1", false);
        repo.write(
            BINDING_FILE,
            "service_url = \"https://other.example\"\nproject_id = \"p2\"\n",
        );
        let error = apply(&change).unwrap_err();
        assert_eq!(error.exit, 5);
        assert!(repo.read(BINDING_FILE).contains("other.example"));
    }

    #[cfg(unix)]
    #[test]
    fn modes_are_kept_for_existing_files_and_ordinary_for_new_ones() {
        use std::os::unix::fs::PermissionsExt;
        let repo = TestRepo::new();
        repo.commit_file(AGENTS_FILE, "Rules.\n");
        let path = repo.root().join(AGENTS_FILE);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        adopt_files(&repo, BOTH);
        let mode = |name: &str| {
            fs::metadata(repo.root().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(AGENTS_FILE), 0o640);
        assert_eq!(mode(CLAUDE_FILE), 0o644);
    }

    #[test]
    fn only_adopts_own_text_counts_as_an_adopt_change() {
        let repo = TestRepo::new();
        let head = b"# Rules\n\nBe kind.\n";
        repo.write(AGENTS_FILE, "# Rules\n\nBe kind.\n");
        let plan = adopt_files(&repo, CODEX);
        let change = &plan.changes[0];
        assert!(holds_only_adopt_text(change, Some(head)));
        assert!(
            !holds_only_adopt_text(change, None),
            "an untracked file with other text is the person's"
        );
        repo.write(
            AGENTS_FILE,
            &format!("{}\nAn unsaved thought.\n", repo.read(AGENTS_FILE)),
        );
        assert!(!holds_only_adopt_text(change, Some(head)));
        let fresh = TestRepo::new();
        let created = adopt_files(&fresh, BOTH);
        assert!(
            created
                .changes
                .iter()
                .all(|change| holds_only_adopt_text(change, None))
        );
    }

    #[test]
    fn readiness_follows_the_import_line() {
        let repo = TestRepo::new();
        assert_eq!(readiness(repo.root(), true), Readiness::Missing);
        adopt_files(&repo, BOTH);
        assert_eq!(readiness(repo.root(), true), Readiness::Block);
        assert_eq!(readiness(repo.root(), false), Readiness::Block);
        let own = TestRepo::new();
        own.write(AGENTS_FILE, "Read `.agent-coordinator.toml`.\n");
        assert_eq!(readiness(own.root(), false), Readiness::OwnInstructions);
    }
}
