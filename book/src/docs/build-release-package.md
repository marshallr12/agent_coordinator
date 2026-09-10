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

Install the pinned toolchain and build only the two packaged crates with locked
dependencies:

```sh
rustup toolchain install 1.98.1 --profile minimal
rustup default 1.98.1
cargo build --release --locked -p coordinator-server -p coordinator-cli
```

Package the binaries with the repository's Markdown guidance and systemd files:

```sh
version="$(python3 -c "import tomllib; print(tomllib.load(open('Cargo.toml','rb'))['workspace']['package']['version'])")"
python3 scripts/package_release.py --platform linux-x86_64 --version "$version" \
  --server target/release/agent-coordinator-server \
  --cli target/release/agent-coordinator \
  --source-date-epoch "$(git show -s --format=%ct HEAD)" \
  --output-dir dist
```

The script prints the archive path, checksum file, and SHA-256. A workstation
build has not passed the release workflow's systemd/HTTPS exercise. Treat it as
suitable for a staging instance, or run that workflow on the same commit before
calling the instance production.

## Copy the package to the host

Both cloud chapters copy `dist/agent-coordinator-VERSION-linux-x86_64.tar.gz`
and its `.sha256` to the virtual machine, then verify and extract there exactly
as [Linux release installation](linux-installation.md#verify-and-inspect-the-archive)
describes. Never extract an unverified archive with elevated privileges.
