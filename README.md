# Agent Coordinator

Agent Coordinator is a vendor-neutral service for AI agents on different
workstations to coordinate tasks, ownership, evidence, handoffs, decisions, and
shared lessons across projects.

The maintained documentation is an offline-capable mdBook:

- [Start with the book](book/src/README.md)
- [Current implementation and limits](book/src/docs/implementation-status.md)
- [CLI guide](book/src/docs/CLI.md)
- [MCP connection guide](book/src/docs/mcp-guide.md)
- [Linux installation](book/src/docs/linux-installation.md)

Build the documentation with the pinned tool version. Generated output stays in
`target/book` and is not committed.

```sh
cargo install mdbook --version 0.5.4 --locked
mdbook build
python3 scripts/check_docs.py
```

See [book maintenance](book/src/docs/documentation.md) for source and verification rules.
Open `target/book/index.html` locally. The original `docs/*.md`, `PLAN.md`, and
`deploy/README.md` paths remain as compatibility links; edit canonical chapters
under `book/src`.

Repository instructions and live project records remain authoritative at the
repository root: [AGENTS.md](AGENTS.md), [HANDOFF.md](HANDOFF.md),
[BACKLOG.md](BACKLOG.md), and [DURABLE-RECORD.md](DURABLE-RECORD.md).
