# Native Windows supervisor: design and prototype

Status: **proposed; phase 2 of the containment prototype is complete; phase 3
is not started; not approved for unattended execution**. Task
`7011c55f-652e-47d3-a409-7d46420c7eb6` activates a native Windows replacement
for the abandoned WSL host approach. Its first phase, task
`a9126e52-654c-4b26-8a7d-7748261d2c4a`, delivers this design and the
containment prototype. Task `3787e738-d381-4e4f-96b0-75f7afe22155` prepares
phase 2. Task `37788fa5-da1c-4a76-80ed-c893d8ae74f3` is phase 3: the
[host-runtime interface and a Windows launcher](#host-runtime-interface-and-windows-backend)
that reuses the prototype's boundary. Its acceptance run under a role account
has not been done. This chapter does not claim that Windows services or a
production Windows sandbox are implemented. The
[current implementation](implementation-status.md) remains authoritative. The
Linux backend remains supported.

Every statement here is one of two kinds. A **proposed** mechanism is design
intent that nothing has exercised. **Demonstrated** behavior was observed by the
prototype on the one evaluated host, under the identity and limits recorded in
[MiniAir findings](#miniair-findings-2026-11-09). A demonstration on that host
is evidence for this design. It is never host admission.

## Threat model and authority

Treat repository files, build scripts, tests, package hooks, model output and
every worker command as hostile. Assume an attacker can execute arbitrary native
code under its worker token and knows all paths, pipe names, process IDs,
package SIDs and launch IDs. A random name is not an access check. Protect the
workstation user, service processes, provider/coordinator credentials, Git
publication key, role state, other launches (including launches of the same
role), immutable tools, policy and mirrors. Protect reviewer credentials from
the reviewed candidate.

The account a launch runs as matters as much as its package. Windows grants
application packages read access to parts of the launching user's own registry
hive, so a launch that runs as the workstation owner can read the owner's
settings. The prototype demonstrated this. A launch must therefore never run as
the owner or as an account that holds provider, coordinator or publication
credentials.

A window station is part of the boundary. A launch that shares the interactive
window station shares its clipboard, atoms and window messages. Windows gives
an account one unnamed window station per logon session, and only an
administrator may create a named one. Launches that run as one account in one
logon session therefore share a station: their desktops are separate, their
clipboard and atom table are not.

Trust the Windows kernel and administrator-installed service binaries and
policy. Administrator/kernel compromise, physical attacks and side channels are
outside this boundary. Resource exhaustion is bounded, not eliminated. Service
tokens and ACLs are local enforcement; coordinator review independence still
depends on distinct eligible principals and the live project policy.

No service offers remote arbitrary command execution. Work is selected through
the existing authenticated coordinator workflow. A local runner accepts only a
validated launch manifest from its supervisor service identity. Provider
enrollment stays a separate interactive setup operation outside worker state.

## Target and support matrix

The **evaluation target** is MiniAir: native Windows 11 Pro x86-64, version
26H2, build 26300.9818, observed 2026-11-09. This is an observation, not a
supported production OS assertion. No Windows edition/build or harness/role
combination is admitted until its exact combination has an acceptance report.
Windows Server, ARM64, Windows Home, older Windows builds and WSL are not
covered by this prototype. A different build needs the same acceptance gates.

| Component | Evaluation choice | Admission state |
| --- | --- | --- |
| Python | Native CPython 3.14, stdlib ctypes probe driver | Experiment driver only |
| Rust/Cargo | Rust 1.99.1, MSVC tools 14.51.36231 and Windows SDK 10.0.26100.0; offline, locked, isolated caches | **Demonstrated** in the prototype: the fixture crate builds and its tests pass inside both candidate boundaries. No worker profile is admitted and the workspace itself is untested |
| Git | Native pinned Git, no hooks, disabled direct push | Not yet exercised in a Windows launch |
| Claude implementer/reviewer | Native pinned release with all command paths examined | Unsupported; no Windows interception evidence |
| Codex implementer/reviewer | Native pinned release with all command paths examined | Unsupported; no Windows interception evidence |
| Browser, MCP and auxiliary tools | Individually pinned and brokered | Unsupported until alternate execution paths are tested |

Record full executable paths, SHA-256, reported versions, OS revision and source
commit/tree in each acceptance report. An available executable or a generic
AppContainer test does not qualify a harness. Do not substitute API billing for
the owner's provider enrollment model. The [earlier Codex evaluation](codex-reviewer-isolation.md)
is historical Linux evidence, not Windows permission to launch or a WSL fallback.

## Launch boundary

A launch is one disposable process tree. The proposed boundary has five layers.
The prototype exercised the last four under the owner's own account, which is
why the first is still a requirement and not a result.

1. **Role identity (proposed, required).** The launch runs as a dedicated role
   account whose registry hive and profile hold nothing worth protecting. This
   is the only layer that keeps a launch out of the owner's registry.
2. **Package identity (demonstrated).** Each launch gets a unique disposable
   AppContainer profile, including concurrent launches of one role. The token is
   *less privileged* (LPAC): the launcher sets
   `PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY`, so the token loses
   everything Windows grants to "all application packages". It carries no
   network capability. It carries `registryRead`, which any Winsock user needs:
   the Winsock catalog keys grant access only to all application packages and
   to that capability, and `cargo` aborts at start without it. It also carries
   one custom capability that the tool directory's access list names, so tools
   need no access entry per launch.
3. **Job Object (demonstrated).** The process is assigned to an unnamed job
   atomically through the process attribute job list. The job kills every member
   when its last handle closes, forbids breakaway, bounds the active process
   count and memory, applies every user-interface restriction and suppresses
   error dialogs. A job controls lifetime and resources; it is not the access
   boundary.
4. **Private desktop in a window station the launcher makes (demonstrated).**
   Each launch gets its own desktop, with an access list naming only the
   service owner and that package. The launcher makes the logon session's
   window station and lists only the service owner and the packages of its live
   launches. A launcher that finds the station already made by another program
   does not control who may open it and must refuse. **A station per launch is
   not established**: it needs a logon session per launch or a named station
   made by an administrator, and neither has been tried.
5. **Explicit inheritance (demonstrated).** The environment block is an exact
   allowlist. On the evaluated build `CreateProcessW` fails with error 203 when
   that block lacks `LOCALAPPDATA`, so the allowlist includes it; the value is a
   path, not a grant. A process attribute handle list names the only inherited
   handles.

The process starts suspended. Before the first instruction runs, the launcher
reads back from the OS the job membership, the AppContainer flag, the package
SID and the less-privileged opt-out. Any mismatch ends the job. There is no
uncontained retry and no partial launch. The evaluated build rejects the
dedicated "is less privileged" token query, so the opt-out is read from the
token's security attributes, where Windows records it as `WIN://NOALLAPPPKG`.

A launch may read its control directory and the tool directory and may modify
only its own working directory, which carries a low integrity label. Host logs
live outside every worker-writable tree; a launch receives only an inherited
handle.

Host code that reads a worker-written file opens it once with its final reparse
point unresolved and checks that handle: not a reparse point, exactly one hard
link, and a real path inside the launch root. A link in a parent directory is
caught because the handle's real path leaves the root. Creation and deletion
inside worker-writable trees need handle-relative operations; that is proposed
and untested.

## Service identities and filesystem

Use separate least-privilege virtual service accounts for `AgentcSupervisor`,
`AgentcImplementer`, `AgentcReviewer`, `AgentcEgress` and `AgentcPublication`.
None is an administrator or LocalSystem worker. Deny interactive logon; grant
only the service and object rights each component actually needs. Inventory token
privileges and groups during installation and acceptance; never assume the
account name proves its privilege level. A privileged installer/policy helper
may install SCM/WFP policy, but cannot run caller-supplied programs.

A credentialed harness and the candidate commands it runs must not share an
account: with `registryRead`, a launch can read the registry hive of the account
it runs as. Keep provider state out of any hive a launch can reach, and keep
user-level environment variables out of every role account.

Keep administrator-owned binaries/configuration under a dedicated Program Files
directory and protected state under a dedicated ProgramData directory. Each
role has its own protected credential/state subtree. The supervisor owns durable
launch manifests and coordinator mutation journals; workers cannot edit them.
The publication service alone reads the signing key. Egress has no publication
key. Credentialed provider harness state is outside candidate-readable grants.

Validate paths by opened handles and file identity, not lexical prefixes.
Reject reparse points in protected ancestry, unexpected volume transitions,
device/UNC paths, alternate data streams and writable hard-linked tool/seed
files. Hold protected parent handles while creating/granting launch files;
validate final identities again before execution and cleanup. NT aliases,
case folding, short names, junction swaps and concurrent rename attacks require
tests. Host brokers must not follow worker-controlled links with service rights.

## Local broker contracts

Named pipes are local-only, reject remote clients, and have an explicit DACL.
Authenticate the peer token and its package SID, service SID, role, launch
generation and current coordinator authority. Derive identity from the OS, never
from claimed request fields. A PID alone is reusable and insufficient. A
per-launch capability is protected and short-lived; it cannot replace token
authorization. Impersonation is bounded to the access check and always reverted.

The prototype demonstrated the first half of this contract with a stub. Each
launch has a pipe whose access list names its package SID. The stub impersonates
the connected client, reads the package SID from the client token, reverts, and
answers with that SID while the client claims to be another launch. A foreign
launch cannot open the pipe at all. Service SIDs, launch generations and
coordinator authority are proposed and untested.

| Interface | Permitted operation | Required rejection/probe |
| --- | --- | --- |
| Supervisor to runner | Start a fixed pinned harness from an immutable exact-source manifest; inspect or stop its saved launch | Arbitrary executable/env, stale generation, duplicate start, forged role and foreign SID |
| Harness to candidate runner | Run a bounded candidate command in a separate credential-free container; return bounded output/status | Access to harness process/state, inherited secret handles/env, alternate tool path bypass |
| Worker to egress | Connect to a configured allowlisted destination through the authenticated launch channel | Raw TCP/UDP, direct DNS, IPv4/IPv6, loopback, redirects/rebinding, foreign launch, policy loss |
| Implementer to publication | Publish only the coordinator-authorized candidate ref/source for that launch | Reviewer calls, arbitrary refs/remotes, wrong tree/task, replay after expiry, signing-key reads |

Use WFP or a demonstrated equivalent to deny direct network access for worker
tokens; proxy variables are configuration only. Do not grant a general network
capability as a workaround for local broker access. Prefer local named-pipe
transport to a broadly reachable loopback listener. The egress service can use
network credentials, but cannot forward arbitrary destinations. Broker outage
or missing filters closes admission and ongoing egress. DNS caching or an
unreachable test destination does not prove a denial: positive controls and
observed policy failures are required.

A token with no network capability is the demonstrated equivalent so far: the
prototype's launches could not reach loopback, the local network, a public
address or the resolver. The owner ran the
[network policy loss test](#network-policy-run-2026-11-09) with every
firewall profile disabled, and the refusal held. What happens when the
firewall service itself is stopped is untested: the owner did not authorize
that condition. The test is described under
[network policy rounds](#network-policy-rounds).

The reviewer harness and candidate commands require distinct package identities
and separate jobs. Every candidate execution route (shell, file tool, code mode,
MCP, browser and child helper) must preserve the boundary. If a native harness
cannot reliably route all such operations through it, that harness/role remains
unsupported. Separate Windows service accounts alone do not meet this condition.

## Linux P2 parity and acceptance map

The source inventory is `deploy/agentc/containment-suite.sh` plus the supervisor's
`preflight`, `confine`, `sandbox`, `candidate`, `relay`, `reaper` and `push_helper`
modules. Every applicable Linux check below needs a Windows counterpart; a
platform-specific mechanism can differ, but its guarantee cannot disappear.

| Existing check/group | Windows enforcement to validate | Required acceptance test |
| --- | --- | --- |
| `check_profiles`, exact versions, trusted binaries | Administrator-owned pins, ACLs and immutable manifest | Wrong version, writable binary/parent, candidate config/hooks refused |
| `check_git`, hardened clone | Exact revision, immutable mirror, hooks disabled, push URL disabled | Planted hook cannot run; raw push fails; candidate contains exact base |
| `check_writes`, `check_seeds` | Package SID grants plus protected role ACLs/parents | Host home/tools/mirror/other role unreadable or unwritable as specified; seed replacement denied |
| `check_codex_sandbox`, `check_claude_sandbox` | Per-launch Windows boundary independent of model behavior | Own workspace write succeeds; outside writes, same-role reads, process injection and registry/IPC access denied |
| `check_egress` | WFP and authenticated egress broker | Positive allowed route; blocked destination, raw HTTPS/DNS, IPv6, loopback and forged request fail |
| `check_claude_launch`, `check_relay_probe`, shell execution | Native role runner with fail-closed broker preflight | Real harness command works through authorized route; missing/broken broker refuses launch |
| `check_claude_token`, `check_candidate_token`, reviewer isolation | Protected provider state and separate candidate package/job | Synthetic credential exists in harness but no candidate env/file/process/handle access; alternate paths exercised |
| `check_push_kill`, `check_push_launch`, `check_live_helper` | Per-launch publication authorization and job ownership | Correct role/launch only; no foreign same-role client, orphan helper or stale capability; crash sweep verified |
| `check_push_key`, `check_attention_token` | Publication/supervisor-only ACLs and no handle/env inheritance | Both worker roles and candidate cannot open synthetic credential equivalents |
| `check_apparmor_bwrap`, group/tool access | Windows token privilege/package and tool ACL policy | Worker cannot obtain elevated/service token or mutate enforcement; mandatory OS boundary absent means refusal |
| `check_browser_install`, `check_pinned_shell`, `check_browser` | Pinned native browser in its own bounded context | Staging page renders; other-role login inaccessible; alternate networking/process paths denied |
| `check_cargo_test` | Native pinned Rust/MSVC/SDK under worker grants | Real offline build/test under each admitted profile; baseline comparison for failures |
| Child cleanup and no privilege gain | Atomic Job Object membership, restricted tokens, broker checks | Child/grandchild and detached/breakaway attempts remain contained; crash kills all descendants |
| Additional Windows path hazards | Handle-based identity and protected parent directories | Junction/reparse/hard-link/ADS/device/UNC/short-name and rename-race attacks fail |

Skipped Linux suite legs are not Windows passes. Windows acceptance expands the
suite with credentialed harness paths, same-role concurrency and SCM lifecycle.

### What the prototype has demonstrated

Every entry below was observed with launches running as the owner on MiniAir.
"Partial" means the rows named passed and the rest of that guarantee is still
only proposed.

| Existing check/group | Prototype evidence |
| --- | --- |
| `check_profiles`, exact versions, trusted binaries | None. The report pins tool hashes; nothing refuses a wrong version |
| `check_git`, hardened clone | None |
| `check_writes`, `check_seeds` | Partial. The host directory, the tool directory and every other launch's directory are denied (`host.file_*`, `host.dir_*`, `host.tool_write`, `peer.*.file_*`). Role state and seeds are untested |
| `check_codex_sandbox`, `check_claude_sandbox` | Partial. Own writes work; host and cross-launch files, processes, pipes, events and the interactive window station are denied. **The owner's registry is denied only to a launch running as a separate account**: `host.registry_environment_open` fails when the launch runs as the owner and passed in the role-account run |
| `check_egress` | Partial. Direct TCP over IPv4 and IPv6, loopback, the local network, UDP and DNS are denied to a token with no network capability, and the authorized pipe route works. With the firewall profiles disabled the refusals held in the [launcher's role-account run](#role-account-run-through-the-launcher-2026-11-09). No WFP policy or allowlist broker exists, and a stopped firewall service is untested |
| `check_claude_launch`, `check_relay_probe`, shell execution | None |
| `check_claude_token`, `check_candidate_token`, reviewer isolation | Partial. A synthetic credential file is denied, and implementer and reviewer packages cannot reach each other. No real harness, environment or credentialed process was involved |
| `check_push_kill`, `check_push_launch`, `check_live_helper` | Partial. A per-launch pipe admits only its package, a foreign launch is denied, and the stub takes identity from the client token. No publication broker exists |
| `check_push_key`, `check_attention_token` | Partial. The synthetic credential stand-in is denied to every launch |
| `check_apparmor_bwrap`, group/tool access | Partial. The launcher refuses a launch whose token or job is not what it asked for, and the Task Scheduler refuses the less-privileged token. Acquiring a service or elevated token is untested |
| `check_browser_install`, `check_pinned_shell`, `check_browser` | None |
| `check_cargo_test` | Partial. A real offline `cargo test` and `cargo build` of the fixture crate pass inside both boundaries. The workspace is untested |
| Child cleanup and no privilege gain | Demonstrated for these launches: children inherit the token and job, breakaway is refused, the process and memory limits hold, and a deliberate orphan dies with the job |
| Additional Windows path hazards | Partial. Nine aliases of a host file are denied; a contained process cannot create a junction or symbolic link, cannot hard-link a host or tool file and cannot write a stream on a host file; the host reader refuses hard links, junctions and a path swapped mid-read. Handle-relative creation and deletion are untested |

## Runtime interface and durable recovery

Keep selection, admission, lease renewal, quiet hours, budget/cost reporting,
checkpoints and review orchestration in the existing platform-neutral
`run_loop::Driver` flow. Extract OS operations from `run_loop::live`, `rooted`,
`push_helper` and process cleanup behind a host-runtime interface with
`prepare`, `start`, `observe`, `request_stop`, `terminate`, `recover` and
`cleanup` operations. Each returns typed identities/outcomes and durable launch
records, not shell command strings. Linux retains its existing implementation
and acceptance suite while Windows adds a separate backend.

The interface and both backends now exist; see
[Host-runtime interface and Windows backend](#host-runtime-interface-and-windows-backend)
for what was built and what was demonstrated. The rest of this section is the
design they answer to.

Persist start intent, exact executable/source, coordinator attempt/generation,
package SID and job/runner identity before launch. Serialize writers and use
protected atomic replacement with flush. A restart observes the saved operation;
it never blindly repeats a start, claim or publish. Unknown producers retain
resource holds. Never infer completion from a lost pipe, PID reuse, expired lease
or root-process exit while descendants live. Coordinator operations retain their
existing origin binding, idempotency journal and post-lock authority checks.

An SCM service control handler records stop/preshutdown and returns promptly;
the loop stops admission, checkpoints and drains to a monotonic deadline.
Terminate the whole job after the deadline and verify quiescence before releasing
holds. On crash or power loss, the restarted service reconciles saved state and
current coordinator authority before any new launch. Configure bounded SCM
recovery; a failure loop cannot silently enable unsandboxed work. Verify boot
without logon, logoff, stop, preshutdown/reboot, forced service/child crashes,
network loss and partially completed updates. These are machine acceptance tests,
not unit-test claims.

## Host-runtime interface and Windows backend

Task `37788fa5-da1c-4a76-80ed-c893d8ae74f3` built these. Everything in this
section is **implemented**; what a run has **demonstrated** is under
[Windows backend development runs](#windows-backend-development-runs-2026-11-09)
and
[Role-account run through the launcher](#role-account-run-through-the-launcher-2026-11-09).

### The interface

`crates/supervisor/src/host_runtime.rs` defines `HostRuntime`. The live loop
reaches the operating system only through it, by `run_loop::Driver::host`.

| Operation | What it does | What it returns |
| --- | --- | --- |
| `prepare` | Makes the launch's source tree and run directory | `StartIntent`: boot id and, where the host has them, source, executable, container and job identity |
| `start` | Starts the launch inside the boundary, or fails | `LaunchIdentity`: pid and start time |
| `observe` | Looks whether the launch ended | `Observation`: running, or exited with a code |
| `request_stop` | Asks the launch to end by itself | Nothing |
| `terminate` | Ends the launch and everything it started | Nothing |
| `recover` | Judges a saved launch record without starting anything | `Recovery`: may be running, or ended |
| `cleanup` | Removes the launch's tree and run directory | Nothing |

The loop writes the `StartIntent` into the launch record before the start and
adds the `LaunchIdentity` after it. A failure to look is "running", and
"ended" needs evidence: a lost observer proves nothing.

`run_loop::Driver` keeps selection, admission, leases, quiet hours, cost,
checkpoints and review orchestration. The reviewer side
(`run_loop::live_review`) still runs its launches itself; putting it behind
the interface belongs to phase 5.

### The Linux backend

`host_runtime::linux::LinuxHost` holds what `run_loop/live.rs` did before:
the mirror fetch and clone as the role account, `prepare`, the role's
credential copy, `launch-root` in its own process group, SIGTERM and SIGKILL
to that group, `/proc` identity and symlink-free removal. `launch-root` still
starts the candidate-push helper and the reaper. The code moved; it did not
change. A Linux launch record is written as before: the new optional fields
are left out when a backend does not set them.

A host with no backend gets `host_runtime::Refusing`. It prepares and starts
nothing, and it never calls a recorded launch ended.

### The Windows backend

`host_runtime::windows` is the prototype's launcher in Rust, one module per
layer of the [launch boundary](#launch-boundary).

| Module | Layer |
| --- | --- |
| `profile` | A disposable AppContainer profile per launch and its capability SIDs. An existing profile is never adopted |
| `job` | An unnamed Job Object: kill on close, no breakaway, process and memory limits, every user-interface restriction, no error dialogs |
| `desktop` | The logon session's window station, made by the launcher, and a private desktop per launch |
| `process` | Creation with the job list, the security capabilities, the handle list and, for a less-privileged token, the all-application-packages opt-out; an exact environment block; suspended start, read-back, resume |
| `token` | The read-back: AppContainer flag, package SID and `WIN://NOALLAPPPKG` |
| `reader` | The handle-based reader, and directory creation with a complete access list |
| `launcher` | One launcher: its station and every launch it holds |
| `journal`, `recovery` | The durable launch record and the judgement of a saved one |
| `runner` | The launcher served to a driver as typed requests |

`WindowsHost` implements `HostRuntime` over one launcher. What a launch runs
comes from a `Planner` the caller supplies: no provider harness is supported
on Windows, and nothing in the crate builds a harness launch.

It differs from the prototype in four ways, each stricter or equal.

- A window station the launcher did not make is **refused**. The prototype
  opened it and reported a failed row.
- A launch's control and working directories are **created with a complete,
  protected access list**: the launcher's account, the system, administrators
  and the launch's package. The prototype added an entry to what the parent
  directory granted. The working directory carries an inherited low
  integrity label.
- A launch's two directories are created by the launcher, never adopted.
- `request_stop` writes a `stop` file in the control directory, which the
  launch can read and cannot change. Windows has no signal to send. The probe
  fixture reads that file; a harness would have to.

The launcher **does not switch accounts**. A launch runs as the account the
launcher runs as. That account must be a dedicated role account, and the
report records which account it was.

### The runner and its journal

`agentc-supervisor windows-runner --state-dir DIR` is a hidden subcommand. It
reads one JSON request per line and answers each with one line. A request
names an operation and typed arguments; none is a command line for a shell.

| Request | Effect |
| --- | --- |
| `identity` | The runner's account and the owner identity of its jobs |
| `prepare` | Profile, both directories, desktop; a journal entry |
| `hold_file` | Opens a file with an inheritable handle the runner keeps |
| `start` | Starts a process; only its log and the listed held handles are inherited |
| `observe`, `request_stop`, `terminate` | As in the interface |
| `watch`, `watched_ended` | Watches a process the launch reported, if Windows says it is alive inside the launch's job |
| `read` | The handle-based reader |
| `cleanup` | Ends the launch and deletes its desktop and profile |
| `recover`, `reclaim` | Judges journaled launches; removes what an ended one left |

The runner's job handles are the only ones. When its input ends it exits, the
handles close and Windows ends every launch.

Before a start, the runner writes the launch's entry (launch, profile,
package SID, job owner, executable, and the source, attempt and generation
the driver gave), flushes it and renames it over the old one. After the
start it adds the pid and creation time the same way.

A restarted runner **never starts a journaled launch**: `prepare` refuses an
id that has an entry, and `start` needs a `prepare` by the same run. It
judges the entry instead. A Windows process is identified for good by its
pid and creation time, and the job owner is recorded as
`runner:<pid>:<creation time>`. A launch is **ended** only when Windows says
both that the owner is gone and that the root process is gone. If Windows
will not answer, or the entry cannot be read, the launch may still be
running and keeps what it holds. An unreadable entry is reported by name.

### Driving the probe through the launcher

`windows-containment-probe.py --launcher PATH` starts the runner from
`agentc-supervisor.exe` at `PATH` and makes every launch through it
(`windows-probe-runner.py`). The driver keeps the canaries, the listeners,
the broker stub and the accepted value of every row, so a report is judged
exactly as the prototype's. With `--owner-plant`, the owner's step stages the
launcher beside the probe and adds `--launcher` to the role account's
command. The report records the launcher's path and SHA-256, the account it
ran as and the owner identity of its jobs. Without `--launcher` the prototype
launcher runs, as before.

### Tests

- `cargo test -p agentc-supervisor` on Windows runs the unit tests of every
  module and two integration tests, `windows_runner` and `windows_host`.
  Where another program made the session's window station, as on an
  interactive desktop, those two check the refusal and nothing else.
- The portable tests in `windows-containment-probe-test.py` drive the
  driver's side of the runner protocol against a stand-in runner.
- The workspace builds on native Windows. Unix-only code is behind
  `cfg(unix)` or `cfg(target_os = "linux")`: the relay's socket half, the
  candidate-push helper's command line, and tests that assert Unix paths,
  ownership or `/proc`.

Two things came up on the way and are recorded here.

- The service's backup wrote files it then flushed through a read-only
  handle, moved a snapshot with a rename that replaces an empty directory on
  Windows, and took a Windows lock violation for an internal error. All three
  are fixed for Windows; Linux is unchanged.
- The kill switch treats a path it cannot check as set. On Windows a path
  below a plain file is reported as not found, the same as an absent switch,
  so that one unreadable case is not told apart there. No Windows supervisor
  loop exists to be affected; phase 4 has to settle it.

## Delivery phases and gates

Each phase is an independently reviewed task. A phase starts only when the gate
before it has passed; a blocked gate stops the phases behind it.

| Phase | Content | Gate to start | State |
| --- | --- | --- | --- |
| 1 | This design and the containment prototype | None | Delivered by task `a9126e52`; the prototype gate is **blocked** |
| 2 preparation | A role-account mode for the prototype and the owner runbook | Owner decision `df37b339` | Delivered by task `3787e738` |
| 2 | The same matrix with launches running as a dedicated role account, plus network policy loss | An owner-controlled elevated session on the target | **Complete.** The role-account matrix **passed** on 2026-11-09 after a fix to the prototype's window station. The network policy loss test (task `96d4927e`) **passed** in the owner's run with the prototype launcher on 2026-11-09; task `4baa4706` records it under [Network policy run](#network-policy-run-2026-11-09). The same runbook, followed with the Rust launcher in phase 3, passed |
| 3 | Shared host-runtime interface and a Windows launcher that reuses this boundary and fixture | Phase 2 gate passes | Started by task `37788fa5` on the owner's go-ahead of 2026-11-09, after the owner's network policy run that task `4baa4706` records. The interface, the Linux backend behind it and the Windows backend are written and tested. The acceptance run through the new launcher under a role account, with the two network rounds, **passed** on 2026-11-09. The phase is complete when the task's review and integration are |
| 4 | SCM services, lifecycle and recovery acceptance | Phase 3 | Not started |
| 5 | Reviewer candidate isolation on real native harness command paths | Phase 3 | Not started |
| 6 | Egress and publication brokers | Phase 3 | Not started |
| 7 | Idempotent installer, update/rollback/uninstall, full host acceptance report | Phases 4 to 6 | Not started |

No intermediate phase is production acceptance. Independent review and guarded
integration complete each task; only the final acceptance can admit a host.

## Containment prototype

`deploy/agentc/windows-containment-probe.py` is a disposable experiment with
five parts.

- `windows-containment-probe.py` is the driver. It holds the accepted value of
  every row, creates the host-side canaries and positive controls, and derives
  the verdict.
- `windows-probe-native.py` is the Windows API layer: the launcher described
  under [Launch boundary](#launch-boundary), the broker stub, the listeners and
  the handle-based reader.
- `windows-probe-fixture/` is a Rust crate with no dependencies. It is the
  workload and the attacker.
- `windows-probe-owner.py` is the [role-account mode](#role-account-mode): the
  owner's step and the manifest that a separate account's run reads.
- `windows-probe-policy.py` is the
  [network policy loss test](#network-policy-rounds): the role run's network
  rounds and the owner's elevated step that changes and restores the firewall.

For each boundary variant the driver does the following.

1. **Build.** It copies the fixture crate into a builder launch and runs
   `cargo test` and `cargo build`, offline and locked, inside the boundary. The
   host reads the built executable by handle and stages it read-only.
2. **Handles.** A first process that is passed the host's canary handle reads
   the canary through it; one that is not passed it must fail.
3. **Launches.** Two implementers and a reviewer start together. Each creates a
   private note, a pipe and an event, then attacks the host and both peers while
   every peer is still alive, and writes one `key=value` row per observation.
4. **Host observations.** The driver records what its listeners received, what
   its broker stub saw, whether its reader refuses the links each launch
   planted, and whether a deliberate orphan dies with the job.

The fixture reports facts and never judges them. A row passes only when its
observation is one of the values the driver accepts for it. A missing row, an
unexpected row, a failed positive control and a skipped check all fail. Each
attack names the positive controls that make its denial evidence: the same
operation succeeding with the host token, or on the launch's own resource. The
fixture is trusted test code; what is evaluated is the boundary around it.

Two variants are evaluated as independent comparisons, never as fallbacks for
each other: an ordinary AppContainer, and the less-privileged AppContainer that
the design proposes. The gate follows the proposed one. No verdict can admit a
host: the report always says `host_admitted: false` and lists what it cannot
establish.

Run it from the source checkout with native Python. Replace `NEW_REPORT` with a
new absolute path outside the repository:

```text
python -I deploy/agentc/windows-containment-probe.py --output NEW_REPORT
```

`--toolchain`, `--msvc` and `--windows-kit` name the Rust toolchain, the MSVC
tools and the Windows SDK when they cannot be discovered. The exit status is 0
only when the gate passes.

The run creates and then deletes its AppContainer profiles and one registry key
under the invoking user's `Software` key. It creates a temporary directory,
changes access lists and integrity labels only inside it, and keeps it with its
logs for inspection; delete it afterwards. With the host token, as positive
controls, it resolves `example.com` and opens one connection to it, and connects
to the local Task Scheduler without registering anything. It never installs
services or firewall rules, never reads a provider, coordinator or publication
credential, and never changes WSL.

`deploy/agentc/windows-containment-probe-test.py` tests the result handling
without any Windows API, so the Linux deploy-script job runs it. On native
Windows it also runs the reader against real hard links, junctions and a
junction swapped during the read.

### Role-account mode

The single-account run above launches as the invoking account, so the protected
owner and the launch identity are one account. Role-account mode separates
them, which is what phase 2 has to show. It has three steps.

1. **Owner step.** `--owner-plant` runs as the owner. It plants the owner's
   canaries (a private directory with a file and a credential stand-in, one
   registry key, a pipe and two events) and stages the interpreter, the probe
   source and a Rust toolchain where another account can read them. It records
   the owner's SID, those targets and the result of each owner-token control in
   a manifest, then waits.
2. **Role run.** A separate account runs the staged probe with
   `--owner-manifest`. The whole matrix then launches as that account. The
   registry and profile rows address the owner's hive and profile by SID, and
   nine `owner.*` rows attack the owner's files, process, pipe and events.
3. **Owner finish.** The waiting owner step reads the role run's report by
   handle, adds rows that its canaries are exactly as planted, recomputes the
   verdict and writes the final report.

Every owner row is paired with an owner control from the manifest, and a control
the owner did not record as passed fails. The row
`identity.role_account_distinct` passes only when the launch identity is not the
owner. With `--same-account-dry-run` the owner step starts the role run itself
under the same account. That exercises every step but proves nothing about a
role account, so that row fails and the gate stays blocked.

### Owner runbook for the role-account run

This is the cross-account run that phase 2 needs. Only the owner can do it,
because step 1 needs elevation. The owner followed it on MiniAir on 2026-11-09;
see [Cross-account runs](#cross-account-runs-2026-11-09). Treat a row that
fails for any reason other than the boundary as a defect in this procedure, and
stop.

1. In an elevated shell, create a temporary local standard account. The command
   asks for a password.

   ```text
   net user AgentcProbeRole * /add
   ```

2. In your own shell, not elevated, from the source checkout, start the owner
   step. Replace `NEW_REPORT` with a new absolute path outside the repository.
   It prints one command and waits for up to an hour.

   ```text
   python -I deploy/agentc/windows-containment-probe.py --owner-plant --staging C:\Users\Public\agentc-probe-staging --role-account AgentcProbeRole --output NEW_REPORT
   ```

3. In a second shell, not elevated, run the printed command as the role account
   and enter its password. It takes a few minutes. Windows may ask whether to
   allow the staged Python to run; allow it.

   ```text
   runas /user:AgentcProbeRole "PRINTED_COMMAND"
   ```

4. The owner step finishes by itself and prints the verdict. In `NEW_REPORT`,
   `identity.role_account_distinct` must pass, and for the less-privileged
   variant `host.registry_environment_open` must be `error:5`.

5. In an elevated shell, remove the account and its profile, then delete the
   staging directory.

   ```text
   net user AgentcProbeRole /delete
   Get-CimInstance Win32_UserProfile | Where-Object LocalPath -like '*AgentcProbeRole' | Remove-CimInstance
   Remove-Item -Recurse C:\Users\Public\agentc-probe-staging
   ```

What each step changes on the host:

| Step | Change | Removed by |
| --- | --- | --- |
| 1 | One local standard account | Step 5 |
| 2 | The staging directory, about half a gigabyte. The owner step cuts its inherited access: the owner, administrators and the system keep full access, and the role account can read the tree and modify only its own report directory | Step 5 |
| 2 | The owner's canaries: a temporary directory, one key under your `Software` registry key, a pipe and two events | The owner step, when it ends |
| 3 | The role account's profile directory and registry hive, created on its first run | Step 5 |
| 3 | A run directory under that profile, AppContainer profiles, and one window station with a desktop per launch | The role run deletes the profiles and closes the station; step 5 removes the directory with the profile |

No step installs a service or a firewall rule, changes WSL or reads a real
credential. A passing run closes the registry finding and nothing more. The
loss of network policy is a separate test, described below and run by the owner
on 2026-11-09 (see the [Network policy run](#network-policy-run-2026-11-09)).

### Network policy rounds

A launch with no network capability is refused every direct connection.
Windows itself enforces that, and the design depends on it until a
launch-specific filter policy exists. The network policy loss test shows
whether the refusal still holds when the Windows Firewall policy is taken
away. It extends role-account mode with network rounds and has three parts.

1. **Conditions.** The owner step takes `--policy-conditions` and lists them in
   its manifest. `baseline` is the firewall as found and always comes first.
   `profiles-off` disables every firewall profile. `service-stop` tries to stop
   the firewall service.
2. **Rounds.** After the normal matrix, the role run handles one condition at a
   time. It asks for the condition, reads the firewall state itself, makes the
   host-token controls, then starts a fresh launch inside the proposed boundary
   that repeats the direct TCP, UDP and DNS attempts. The rows are judged with
   the same accepted values as the matrix, and the host's listeners stay the
   authority on whether anything arrived.
3. **Owner's elevated step.** `windows-probe-policy.py` runs elevated as the
   owner. It saves the original state before any change and changes nothing
   unless the firewall was fully on to begin with. It applies a condition
   only when the role run asks for it, and restores the original state as soon
   as the round ends, after 180 seconds at the latest, and when it is
   interrupted. It then writes a restore record.

A round counts only when the role run's own reading of the firewall matches the
condition; the owner's record alone proves nothing. A condition that was not
reached, a missing round, a failed control and a missing restore record all
fail. One exception is recorded instead of failed: Windows may refuse to stop
the firewall service, which shows only that the condition cannot be reached
that way.

`--observe-only` makes the elevated step change nothing and needs no
elevation. It exercises the handshake; the `profiles-off` round then fails, as
it must.

### Owner runbook for the network policy test

Only the owner can do this, because step 2 needs elevation. It turns the host
firewall off for the length of one round, a few seconds, so use a network you
trust. The owner followed it twice on MiniAir on 2026-11-09: with the
prototype launcher (see [Network policy run](#network-policy-run-2026-11-09))
and with the Rust launcher (see
[Role-account run through the launcher](#role-account-run-through-the-launcher-2026-11-09)).
Treat a
row that fails for any reason other than the boundary as a defect in this
procedure, and stop. It needs the role account from the
[role-account runbook](#owner-runbook-for-the-role-account-run) and a firewall
that is on for every profile.

The three steps share one clock. The owner step waits one hour for the role
run, counted from when it prints its command, and the policy step changes
nothing until the role run asks. Start steps 2 and 3 as soon as step 1 has
printed; if the hour runs out first, the owner step reports a missing role run
and ends, the staging directory must be deleted, and the run starts again from
step 1.

1. In your own shell, not elevated, from the source checkout, start the owner
   step with the conditions. Replace `NEW_REPORT` with a new absolute path
   outside the repository. It prints one command and waits for up to an hour.

   ```text
   python -I deploy/agentc/windows-containment-probe.py --owner-plant --staging C:\Users\Public\agentc-probe-staging --role-account AgentcProbeRole --policy-conditions baseline,profiles-off --output NEW_REPORT
   ```

2. In an elevated shell, from the same checkout, start the policy step. It
   changes nothing until the role run asks for a condition.

   ```text
   python -I deploy/agentc/windows-probe-policy.py --staging C:\Users\Public\agentc-probe-staging
   ```

3. In a third shell, not elevated, run the printed command as the role account
   and enter its password. The matrix takes a few minutes; each round then
   takes a few seconds. Windows Security may report that the firewall was
   turned off.

   ```text
   runas /user:AgentcProbeRole "PRINTED_COMMAND"
   ```

4. The policy step prints its restore record and exits 0 only when the firewall
   is as it was. The owner step then prints the verdict. In `NEW_REPORT`, under
   the launch `policy-profiles-off`, `policy.condition_applied` and
   `policy.state_observed` must pass, every `net.` row must be a refusal, and
   every `net.host_received.` row must pass. `policy.host_firewall_restored`
   must pass.

5. Check the firewall yourself. If the policy step was interrupted, restore
   the saved state from the elevated shell.

   ```text
   Get-NetFirewallProfile | Select-Object Name, Enabled
   python -I deploy/agentc/windows-probe-policy.py --staging C:\Users\Public\agentc-probe-staging --restore
   ```

6. Delete the staging directory. When no further run needs the role account,
   remove it as step 5 of the role-account runbook describes.

To also try the firewall service, use
`--policy-conditions baseline,profiles-off,service-stop`. If Windows stops the
service, a round runs and the step starts the service again.

What each step changes on the host:

| Step | Change | Undone by |
| --- | --- | --- |
| 1 | The staging directory and the owner's canaries, as in the role-account run | Step 6; the owner step, when it ends |
| 2 | `policy\original.json` in the staging directory: the firewall state as found | Step 6 |
| 3 | Every firewall profile is disabled while the `profiles-off` round runs | The policy step, when the round ends or after 180 seconds |
| 3 | With `service-stop`: the firewall service is stopped, if Windows allows it | The policy step, when the round ends or after 180 seconds |

No step installs a service or a firewall rule, writes a filter policy, changes
WSL or reads a real credential. A passing run shows that a launch with no
network capability stays refused while the firewall profiles are disabled. It
does not test the production egress broker or its filters, which do not exist,
or a firewall service that Windows would not stop. A failing run means the
refusal depends on the firewall policy, and the design then needs its own
filters before any launch is admitted.

### MiniAir findings, 2026-11-09

Registered producer `20861fb2-7ab3-41f4-83f3-b5fdc215f66d` ran source
`fda2a87bbf00c822f679ff0db1e6faa92cfaf9a8` on MiniAir, not elevated, with every
launch running as the owner. Its inputs stayed unchanged and it exited 1, which
is the blocked gate. The report is coordinator artifact
`359785c5-7aaf-4c5f-b3a7-59582bdc3a83`, SHA-256
`bb95c2fabec9ecfda21ffc0bb188cba630621e9e61a39a9244ba291716757773`.

The report holds 624 rows, of which 582 are mandatory and 42 are informational.

| Group | Mandatory rows | Passed | Failed |
| --- | --- | --- | --- |
| Host positive controls and reader self-test | 18 | 18 | 0 |
| Ordinary AppContainer | 282 | 276 | 6 |
| Less-privileged AppContainer, the proposed boundary | 282 | 279 | 3 |

| Area | Ordinary AppContainer | Less-privileged AppContainer |
| --- | --- | --- |
| Real Rust/MSVC build and test inside the boundary | Passed | Passed |
| Host files, credential stand-in, tool write, nine path aliases | Denied | Denied |
| Host process, opened with four different rights | Denied | Denied |
| Host pipe, events, interactive window station and windows | Denied | Denied |
| Owner's registry: canary read and write, `Run` key write | Denied | Denied |
| Owner's registry: `Environment` key | **Opened** | **Opened** |
| Implementer to reviewer, reviewer to implementer and same role: files, process, pipe, event, broker route | Denied, 48 of 48 | Denied, 48 of 48 |
| Direct TCP to a public address | Refused, error 10013 | Refused, error 10013 |
| Direct TCP to loopback (IPv4, IPv6) and the local network | Timed out; nothing arrived | Refused, error 10013; nothing arrived |
| UDP to loopback | Send returned success; nothing arrived | Refused, error 10013 |
| DNS lookup of a public name | Failed, error 11001 | Failed, error 11003 |
| Child outside the job | Refused | Refused |
| Handle the host holds but did not list | Process ended with `STATUS_INVALID_HANDLE` | Process ended with `STATUS_INVALID_HANDLE` |
| Task Scheduler connection | Activation failed with `REGDB_E_CLASSNOTREG`; **not accepted** | Refused, `E_ACCESSDENIED` |
| Job limits | Limit of 24 processes held: 23 children started, the next was refused with error 1816; memory refused at 448 MiB of a 512 MiB limit; a deliberate orphan died with the job | Same |
| Links planted from inside | Junction and symbolic link creation refused; hard links and stream write denied | Same |
| Broker identity | Package SID read from the token; forged claim ignored | Same |

What follows from these rows:

- **The owner's registry is reachable, so the gate is blocked.** A launch
  running as the owner opened the owner's `Environment` key, where user-level
  environment variables live. The hive root, `Software` and `Environment` each
  carry an access entry for all application packages and one for the
  `registryRead` capability, so both variants can read them. No narrower
  capability exists for Winsock, and a token without `registryRead` cannot run
  `cargo`. Only a different account closes this. A key created by an ordinary
  program does not inherit those entries, which is why the canary key was
  denied; that does not protect the keys Windows itself creates.
- **The less-privileged token is the stricter boundary.** It was denied a file
  granted only to all application packages, which the ordinary token read. It
  was refused every network attempt at once, where the ordinary token's local
  attempts were dropped silently. Its Task Scheduler connection was refused
  with an access error.
- **The ordinary AppContainer is not a candidate on this build.** Its Task
  Scheduler activation never reached an access check. Unregistered development
  runs also saw the helper end with heap-corruption and access-violation
  statuses there, and `schtasks.exe` do the same. That is not a clean denial.
- **A contained process cannot plant the links a host broker must fear.**
  Junction and symbolic link creation failed inside both boundaries. The host
  reader still refuses them, proven against links the host planted itself: over
  two seconds of swapping a path between an inside and an outside junction it
  returned the inside file 275 times, refused 559 reads as outside the root and
  never returned the outside file.
- **Cargo hard-links its output.** The reader correctly refused the built
  executable, so the builder exports a plain copy. A host that stages worker
  output must expect this.

### Same-account dry run, 2026-11-09

Registered producer `95d67b03-b3ba-4c0c-b159-4d0a67dd40c2` ran source
`eb05df6d00905e5031f3b3a83072101af948a9ae` in role-account mode with
`--same-account-dry-run`. The staged interpreter ran the staged probe, the owner
step read the role report by handle and wrote the final report, coordinator
artifact `b13c0e79-191e-45e6-9d7d-bf8cb108cdf7`, SHA-256
`1a0d1bd909c3736a51bf29466c2e94b9f639b3a0d5512d671af45e7f0c39852c`.

This is plumbing evidence only. Of 644 mandatory rows, 634 passed. All five
owner controls passed, all 54 owner attack rows were denied and the three
canary-integrity rows passed. `identity.role_account_distinct` failed, as it
must when one account plays both parts, and the registry `Environment` rows
failed as they did in the single-account run. Nothing here shows that a role
account is denied the owner's registry; only the runbook above can.

### Cross-account runs, 2026-11-09

The owner answered decision `df37b339`, created the temporary standard account
`AgentcProbeRole` and followed the runbook twice on MiniAir. The owner step ran
not elevated as a registered producer; the owner started the role run with
`runas`.

**First run: a defect in the prototype.** Registered producer
`da297742-44bf-4bae-a6f9-e87f19ed3073` ran source
`d5d6b1eaeb0a5cb8c47eff0b7c22788586b0a9b7` and exited 1. The report is
coordinator artifact `9b76fb34-b485-4b93-a806-ed0ea55e2d64`, SHA-256
`4dec806133ff7ad51d07511814e2bd9fb5a9fec288529c6d8eca083bd8892cfb`. Of 640
mandatory rows, 243 passed. `identity.role_account_distinct` passed. In each
variant the first launch ran and was denied the owner's `Environment` key with
error 5. The other two launches ended at startup with status `0xC0000142`, so
every row of theirs was missing, and Windows showed an error dialog for each on
the owner's desktop.

The cause was the window station. The prototype asked for an unnamed station
per launch and gave it an access list naming one package. Windows names an
unnamed station after the caller's logon session, so the second and third
requests reached the first launch's station, which did not admit their
packages. This had stayed hidden in every run under the owner's account. There
the same request opens a station that another program in the owner's logon
session already holds, and its access list admits all application packages and
all restricted application packages. That program was not identified; browser
sandboxes make such stations. The earlier `desktop.station` rows compared names
only, so they passed. Those runs did not show a station private to a launch.

**Second run: the gate passes.** Source
`a450451d8c339704d19a7fe254c8a80d8e59a1b5` makes one station per run, admits
each launch's package to it and gives each launch its own desktop. The new row
`desktop.station_made_by_run` fails when the run did not make the station.
Registered producer `c055e4bb-d9fa-432b-8784-cf697ee8db71` ran that source and
exited 0. The report is coordinator artifact
`edc18a08-82ca-46e5-9b51-bccc4652035e`, SHA-256
`a45d77159c770d62ff2ef9ffd396ef34479c5e5dd6567349104c37079ae1ec0f`. It holds
692 rows, of which 645 are mandatory.

| Group | Mandatory rows | Passed | Failed |
| --- | --- | --- | --- |
| Host and owner controls, reader self-test, identity, station, canary integrity | 27 | 27 | 0 |
| Ordinary AppContainer | 309 | 306 | 3 |
| Less-privileged AppContainer, the proposed boundary | 309 | 309 | 0 |

- `identity.role_account_distinct` and `desktop.station_made_by_run` passed.
- All six launches were denied the owner's `Environment` key with error 5.
- All 54 attacks on the owner's files, process, pipe and events were denied,
  and the owner's canaries were exactly as planted afterwards.
- The three failures are the ordinary AppContainer's Task Scheduler rows, as
  in the single-account run: one launch ended with a heap-corruption status
  and two saw `REGDB_E_CLASSNOTREG`. That variant is not the candidate.
- A launch can still open the role account's own `Environment` key. That row
  is informational. It is why a role account must hold no credential and no
  setting worth reading.

Two things in the procedure were looser than it said. In these runs the
staging directory inherited modify access for interactive users, so the role
account could write to the staged tree; the owner step now cuts that
inheritance. And a launch that fails to start shows a system error dialog on
the interactive desktop, although the job suppresses error dialogs for a
launch that runs.

The owner chose to keep `AgentcProbeRole` for the network policy test instead
of removing it as step 5 describes. The staging directory was deleted.

### Network policy dry run, 2026-11-09

Registered producers `2ae92d05-7251-42e4-a15f-a945ab7898e4` (the owner step,
with `--same-account-dry-run` and `--policy-conditions baseline,profiles-off`)
and `f3889e40-7c2e-4407-aafc-bb90d5c6c60a` (the policy step, with
`--observe-only`) ran source `677c2a592ece185063d66a512f84c873b3355278` on
MiniAir, not elevated. The report is coordinator artifact
`8007ea77-2af8-40c5-bcaf-365833d14101`, SHA-256
`faa71a3640c84d307b157c3d22ee47d00e9687248e30e8157d3745f8af782add`.

This is plumbing evidence only. No firewall state was changed, and one account
played every part.

- The `baseline` round ran: the role run read every profile as on, the six
  host-token controls passed, the launch's four TCP attempts were refused with
  error 10013, its lookup failed with error 11003, and no listener received
  anything. The round took about five seconds.
- The `profiles-off` round failed, as it must: the policy step recorded
  `not-applied`, and the role run still read every profile as on.
- `policy.host_firewall_restored` passed, with nothing changed.
- The gate was blocked by those rows and by the rows a same-account run always
  fails.

Nothing here shows what a launch can reach when the firewall policy is lost;
only the run below can.

### Network policy run, 2026-11-09

The owner authorized the conditions `baseline` and `profiles-off`, not
`service-stop`, and followed the
[runbook](#owner-runbook-for-the-network-policy-test) on MiniAir with the
account `AgentcProbeRole` kept from the cross-account runs. Registered
producer `9b6180db-93d3-41d8-bb76-0e1449ffe48a` ran source
`6be041f0d3249b5d2c1dfe2b2af1c651ddd1928a` as the owner step, not elevated,
and exited 0. The owner ran the policy step in an elevated shell and started
the role run with `runas`. The report is coordinator artifact
`a679dd83-2945-4af8-bc93-29d2cfed3467`, SHA-256
`8fb0d0c9243bc8830c96ccc2273df2225f1348ed84c0d0b49096f2645353f44d`. It holds
735 rows, of which 684 are mandatory.

| Group | Mandatory rows | Passed | Failed |
| --- | --- | --- | --- |
| Host and owner controls, reader self-test, identity, station, canary integrity, firewall restore | 28 | 28 | 0 |
| Ordinary AppContainer | 309 | 306 | 3 |
| Less-privileged AppContainer, the proposed boundary, matrix and two network rounds | 347 | 347 | 0 |

The matrix repeated the cross-account result: `identity.role_account_distinct`
and `desktop.station_made_by_run` passed, all 57 owner rows were denied, every
launch was denied the owner's `Environment` key with error 5, and the three
failures are the ordinary AppContainer's Task Scheduler rows.

The `profiles-off` round is what phase 2 was waiting for:

- The policy step recorded `applied: every profile disabled`, and the role
  run's own reading agreed: Domain, Private and Public all off, the firewall
  service running. By the staging directory's timestamps the role run asked
  for the condition at 15:51:32 local time, the policy step recorded it
  applied at 15:51:38, the round ended at 15:51:43 and the restore record
  followed at 15:52:06: the profiles were off for well under a minute.
- All six host-token controls passed in that state: the host reached its
  three TCP listeners and its UDP listener, resolved the public name and
  connected to it.
- The fresh launch inside the proposed boundary was refused exactly as under
  the baseline: TCP to loopback IPv4, loopback IPv6, the local network and the
  public address each failed with error 10013; UDP to loopback failed with
  error 10013; the lookup failed with error 11003. No listener received
  anything.
- `policy.host_firewall_restored` passed: the policy step's record shows
  `profiles-off` as the one change and a final state equal to the original.
  The owner's shell and the agent each read every profile enabled and `MpsSvc`
  running afterwards.

A first attempt the same day, registered producer
`fd7cfac7-bf4f-400f-9d45-a8586aaa1b76`, produced no valid report. The owner
step's one-hour wait for the role run ran out twenty seconds before the
owner's elevated step began, so it reported a missing role run and removed
its canaries, and the rounds that followed ran against a staging directory
nobody was waiting on. The elevated step still restored the firewall, which
was confirmed. The runbook now states the shared clock. The staging directory
was deleted and the run repeated from step 1.

What this establishes: a launch with no network capability, inside the
proposed boundary and running as a separate account, stays refused every
direct TCP, UDP and DNS attempt while every Windows Firewall profile is
disabled, on this host and build. The refusal does not depend on the profile
policy. What it does not establish: behavior when the firewall service itself
is stopped (`service-stop` was not run), behavior of a launch-specific filter
policy or the production egress broker, which do not exist, and anything
about a different host or build.

### Windows backend development runs, 2026-11-09

These runs exercised the [Windows backend](#the-windows-backend) of task
`37788fa5` at source `7c2b0f2cd6237f569e9a3298af1417eeb6603cc2` on MiniAir,
not elevated, as the owner's account. None is a registered producer and none
has a coordinator artifact. They show that the launcher works; they are not
acceptance evidence and they prove nothing about a role account.

- **In the owner's desktop session the launcher refuses.** Another program in
  that logon session had already made the session's window station.
  `prepare` failed with the typed refusal, no profile, directory or journal
  entry was left, and a start was refused. The prototype launcher opened that
  station and went on; the backend does not.
- **In a fresh logon session of the same account the whole path runs.** The
  integration tests started `cmd.exe` under a less-privileged token with
  `registryRead` only. The exit code came back. The process saw exactly the
  three variables it was given plus four the interpreter adds; Windows itself
  pointed `LOCALAPPDATA`, `TEMP` and `TMP` at the package's own folder. A
  write below the working directory succeeded and was read back by handle;
  writes to the control directory and to a host directory failed. A process
  that never exits was reported running, was a member of its job, and was
  gone after the job was terminated. The journal recorded `prepared`,
  `started` and `ended`, and cleanup deleted the profile.
- **The matrix through the launcher matches the prototype's single-account
  result.** The probe driver ran with `--launcher` in a fresh logon session:
  625 rows. For the less-privileged boundary 279 mandatory rows passed and 3
  failed, the three `host.registry_environment_open` rows that every
  same-account run fails. For the ordinary AppContainer 276 passed and 6
  failed: those three and its three Task Scheduler rows. The contained
  `cargo test` and `cargo build`, the handle rows, the orphan rows and the
  direct network rows all passed. No row failed for a reason other than the
  known two.
- **A same-account dry run of the owner's step with `--launcher`** staged the
  launcher, passed it to the role command and failed closed on the window
  station, as it must in the desktop session.

The fresh logon session was made with `CreateProcessWithLogonW` and
`LOGON_NETCREDENTIALS_ONLY`, which keeps the caller's identity and needs no
real credential. It is a development aid, not part of the procedure.

### Role-account run through the launcher, 2026-11-09

This is the acceptance run of task `37788fa5`. Registered producer
`1efaf336-eb85-4220-afd1-11b642f1c197` (job
`fdf66d21-728f-4607-819b-72b9c13a3357`) ran the owner step at source
`c3787158721ea696aab3cf8f6f72e065809f5f85` on MiniAir, not elevated, with
`--launcher`, `--role-account AgentcProbeRole` and
`--policy-conditions baseline,profiles-off`. The owner ran the elevated policy
step and started the role run with `runas`, as the
[network policy runbook](#owner-runbook-for-the-network-policy-test)
describes. The report is coordinator artifact
`6c33cd9d-69c9-42c3-81ca-f44669469450`, SHA-256
`1ff7795187c59db5085407574fbfac0cced695480bb1bc3f6c8eb4a33e62058f`, pinned.

- **Every launch was made by the Rust launcher as the role account.** The
  report records the launcher as `rust-runner`, the staged
  `agentc-supervisor.exe` with SHA-256
  `3ad144e142306492ebe167eef6898ddcf91eb60a94fb2092bbf41d16e07b4130`, which is
  the debug build of that source, and the account and launch identity as the
  SID of `AgentcProbeRole`, a temporary local standard account.
  `identity.role_account_distinct` passed.
- **The gate passed for the less-privileged boundary.** The report has 735
  rows. For that boundary 347 mandatory rows passed and none failed, and the
  28 mandatory host-side rows passed. `host.registry_environment_open` was
  `error:5` on all three launches.
- **Both network rounds ran and were refused.** In the `baseline` round the
  role run read every firewall profile as on; in the `profiles-off` round it
  read every profile as off. In each round the six host-token controls
  passed, the launch's four TCP attempts failed with error 10013, its lookup
  failed with error 11003, and no listener received anything.
- **The firewall was restored.** `policy.host_firewall_restored` passed, the
  policy step's record shows `restored: true`, and every profile read as on
  afterwards.
- **The ordinary AppContainer is still not acceptable.** It failed its three
  Task Scheduler rows and nothing else, as it does under the prototype
  launcher.

No row failed for a reason other than the boundary. An earlier start of the
owner step (job `08514664-68e6-45e1-ab85-b7fd2ad5497c`) exited before staging
anything because the report's directory did not exist; it produced no
evidence.

This shows one run on one host with a temporary standard account started by
`runas` in the owner's interactive session. It does not show a service
account, a supervisor loop or a provider harness.

### Disposition

**NO-GO for Windows supervised execution.** A launch running as the owner can
read the owner's registry, so the single-account gate stays blocked. A launch
running as a separate role account cannot: the role-account run passed the
gate for the proposed boundary, with one temporary standard account, and the
network policy run showed that its network refusal survives the loss of every
firewall profile.

Phase 2 is complete: the owner's
[network policy run](#network-policy-run-2026-11-09) passed. Completing
phase 2 admits nothing; it only let the next phase begin.

Phase 3 was started on the owner's go-ahead of 2026-11-09. Its interface and
both backends are written, and the Windows backend reproduces the prototype's
single-account result. Run as a separate role account, it passed the gate for
the proposed boundary, with a baseline network round and one with the
firewall profiles disabled; see
[Role-account run through the launcher](#role-account-run-through-the-launcher-2026-11-09).
That is phase 3's acceptance run. No service, broker or installer work starts
before the task's review and integration.

Still unverified, and never to be read as passed:

- a mismatch between the boundary asked for and the one Windows applied: the
  refusal is unit-tested on reported facts, and no real launch has produced a
  mismatch;
- a harness that honors the stop marker: `request_stop` only writes it, and
  the fixture is the one program that reads it;
- role service identities: the role account was a temporary standard account
  started with `runas` in the owner's interactive session, not a service
  account;
- clipboard and atom separation between launches of one account, which share
  a window station;
- behavior when the firewall service itself is stopped (the rounds covered
  disabled profiles only), and any launch-specific filter policy, which
  does not exist;
- native provider harness command paths and reviewer candidate interception;
- production egress and publication brokers;
- SCM lifecycle, crash recovery and reboot behavior;
- handle-relative creation and deletion by host brokers inside worker-writable
  trees;
- COM, RPC and broker surfaces beyond the Task Scheduler.

No service, firewall rule, provider login, WSL configuration or production
admission was changed. The owner's network policy run disabled the firewall
profiles for seconds at a time and restored them, as its restore record and
two independent readings confirm.

### Earlier findings, 2026-10-25

The first experiment ran a synthetic C# build under an ordinary AppContainer
and a profile it called LPAC. Registered producer
`4c7bf0b1-2928-42ee-adf0-b5bec54cb344` ran source
`10496fd7e15ea13986517e40eb6b8f0bdc2af95a`; its report is artifact
`ae638761-cbb9-4a30-b05e-206399191900`. The ordinary AppContainer passed that
experiment's small subset.

Its second finding was wrong and is withdrawn. The profile it called LPAC set
process attribute `0x2000E`, the child-process restriction, not `0x2000F`, the
all-application-packages opt-out. The compiler exited with `0xC0000142` because
it could not start a child, not because a less-privileged token cannot run it.
That run says nothing about LPAC. With the correct attribute the
less-privileged token builds and tests Rust, as recorded above.

Attempts to run that experiment's fixture outside an AppContainer timed out for
an unknown reason. They are not successful controls, and the current prototype
does not rely on an uncontained fixture.

## Primary references

- [Launching AppContainer and LPAC processes](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer)
- [AppContainer isolation](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation)
- [AppContainer for legacy applications](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-for-legacy-applications-)
- [Process creation attributes](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute)
- [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)
- [Window stations](https://learn.microsoft.com/en-us/windows/win32/winstation/window-stations)
- [Windows Filtering Platform](https://learn.microsoft.com/en-us/windows/win32/fwp/windows-filtering-platform-start-page)
- [Service control handlers](https://learn.microsoft.com/en-us/windows/win32/services/service-control-handler-function)
- [Interactive service limitations](https://learn.microsoft.com/en-us/windows/win32/services/interactive-services)
