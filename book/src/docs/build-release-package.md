# Build the release package

The cloud deployment chapters install the same verified Linux x86-64 archive
that [Linux release installation](linux-installation.md) describes. This chapter
covers obtaining that archive: either from the release workflow or by building it
on a workstation. Build on Ubuntu 24.04 x86-64 so the binary matches the
platform the package is accepted for.

## Option A: download the accepted package from CI

Pushing a `v*` tag runs the release workflow, which builds twice, requires
byte-identical archives, exercises a disposable systemd/HTTPS installation, and
uploads the archive and its `.sha256` as the `agent-coordinator-linux-x86_64`
artifact. The artifact is retained for 14 days.

```sh
git tag v0.1.0
git push origin v0.1.0
gh run list --workflow "Release package acceptance" --limit 1
gh run download RUN_ID --name agent-coordinator-linux-x86_64 --dir dist
sha256sum --check dist/agent-coordinator-0.1.0-linux-x86_64.tar.gz.sha256
```

Replace `RUN_ID` with the identifier printed by the list command and `0.1.0`
with the workspace version in `Cargo.toml`. Record the run identity with the
installation evidence; that run is the acceptance claim for the archive.

## Option B: build on a workstation

Install the pinned toolchain and build the three packaged crates with locked
dependencies:

```sh
rustup toolchain install 1.98.1 --profile minimal
rustup default 1.98.1
build_commit="$(git rev-parse HEAD)"
COORDINATOR_BUILD_COMMIT="$build_commit" \
COORDINATOR_SOURCE_REPOSITORY=https://github.com/marshallr12/agent_coordinator \
cargo build --release --locked -p coordinator-server -p coordinator-cli -p coordinator-mcp-transport
```

Package the binaries with the repository's Markdown guidance and systemd files:

```sh
version="$(python3 -c "import tomllib; print(tomllib.load(open('Cargo.toml','rb'))['workspace']['package']['version'])")"
python3 scripts/package_release.py --platform linux-x86_64 --version "$version" \
  --source-commit "$build_commit" \
  --server target/release/agent-coordinator-server \
  --cli target/release/agent-coordinator \
  --mcp-adapter target/release/agent-coordinator-mcp \
  --source-date-epoch "$(git show -s --format=%ct HEAD)" \
  --output-dir dist
```

Pass `--source-commit` to assert that the embedded CLI and server metadata match
the exact clean revision. The package contains `BUILD-IDENTITY.json` and its
internal `SHA256SUMS` includes every artifact. The CLI and server also expose
their build identities directly (`agent-coordinator client-info --json` and
`agent-coordinator-server build-info`). The script prints the archive path,
checksum file, and SHA-256. A workstation build has not passed the release
workflow's systemd/HTTPS exercise. Treat it as
suitable for a staging instance, or run that workflow on the same commit before
calling the instance production.

## Copy the package to the host

Both cloud chapters copy `dist/agent-coordinator-VERSION-linux-x86_64.tar.gz`
and its `.sha256` to the virtual machine, then verify and extract there exactly
as [Linux release installation](linux-installation.md#verify-and-inspect-the-archive)
describes. Never extract an unverified archive with elevated privileges.

## Host updater release contract

The release workflow also builds native host bundles on Ubuntu 24.04 for
`x86_64` and `aarch64`. These are separate from the server installation archive:
`agentc-host-VERSION-linux-ARCH.tar.gz` contains one top directory of the same
name (without `.tar.gz`), with `bin/agentc-supervisor`, `bin/agent-coordinator`,
`bin/agentc-push`, `BUILD-IDENTITY.json`, and an internal `SHA256SUMS` covering
every other file. The binaries are executable regular files; the archive contains
no links, host configuration, credentials, or vendor harness binaries. Therefore
these releases update only the core binaries; they do not promote a harness.

`scripts/package_host.py` checks the clean source revision, workspace and binary
versions, ELF architecture, and embedded CLI build identity. Fixed timestamps,
file order and metadata produce identical archives from identical inputs.
Each matrix job packages twice and compares the bytes. The producer/consumer
regression extracts each architecture's archive using the updater's verifier.

Non-PR workflow runs attest the archives with GitHub build provenance. For a
`vVERSION` tag matching the workspace version, after both host builds and the
existing Linux installation, load and Windows acceptance jobs succeed, the
workflow verifies each attestation and creates a GitHub release containing both
host archives and a combined `SHA256SUMS` asset. It refuses to overwrite an
existing release. Dispatch and PR runs retain CI artifacts without publishing a
release; PR runs do not request an attestation. A workflow artifact alone is not
a published updater release.

On a host with the updater installed, run `agentc-update --check` before applying
a new release. For an available update, it downloads into temporary storage and
verifies the release checksum, GitHub build attestation and internal manifest,
then reports `would-update` with `verified: true`. It does not stage a release,
drain the supervisor or replace installed files. The host must have `gh` and
the authentication needed by `gh attestation verify`. No published release, or
no bundle for the host architecture, reports `no-release` without an update.
The normal canary and rollback requirements still apply to installation.
