//! The local-action part of the privilege gate's scope: directories and
//! files outside `.github/` whose change alters what CI runs.
//!
//! Trust model: the scope is read from target tip X only, the approved
//! target. Every path R changes under `.github/` is gated anyway, so a
//! workflow or action definition that R adds or rewrites already needs a
//! human decision; only X's definitions decide which other paths CI runs
//! without one. From X the collector takes every `uses: ./<dir>` reference in
//! files under `.github/`, the paths an action's `main`, `pre`, `post` and
//! local `image` name, and the targets of symbolic links under `.github/` or
//! a collected directory, repeating for every directory it adds. Symbolic
//! links that R adds or changes inside the scope are followed too. Any
//! reference that cannot be read as a plain path makes the whole repository
//! the scope (fail closed).
use crate::action_refs::{self, Ref};
use crate::git;
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::Path;

/// File names that define a local action inside its directory.
const ACTION_FILES: &[&str] = &["action.yml", "action.yaml"];

/// Paths whose change needs a decision because CI at X runs them.
#[derive(Debug, Default)]
pub struct LocalScope {
    /// Scope entries: a path and everything below it; empty is everything.
    entries: BTreeSet<String>,
    /// True when some reference at X could not be read; everything is gated.
    pub unparsed: bool,
}

impl LocalScope {
    /// True when `path` is an entry or lies below one, ignoring letter case.
    pub fn covers(&self, path: &str) -> bool {
        let path = path.to_ascii_lowercase();
        self.entries.iter().any(|entry| {
            let entry = entry.to_ascii_lowercase();
            entry.is_empty() || path == entry || path.starts_with(&format!("{entry}/"))
        })
    }
}

/// Collects the scope from `x`, following symbolic links R adds or changes
/// among `changed`.
pub fn collect(mirror: &Path, x: &str, r: &str, changed: &[String]) -> Result<LocalScope> {
    let mut collector = Collector {
        mirror,
        x,
        scope: LocalScope::default(),
        pending: Vec::new(),
    };
    collector.scan_ci_dir()?;
    collector.drain()?;
    collector.follow_changed_links(r, changed)?;
    collector.drain()?;
    Ok(collector.scope)
}

/// Worklist state of one collection.
struct Collector<'a> {
    mirror: &'a Path,
    x: &'a str,
    scope: LocalScope,
    pending: Vec<String>,
}

impl Collector<'_> {
    /// Reads every file and link under `.github/` at X.
    fn scan_ci_dir(&mut self) -> Result<()> {
        for (mode, path) in git::tree_entries(self.mirror, self.x, ".github")? {
            if mode == git::SYMLINK_MODE {
                self.add_link(self.x, &path)?;
            } else {
                let text = self.read(&path);
                self.add_uses(&text);
            }
        }
        Ok(())
    }

    /// Visits pending directories until none is left.
    fn drain(&mut self) -> Result<()> {
        while let Some(dir) = self.pending.pop() {
            self.visit(&dir)?;
        }
        Ok(())
    }

    /// Reads one collected directory at X: its links and its action file.
    fn visit(&mut self, dir: &str) -> Result<()> {
        for (mode, path) in git::tree_entries(self.mirror, self.x, dir)? {
            if mode == git::SYMLINK_MODE {
                self.add_link(self.x, &path)?;
            } else if is_action_file(dir, &path) {
                let text = self.read(&path);
                self.add_uses(&text);
                let (paths, unparsed) = action_refs::runs_paths(&text, dir);
                self.scope.unparsed |= unparsed;
                paths.into_iter().for_each(|p| self.add(p));
            }
        }
        Ok(())
    }

    /// Follows links that R adds or changes inside the scope.
    fn follow_changed_links(&mut self, r: &str, changed: &[String]) -> Result<()> {
        for path in changed {
            let inside =
                path.to_ascii_lowercase().starts_with(".github/") || self.scope.covers(path);
            if !inside {
                continue;
            }
            let entries = git::tree_entries(self.mirror, r, path)?;
            if entries
                .iter()
                .any(|(mode, p)| p == path && mode == git::SYMLINK_MODE)
            {
                self.add_link(r, path)?;
            }
        }
        Ok(())
    }

    /// Adds the local directories of every `uses` in `text`.
    fn add_uses(&mut self, text: &str) {
        for reference in action_refs::uses_refs(text) {
            match reference {
                Ref::Local(dir) => self.add(dir),
                Ref::Remote => {}
                Ref::Unparsed => self.scope.unparsed = true,
            }
        }
    }

    /// Adds the target of link `path` at `rev`, resolved against the link's
    /// directory; an absolute or unreadable target covers everything.
    fn add_link(&mut self, rev: &str, path: &str) -> Result<()> {
        let target = git::file_at(self.mirror, rev, path).ok().flatten();
        let parent = path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let entry = match target {
            Some(t) if !t.starts_with('/') => action_refs::resolve(&format!("{parent}/{t}")),
            _ => String::new(),
        };
        self.add(entry);
        Ok(())
    }

    /// Adds a scope entry and queues it for a visit when it is new.
    fn add(&mut self, entry: String) {
        if self.scope.entries.insert(entry.clone()) && !entry.is_empty() {
            self.pending.push(entry);
        }
    }

    /// The content of `path` at X; an unreadable file fails closed.
    fn read(&mut self, path: &str) -> String {
        match git::file_at(self.mirror, self.x, path) {
            Ok(Some(text)) => text,
            _ => {
                self.scope.unparsed = true;
                String::new()
            }
        }
    }
}

/// True when `path` is the action definition directly inside `dir`.
fn is_action_file(dir: &str, path: &str) -> bool {
    let name = path.strip_prefix(dir).and_then(|p| p.strip_prefix('/'));
    name.is_some_and(|n| ACTION_FILES.contains(&n.to_ascii_lowercase().as_str()))
}
