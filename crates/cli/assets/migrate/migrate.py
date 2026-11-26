#!/usr/bin/env python3
"""coordinator-migrate helper.

Inventories a repository's Markdown project memory, validates a migration plan
(JSON fragments written by the agent), and applies it idempotently to Agent
Coordinator through the `agent-coordinator` CLI. Standard library only.

Subcommands: preflight, inventory, validate, apply [--dry-run], verify.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

KNOWN_SOURCES = ["HANDOFF.md", "BACKLOG.md", "DURABLE-RECORD.md", "SAVERS.md",
                 "HANDOFF-archive.md", ".claude/decisions-pending.md"]
OTHER_HINT = re.compile(r"^(PLAN|TODO|ROADMAP|NOTES|DECISIONS|STATUS)", re.I)
NOT_SOURCES = {"README.md", "AGENTS.md", "CLAUDE.md", "CONTRIBUTING.md",
               "CHANGELOG.md", "LICENSE.md", "SECURITY.md"}
MARKER = "migration key `{key}`"
MARKER_RE = re.compile(r"migration key `([^`]+)`")
KEY_RE = re.compile(r"^[a-z0-9][a-z0-9/_.-]{0,119}$")
KINDS = ("tasks", "knowledge", "decisions")


class MigrationError(Exception):
    """A failure the operator must fix before re-running."""


# ---------------------------------------------------------------- environment

def repo_root() -> Path:
    """Return the git top-level directory of the current working directory."""
    out = run(["git", "rev-parse", "--show-toplevel"])
    return Path(out.strip())


def run(argv, stdin=None, cwd=None) -> str:
    """Run a command, returning stdout or raising MigrationError with stderr."""
    proc = subprocess.run(argv, input=stdin, capture_output=True, text=True, cwd=cwd)
    if proc.returncode != 0:
        detail = (proc.stderr or proc.stdout).strip()
        raise MigrationError(f"{' '.join(argv[:3])} exited {proc.returncode}: {detail}")
    return proc.stdout


def binding(root: Path) -> dict:
    """Read the non-secret repository binding (.agent-coordinator.toml)."""
    path = root / ".agent-coordinator.toml"
    if not path.is_file():
        raise MigrationError(f"{path} not found; the skill requires it at the repo root.")
    with path.open("rb") as fh:
        data = tomllib.load(fh)
    for field in ("service_url", "project_id"):
        if not data.get(field):
            raise MigrationError(f".agent-coordinator.toml lacks {field}.")
    return data


def state_dir(project_id: str) -> Path:
    """Directory outside the repo holding the plan fragments and the ledger."""
    base = os.environ.get("XDG_STATE_HOME") or str(Path.home() / ".local/state")
    return Path(base) / "coordinator-migrate" / project_id


def cli(root: Path, *args, body=None) -> dict:
    """Call the agent-coordinator CLI with --json and return its `data` value."""
    argv = ["agent-coordinator", *args, "--json"]
    if body is not None:
        argv.extend(["--input", "-"])
    out = run(argv, stdin=json.dumps(body) if body is not None else None, cwd=root)
    parsed = json.loads(out) if out.strip() else {}
    return parsed.get("data", parsed)


def paged(root: Path, *args) -> list:
    """Collect every item from a cursor-paginated CLI list command."""
    items, cursor = [], None
    while True:
        extra = ["--cursor", cursor] if cursor else []
        data = cli(root, *args, "--limit", "200", *extra)
        items.extend(data.get("items", []))
        cursor = data.get("next_cursor")
        if not cursor:
            return items


# ------------------------------------------------------------------ preflight

def cmd_preflight(args) -> None:
    """Check binding, CLI compatibility, auth, git state and existing records."""
    root = repo_root()
    bind = binding(root)
    run(["agent-coordinator", "compatibility", "--json"], cwd=root)
    policy = cli(root, "policy", "show")
    print(f"repo:      {root}")
    print(f"project:   {policy.get('name')} ({bind['project_id']}) at {bind['service_url']}")
    print(f"policy:    revision {policy.get('policy_revision')}, repo {policy.get('repository_url')}")
    directory = plan_dir(args, bind)
    directory.mkdir(parents=True, exist_ok=True)
    print(f"plan dir:  {directory}")
    report_git(root)
    report_existing(root)


def report_git(root: Path) -> None:
    """Print HEAD, branch and whether the working tree is clean."""
    head = run(["git", "rev-parse", "HEAD"], cwd=root).strip()
    branch = run(["git", "branch", "--show-current"], cwd=root).strip()
    dirty = run(["git", "status", "--porcelain"], cwd=root).strip()
    print(f"git:       {branch or '(detached)'} @ {head[:12]}")
    print("tree:      " + ("DIRTY — commit or stash before migrating" if dirty else "clean"))


def report_existing(root: Path) -> None:
    """Print how many records exist already and how many carry migration keys."""
    for kind, field in (("tasks", "description"), ("knowledge", None), ("decisions", "rationale")):
        items = paged(root, kind, "list")
        keyed = sum(1 for i in items if marker_of(i, field))
        print(f"existing:  {len(items)} {kind} ({keyed} from a previous migration run)")


# ------------------------------------------------------------------ inventory

def cmd_inventory(args) -> None:
    """List candidate source files with byte size and a heading outline."""
    root = repo_root()
    for rel in candidate_sources(root):
        path = root / rel
        text = path.read_text(encoding="utf-8", errors="replace")
        label = "known" if rel in KNOWN_SOURCES else "other"
        print(f"\n## {rel}  [{label}] {len(text.encode())} bytes, {text.count(chr(10))} lines")
        for line in outline(text):
            print(line)


def candidate_sources(root: Path) -> list:
    """Known memory files that exist, then other root docs that may hold work."""
    found = [rel for rel in KNOWN_SOURCES if (root / rel).is_file()]
    for path in sorted(root.glob("*.md")):
        if path.name not in NOT_SOURCES and path.name not in found and OTHER_HINT.match(path.name):
            found.append(path.name)
    return found


def outline(text: str) -> list:
    """Return '  L<start>-<end> (<bytes>B) <heading>' rows for #..### headings."""
    lines = text.splitlines()
    starts = [i for i, l in enumerate(lines) if re.match(r"^#{1,3} ", l)]
    rows = []
    for n, start in enumerate(starts):
        end = starts[n + 1] if n + 1 < len(starts) else len(lines)
        size = sum(len(l.encode()) + 1 for l in lines[start:end])
        rows.append(f"  L{start + 1}-{end} ({size}B) {lines[start][:100]}")
    return rows


# ------------------------------------------------------------------- the plan

def plan_dir(args, bind: dict) -> Path:
    """The plan directory: --plan-dir, else <state dir>/plan."""
    return Path(args.plan_dir) if args.plan_dir else state_dir(bind["project_id"]) / "plan"


def load_plan(directory: Path) -> dict:
    """Merge every *.json fragment in the plan directory, tagging each item's fragment."""
    plan = {kind: [] for kind in KINDS}
    fragments = sorted(directory.glob("*.json"))
    if not fragments:
        raise MigrationError(f"No fragments in {directory}.")
    for path in fragments:
        data = json.loads(path.read_text(encoding="utf-8"))
        for kind in KINDS:
            for item in data.get(kind, []):
                item["_fragment"] = path.name
                plan[kind].append(item)
    return plan


def footer(item: dict, sha: str) -> str:
    """Provenance footer appended to task descriptions and decision rationales."""
    src = item.get("source", {})
    return (f"\n\n---\n_Migrated from `{src.get('path', '?')}` § {src.get('section', '?')} "
            f"at `{sha[:12]}` ({MARKER.format(key=item['key'])})._")


# ----------------------------------------------------------------- validation

def cmd_validate(args) -> None:
    """Validate the merged plan against service limits and print a review summary."""
    root = repo_root()
    plan = load_plan(plan_dir(args, binding(root)))
    errors = validate(plan)
    for err in errors:
        print(f"ERROR {err}")
    for warning in size_warnings(plan):
        print(f"WARN  {warning}")
    summarize(plan)
    if errors:
        raise MigrationError(f"{len(errors)} validation error(s).")
    print("\nplan valid")


def validate(plan: dict) -> list:
    """Return every validation error across the plan."""
    errors = check_keys(plan)
    task_keys = {t["key"] for t in plan["tasks"]}
    for task in plan["tasks"]:
        errors += [f"task {task.get('key')}: {e}" for e in task_errors(task, task_keys)]
    for rec in plan["knowledge"]:
        errors += [f"knowledge {rec.get('key')}: {e}" for e in knowledge_errors(rec, task_keys)]
    for dec in plan["decisions"]:
        errors += [f"decision {dec.get('key')}: {e}" for e in decision_errors(dec, task_keys)]
    if not errors:
        errors += cycle_errors(plan["tasks"])
    return errors


def size_warnings(plan: dict) -> list:
    """Records over the context-guard targets (orientation and context search stay small)."""
    warnings = [f"task {t['key']}: description {len(t.get('description', ''))} chars > 1500 target"
                for t in plan["tasks"] if len(t.get("description", "")) > 1500]
    warnings += [f"knowledge {r['key']}: body {len(r.get('body', ''))} chars > 2000 target; split it?"
                 for r in plan["knowledge"] if len(r.get("body", "")) > 2000]
    return warnings


def check_keys(plan: dict) -> list:
    """Keys must be well-formed and unique across all kinds and fragments."""
    seen, errors = {}, []
    for kind in KINDS:
        for item in plan[kind]:
            key = item.get("key", "")
            if not KEY_RE.match(key):
                errors.append(f"{item['_fragment']}: bad key {key!r}")
            elif key in seen:
                errors.append(f"duplicate key {key} in {item['_fragment']} and {seen[key]}")
            seen[key] = item["_fragment"]
    return errors


def length_error(name: str, value, low: int, high: int) -> list:
    """One error if len(value) is outside [low, high]."""
    size = len(value.encode()) if isinstance(value, str) else len(value)
    return [] if low <= size <= high else [f"{name} length {size} not in {low}..{high}"]


def source_errors(item: dict) -> list:
    """Every item needs source.path and source.section for provenance."""
    src = item.get("source") or {}
    return [] if src.get("path") and src.get("section") else ["source.path and source.section required"]


def task_errors(task: dict, task_keys: set) -> list:
    """Validate one task against TaskInput limits."""
    errs = length_error("title", task.get("title", ""), 1, 300)
    errs += length_error("description", task.get("description", "") + footer(task, "0" * 40), 0, 32768)
    crit = task.get("acceptance_criteria", [])
    errs += length_error("acceptance_criteria", crit, 1, 100)
    errs += [e for c in crit for e in length_error("criterion", c, 1, 2048)]
    if task.get("kind", "code") not in ("code", "general"):
        errs.append("kind must be code or general")
    if task.get("priority", 2) not in (0, 1, 2, 3):
        errs.append("priority must be 0..3")
    errs += [f"depends_on unknown key {k}" for k in task.get("depends_on", []) if k not in task_keys]
    return errs + source_errors(task)


def knowledge_errors(rec: dict, task_keys: set) -> list:
    """Validate one knowledge record against KnowledgeInput limits."""
    errs = length_error("title", rec.get("title", ""), 1, 255)
    errs += length_error("body", rec.get("body", ""), 1, 32768)
    errs += length_error("applicability", rec.get("applicability", ""), 0, 4096)
    if rec.get("kind") not in ("lesson", "fact", "rejected_approach", "checkpoint"):
        errs.append("kind must be lesson/fact/rejected_approach/checkpoint")
    if rec.get("status") not in ("observed", "validated"):
        errs.append("status must be observed or validated for migrated records")
    errs += unique_list_errors("tags", rec.get("tags", []), 50, 64)
    errs += unique_list_errors("components", rec.get("components", []), 50, 255)
    errs += [f"task_keys unknown key {k}" for k in rec.get("task_keys", []) if k not in task_keys]
    return errs + source_errors(rec)


def unique_list_errors(name: str, values: list, max_count: int, max_len: int) -> list:
    """Bounded list of non-empty strings, unique ignoring case."""
    errs = length_error(name, values, 0, max_count)
    errs += [e for v in values for e in length_error(name[:-1], v, 1, max_len)]
    if len({v.lower() for v in values}) != len(values):
        errs.append(f"{name} must be unique ignoring case")
    return errs


def decision_errors(dec: dict, task_keys: set) -> list:
    """Validate one decision against DecisionInput limits."""
    errs = length_error("question", dec.get("question", ""), 1, 4096)
    errs += length_error("rationale", dec.get("rationale", "") + footer(dec, "0" * 40), 1, 8192)
    errs += length_error("options", dec.get("options", []), 2, 20)
    if dec.get("required_actor") not in ("human", "agent", "either"):
        errs.append("required_actor must be human/agent/either")
    keys = dec.get("task_keys", [])
    errs += length_error("task_keys", keys, 1, 100)
    errs += [f"task_keys unknown key {k}" for k in keys if k not in task_keys]
    return errs + source_errors(dec)


def cycle_errors(tasks: list) -> list:
    """Report a dependency cycle, if any, by attempting a topological order."""
    try:
        topo_order(tasks)
        return []
    except MigrationError as err:
        return [str(err)]


def topo_order(tasks: list) -> list:
    """Tasks ordered so every dependency precedes its dependents."""
    by_key = {t["key"]: t for t in tasks}
    done, visiting, order = set(), set(), []

    def visit(key):
        if key in done:
            return
        if key in visiting:
            raise MigrationError(f"dependency cycle through {key}")
        visiting.add(key)
        for dep in by_key[key].get("depends_on", []):
            visit(dep)
        visiting.discard(key)
        done.add(key)
        order.append(by_key[key])

    for task in tasks:
        visit(task["key"])
    return order


def summarize(plan: dict) -> None:
    """Print counts by source and kind, every ready task, and every decision."""
    counts = {}
    for kind in KINDS:
        for item in plan[kind]:
            label = kind if kind != "knowledge" else f"knowledge:{item.get('kind')}"
            path = item.get("source", {}).get("path", "?")
            counts[(path, label)] = counts.get((path, label), 0) + 1
    print("\nsource | record | count")
    for (path, label), n in sorted(counts.items()):
        print(f"{path} | {label} | {n}")
    ready = [t for t in plan["tasks"] if not t.get("planned", False)]
    print(f"\nready tasks ({len(ready)}):")
    for t in ready:
        print(f"  P{t.get('priority', 2)} {t['key']}: {t['title']}")
    print(f"\ndecisions ({len(plan['decisions'])}):")
    for d in plan["decisions"]:
        print(f"  {d['key']}: {d['question'][:160]}")


# ---------------------------------------------------------------------- apply

class Applier:
    """Creates plan records in dependency order, recording each ID in a ledger."""

    def __init__(self, root: Path, bind: dict, directory: Path, dry_run: bool):
        self.root, self.dry_run = root, dry_run
        self.ledger_path = directory.parent / "ledger.json"
        self.ledger = self.load_ledger()
        self.sha = run(["git", "rev-parse", "HEAD"], cwd=root).strip()
        self.policy = cli(root, "policy", "show")
        self.created = {kind: 0 for kind in KINDS}
        self.skipped = {kind: 0 for kind in KINDS}

    def load_ledger(self) -> dict:
        """Read the ledger, or start an empty one."""
        if self.ledger_path.is_file():
            return json.loads(self.ledger_path.read_text())
        return {kind: {} for kind in KINDS}

    def save_ledger(self) -> None:
        """Atomically persist the ledger after every successful create."""
        tmp = self.ledger_path.with_suffix(".tmp")
        tmp.write_text(json.dumps(self.ledger, indent=1, sort_keys=True))
        tmp.replace(self.ledger_path)

    def adopt_existing(self) -> None:
        """Add service records carrying a migration marker to the ledger (crash recovery)."""
        for kind, field in (("tasks", "description"), ("knowledge", None), ("decisions", "rationale")):
            for item in paged(self.root, kind, "list"):
                key = marker_of(item, field)
                if key and key not in self.ledger[kind]:
                    self.ledger[kind][key] = {"id": item["id"]}
        if not self.dry_run:
            self.save_ledger()

    def create(self, kind: str, key: str, args: list, body: dict) -> None:
        """Create one record unless the ledger already has its key."""
        if key in self.ledger[kind]:
            self.skipped[kind] += 1
            return
        self.created[kind] += 1
        if self.dry_run:
            print(f"would create {kind[:-1] if kind != 'knowledge' else kind} {key}")
            return
        data = cli(self.root, *args, body=body)
        self.ledger[kind][key] = {"id": data["id"], "revision": data.get("revision")}
        self.save_ledger()
        print(f"created {kind} {key} -> {data['id']}")

    def task_id(self, key: str) -> str:
        """Resolve a task key to its created ID (a placeholder in dry runs)."""
        entry = self.ledger["tasks"].get(key)
        if entry:
            return entry["id"]
        if self.dry_run:
            return f"<{key}>"
        raise MigrationError(f"task {key} has no ID; re-run apply after fixing earlier errors")

    def apply_tasks(self, tasks: list) -> None:
        """Create tasks so that each dependency exists before its dependents."""
        for task in topo_order(tasks):
            body = {"title": task["title"],
                    "description": task.get("description", "") + footer(task, self.sha),
                    "acceptance_criteria": task["acceptance_criteria"],
                    "kind": task.get("kind", "code"), "priority": task.get("priority", 2),
                    "planned": task.get("planned", True),
                    "depends_on": [self.task_id(k) for k in task.get("depends_on", [])]}
            self.create("tasks", task["key"], ["tasks", "create"], body)

    def apply_knowledge(self, records: list) -> None:
        """Create knowledge records scoped to their migrated tasks."""
        for rec in records:
            body = {"kind": rec["kind"], "title": rec["title"], "body": rec["body"],
                    "status": rec["status"], "tags": rec.get("tags", []),
                    "applicability": rec.get("applicability", ""),
                    "scope": {"task_ids": [self.task_id(k) for k in rec.get("task_keys", [])],
                              "components": rec.get("components", [])},
                    "provenance": self.provenance(rec)}
            self.create("knowledge", rec["key"], ["knowledge", "create"], body)

    def provenance(self, rec: dict) -> dict:
        """Provenance naming the source file, section, lines, commit and key."""
        src = rec["source"]
        lines = src.get("lines", "")
        summary = (f"Migrated from {src['path']} § {src['section']}"
                   f"{' lines ' + lines if lines else ''} at {self.sha} by coordinator-migrate; "
                   + MARKER.format(key=rec["key"]) + ".")
        return {"summary": summary, "source_uri": self.source_uri(src["path"], lines)}

    def source_uri(self, path: str, lines: str) -> str:
        """A permalink to the source lines when the repository is on GitHub."""
        repo = (self.policy.get("repository_url") or "").removesuffix(".git")
        anchor = "#L" + lines.replace("-", "-L") if lines else ""
        if repo.startswith("https://github.com/"):
            return f"{repo}/blob/{self.sha}/{path}{anchor}"
        return f"git:{self.sha}:{path}{anchor}"

    def apply_decisions(self, decisions: list) -> None:
        """Open decisions pinned to the current revision of each affected task."""
        for dec in decisions:
            body = {"question": dec["question"], "options": dec["options"],
                    "rationale": dec["rationale"] + footer(dec, self.sha),
                    "required_actor": dec["required_actor"],
                    "affected_tasks": [self.task_pin(k) for k in dec["task_keys"]],
                    "policy_revision": self.policy["policy_revision"],
                    "environment": dec.get("environment", ""),
                    "conditions": dec.get("conditions", ""), "expires_at": None}
            self.create("decisions", dec["key"], ["decisions", "create"], body)

    def task_pin(self, key: str) -> dict:
        """The {task_id, task_revision} pin for a decision, read fresh from the service."""
        task_id = self.task_id(key)
        if self.dry_run:
            return {"task_id": task_id, "task_revision": 0}
        detail = cli(self.root, "tasks", "show", "--id", task_id)
        return {"task_id": task_id, "task_revision": detail["revision"]}


def marker_of(item: dict, field) -> str:
    """The migration key embedded in a record, or '' if it has none."""
    text = item.get(field, "") if field else (item.get("provenance") or {}).get("summary", "")
    match = MARKER_RE.search(text or "")
    return match.group(1) if match else ""


def cmd_apply(args) -> None:
    """Validate, then create every plan record not already migrated."""
    root = repo_root()
    bind = binding(root)
    directory = plan_dir(args, bind)
    plan = load_plan(directory)
    errors = validate(plan)
    if errors:
        raise MigrationError(f"plan invalid ({len(errors)} errors); run validate")
    applier = Applier(root, bind, directory, args.dry_run)
    applier.adopt_existing()
    applier.apply_tasks(plan["tasks"])
    applier.apply_knowledge(plan["knowledge"])
    applier.apply_decisions(plan["decisions"])
    verb = "would create" if args.dry_run else "created"
    for kind in KINDS:
        print(f"{kind}: {verb} {applier.created[kind]}, already present {applier.skipped[kind]}")


# --------------------------------------------------------------------- verify

def cmd_verify(args) -> None:
    """Re-read every ledger ID from the service and report missing records."""
    root = repo_root()
    bind = binding(root)
    ledger_path = plan_dir(args, bind).parent / "ledger.json"
    ledger = json.loads(ledger_path.read_text())
    present = {kind: {i["id"] for i in paged(root, kind, "list")} for kind in KINDS}
    missing = [(kind, key) for kind in KINDS for key, entry in ledger[kind].items()
               if entry["id"] not in present[kind]]
    for kind in KINDS:
        print(f"{kind}: {len(ledger[kind])} in ledger")
    for kind, key in missing:
        print(f"MISSING {kind} {key} ({ledger[kind][key]['id']}) — closed/archived or deleted?")
    print("verify ok" if not missing else f"{len(missing)} ledger entries not listed as current")


# ----------------------------------------------------------------------- main

def main() -> int:
    """Parse arguments and dispatch to a subcommand."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--plan-dir", help="override the plan fragment directory")
    sub = parser.add_subparsers(dest="cmd", required=True)
    for name in ("preflight", "inventory", "validate", "verify"):
        sub.add_parser(name)
    sub.add_parser("apply").add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    handler = globals()[f"cmd_{args.cmd}"]
    try:
        handler(args)
    except MigrationError as err:
        print(f"error: {err}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
