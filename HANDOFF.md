# Implementation handoff — 2026-09-09

Backlog item 3 implementation is ready: shared knowledge, scoped decisions,
bounded artifacts, and authoritative Markdown imports/exports. Local integrated
validation has passed; CI is pending. Do not mark the item complete until CI passes.
Continue sequentially with item 4 after the main agent reviews that evidence.

Instruction version 4 describes the implemented shared-record workflow and CLI
commands. Existing harnesses must read and acknowledge it before new claims.
Knowledge revisions keep source/applicability, explicit sharing, feedback, and
immutable history. Submission lessons and finalized artifact references commit
in the same transaction as the submission; subsequent corrections cannot rewrite
submitted evidence. Binding-rule revisions preserve their source/reason.

Typed decision answers pin current task/policy revisions and conditions. Denied,
pending, expired, and stale decisions block dependent work and authority displays.
Inspection, checkpointing, and release remain available. Recovery receipts use
current attempt mode and decisions; they cannot resurrect old permission.

Artifact uploads have a 16 MiB hard bound, configurable 10 GiB default quota and
256 MiB disk reserve, exact size/digest, bounded streaming, and explicit tombstones.
Native retries preserve exact bytes/key, reauthenticate, and reconcile matching
finalized metadata. Downloads verify recorded size and SHA-256 before publishing
new files. Source checkpoints travel through Git remotes, not artifact bundles.

Markdown apply requires a human actor after an immutable preview. Agents can
preview and export. Imports never reopen historical closure or complete active
work; changed service records invalidate stale previews. Historical knowledge
reimports preserve immutable kind, append revisions in trigger-safe order, and
use the standard searchable/correctable scope and provenance shapes. Sanitized
SithBit/Submission fixtures came from read-only source review; those repositories
and Submission's pre-existing edits were preserved.

Current local tests, service/CLI smoke, and browser evidence are described in
[implementation status](docs/implementation-status.md). Each worktree must use its
own Cargo target directory. Main-agent review corrected decision authority/read
projections, historical revision ordering, SQL parameter binding, native transfer
integrity/current authentication, and a dashboard project-selector mismatch.

Complete remaining items through 6.2 without operator intervention unless missing
information is essential. Item 6 is Linux acceptance; MCP is 6.1 and mdBook is 6.2.
The operator will commence native Windows workstation acceptance as final item 7;
keep Windows CI checks but do not substitute them for that exercise. No production
deployment, load benchmark, or backup/restore rehearsal has occurred yet.
