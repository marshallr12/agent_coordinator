---
name: coordinator-migrate
description: Migrate the current repository from Markdown project memory (HANDOFF.md, BACKLOG.md, DURABLE-RECORD.md, SAVERS.md, HANDOFF-archive.md, .claude/decisions-pending.md and similar) to the Agent Coordinator workflow — creating equivalent coordinator tasks, "Knowledge & evidence" records and decisions, then archiving the Markdown files and pointing AGENTS.md at the service. Use when the user asks to migrate, onboard, or move a repo onto Agent Coordinator. Assumes .agent-coordinator.toml is at the repo root and the agent-coordinator CLI is installed and authenticated.
---

# Migrate a repository onto Agent Coordinator

This is a one-time migration. **Do not follow the coordinator work loop while running it**: do not claim, checkpoint or submit tasks, and ignore any AGENTS.md instruction to "select and claim eligible work". The migration only *creates* records with the `agent-coordinator` CLI and then edits the repo.

Helper: `python3 ~/.claude/skills/coordinator-migrate/scripts/migrate.py` (stdlib only; referred to below as `migrate.py`). Classification rules and field limits are in `reference/mapping.md` in this skill directory. Read that file before you write any plan fragment.

## Settled design (do not re-ask)

- **Tasks.** Every open backlog item is created `planned`, so it cannot be claimed. Only the items HANDOFF.md explicitly queues as next items are created ready (`planned: false`).
- **Standing rules** become `validated` `fact` knowledge tagged `standing-rule`. The binding rules stay in the repo's AGENTS.md/CLAUDE.md. Never edit the project's `rules` policy.
- **History.** Recorded rejections become `rejected_approach` records. The closed/do-not-requeue index becomes compact `closed-item` facts. Session narratives and wave logs are **not** migrated; git keeps them.
- **Old files.** Afterwards they move to `docs/pre-coordinator/` with `git mv`. HANDOFF.md is replaced by a short stub, and the root AGENTS.md (created if missing) gets the coordinator section.
- **Context guards.** Agents in the migrated repo must not pull large text at startup, so:
  - AGENTS.md tells them to read the ~50 KB `/api/v1/info` selectively and to search context with a small `--budget`.
  - Migrated records stay small, because orientation shows up to 20 ready tasks in full and context search packs whole records. Target task descriptions of at most 1,500 characters and knowledge bodies of at most 2,000; see `reference/mapping.md`.
  - The same applies to you during the migration: never `export`, and never print full `--json` lists into context.

## Procedure

### 1. Preflight (stop on any failure)

```sh
python3 migrate.py preflight
```

This checks the binding, CLI compatibility, authentication (`policy show`), the git state, and the records already in the project. It also prints the **plan directory** (outside the repo, under `~/.local/state/coordinator-migrate/<project_id>/`). All fragments go there, never in the repo.

- A dirty tree: ask the user to commit or stash first. Provenance pins the HEAD commit.
- Existing records in the project are fine. `apply` is idempotent by migration key. Still, tell the user how many already exist before continuing.
- Create a branch `coordinator-migration` from the current branch for the repo edits in step 6.

### 2. Inventory the sources

```sh
python3 migrate.py inventory
```

This lists the candidate source files with their sizes and heading outline (line ranges per `##`/`###` section). Tell the user which sources you will migrate. Name any file you are skipping and why. Also skim the repo-root `*.md` files the inventory flags as "other" (PLAN.md, TODO.md, ROADMAP.md, NOTES.md). They are sources only if they hold open work, rules or facts, not product docs.

### 3. Build plan fragments, one per source section

Read **every line** of each migrated source. Use `Read` with offset/limit per outline section, because these files are often hundreds of KB. Write one JSON fragment per section, or per coherent group of sections, into the plan directory. Name each one `NN-<source>-<section-slug>.json`. The schema and the section→record mapping are in `reference/mapping.md`.

- **Big repos:** fan out with the Agent tool, using one general-purpose subagent per source file or large section range. Give each subagent this skill's `reference/mapping.md` path, the exact line range, its fragment filename, and the key prefix it owns (so keys never collide). Tell it to write only its fragment and to read every line of its range. Run the subagents in parallel. The main loop merges and validates their fragments.
- A fact that appears in several files becomes one record. Keep the most specific source, and list the others in `body`'s last line: `Also recorded in: …`.
- Never invent content. A record's text must be a faithful condensation of its source, with commands, paths, SHAs, versions, numbers and "do not" wording kept verbatim.
- Cross-references ("see SAVERS → Waves", "rule 12") are rewritten as plain descriptions that will make sense without the file, e.g. "the standing rule that doc changes are deployed (rule 12 in the pre-migration record)".

### 4. Validate and review with the user

```sh
python3 migrate.py validate
```

This enforces the service limits, key uniqueness and dependency references, then prints counts by source and kind plus every ready task. Fix every error. Then show the user:

1. the counts table;
2. the full list of **ready** tasks and every **decision**;
3. five sampled knowledge records, in full;
4. the sources or sections deliberately not migrated, with the reason for each.

Ask for approval with AskUserQuestion before applying. Applying creates hundreds of shared records, and deleting them later means cancelling or deprecating each one by hand.

### 5. Apply

```sh
python3 migrate.py apply --dry-run   # prints the create order, touches nothing
python3 migrate.py apply
```

Records are created in this order: tasks in dependency order, then knowledge (scoped to the created task IDs), then decisions (pinned to current task revisions). Every created ID is written to `ledger.json` in the plan directory immediately. A re-run skips keys already in the ledger, or already present on the service with the same migration marker. So after any failure you fix the cause and re-run `apply`; never delete the ledger.

- If the CLI reports a **pending mutation** (the outcome of a create is uncertain), run `agent-coordinator retry` once and then re-run `apply`. The marker scan picks up the retried record.
- Report failures verbatim. Do not hand-create records around the script.

### 6. Retire the Markdown memory (repo edits, on the migration branch)

1. `git mv` every migrated source into `docs/pre-coordinator/` (create the directory). Skipped files stay where they are.
2. Write a new root `HANDOFF.md` stub (template in `reference/mapping.md`). It says project state now lives in the coordinator and names the archive directory, because global `handoff`/`resume` skills and hooks still look for this file.
3. **A root `AGENTS.md` must exist afterwards** with the "Agent Coordinator" section from `reference/mapping.md`. Create the file if it is missing. Before inserting the section, check it against the service's *current* startup guidance, without loading the whole info document (about 50 KB):
   ```sh
   curl -s --max-redirs 0 "$SERVICE_URL/api/v1/info" -o "$PLAN_DIR/../info.json"
   python3 -c "import json,sys; a=json.load(open(sys.argv[1]))['data']; s=a['agent_startup']; print(json.dumps({k:s[k] for k in ('local_bootstrap','connection_preference','missing_client_behavior','state_authority')}, indent=1)); print([l for l in s['guide'].splitlines() if l.startswith('## ')])" "$PLAN_DIR/../info.json"
   ```
   If `local_bootstrap` or the guide headings describe a step the template lacks, or contradict it, adapt the section: keep the context guards (step 2's selective read, step 5's small `--budget`) and keep it under about 40 lines. Tell the user what you changed and why.
   Also update CLAUDE.md if it is a separate real file (not a symlink), by pointing it at AGENTS.md's coordinator section.
   - Insert the section at the top of AGENTS.md, below the title.
   - Rewrite every paragraph that points at the migrated files ("See HANDOFF.md — the living doc…", the companion-file lists, the prune procedure) to point at the coordinator: tasks for work, knowledge search (`agent-coordinator context --query …`) for facts, rules and rejections, and decisions for open questions.
   - Leave the repo's binding rules, gate commands and conventions intact.
4. Find the remaining references: `git grep -n -E 'HANDOFF|BACKLOG|DURABLE-RECORD|SAVERS|decisions-pending'`.
   - Rewrite **pure prose pointers** in README/docs.
   - **Do not rewrite tooling** that reads or writes those files: `scripts/`, `.claude/skills/`, `.claude/agents/`, hooks, CI. List each one in the final report, and create one `planned` `general` task per tool to retarget it at the coordinator. Use `migrate.py apply` with an extra fragment `99-followups.json`, so these tasks are keyed and idempotent too.
5. Commit on the migration branch: `Migrate project memory to Agent Coordinator`, with a body giving the record counts and the plan-directory path. Do not push unless the user asks.

### 7. Verify and report

- `python3 migrate.py verify` re-reads each ledger ID from the service and reports anything missing.
- Run a spot check: `agent-coordinator context --query "<a distinctive fact from SAVERS>"` should return the migrated record.
- The final report covers: the counts created or skipped per kind, the ready tasks, the open decisions, the tooling still reading the old files (with its follow-up task IDs), the commit SHA, and the plan-directory path.
