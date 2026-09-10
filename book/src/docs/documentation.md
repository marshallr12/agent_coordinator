# Maintaining this book

Install the pinned mdBook release and build from the repository root:

```sh
cargo install mdbook --version 0.5.4 --locked
mdbook build
```

Open `target/book/index.html` in a browser. Navigation, search, styles, and fonts
are bundled with the generated book. External source and reference links still
need network access; the private GitHub repository requires appropriate access.
No public documentation site is deployed by this build.

## Edit the maintained source

Edit chapters under `book/src`, and add each new chapter exactly once to
`book/src/SUMMARY.md`. The short files under the original `docs` directory,
root `PLAN.md`, and `deploy/README.md` direct existing readers to their maintained
chapters. Keep those compatibility links when moving an established document.

`AGENTS.md`, `BACKLOG.md`, `HANDOFF.md`, and `DURABLE-RECORD.md` remain authoritative
at the repository root. Their book chapters include those files directly during
each build, so update the root record rather than copying its prose into a
second maintained document. Each included chapter links to its original source.

Use relative chapter links and retain the distinction between current behavior
and historical design. The [implementation status](implementation-status.md)
records implemented routes and acceptance evidence. Preserve the exact accepted
revision and binary identities when relocating evidence. Historical workstation
file references are literal paths because those files are not shipped with the
book.

## Verify a change

From a source checkout, run the documentation checker:

```sh
python3 scripts/check_docs.py
```

It builds with the pinned version and checks chapter coverage, live-record
includes, generated local links, fragments, and assets. CI runs this check as
well. Inspect the changed pages in a browser, including search and narrow layouts
when their content or navigation changes. A successful build alone does not prove
that a link's destination or anchor exists.

Inspect an existing Linux package without installing it:

```sh
python3 scripts/linux_install_smoke.py \
  --package /absolute/path/to/agent-coordinator-VERSION-linux-x86_64.tar.gz
```

This checks the archive and its offline source links. It does not repeat the
systemd, HTTPS, or sustained-capacity acceptance exercises.

Release archives contain the Markdown sources, `book.toml`, the root records,
and deployment examples, allowing readers to build the book from the extracted
package. Generated HTML is not committed or included in binary packages. Build
and test scripts are maintained in the source checkout. Do not place credentials,
application state, build trees, or unrelated files in `book/src`.
