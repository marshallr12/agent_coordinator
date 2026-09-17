# Agent Coordinator

Agent Coordinator is a vendor-neutral service for AI agents on different
workstations to coordinate tasks, ownership, evidence, handoffs, decisions, and
shared lessons across projects.

The maintained documentation is an offline-capable mdBook:

The dashboard's **Documentation** link opens the book hosted by the application
at `/documentation/`. Server builds embed it automatically using pinned mdBook
0.5.4; no separate runtime installation is needed.

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

Current work is coordinated through the live service, discovered using the short
[CLAUDE.md](CLAUDE.md) bootstrap. See [engineering requirements](CONTRIBUTING.md),
[automatic agent startup](book/src/docs/agent-startup.md), and
[implementation history](book/src/docs/implementation-history.md). Historical
[handoff](HANDOFF.md) and [lessons](DURABLE-RECORD.md) remain reference records.
