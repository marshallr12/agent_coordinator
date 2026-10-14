# Portable supervisor hosts — design

Status: **DESIGN, awaiting owner sign-off** (coordinator task `c2aea126`, owner goal U37, 2026-10-10).
Written 2026-10-14 against `main` at `e009102`. **Design only: no runtime change is made or implied
until the owner answers §8 and files the §9 tasks.** Builds on `multi-project-design.md` (task
`cbb05ecb`, U36), the sysvinit/cron/quiet-hours work already on `main` (task `6cc3f673`), and the
planned WSL2 task `2a387d02`, which this document re-scopes (§1.3, §9 PH-7).

Goal: a supervisor should run on any workstation that has Codex or Claude available — Linux with or
without systemd, Windows, macOS — not only on a dedicated Linux VM, **without weakening the rule that
an unattended launch never uses the owner's own login, home or credentials.** A harness being
installed is not sufficient.

Conventions. **PH-n** is a proposed implementation task (§9). **PD-n** is a user-owned decision (§8).
**P1…P12** are the backend-neutral containment properties of §2. Two kinds of evidence are marked:

- **[code]** — read from this repository at `e009102`, cited `file:line`. These statements were
  checked against the code, not against plans.
- **[claim]** — a statement about a product outside this repository (WSL2, Podman, Docker Desktop,
  launchd, Windows Task Scheduler, …). None could be run in the sandbox that wrote this document, so
  each is a hypothesis the named spike (§9) must confirm before the design relies on it. Where a
  recommendation depends on a [claim], the dependency is stated.

"Host owner" is whoever controls the machine's administrator account; "project owner" owns the
repository and its GitHub settings. On a workstation both are normally the same person, who is also the
interactive user of the machine. That last role is new and is why §5 exists.

## 0. What the code says (corrections to the premises)

1. **The safety guarantee is seven mechanisms composed, and most are Linux-only.** [code]

   | Mechanism | Where | Needs |
   |---|---|---|
   | Dedicated role accounts `agentc-impl` / `agentc-rev`; a launch refuses to start under any other uid | `preflight.rs:95` (`account_problem`), `profile.rs:92` (`Role::user`) | root to create accounts and to switch to them |
   | Firewall that rejects everything except loopback ports for **those two uids only** | `host-setup.sh:1537` (`install_firewall`: `meta skuid { agentc-impl, agentc-rev }`), proven live by `preflight.rs:46` → `network_probe.rs` | nftables, root |
   | Allowlisting CONNECT proxy; refuses non-public targets | `egress.rs:3`, `egress.rs:108` | none beyond a loopback port |
   | **Claude only:** Bubblewrap with read-only `/`, user/PID/IPC/UTS/net namespaces, `--cap-drop ALL`, other runs hidden by tmpfs, per-launch Unix-socket relay into the empty network namespace | `sandbox.rs:19-50` (flags), `sandbox.rs:123` (`wrap`), `relay.rs:1-13` | bwrap, unprivileged userns |
   | **Codex is not wrapped at all**: `wrap` returns the command unchanged for any harness but Claude; Codex relies on the uid, the firewall and its own `workspace-write` sandbox | `sandbox.rs:127`, `profile.rs:207` | — |
   | Root-owned, pinned, non-agent-writable harness binaries; the harness may not update itself | `preflight.rs:109` (`binary_problems`), `profile.rs:245` (`DISABLE_AUTOUPDATER`) | root-owned prefix |
   | Reviewer candidate code runs in an inner Bubblewrap with the token hidden; **Codex reviewers are refused** because Codex has no shell-prefix hook | `candidate.rs:1-18`, `candidate.rs:65-67` | nested userns |

   Process lifetime is a further Linux-only piece: `reaper.rs` is `cfg(linux)` throughout.

2. **The supervisor loop does not compile on a non-Linux host.** `run_loop::live`, `live_health`,
   `live_review` and `rooted` are `#[cfg(target_os = "linux")]` (`run_loop.rs:19-27`), `launch-root` and
   `run` bail with "requires Linux" elsewhere (`main.rs:270-275`, `main.rs:286-290`), and `libc` is a
   Linux-only dependency (`crates/supervisor/Cargo.toml`). The Windows CI job builds the **client**
   only (`ci.yml:72`, `release.yml:100`), and the update bundle is named
   `agentc-host-<v>-linux-<arch>.tar.gz` (`agentc-update.py:376`). "Portable" therefore means a new
   backend implementation behind a seam, not flipping a flag.

3. **The loop runs as root and its whole model is uid switching.** `live.rs:1-10`: it runs as root,
   runs every step touching a role's files *as* the role account, and reads root-owned role directories
   through `rooted`. A rootless container host has no root and no second uid; the boundary is the
   container, not the account. The seam must sit above "switch uid", not below it (§1.1).

4. **The suite tests writes to the owner's home, not reads.** `containment-suite.sh:103-108`
   (`check_writes`) touches `~owner/.agentc-escape`; nothing tries to *read* the owner's `~/.claude`,
   `~/.codex`, `~/.ssh`, `~/.config/gh` or browser profiles. On a dedicated VM the owner's home is empty;
   on a workstation it is the most valuable thing on the machine, and a home directory that is `0755`
   (the Debian default is `0755`, Ubuntu 22.04+ `0750`) exposes anything inside it that is
   group/world-readable. The suite needs read probes before any workstation may pass it (P1, §2).

5. **Host identity does not exist.** The only identity is `workstation_id`, which the client defaults to
   `$HOSTNAME`, then `$COMPUTERNAME`, then the literal `unknown-workstation`
   (`crates/cli/src/main.rs:4309`). The checkout registered for *this very task* carries
   `unknown-workstation`. The supervisor asserts `distinct_launch` for every review
   (`review.rs:20`, `live_review.rs:558`) and the service stores, but does **not** enforce,
   `distinct_host` / `distinct_vendor` (`api-contract.md:692`, migration `0029_review_independence.sql`).
   Mixed backends cannot be told apart today (§7).

6. **Quiet hours exist but are a Linux cron shim; nothing else in §5 exists.** `quiet-hours.sh` holds
   `<state>/kill-switch` (marker `quiet-hours`) outside a window and is driven by `/etc/cron.d`
   (`host-setup.sh:1470`, `quiet-hours.sh:1-66`). There is **no** idle detection, no power-state check,
   and no CPU or memory cap on a launch (a search for `nice`, `rlimit`, `cgroup`, `MemoryMax` finds only
   the integrator's unit, `integrator-host-setup.sh:106`). Disk has `min_free_mib` (default 20 GiB,
   `run_loop.rs:65,108,397`) and run retention.

7. **The pinned harnesses are copied from the owner's own installation.**
   `host-setup.sh:354-355` installs `readlink -f $OWNER_HOME/.local/bin/{claude,codex}` into
   `/opt/agentc/bin`. The *binary* is then root-owned and checked against `[pinned]`
   (`config.rs` `Pinned`, `preflight.rs:109-136`), but its provenance is "whatever the owner last
   updated". The update bundle can already carry harness binaries (`harness.json`,
   `agentc-update.py:461`), so a better source exists (§4).

8. **Project-specific defaults are baked into the host installer.** `REPO_URL`, `PUSH_APP_ID`,
   `PUSH_INSTALLATION_ID` and `EXTRA_EGRESS=agents.sithbit.com` default to this project's values
   (`host-setup.sh:44,47,50-51`). A workstation joining another coordinator must override them or
   inherit the wrong ones. (`multi-project-design.md` §0/§9 reaches the same conclusion for projects.)

9. **The service manager is spelled three times.** The same job exists as a systemd unit
   (`install_run_unit`, `host-setup.sh:1567`; timers `attention_*_timer` etc.), as an LSB script
   (`run_sysv_script`/`sysv_script`, `host-setup.sh:1609-1717`, wrapper `agentc-run-sysv.sh`) and as a
   cron line (`cron_entries`, `host-setup.sh:1409`, runner `agentc-cron.sh`). The updater is
   systemd-only (`agentc-update.py:510-568` calls `systemctl`).

10. **Drain is signal-based.** SIGTERM/SIGINT set `STOP` (`live.rs:30`), the loop stops claiming,
    waits `drain_seconds` (30), then releases (`run_loop.rs` `RunConfig`). systemd gives it
    `TimeoutStopSec=120` with `KillMode=mixed` (`host-setup.sh:1567-1590`); the sysv wrapper forwards
    SIGTERM (`agentc-run-sysv.sh:17-21`). Windows has no SIGTERM for a console process, so the drain
    trigger must also exist as a file (§3.3).

## 1. Containment backend abstraction

### 1.1 Where the seam goes

Most of the containment code already has the shape *"take a `LaunchCommand`, return a wrapped
`LaunchCommand`"* (`sandbox::wrap`, `sandbox.rs:123`; `relay::namespace_args`, `relay.rs:250`). What is
**not** of that shape, and must move behind the seam, is: account creation and switching
(`push_helper::launch_root`, `push_helper.rs:68`; `rooted`), the firewall (an external service), the
reaper, and the checks in `preflight::check` (`preflight.rs:18`). The proposal is a `backend` module with
one implementation per backend and a plain data type for what each can promise:

```rust
// crates/supervisor/src/backend.rs  (proposed; names illustrative)
pub trait Backend {
    fn id(&self) -> BackendId;                    // "linux-native", "wsl2", "container", ...
    fn capabilities(&self) -> Capabilities;       // what this backend can honestly provide
    fn preflight(&self, spec: &LaunchSpec, cfg: &Config) -> Vec<String>; // today preflight::check, split
    fn prepare(&self, spec: &LaunchSpec, cfg: &Config) -> Result<()>;     // run dir, clone ownership
    fn wrap(&self, cmd: LaunchCommand, spec: &LaunchSpec, cfg: &Config) -> Result<LaunchCommand>; // sandbox::wrap
    fn start_push_helper(&self, ...) -> Result<PushHelper>;               // launch_root's second half
    fn end_launch(&self, spec: &LaunchSpec) -> Result<Leftovers>;         // reaper / container rm
    fn host_assertions(&self) -> Vec<Assertion>;  // backend-specific checks mapped to P1..P12 (§2)
}
pub struct Capabilities {
    pub harnesses: Vec<Harness>,          // Codex wrapped? see below
    pub roles: Vec<Role>,                 // reviewer only with candidate isolation (candidate.rs)
    pub per_project_egress: Enforcement,  // Enforced | Advisory   (multi-project §3.3)
    pub project_setup: bool,              // setup.rs needs the sandbox
    pub resource_caps: ResourceCaps,      // which of cpu/mem/pids/disk the backend can enforce
    pub needs_privilege: Privilege,       // Root | Rootless | Admin
}
```

Two properties of this design are deliberate:

- **The loop asks `capabilities()`, it does not branch on the OS.** A backend that cannot isolate
  candidate code (Codex today, the container backend's first version) simply does not list
  `Role::Reviewer`; `run` never launches a review there. This is the existing rule in
  `candidate.rs:65` generalised from "Codex" to "any backend/harness pair without isolation".
- **`linux-native` is the extraction of existing code with a golden test, not a rewrite.**
  `agentc-supervisor launch --dry-run` (`main.rs:232`) already prints the exact command; PH-2's acceptance
  test is byte-equality of that output for both roles and both harnesses before and after the move.

Two backend *families* result:

| Family | Boundary | Needs | Members |
|---|---|---|---|
| **Host-account** | role uids + nftables + bwrap on a real Linux kernel | root | `linux-native`, `wsl2`, `vm` (§1.2 B2, B4) |
| **Jail-per-launch** | one container (or micro-VM) per launch; no agent uid on the host | rootless engine | `container` (B3); native sandboxes (B5/B6) if ever built |

### 1.2 Candidates

Each candidate is assessed against the containment suite properties of §2 (the existing suite's
checks mapped onto P1…P12). Verdicts: **pass** (equivalent to today), **adapt** (needs a different
mechanism, same property), **gap** (property not provided; capability withheld), **fail** (cannot
provide it).

#### B1 `linux-native` (today)

All properties by construction — the suite was written against it. Works under systemd or LSB
(`6cc3f673`, done). Constraints that already exist and carry over: unprivileged userns must be allowed
(Ubuntu 24.04's AppArmor restriction needs `APPARMOR_BWRAP=1`, `host-setup.sh:372`), `close_range`
support, nftables. **Recommendation: keep as the reference backend and the only one that serves
reviewers until B3's phase 2.**

#### B2 `wsl2` — the same code inside a WSL2 distro

Runs `linux-native` unchanged in a dedicated distro (Debian/Ubuntu with systemd). Task `2a387d02` is
this backend. Assessment against the suite:

| Property | Verdict | Why / what must be configured |
|---|---|---|
| P1 owner isolation | **adapt** | WSL2 mounts the Windows drives under `/mnt/c` by default [claim], giving every Linux uid the owner's Windows files. Requires `[automount] enabled=false` and `[interop] enabled=false, appendWindowsPath=false` in the distro's `wsl.conf` [claim]; the suite must probe `/mnt/c` absence and that a Windows `.exe` cannot be started. This is the single most important WSL2 fact to verify. |
| P2/P3 writes, roles | pass | Linux semantics inside the distro's ext4 VHDX. Clones on the VHDX, never on `/mnt` (permissions and bwrap behave differently on drvfs). |
| P4 network | **adapt** | The agent chain accepts only `oifname "lo"` and rejects the rest (`host-setup.sh:1537`), so in NAT mode the Windows host and gateway (reached over `eth0`) are already refused for the agent uids. *Mirrored* networking shares the Windows loopback [claim], and that chain also accepts the ephemeral loopback range (`32768-60999`), so a launch could reach Windows-side loopback listeners. Use NAT mode, and make the suite probe the gateway address and a Windows-side listener (§2.1 P4). |
| P5 secrets, P6, P7, P9, P11 | pass | In-distro, unchanged. |
| P8 lifetime | **adapt** | The distro is stopped when idle or on `wsl --shutdown` [claim]; a Scheduled Task must keep it alive and send the drain first (§3.3). |
| P10 resources | **adapt** | `.wslconfig` `memory`, `processors`, `swap` cap the whole VM [claim]; per-launch caps then use the Linux mechanisms of §5.4 inside it. |
| P12 persistence | pass | Nothing in the distro is executed by the Windows user unless interop is on. |

Kernel features the suite needs (userns, nftables, netns) are normally present in Microsoft's WSL2
kernel [claim]; PH-7's first step is running the *existing* suite and recording the result, exactly as
`2a387d02` says, and stopping with options if any check fails.

**Recommendation: adopt as the Windows backend (PH-7), with the three extra `wsl.conf` requirements
above promoted to suite assertions.** Cost: a Linux VM resident on the owner's machine.

#### B3 `container` — rootless Podman / Docker, network limited to the proxy

One container per launch from a digest-pinned image carrying the harness, the toolchain profile and the
supervisor's relay. Sketch (Linux, Podman):

```
podman run --rm --read-only --network none --cap-drop ALL --security-opt no-new-privileges \
  --pids-limit N --memory M --cpus C --user 65532 \
  --tmpfs /tmp:rw,size=… \
  -v $RUN:$RUN:rw  -v $CLONE:$CLONE:rw|ro  -v $STATE/<role>/claude-config:…:ro \
  --env-file <profile env, no secrets>  --secret/--env <role token, harness only> \
  IMAGE@sha256:… agentc-supervisor netns-relay --relay 127.0.0.1:3128=$RUN/net/proxy.sock -- <harness …>
```

The important observation is that **the existing relay design is exactly what a container needs.**
`relay.rs:12` states the principle ("pathname Unix sockets cross network namespaces; nothing else
does"): a launch with `--network none` and a mounted socket directory can reach only what the host half
of the relay forwards to. No host firewall, no uid rule and no `--unshare-net` are needed — the engine's
`none` network replaces them, and the supervisor's own `netns-relay` is the in-container half unchanged.

Assessment:

| Property | Verdict | Notes |
|---|---|---|
| P1 owner isolation | **pass** | Only the run directory, clone and seeds are mounted. Nothing else of the host is visible; strictly stronger than "ro-bind `/` and mask" (`sandbox.rs:123-170`) because absent is safer than masked. Never mount the engine socket (a socket mounted in is root on rootful Docker). |
| P2/P3 | **pass** | `--read-only` root, read-only seed mounts replace `sandbox.rs`'s `--ro-bind`s one for one; other runs are simply not mounted (replaces `hide_other_runs`, `sandbox.rs:111`). |
| P4 network | **pass on Linux**, **adapt on VM engines** | `--network none` + socket relay on Linux. On Docker Desktop / Podman machine (macOS, Windows) host Unix sockets do not cross the VM boundary [claim]; use a per-launch `--internal` network with a proxy sidecar container attached to both it and a normal network. Same suite, different mechanism; the suite's direct-egress, DNS, LAN and metadata-address probes (§2) are what establish that the internal network really has no route. |
| P5 secrets | **pass** | The role token enters as an env value for the harness process only (as `launch.rs:124` does), not in the image or `docker inspect`-visible command line (use the engine's secret/`--env-file` mechanisms, and verify `inspect` output in the suite). |
| P6/P7 | pass | Clone hardening is in the clone (`clone.rs:21`), independent of backend. |
| P8 lifetime | **pass, better** | Removing the container kills every descendant; replaces `reaper.rs`. |
| P9 binaries | **pass** | The harness lives in a digest-pinned image; the image is the pin. `DISABLE_AUTOUPDATER` stays. |
| P10 resources | **pass** | `--memory --cpus --pids-limit` are first-class; disk needs a size-limited tmpfs/volume. |
| P11 escalation | **pass** | Rootless engine, `no-new-privileges`, no caps; the implementer's "no nested userns" (`--assert-userns-disabled`, `sandbox.rs:22`) is the container's default seccomp/userns posture — to be asserted, not assumed. |
| P12 persistence | **pass** | The container filesystem is discarded. |
| Reviewer candidate isolation (`candidate.rs`) | **gap** | The inner sandbox needs a user namespace inside the container, which a default rootless container's seccomp profile forbids. Relaxing that would weaken P11. Phase 1: **implementer-only** (`roles = [Implementer]`). Phase 2 (PH-13): the candidate shell asks the supervisor (over a relay socket) to run each command in a *sibling* container with no token and a read-only clone — Linux only, same VM-boundary caveat as above. |
| Codex | **pass, and improves today's Codex** | Codex has no wrapper today (§0.1); inside a container it gets the same boundary as Claude. Codex reviewers remain refused (`candidate.rs:65`). |
| Project `setup` (`setup.rs`) | adapt | `setup` runs in the same jail with only declared caches writable; the container equivalent is a first container with those mounts. |

Blast radius if the engine or kernel is escaped: the account the supervisor runs as (there is no
separate agent uid on the host). That is the main cost of this backend and drives PD-3 (a dedicated OS
account) and the attestation field `supervisor_account` (§2.3).

Engine choice (PD-2): **rootless Podman** is the recommended primary [claim: daemonless, user-namespace
by default]. Docker is acceptable only where the daemon is itself isolated from the owner — rootless
Docker, or Docker Desktop's VM. A rootful Docker daemon on a bare Linux host is **refused**: access to its
socket is root-equivalent and the supervisor's user would hold it. The backend probes `podman info` /
`docker info` for "rootless" or a VM-backed engine and fails closed otherwise.

**Recommendation: build it second (PH-9, PH-10); Linux/Podman first.** It is the only candidate that
gives Linux hosts without bwrap/userns support, macOS and Windows one design, and it improves Codex.

#### B4 `vm` — the linux-native backend inside a VM the installer provisions

Generalises B2: a Linux guest (Lima/Colima on macOS, Multipass or Hyper-V on Windows, any hypervisor on
Linux) in which `host-setup.sh` runs as it does on a dedicated VM. No supervisor code changes — only
provisioning (guest image, shared-nothing mounts, start/stop with the host, memory/CPU caps in the
hypervisor). All of B2's `/mnt`-style hazards apply to any shared-folder feature: **no host directory is
shared into the guest** is the requirement, and a P1 probe enforces it. For macOS this is the cheapest
route [claim: Lima is installable without admin, runs Debian with systemd]. **Recommendation: use for
macOS first (PH-8)**, while B3 matures.

#### B5 native Windows sandbox — evaluated, **deferred**

Candidates: restricted token + Job object + AppContainer; Windows Sandbox; Hyper-V isolated containers.
Obstacles [claim]: no per-launch network namespace, so P4 depends on per-SID Windows Filtering Platform
rules plus a loopback exemption mechanism whose behaviour differs by Windows build; Windows Sandbox is a
single interactive disposable VM (Pro/Enterprise only) and cannot run unattended; NTFS ACLs would replace
every mode-bit check in `confine.rs`/`preflight.rs` (`protected_executable`, `protection_facts`,
`preflight.rs:397`), i.e. rewriting the security-critical file checks with no existing test oracle.
WSL2 (B2) delivers the same user outcome with the code that already has tests. **Re-evaluate only if
WSL2 proves unacceptable to owners (PD-12).**

#### B6 native macOS sandbox — evaluated, **deferred**

Candidates: dedicated standard users + `pf` rules keyed on user (the analogue of the nft uid rule) +
`launchd`; Seatbelt (`sandbox-exec`). Obstacles [claim]: `sandbox-exec` is deprecated and undocumented;
a uid-only boundary has no read-only root, no PID namespace and a shared `/tmp`, so P2/P3/P8 would be
weaker than today's Claude launch; no equivalent of the per-launch network namespace. Not recommended;
B4 (Lima) or B3 (Docker Desktop/Podman machine) give stronger properties with less new code.

### 1.3 Summary and recommendation

| Backend | Windows | macOS | Linux w/ systemd | Linux w/o systemd | Reviewers | Codex wrapped | Code change |
|---|---|---|---|---|---|---|---|
| B1 linux-native | — | — | yes | yes (`6cc3f673`) | yes | uid+fw only | none |
| B2 wsl2 | **yes** | — | — | — | yes | uid+fw only | provisioning (+Task Scheduler) |
| B3 container | later | later | **yes** | yes | phase 2 | **yes** | new backend |
| B4 vm | possible | **yes** | — | — | yes | uid+fw only | provisioning |
| B5/B6 native | deferred | deferred | — | — | — | — | — |

Order: **B1 (exists) → B2 and B4 (no supervisor code, in parallel with the seam) → B3 Linux → B3 on
VM-backed engines → candidate-shell sibling containers.** Task `2a387d02` is superseded by PH-7 and
should be edited, not left to compete.

## 2. One containment suite, and how a host proves it

### 2.1 Properties, not scripts

The current suite is one 814-line bash script run by root on a Linux host (`containment-suite.sh:779`
`main`); it cannot run on a backend without `sudo -u agentc-impl`. The contract becomes twelve
properties. Each existing check maps onto one; **bold** items are new.

| # | Property: a launch can never… | Existing checks (`containment-suite.sh`) | New |
|---|---|---|---|
| P1 | …act as, read, or write the **owner**: home, owner's harness logins (`~/.claude`, `~/.codex`), `~/.ssh`, `~/.config/gh`, browser profiles, Windows `/mnt/c`, shared folders | `check_writes:103` (write only) | **read probes of each of those paths seeded with a canary file; `/mnt/c` and shared-folder absence** |
| P2 | …write anywhere but its clone (implementer) and its own run directories; seeds, binaries, mirror immutable | `check_writes`, `check_seeds:117`, `check_claude_sandbox:152` | |
| P3 | …see or touch the other role's clones, runs or state | `check_writes:103-112` | |
| P4 | …reach any network endpoint except the allowlisting proxy: direct HTTPS, DNS, host loopback services, LAN, link-local/cloud-metadata fail; the proxy refuses non-allowlisted names | `check_egress:133`, `check_relay_probe:320`, `check_claude_launch:282` | **LAN, `169.254.169.254`, host-gateway and host-loopback probes** |
| P5 | …expose a role credential beyond the harness process: reviewer candidate commands cannot read the token or env; push App key unreadable | `check_claude_token:664`, `check_candidate_token:682`, `check_push_key:690` | **`inspect`/`ps` visibility probe (container backends)** |
| P6 | …load candidate-controlled instructions (hooks, CLAUDE.md, AGENTS.md) | `check_git:91`, `check_profiles:75` | |
| P7 | …push except through the secret-scanned helper | `check_git:91-100`, `check_push_launch:606` | |
| P8 | …leave a process running after the launch, and a drain stops claiming and releases | `check_push_kill:560` | **drain-file and stop-while-running test (§3.3)** |
| P9 | …run an unpinned or agent-writable harness, or update itself | `check_profiles:75` (pinned), preflight | **sha256 equals the manifest (not only `--version`)** |
| P10 | …exceed its resource caps | — | **fork-bomb, memory and disk-fill probes against the configured caps** |
| P11 | …escalate: setuid, capabilities, nested userns (implementer), engine/hypervisor socket, `sudo` | `check_claude_sandbox` (`sandbox.rs:22` assertion), `role_settings.rs:20` | **engine-socket/`sudo` absence** |
| P12 | …leave anything the owner will execute later: shell rc files, PATH dirs, autostart folders, cron, LaunchAgents, Scheduled Tasks | — | **post-launch tree diff of owner-executed locations** |

The existing suite additionally checks the browser (`check_browser:741`), AppArmor bwrap
(`check_apparmor_bwrap:628`), the attention token and the project's own tests (`check_cargo_test:767`);
these stay as optional "feature" checks, not properties.

### 2.2 Structure: probes inside the jail, assertions outside

To get "one suite" on backends with no `sudo -u`, split the work:

- **Jail-internal probes** — a hidden `agentc-supervisor mock-harness` subcommand (the Rust
  successor of `write_mock_claude`, `containment-suite.sh:216`) that attempts each violation of P1–P5,
  P9–P11 from *inside*, and prints JSON. It is run through the real `Backend::wrap` path, with the real
  environment from `profile::environment` (`profile.rs:227`), so the thing tested is the thing launched.
  The same binary and the same probe list run on every backend.
- **Host-side assertions** — each backend's `host_assertions()` lists the checks only it can make
  (accounts and nft table; `wsl.conf` flags; container engine rootless; image digest; Lima mounts), each
  tagged with the property it supports. A backend is incomplete if any property has no assertion *and*
  no probe.
- The bash suite remains the implementation for `linux-native`/`wsl2` host assertions until ported;
  `agentc-supervisor selftest` calls it and merges its output. Nothing is rewritten up front (PH-1, PH-3).

### 2.3 Proof: the attestation record

`agentc-supervisor selftest` emits `suite-report.json`:

```json
{ "suite_version": 3, "host_id": "<uuid>", "machine_id_hash": "<sha256>",
  "backend": "container", "backend_version": "podman 5.x rootless", "os": "...", "kernel": "...",
  "supervisor_sha256": "...", "harness": {"claude": {"version": "...", "sha256": "..."}, "codex": {...}},
  "image_digest": "sha256:...", "supervisor_account": "agentc | owner",
  "roles": ["implementer"], "properties": {"P1": "pass", "P4": "pass", "P10": "skipped: no cgroup"},
  "passed_at": "...", "expires_at": "...", "report_sha256": "..." }
```

A property that is `skipped` or `fail` removes the dependent capability (a host whose P10 is skipped
cannot claim a "workstation" profile; whose reviewer-candidate probe is absent is implementer-only). Rules:

1. **Self-gating (Stage 1, no server change).** `run` refuses to claim unless a report for the current
   `(supervisor_sha256, harness sha256s, image digest, backend_version, kernel, protected-config hash)`
   passed within `suite_ttl` (default 7 days). Any change to those inputs invalidates it; the updater
   (`agentc-update.py:624` `run_suite`) already runs the suite as a promotion gate and keeps doing so.
2. **Published.** The heartbeat (`run_loop.rs:620` `beat`) gains `attestation:{backend, suite_version,
   passed_at, report_sha256}` so the existing `attention.py` heartbeat probe
   (`attention.py:245`) can page on an expired or missing attestation; the full report is registered as a
   coordinator artifact (`agent-coordinator artifacts` exists) beside the host's canary evidence.
3. **Registered (Stage 2, needs server work, PD-4/PD-11).** A host registry
   (`host_id`, `machine_id_hash`, backend, attestation digest, expiry) written by a host-enrollment
   credential; a project may require an unexpired attestation of a minimum suite version to claim.

An attestation is the host owner vouching for their own machine. It proves the suite passed on that
configuration, not that the owner is honest; it is sufficient under the existing trust model (the host owner
is trusted, `multi-project-design.md` §1.2/§4) and is stated that way in the docs.

## 3. Service-manager abstraction

### 3.1 One job table

Today's jobs (all in `host-setup.sh`): `agentc-firewall` (oneshot, ordered before the rest),
`agentc-egress` (daemon, own uid), `agentc-run` (daemon; restart on failure, 30 s delay; drain),
`agentc-canary` (every 10 min), `agentc-digest` (daily), `agentc-e2e-canary@<harness>` (daily),
`agentc-update` (daily, systemd-only) and the quiet-hours shim (every minute). A `ServiceManager` renders
one in-code table:

```
Job { name, kind: Daemon | Timer | Oneshot, command, env_files, run_as,
      after: [job], restart: Never | OnFailure{delay}, stop_grace, schedule: Every(d) | Daily(local_time) | Calendar(..),
      log }
```

into the host's manager: `systemd` (units + `.timer`), `lsb` (script + `agentc-run-sysv`, the cron
runner for timers), `launchd` (LaunchDaemon plist; `StartInterval`/`StartCalendarInterval` for timers,
`KeepAlive` for the loop) [claim], `windows` (Scheduled Task: trigger at boot/logon, "restart on
failure", run whether or not the user is logged on; timers as repetition triggers) [claim], and
`inprocess` for the container backend on a bare user account (a user-level systemd unit or
launchd/Task entry started by the installer, i.e. the same renderers with a user scope).

### 3.2 Where it lives

Render logic goes in the supervisor binary (`agentc-supervisor host service render|install|status|
start|stop|drain`) so it runs on every OS, with **golden tests proving byte-identical output to what
`host-setup.sh` writes today** for systemd, LSB and cron. `host-setup.sh` then calls the binary for
service installation instead of its three heredocs; it is not rewritten wholesale (PH-5). The cron
schedule translation (`cron_schedule`, `host-setup.sh:1365-1400`, with its refusal of schedules cron
cannot express) moves into the cron renderer with its tests (`host-setup-test.py`).

Timer jobs are Python scripts (`attention.py`, `e2e-canary.py`, `agentc-update.py`; `tomllib` makes
them need Python ≥ 3.11). Porting them is out of scope: they run **where Python 3.11 is already a
dependency** — inside the Linux guest for B1/B2/B4, and for B3 on a bare host either via an image
carrying Python or by requiring Python on the host (PD-10). Native Windows/macOS hosts without Python are
not supported in this design.

### 3.3 Drain without signals

Add a **drain file** (`<state>/drain`) beside the kill switch (`health.rs:147`). The loop already polls
`stopping()` between steps (`stop_requested`, `live.rs:530`); it additionally treats the file as "stop claiming,
wait `drain_seconds` for the running launch, release, exit 0". Every renderer's *stop* is
"write the drain file, wait up to `stop_grace` for the loop to exit, then force": for systemd this is
`ExecStop`; for the Windows Scheduled Task it is the task's stop action; for WSL2 the Windows-side
shutdown hook runs `wsl -d <distro> -u root -- systemctl stop agentc-run` before the VM is torn down
[claim]. The existing signal path is unchanged; the file is the portable trigger. P8's test is "write the
drain file with a launch running; the lease is released and the process exits within `stop_grace`".

## 4. Harness provisioning per backend

### 4.1 Today [code]

- Binaries: copied from the owner's `~/.local/bin` into root-owned `/opt/agentc/bin`
  (`host-setup.sh:354-355`), then required to report `[pinned]` (`preflight.rs:109-136`) and not to be
  agent-writable (`preflight.rs:138`). Self-update is off (`profile.rs:245`).
- Claude credential: a per-role long-lived token from `claude setup-token`, stored `root:<role> 0440` at
  `<state>/<role>/claude-token` (`profile.rs:68`, `host-setup.sh` `next_steps`), read by the supervisor
  and passed to the child's environment only (`launch.rs:124`, `launch.rs:151-160`); `claude-config` is
  root-owned and read-only (`sandbox.rs:75`, `preflight.rs:236`).
- Codex credential: `codex login` run as the role account with `CODEX_HOME=<state>/<role>/codex-home`
  (`profile.rs:262`, `host-setup.sh` `next_steps`). Unlike Claude's token this directory is writable by the
  role (Codex refreshes it), which is acceptable because the implementer is the only role Codex serves.
- Health: `claude auth status` / `codex login status` before use, 429 exhaustion state in
  `<state>/vendors.json`, expiry warnings (`live_health.rs:24`, `health.rs:100`).
- Updates: `agentc-update.py` can stage new harness binaries and pins from a release bundle's
  `harness.json` and promotes them only when preflight, the end-to-end canary and the suite pass
  (`agentc-update.py:20-31`, `:461`, `:711`).

### 4.2 Proposal

**One rule: the harness's identity on a supervisor host is a (version, sha256) pair from the
release bundle's manifest, never "what the owner has installed".** Per backend:

| Backend | Binary source and pin | Credentials | Update |
|---|---|---|---|
| B1/B2/B4 | From the verified release bundle (`harness.json`) into the guest's root-owned `/opt/agentc/bin`; `host-setup.sh` stops reading `$OWNER_HOME` (PH-6). `[pinned]` is written from the manifest. | As today: role token file / role `codex-home` in the guest, entered by the owner at the guest's terminal. | Existing `agentc-update` staged harness step, extended with the sha256 check (P9). |
| B3 | Baked into the image at build by version + sha256; the image is referenced by digest in config; the host never has an unpinned harness binary on its PATH. | Role token as an engine secret / `--env-file`, mode `0600`, in the supervisor's state dir; Codex login through `agentc-supervisor login --role impl --harness codex`, which runs the vendor's login in a one-off container with a named volume as `CODEX_HOME`, so no credential ever lands in `~/.codex`. | New image digest verified against the release attestation → pull → drain → suite → canary → promote, rollback to previous digest (the `agentc-update` sequence with "image digest" in place of "binary"). |

Constraints that apply to every backend:

- **Per-role logins are distinct, revocable credentials, never the owner's interactive session.**
  `claude setup-token` yields a token separate from `~/.claude`; the role's `CLAUDE_CONFIG_DIR` and
  `CODEX_HOME` replace any home-relative default (`profile.rs:259-262`) and `HOME` itself is replaced
  (`profile.rs:238`). The *vendor account* behind the tokens is the owner's choice (PD-5): subscription
  tokens as on the dedicated hosts, or — on a shared workstation where the owner's own use competes for
  the same rate limit — API keys. The vendor-exhaustion state machine (`health.rs`) already handles a
  429 by switching to the fallback vendor.
- **Credentials are files with owner-only permissions in the supervisor's state directory, not
  entries in the OS keychain.** The loop must read them unattended and non-interactively, and a keychain
  would bind them to a logged-in session on macOS/Windows [claim]. On Windows-native (deferred) they would
  need ACLs; inside WSL2 and containers they are ordinary Unix files.
- **The owner's own harness installation is never used and never touched**; the installer detects it
  only to tell the owner "yours stays as is".

## 5. Workstation courtesy

The workstation profile exists because the machine has another user: the owner. All settings have
in-code defaults; the profile is chosen once at onboarding (§6) and is a single `[courtesy]` table. The
default for a machine that is not declared dedicated is the conservative one.

### 5.1 Quiet hours (native, any OS)

Move the window into the supervisor (`[run] quiet_hours = "22:00-07:00"`, local time; already proposed
as MP-11 in `multi-project-design.md` §10 and reused here). The cron shim is retired only where the loop
can read the window itself; `quiet-hours.sh` stays for hosts that have not upgraded. The loop reports
`quiet` distinctly from an owner kill switch in the heartbeat. Rules unchanged from the shim: a running
launch is never killed by quiet hours; the daily e2e canary and updater must start inside the window
(`book/src/deploy/README.md`, quiet-hours section). DST: the window is local wall-clock; a transition
that makes an hour repeat or vanish is resolved by re-evaluating every poll, not by scheduled edges (the
shim already does this by checking every minute).

### 5.2 Idle and presence

Claim only when the owner is away. A small per-OS **presence probe** writes `<state>/presence.json`
(`{idle_seconds, on_ac, metered}` with a timestamp); the loop reads it and treats a missing or stale file
as "present" (fail safe). Probes [claim]: Linux `loginctl show-session -p IdleHint,IdleSinceHint` (or
`xprintidle`); macOS `ioreg -c IOHIDSystem` HID idle time and `pmset -g batt`; Windows
`GetLastInputInfo`, which only works from the user's interactive session, so the probe is a tiny
Scheduled Task running in the user's session, not part of the service. Inside WSL2 the guest cannot see
Windows input, so the Windows-side probe writes the file into the distro (or, equivalently, the Task
pauses the distro's loop with the drain file). Policy defaults: claim when idle ≥ 10 min **and** on AC
power **and** not metered. **A launch already running is not killed when the owner returns** (launches are
minutes to hours and an abort loses the attempt); instead it is already at low priority (§5.4), and no
new claim starts. Pausing a running launch (`SIGSTOP`/`podman pause`) is a later option, not proposed.

### 5.3 Drain on shutdown and sleep

- Shutdown/logoff: the drain file (§3.3) is the trigger; Linux hosts additionally take a systemd
  shutdown/sleep **delay inhibitor** while a launch runs so `drain_seconds` is actually available
  [claim]; macOS uses `caffeinate`-style assertions and Windows `SetThreadExecutionState`/power
  requests for the same purpose [claim].
- Sleep and resume: assume a suspended machine loses its lease. After a wall-clock vs monotonic-clock
  jump larger than the poll interval, the loop treats any running launch as suspect: it re-reads the
  attempt state before any renewal (the 409/"attempt no longer active" handling already exists,
  `live.rs:32` `CONFLICT_EXIT`, `renewal.rs`), and kills and releases a launch whose attempt has ended.
  The service rule that a receipt never implies ownership (`CONTRIBUTING.md`) already forces this; the
  change is detecting the gap. Whether to inhibit idle sleep during a launch is PD-8.

### 5.4 CPU, memory, disk and priority caps

There are none today (§0.6). Defaults for the workstation profile (a dedicated host sets none):
`cpu = 50%` of cores (min 1), `memory = 4 GiB`, `pids = 2048`, `disk` as below, background priority.

| Backend | Mechanism |
|---|---|
| linux-native, systemd | each launch started under a transient scope: `systemd-run --scope -p MemoryMax=… -p CPUQuota=… -p TasksMax=… -p CPUWeight=10 -p IOWeight=10` [claim] — the launch-root child is the natural place |
| linux-native, sysv | `nice`/`ionice` + `ulimit`/`prlimit` (`RLIMIT_AS`, `RLIMIT_NPROC`); no cgroup guarantee, stated in the attestation (`P10: partial`) |
| WSL2 / VM | VM-wide `memory`/`processors` in `.wslconfig` or hypervisor config [claim], then the Linux row inside |
| container | `--memory --cpus --pids-limit` (+ `--cpu-shares`) |
| macOS (B4 guest) | hypervisor caps; `taskpolicy -b` for any host-side process |

Disk: keep `min_free_mib` (`run_loop.rs:397`) and raise the workstation default's *retention* strictness
(today five terminal runs are kept, `confine.rs:161` `prune_terminal`; a Rust `target/` per launch is large,
`profile.rs:241`) to one, with the run directory deleted on terminal state, plus a new `max_state_gib`
check before each claim over `runs/ clones/ caches/`. A fixed-size VHDX/volume is the only hard disk cap
and is the recommended arrangement for the VM backends [claim].

## 6. Onboarding: one command

### 6.1 What it costs today

`host-setup.sh` takes `SUPERVISOR=`, `CLI=`, `PUSH=` paths to pre-built binaries, about fifteen environment
variables whose defaults are this project's (`host-setup.sh:3-4, 40-70`), and prints a list of manual steps
(`next_steps`, `host-setup.sh:1896`): two harness logins per role, coordinator credentials saved by hand,
a push App key, an attention token, an e2e token, then the containment suite and an opt-in command. The
file is 1,988 lines of bash that only runs on Linux with root.

### 6.2 The command

```
agentc-supervisor host install            # run inside a checkout with .agent-coordinator.toml,
                                          # or:  --coordinator https://… --project <id>  [--invite <code>]
```

Phases, each idempotent and each printing exactly what it will do before doing it (`--plan` stops there,
and prints the administrator steps as a script for the owner to review):

1. **Detect** — OS, init system, hypervisor/WSL, container engines and whether each is rootless, whether
   userns/nftables/bwrap work, Python version, free disk, the presence of `claude`/`codex` (information
   only, §4.2), battery/desktop session (picks the courtesy default).
2. **Choose** — pick the strongest backend whose `capabilities()` and host assertions can pass, in the
   order B1 → B3 → B2/B4; refuse with the specific failing property if none can. `--backend` overrides
   only downward.
3. **Provision** — fetch the verified release bundle (or image digest), install pinned harnesses (§4.2),
   create accounts/volumes/distro, render the jobs (§3), write `supervisor.toml` **only** for values that
   differ from in-code defaults.
4. **Credentials** — role harness logins guided on the owner's terminal (§4.2); coordinator credentials by
   **enrollment**: the owner mints a one-time invite in the dashboard, the host exchanges it for the
   supervised principal pair and its `host_id` (replaces "issue in the dashboard and save
   `credentials.toml` by hand", `host-setup.sh` `next_steps`; needs PH-12, until then the manual step
   remains and the command prints it).
5. **Attest** — run the suite (§2); stop on any failing property.
6. **Shadow** — the existing read-only would-launch mode (`shadow.rs`) for a configurable number of polls.
7. **Enable** — never automatic. `agentc-supervisor host enable` is the owner's explicit opt-in, as
   "installed but not enabled" is today (`host-setup.sh:1932-1937` `run_opt_in`).

`host install` replaces project-specific shell defaults (§0.8): the repository URL, mirror branch and
egress extras come from the coordinator's own project record (`multi-project-design.md` §1.3 already
specifies this), and the push App ids from `push.toml`, which the project owner supplies, not from baked
constants. It does not replace `multi-project-design.md`'s `project add`; `host install` makes a host,
`project add` attaches projects to it (MP-7).

### 6.3 Per-OS in-code defaults

Paths are the only per-OS default needed; everything else already defaults in `Config`
(`config.rs:112`, "a host runs with no configuration file at all").

| | binaries | state | config |
|---|---|---|---|
| Linux, root backends | `/opt/agentc` | `/var/lib/agentc` | `/etc/agentc` (today) |
| Linux, rootless container | `~/.local/share/agentc/bin` | `~/.local/state/agentc` (XDG) | `~/.config/agentc` |
| macOS (Lima/VM) | inside the guest: as Linux root backends | guest | guest |
| Windows (WSL2) | inside the distro: as Linux root backends | distro | distro |
| macOS/Windows container (VM engine) | `~/Library/Application Support/agentc` / `%LOCALAPPDATA%\agentc` | same root | same root |

### 6.4 What root/admin remains

The owner cannot be spared these; `--plan` lists exactly which apply.

| Backend | Administrator steps |
|---|---|
| linux-native | create the two role accounts, the egress and push accounts; root-owned `/opt`, `/etc`; nft table; services; on Ubuntu 24.04 the userns AppArmor allowance (`APPARMOR_BWRAP=1`) |
| wsl2 | enable virtualization in firmware (if off); `wsl --install` (may reboot); write `.wslconfig`; create the Scheduled Task (run whether logged on or not needs the owner's password or a service-account decision, PD-3) |
| vm (Lima etc.) | install the hypervisor tool; (LaunchDaemon instead of per-login LaunchAgent needs admin) |
| container (Linux) | install Podman via the package manager; `/etc/subuid`,`/etc/subgid` entries if absent; `loginctl enable-linger <user>` to run while logged out [claim] |
| container (macOS/Windows) | install Docker Desktop or Podman machine (Docker Desktop's licence terms are the owner's to read) |

For every backend the owner also performs, once and on their own terminal: the vendor logins and the
enrollment invite. These are never automated: a credential entering the host must be a conscious act
(`CONTRIBUTING.md`: never expose credentials; `host-setup.sh` header: "Claude tokens, Codex logins,
coordinator credentials and the push App key stay manual").

## 7. Host identity and review independence across mixed backends

### 7.1 Identities

- **`host_id`** — random UUID generated at first install, stored read-only to launches
  (`<state>/host-id`), included in every attestation and every claim. Stable across reinstall of the
  supervisor, replaced on `host reset`. Replaces `workstation_id`-from-`$HOSTNAME` for supervisor traffic.
- **`machine_id_hash`** — SHA-256(project-salt ‖ platform machine id): `/etc/machine-id`, macOS
  `IOPlatformUUID`, Windows `MachineGuid`. Salted so the coordinator never stores a raw hardware id.
  Inside WSL2/VM/container it is read from the **outer** machine where the backend can (WSL2 exposes the
  Windows host's identity only through interop, which is disabled — so the Windows-side installer passes it
  in at install and the attestation records its source). Where it cannot be determined, the field is
  `unknown` and the host counts as a distinct machine from nothing (it never *satisfies* `distinct_host`
  against another `unknown`).
- **`vendor`** — the harness's vendor (`claude` → Anthropic, `codex` → OpenAI), already recorded per
  launch in the ledger (`costs.jsonl`) and used by `health.rs`.

### 7.2 Semantics for the three existing levels

The service already accepts `distinct_launch`, `distinct_host`, `distinct_vendor`. Define them in terms
of the attempt metadata above, not the reviewer's say-so:

| Level | Means, given a submission's contributing attempts and the reviewing attempt |
|---|---|
| `distinct_launch` | different launch/session (what every supervised review asserts today, `review.rs:20`) |
| `distinct_host` | different `machine_id_hash` (not merely a different `host_id`: a WSL2 distro and a container on the same Windows PC share a failure and trust domain) |
| `distinct_vendor` | different `vendor`; implies nothing about hosts |

`distinct_vendor` is naturally produced by the mixed fleet the owner wants: Codex hosts can implement but
cannot review (`candidate.rs:65`), Claude hosts can do both, so "Codex implements, Claude reviews" is
`distinct_vendor` by construction. Same-vendor review on a second machine is `distinct_host`. Review on the
same machine by another backend is only `distinct_launch` and is reported as such.

### 7.3 Enforcement path

1. **Today (no server change):** the supervisor reports the strongest level it can *prove locally* — it
   knows its own `host_id`/`machine_id_hash`/vendor and the implementer attempt's from the coordinator
   record when published — and never asserts `distinct_host` or higher on its own say-so while the
   implementer's identity is unknown. `distinct_launch` stays the default.
2. **PH-12 (server, PD-11):** supervised principals are bound to a registered `host_id` at enrollment;
   claims carry `{host_id, machine_id_hash, backend, harness, vendor, model}` as attempt metadata; the
   service computes the level when a review is claimed with `--review-independence=…` and refuses a claim
   that overstates it (`api-contract.md:692` becomes "enforced"). A project can set a floor
   (`review_independence_floor`, the successor of `multi-project-design.md` §8's `review_vendor`).
   A host with an expired attestation (§2.3) cannot satisfy a floor above `distinct_launch`.
3. Because a registered principal maps to one host, the existing contributor rule ("principals, sessions
   and landing-range authors cannot review their own submission", `multi-project-design.md` §8) continues
   to hold with no change.

The trust note of §2.3 applies: all of this is as honest as the host owners, which is the existing
model; enforcement changes who has to be honest from "the supervisor's report" to "the registered host's
enrollment".

## 8. User-owned decisions

| # | Decision | Options | Recommendation |
|---|---|---|---|
| PD-1 | Backend scope and order | (a) B1 → B2/B4 → B3 Linux → B3 VM-engines; native sandboxes deferred. (b) B3 first, everywhere. (c) B2 only. | **(a)** (§1.3): Windows and macOS get a working host sooner with no supervisor code; the container backend then arrives with the seam already in place. |
| PD-2 | Container engine | (a) rootless Podman primary; Docker only rootless or Desktop-VM. (b) Docker everywhere. (c) refuse containers. | **(a)**; rootful Docker on bare Linux refused (§1.2 B3). |
| PD-3 | Which OS account supervises a container/VM-less host | (a) a dedicated standard account (`agentc`) created at install where the OS permits. (b) the owner's own account. | **(a)** where possible; **(b) allowed** on container/Docker-Desktop hosts only with `supervisor_account = "owner"` recorded in the attestation and a stated warning (escape lands as the owner). The Windows "run whether logged on or not" Task needs this decided per host. |
| PD-4 | Attestation depth | (a) local self-gate + heartbeat + artifact (Stage 1) only. (b) also a server host registry (Stage 2). | **(a) now, (b) as PH-12**, each separately signed off. |
| PD-5 | Vendor credentials on shared workstations | (a) subscription tokens as today. (b) API keys. (c) per-host choice. | **(c)**: subscription tokens on dedicated hosts; on a workstation whose owner also uses the vendor interactively, API keys avoid fighting for the same rate limit. The owner should confirm each vendor's terms allow unattended agents on a personal subscription before (a) is used on a workstation. |
| PD-6 | Harness source | (a) release-bundle manifest only. (b) keep copying the owner's install. | **(a)** (§4.2, PH-6). |
| PD-7 | Courtesy default | (a) the workstation profile unless the owner declares the machine dedicated (one onboarding question, default "also used interactively"). (b) off unless asked. | **(a)**: quiet hours 22:00–07:00, idle ≥ 10 min, AC only, 50% CPU, 4 GiB. A workstation silently using its owner's CPU is the failure mode to prevent. |
| PD-8 | Sleep during a launch | (a) hold an idle-sleep inhibitor while a launch runs on AC. (b) never inhibit; rely on recovery. | **(a) on AC only**; on battery the presence rule already prevents claiming. Lid-close sleep is never fought. |
| PD-9 | Drain trigger | (a) add the drain file beside signals. (b) signals only. | **(a)**; required by any Windows service manager (§3.3). |
| PD-10 | Timer scripts and Python | (a) keep Python ≥ 3.11 as a dependency; run timers where it exists (guests, images). (b) port the three scripts into the supervisor binary. | **(a)**; (b) is a separate task if native hosts are ever wanted. |
| PD-11 | Server work: host registry, attempt metadata, enrollment invite, enforced independence | (a) approve as PH-12 now. (b) defer; Stage 1 only. | **(a) approved as a design, built after the dogfood window** (see PD-14); it is additive, opt-in per project, and the only way `distinct_host` becomes real. |
| PD-12 | Native Windows/macOS sandboxes | (a) defer indefinitely. (b) spike now. | **(a)**; reopen only if owners reject running a Linux VM, with the criteria in §1.2 B5/B6. |
| PD-13 | Reviewers on the container backend | (a) implementer-only until sibling-container candidate shells exist (PH-13). (b) build PH-13 first. | **(a)**: reviews stay on linux-native/wsl2/vm hosts, which already have candidate isolation. |
| PD-14 | Timing | (a) docs and measurement spikes (PH-0, PH-1) now; supervisor/server code after the dogfood window closes (2026-10-23 13:07 Z, `multi-project-design.md` D-19) and after MP-1's config refactor. (b) interleave. | **(a)**: the dogfood measures the current loop, and `config.rs`/`run_loop.rs` are about to change under MP-1/MP-2. |
| PD-15 | WSL2 distro isolation of the owner's Windows files | (a) require `automount` and `interop` off (the supervisor distro is a dedicated, throwaway distro). (b) allow them. | **(a)**, enforced by suite assertion; (b) would defeat P1. |
| PD-16 | `host_id` / machine hash privacy | (a) salted hash, never the raw id. (b) raw. | **(a)**. |

## 9. Implementation plan

Each task is independently shippable (the repository gate green, existing hosts unchanged, docs updated
in `book/src`) and is filed as a normal coordinator task **after sign-off**. "Deps" are hard
prerequisites; sizes as in `multi-project-design.md` §12 (S ≤ 1 day of agent time, M ≈ 2–3, L > 3).
`MP-n` are that document's tasks.

| Task | Content | Deps | Size | Ship effect |
|---|---|---|---|---|
| **PH-0** WSL2 viability spike (`2a387d02` re-scoped) | Owner-run: run the *existing* suite inside a WSL2 distro with `automount`/`interop` off, NAT networking; record per-property results and the exact `wsl.conf`/`.wslconfig`; stop with options if any fails. Confirms every WSL2 [claim] of §1.2 B2 | — | S (owner time) | evidence only |
| **PH-1** Property-tagged suite + gap closure | Tag every existing check P1–P12 in the output; add P1 read probes (owner credential canaries), P4 LAN/metadata/host-loopback probes, P9 sha256, P10 resource probes (skipped with reason where unsupported), P12 tree-diff; emit `suite-report.json`; extend `host-setup-test.py` coverage. May reveal real failures on existing hosts — fix or document | — | M | stricter suite on linux-native |
| **PH-2** `Backend` seam, `linux-native` extraction | `backend` module, `Capabilities`, move `sandbox::wrap/check`, account switching, reaper and preflight pieces behind it; `launch --dry-run` byte-equality tests for both roles and harnesses; no behaviour change | MP-1 (config churn) | M | none visible |
| **PH-3** `selftest` + Stage-1 attestation | `agentc-supervisor selftest` (mock-harness probes through the real wrapper + host assertions + bash suite merge); `host-id`; `run` refuses to claim without a fresh matching report; heartbeat `attestation`; `attention.py` probe and test; updater uses it as its gate | PH-1, PH-2 | L | claiming requires a fresh pass |
| **PH-4** Courtesy core | `[courtesy]` config; drain file; native quiet hours (= MP-11); presence file + Linux probe; clock-jump suspect handling; resource caps via `systemd-run --scope` / `prlimit`; heartbeat `quiet`/`idle`; `max_state_gib` | MP-1 (and MP-11 folded in) | M | optional, defaults off for existing hosts |
| **PH-5** Service-manager renderers | Job table; systemd/LSB/cron renderers with golden tests equal to today's output; `host service …` subcommands; `host-setup.sh` calls them; updater uses `service drain/start` instead of `systemctl` | — | M | none visible |
| **PH-6** Harness provisioning from the release bundle | `host-setup.sh` and `agentc-update` install harnesses from `harness.json` only; `[pinned]` from the manifest; sha256 enforced in preflight (P9); stop reading `$OWNER_HOME` | — | S | new hosts only |
| **PH-7** WSL2 host | Windows bootstrap script (WSL enable, distro with `wsl.conf`, `.wslconfig`), Scheduled Task renderer and keep-alive, drain-on-shutdown hook, presence probe task, suite assertions for `automount`/`interop`/network mode, docs; supersedes `2a387d02` | PH-0, PH-1, PH-4, PH-5 | L | Windows hosts |
| **PH-8** VM host recipes (macOS/Lima first) | Provision-and-start recipe, no-shared-folder assertion, launchd job to start/stop (with drain) the VM, docs | PH-1, PH-4, PH-5 | M | macOS hosts |
| **PH-9** Container backend, Linux/Podman, implementer-only | Image build in `release.yml` pinned by digest; `container` `Backend`; `--network none` + relay; resource flags; push helper without a dedicated uid (supervisor-user helper reading its key from a `0600` file); suite passes P1–P12 (reviewer probes absent → implementer-only); Codex inside the jail | PH-2, PH-3, PH-6 | L | Linux hosts without bwrap/root |
| **PH-10** Non-Linux supervisor build + VM-engine variant | Gate or replace the Linux-only pieces (`run_loop::live`, `rooted`, `libc`, mode-bit checks) with portable equivalents for a user-account supervisor; internal-network + proxy sidecar P4 variant; macOS and Windows release artifacts; CI jobs; ACL checks if the host process runs on Windows | PH-9 | L | macOS/Windows container hosts |
| **PH-11** `host install` | Detect/choose/provision/attest/shadow/enable (§6.2), per-OS default paths, `--plan`, removal of project-specific defaults from `host-setup.sh` (or its wrapping) | PH-3, PH-5, PH-6, one of PH-7/PH-8/PH-9; MP-7 for the project half | L | one-command onboarding |
| **PH-12** Server: host registry, attempt metadata, enrollment invites, enforced independence, optional attestation floor per project | Migration + routes + dashboard line; enrollment replaces hand-copied `credentials.toml`; `distinct_host`/`distinct_vendor` computed server-side; additive, opt-in per project; docs in `book/src/docs` | PD-11; PH-3 for the data | L | independence becomes real |
| **PH-13** Reviewer isolation in containers | Candidate shell asks the supervisor to run each command in a sibling container (no token, read-only clone); container backend gains `Role::Reviewer` | PH-9 | M | reviewers on container hosts |
| **PH-14** Native sandbox re-evaluation | Only if PD-12 reopens: time-boxed spike with a written verdict against P1–P12 | PD-12 | S | none |

Critical paths. **Windows:** PH-1 → PH-4/PH-5 → PH-7 (PH-0 first, in parallel). **macOS (cheapest):** PH-1 →
PH-4/PH-5 → PH-8. **Universal container:** PH-2 → PH-3 → PH-9 → PH-10. **Independence:** PH-3 → PH-12.
PH-1, PH-5 and PH-6 touch only deploy scripts/updater and can start at once; PH-2, PH-3, PH-4, PH-9 touch
`config.rs`, `run_loop.rs`, `sandbox.rs`, `preflight.rs` and so are sequenced behind MP-1/MP-2 and the
dogfood window to avoid conflicting edits (PD-14).

## 10. Risks and open questions

1. **Every [claim] is untested here.** The WSL2, Podman, Docker Desktop, Lima, launchd and Task Scheduler
   statements are from general knowledge, not from running them. PH-0 and the first slice of PH-8/PH-9 are
   written to falsify them cheaply; a failed claim changes the recommendation for that backend, not the
   structure of the design.
2. **A shared kernel is a shared risk.** B1–B3 all rely on kernel isolation (namespaces/containers). The
   container backend moves the blast radius from "an unprivileged agent uid" to "the supervising user"
   (§1.2 B3); a VM backend (B2/B4) adds a hypervisor boundary and is the stronger choice for a
   high-value personal machine. The design lets the owner choose (PD-1/PD-3), and states the difference in
   the attestation.
3. **The suite can only test what its authors thought of.** P1's read probes must be seeded with canary
   files in every owner-credential location the installer knows about, but an application storing
   secrets somewhere unforeseen is not covered. The mitigation is the dedicated account (PD-3) and, for
   B2/B4, the absence of any shared folder; the suite reports what it did and did not probe.
4. **Vendor terms.** Running unattended agents from several machines on a personal subscription may be
   restricted; this is a decision for the owner (PD-5), not something the design can establish.
5. **Windows ACL semantics** (PH-10 only) would require redoing `confine.rs`/`preflight.rs`'s mode-bit
   security checks; this is why a user-account Windows supervisor is not on the early path.
6. **Overlap with `multi-project-design.md`.** PH-2/PH-4 edit the same config and loop code as MP-1/MP-2/
   MP-11; the sequencing in §9 assumes MP-1 lands first. If it does not, PH-2 should be done first and MP-1
   rebased onto it — either order works if they are not concurrent.
7. **WSL2 networking mode changes between Windows builds** [claim]; the P4 probes must run at every
   attestation and after any Windows update, not only at install.
8. **Python as a dependency** (PD-10) keeps native macOS/Windows container hosts dependent on a system
   interpreter; acceptable while those hosts are secondary.

## Appendix: code and document references (revision `e009102`)

- Containment: `crates/supervisor/src/sandbox.rs:19` (`base_args`), `:32` (`shared_args`), `:111`
  (`hide_other_runs`), `:123` (`wrap`; Codex early return `:127`), `:173` (`check`), `:346` (`probe`);
  `relay.rs:1-13`, `:53`, `:250`; `egress.rs:3,108`; `network_probe.rs:1-60`; `candidate.rs:1-18,65-67`;
  `reaper.rs:1-24`.
- Identity and profile: `profile.rs:68` (`claude_token`), `:92` (`Role::user`), `:142` (`command`),
  `:194-207` (`codex_args`), `:227-284` (`environment`); `preflight.rs:18,46,95,109`; `launch.rs:105-160`.
- Loop and health: `run_loop.rs:19-27` (Linux gating), `:65` (`min_free_mib`), `:620` (`beat`); `live.rs:1-30`;
  `run_loop/health.rs:100,147`; `live_health.rs:24`; `review.rs:20`; `live_review.rs:541-558`;
  `main.rs:259-290`; `config.rs:11,112,156`.
- Host scripts: `host-setup.sh:1-70` (header/defaults), `:128` (`create_users`), `:344-355`
  (`install_binaries`), `:1365-1470` (cron), `:1537` (`install_firewall`), `:1567` (`install_run_unit`),
  `:1609-1716` (sysv), `:1896` (`next_steps`), `:1960` (`main`); `quiet-hours.sh`; `agentc-run-sysv.sh`;
  `agentc-cron.sh`; `sysvinit-check.sh`; `containment-suite.sh:55-779`; `agentc-update.py:1-40,369-376,461,624,711`;
  `attention.py:245`; `e2e-canary.py`; `integrator-host-setup.sh:106`.
- Elsewhere: `crates/cli/src/main.rs:4309` (`default_workstation`); `book/src/docs/api-contract.md:692`;
  `crates/server/migrations/0029_review_independence.sql`; `.github/workflows/ci.yml:72`,
  `release.yml:100`; `book/src/deploy/README.md` (quiet hours, sysvinit); `planning/autonomy/multi-project-design.md`.
