# Native Windows supervisor: design and prototype

Status: **proposed; phase 2 of the containment prototype is complete; phase 3
is under way; not approved for unattended execution**. Task
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
   does not control who may open it and must refuse. A service's logon session
   always has its station already, so **the runner cannot launch as a
   service** with this rule ([4c run](#phase-4c-machine-run-2026-11-11)). Task
   `7f6f242d` added a [take-over of that station](#the-station-of-a-runner-that-is-a-service),
   which the [second 4c run](#phase-4c-second-machine-run-2026-11-12) showed
   Windows does not allow a real service as first written. Task `8981c84a`
   read what that station grants and opens it with those rights only; with
   that, the runner [made a launch as a service](#station-observation-and-step-6-2026-11-12).
   **A station per launch is
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

### The station of a runner that is a service

Task `7f6f242d-716b-47a9-9a66-3be547b7e64f`. Layer 4 above makes the logon
session's unnamed window station and refuses one that exists. The phase 4c
machine run (task `62d9942f`) showed what that means under the Service
Control Manager: a service's logon session already has its station,
`Service-0x0-<logon session>$`, when the service's first instruction runs, so
creation fails with `ERROR_ALREADY_EXISTS` and every launch was refused with
`station_not_ours`. This section records how the runner gets a station whose
access list it controls, and what was considered.

| Mechanism | What it would establish | What it needs | Verdict |
| --- | --- | --- | --- |
| **Take over the session's station after verifying it** | The same as a station the launcher made, from the moment its access list is rewritten: only the runner's account and the packages of its live launches may open it. Nothing about what was in it before | Nothing at installation and no privilege: the checks and the rewrite are the runner's own | **Chosen and implemented.** Demonstrated in a service-like logon session and, since task `8981c84a`, [under the Service Control Manager](#station-observation-and-step-6-2026-11-12) for one launch |
| A named station made by an administrator at installation | A station no other program was ever attached to | A window station exists only while a handle to it is open, so installation cannot leave one behind. A privileged process would have to make it at every boot, keep it open and grant the runner the right to rewrite its access list: a new elevated component | Not chosen, not tried |
| A logon session per launch | A station per launch, so separate clipboards and atom tables: the only mechanism here that would | The runner would create logon sessions, which needs credentials for the account or the privilege to act as part of the operating system. Either changes what the role runner holds | Not chosen, not tried. It stays the route to a station per launch |
| Keep refusing | Nothing new | The runner could not be a service; it would have to be started in a logon session without a station, which is not a boot-time service | Rejected: it gives up phase 4 |

**The chosen mechanism** (`host_runtime::windows::adoption`). When the
station already exists, the launcher opens it with the rights the take-over
needs and no others: to read its name and its security, to rewrite its
access list, and to set its label (`WINSTA_READATTRIBUTES`, `READ_CONTROL`,
`WRITE_DAC`, `WRITE_OWNER`). It uses the station only if Windows grants that
open and confirms all of the following. Otherwise it refuses with
`station_not_ours`, as before. The refusal's text names the fact that was
missing and then [what Windows shows](#what-a-refusal-says) about the
station. A refused station is not changed.

1. Its name is the one Windows gives the unnamed station of this process's
   own logon session, read from the process token.
2. It is the station this process runs in. Windows attaches a service's
   process to its session's station at start; on an interactive desktop a
   process runs in `WinSta0`, so the session station some other program made
   there is refused by this fact alone.
3. Its owner is the runner's account, the system or administrators. An
   owner can always rewrite an access list.
4. No other process is in the logon session. A process there could hold a
   handle that the rewrite below does not take away. Processes are listed
   and each one's token is read; a process Windows will not let the runner
   open is taken to belong to another account.

The launcher then replaces the station's access list with its own, the
runner's account alone, and sets the low integrity label a launch needs to
use the station. It reads the list back and refuses unless Windows reports
exactly that list. It then opens the station again with every right, which
the new list grants its account, and refuses if Windows does not allow
that; the first handle is closed. From then on the station is handled like
one the launcher made: a launch's package is added for the launch's life and
removed at cleanup. A refusal after the rewrite leaves the station with the
launcher's own list, the runner's account alone, which admits nobody new.

**What the Service Control Manager's station is** (task `8981c84a`,
[observed on MiniAir](#station-observation-and-step-6-2026-11-12)). The
first version of the take-over opened the station with every right
(`GENERIC_ALL`, `0x000f037f` for a window station) and a real service was
refused with error 5. The station of `NT SERVICE\agentc-runner`'s logon
session reads:

```text
O:<runner>G:<runner>D:(A;;DCLCSWWPDTSDRCWDWO;;;<runner>)(A;OINPIO;CCDCLCSWDTLOSDRCWDWO;;;<runner>)(A;;CR;;;BA)(A;OINPIO;CCDTLO;;;BA)S:NO_ACCESS_CONTROL
```

| Fact | Observed |
| --- | --- |
| Owner and group | The runner's own service account, `S-1-5-80-2505214551-…` |
| Access list | The runner's account: `0x000f006e`. Administrators: `WINSTA_ENUMERATE` only. Two inherit-only entries for desktops made in it. Nobody else; the logon SID is not listed |
| Label | None |
| Granted to the runner, each right asked for by itself | `0x000f006e`: `DELETE`, `READ_CONTROL`, `WRITE_DAC`, `WRITE_OWNER`, `WINSTA_READATTRIBUTES`, `WINSTA_ACCESSCLIPBOARD`, `WINSTA_CREATEDESKTOP`, `WINSTA_ACCESSGLOBALATOMS`, `WINSTA_EXITWINDOWS` |
| Refused to the runner | `WINSTA_ENUMDESKTOPS`, `WINSTA_WRITEATTRIBUTES`, `WINSTA_ENUMERATE`, `WINSTA_READSCREEN` |
| The runner's process | Runs in that station and is alone in its logon session |

So the runner owns its station and may rewrite its access list and label;
only an open that also asks for the four refused rights fails. The take-over
therefore needs no other account, privilege or object: it asks for what it
uses. All four facts above hold for a real service.

**What this does not establish**, beyond what layer 4 already leaves open:

- Anything about the station's contents from before the rewrite. A process
  that was in the session and has exited may have left atoms or clipboard
  data. Under the Service Control Manager the session belongs to the
  service's virtual account, so that would be an earlier process of the
  runner itself.
- The desktop Windows made in the station (`Default`). Its access list is
  Windows' own and is not rewritten. A launch's package is not on it, and
  no launch has tried to open it.
- A process of the runner's own account that refuses the runner access to
  itself would not be counted in fact 4. Such a process already has
  everything the runner's account has.
- A station per launch. Launches of one runner still share one station.

**Demonstrated or not.**

| Claim | State |
| --- | --- |
| A station made before the launcher ran, in a logon session where the runner's process is alone and attached to it, is taken over; Windows then reports that the station and the launch's desktop admit only the runner's account and that launch's package; a launch runs in it and its exit code comes back; cleanup takes the package off again | **Demonstrated** on MiniAir by the `windows_station` test, in a new logon session of the test's account (network-only credentials). On 2026-11-11 the test's process made the station with Windows' default access list. Since 2026-11-12 it makes it with the security observed above, the test's account in the service account's place, so an open with every right is refused there with error 5 as it was for the real service; the test fails with the first version of the take-over |
| A station the process does not run in, and a session with a second process, are refused; the station's access list is unchanged and no launch directory or journal entry is left | **Demonstrated** by the same test |
| On an interactive desktop the launcher still refuses | **Demonstrated** by `windows_runner`, `windows_host` and `windows_service` in the owner's desktop session |
| Under the Service Control Manager: who owns the service's station and what it grants, that the service's process is alone in its logon session, and that the rewrite, the label and the open with every right afterwards succeed on a station Windows made | **Demonstrated once** on MiniAir on 2026-11-12 ([record](#station-observation-and-step-6-2026-11-12)): the runner, as a service, took its station over and made one launch, whose exit was seen. The [second 4c run](#phase-4c-second-machine-run-2026-11-12) had failed here, with the first version of the take-over. What Windows reports about the station after the rewrite was checked by the launcher's own read-back, not recorded separately |
| Launches after the first, a restart of the runner into a station it already rewrote, and every lifecycle round | In the [third 4c run](#phase-4c-third-machine-run-2026-11-13), 2026-11-13: further launches by the same runner process in its station **demonstrated**, and a launch by a runner that was stopped and started again through the Service Control Manager **demonstrated** once; whether that runner's station was the one it had rewritten was not recorded. The lifecycle rounds are recorded there |

The mechanism needs no installation step and no privilege, so the runbook's
installation steps are unchanged; its step 6 says what to expect now.

#### What a refusal says

Task `8981c84a` (`host_runtime::windows::inspection`). The second 4c run
recorded only an error code. Every refusal to take a station over now ends
with what Windows shows, read without changing anything: the station is
opened by name, which never makes one, with one right at a time.

- the account of the runner's process and the station it runs in;
- the name of the logon session's station and its owner, group, access list
  and label as SDDL, the form `sc.exe sdshow` prints;
- which of the rights `0x000f037f` Windows grants the process on it, each
  asked for by itself, and the names of those it refuses;
- the other processes in the logon session.

```text
the logon session's window station was made by another program (<the fact that was missing>; Windows shows: this process is <account> in <station>; <station> has security "<SDDL>"; of the rights 0x000f037f it grants this process 0x000f006e (not WINSTA_ENUMDESKTOPS, …); other processes in the logon session: []); the launcher starts nothing in a station it does not control
```

It stays a refusal with `station_not_ours`, and a refused prepare still
leaves nothing: the service-like test checks the text, the unchanged access
list and the empty launch root together.

**A refused or failed prepare leaves nothing.** The 4c run also showed that
the runner made a launch's two directories before the station step and left
them when that step was refused. The launcher now removes every directory a
prepare made when a later step of that prepare fails or is refused, and the
runner removes them too when the journal entry cannot be written, so nothing
exists that the journal does not name. Only directories that prepare itself
created are removed, each as the empty directory it still is. The host
runtime's `cleanup` now says whether it found anything to remove, and the
stand-in's ledger records it: `"removed": "launch"`, or `"removed":
"nothing"` when nothing of the launch existed at the runner. A prepare that
fails is recorded as `prepare_result` with its reason.

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
| `desktop` | The logon session's window station, made by the launcher or taken over once verified, and a private desktop per launch |
| `adoption` | The facts Windows must confirm before an existing station is taken over, and the read-back of a station's or desktop's access list |
| `inspection` | What Windows shows about a station the launcher refuses: its security, the rights granted and the session's other processes. Read-only |
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

- A window station the launcher did not make is **refused**, unless
  Windows confirms it was made for the launcher's process alone
  ([the station of a runner that is a service](#the-station-of-a-runner-that-is-a-service)).
  The prototype opened any station and reported a failed row.
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
| `prepare` | Profile, both directories, desktop; a journal entry. A prepare that fails or is refused removes what it made |
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
  interactive desktop, those two check the refusal, and that it left no
  launch directory, and nothing else.
- `windows_station` starts itself again in a new logon session of the same
  account, where the station exists before the launcher runs with the
  security the Service Control Manager gives a service's station, and checks
  the take-over, the two refusals, what each refusal says and what it
  leaves; see
  [the station of a runner that is a service](#the-station-of-a-runner-that-is-a-service).
  A host without the secondary logon service skips it and says so.
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
- The kill switch treats a path it cannot check as set. Windows reports a
  path below a plain file as not found, the same as an absent switch, and
  phase 3 left that one unreadable case admitted there. Task `0092fc75`
  settled it: a switch reported not found counts as absent only when its
  nearest existing ancestor is a directory. Anything else cannot be checked
  and stops claims, on both platforms. Linux already refused the case and is
  unchanged.

### Phase 4a: service lifecycle core, SCM binding and the runner service

Task `e0459185-54d5-464f-a244-162f6fd339d3` built these. Everything here is
**implemented**. **No service was installed, started or configured on any
machine, and nothing here is machine-accepted.** What a service under the
Service Control Manager really does at boot, logoff, stop, preshutdown and
reboot, and after a crash, is phase 4c's owner-run acceptance and is not
claimed. The Windows-only modules (`scm`, `pipe` and `runner_service`, and the
additions to `runner`) were written on a Linux host that has no Windows
compiler. Their first native build and test run was the task's review on
MiniAir on 2026-11-10. They compiled without a change. Five pipe tests failed
for one cause, which the revision fixed: the pipe asked Windows for the
client's account before anything had been read from the connection, and
Windows refuses that with error 1368, so no client could have been admitted.
The same review bounded the waits a client may ask for. The supervisor
crate's tests pass on native Windows after the revision.

**Lifecycle core** (`crates/supervisor/src/service`, platform neutral). It
calls no operating system, so its unit tests run the same on Linux and on
Windows, against a fake clock.

| Piece | What it decides |
| --- | --- |
| `lifecycle` | Running, a stop or preshutdown request that ends admission at once, a drain to a monotonic deadline, termination, and `Stopped { quiescent }`. A preshutdown that arrives during a stop's drain shortens the deadline and never lengthens it. After the deadline a termination has a patience; when it runs out the service stops with `quiescent: false`, which is never made clean later |
| `control` | What the control handler does with a control code: record stop or preshutdown in an atomic signal and answer. Interrogate records nothing; every other control is "not implemented" |
| `drain` | Carries a service's launches through the ending: asks each to stop once, kills one whose root ended, kills what remains at the deadline, and lets the service report stopped in quiescence only when every launch is confirmed ended. Killing does not wait: every remaining launch is killed in one step and later steps only look whether each job has emptied (changed by the second part of 4b; 4a waited for each in turn) |
| `startup` | Judges every journaled launch: one Windows says ended is reclaimed, one that may still run is left untouched and reported, a failed reclaim and an unreadable file are reported. Its journal interface has no way to start a launch |
| `pipe_policy` | The pipe's access list as SDDL text and the decision to admit a client from the facts Windows reported |
| `settings` | The `[windows_service]` configuration |

**SCM binding** (`host_runtime::windows::scm`, `windows-sys`, no new
dependency; the `Win32_System_Services` and `Win32_System_Pipes` features were
added). The hidden subcommand is `agentc-supervisor windows-service`. It
connects to the Service Control Manager, registers a control handler for stop,
preshutdown and interrogate, and runs the runner on the service thread. The
handler records the request, sets the worker's wake event and returns; it does
no work. Status is reported with a checkpoint and a wait hint on every report
while the service is stopping, and the service keeps accepting preshutdown
while it stops. A service that stops with launches Windows did not confirm
ended reports a service-specific exit code of 1; one that could not start
reports 2. Started from a console, the dispatcher fails at once and the
command exits with an error that says it must be started by the service
manager. At start the service tries to set its own preshutdown time limit from
`preshutdown_timeout_seconds`; the account it runs as may not be allowed to,
so a refusal is logged and does not stop the service. Under the SCM in 4c both
services were refused (error 5), so installation sets the limit; see the
[4c runbook](#owner-runbook-for-phase-4c).

**The runner as a service** (`host_runtime::windows::runner_service`). It
starts in this order and reports running only at the end: validate the
settings, make the state directory, try the preshutdown limit, create the
pipe, open the runner and judge the journal. A failure at any step stops the
service. One thread then waits for a client, serves the admitted client one
request line at a time, and steps the service's ending between waits, so a
recorded stop reaches the launches within a quarter of a second. During the
ending, requests that begin a launch (`prepare`, `hold_file`, `start`) are
refused with `refusal: "service_stopping"`; requests about a launch that
exists are still served. A wait a client asks for (`observe`,
`watched_ended`) is cut to a quarter of a second at all times, running or
ending, because a longer one would hold the one thread and delay a stop; the
client sees the launch still running and asks again. At
the deadline what remains is terminated, and a launch is journaled `ended`
only when Windows reported its job empty. The service reports stopped
afterwards. The state directory, by default
`C:\ProgramData\agentc\runner`, is created with an access list for the
service's account, the system and administrators; a directory that already
exists is used as found, and its access list is its installer's. The service
logs to `service.log` there. The service still does not switch accounts: a
launch runs as the account the service runs as. The configuration is found
only through `--config`: there is no default configuration path on Windows,
so the command line a service is registered with has to name the file.

**The pipe** (`host_runtime::windows::pipe`). It is created as the first
instance of its name with one instance in all, so a second server or a
squatter on the name makes creation fail, and with remote clients rejected.
Its access list is protected and has two entries: the service's own account in
full, and the one configured client SID with read data, write data, read
attributes, read the security descriptor (added by 4b so the client can
check the pipe's owner) and synchronize, and without the right to create
pipe instances. A
client therefore has to ask for that access when it opens the pipe (an open
for generic write is refused). A pipe whose list cannot be built or applied is
not created, and there is no fallback to a default list. Windows names the
account of a connection only after something was read from it, so when a
client connects the pipe first takes in the client's first bytes and keeps
them unparsed. It then asks Windows for the client's process id and, while
the serving thread impersonates the connection, for the account of the
connection's token, then reverts; the thread aborts the process if it cannot
revert. A client whose account or process Windows did not report, or whose
account is not the configured client or the service's own, is disconnected
and its bytes are thrown away: no request of its is looked at and nothing is
written to it. A client that connects and sends nothing is disconnected
after ten seconds, unidentified. Requests and answers can only pass through
a session that exists after that check. One client is served at a time; a
client that does not read an answer for ten seconds is dropped.

**Settings.** Every setting has an in-code default. The example
configuration, `deploy/agentc/supervisor-windows.example.toml`, shows each one
commented out with that default, and a test keeps the file equal to the
defaults and complete.

| Setting | Default |
| --- | --- |
| `name` | `agentc-runner` |
| `state_dir` | `C:\ProgramData\agentc\runner` |
| `pipe_name` | `agentc-runner` (the pipe `\\.\pipe\agentc-runner`) |
| `client_sid` | empty: the service refuses to start, so the pipe never serves nobody in particular |
| `stop_grace_seconds` | 30 |
| `preshutdown_grace_seconds` | 60 |
| `preshutdown_timeout_seconds` | 120; must exceed the preshutdown grace plus the termination patience |
| `terminate_patience_seconds` | 20 |
| `status_interval_seconds` | 3 |

**Tests that need no elevation.** The lifecycle, drain, control, start-up and
pipe-policy tests are portable. On Windows, `windows_service` adds: the
console refusal through the real binary; the request gate for every request;
start-up judgment against a journal an ended runner left (ended launches
reclaimed, a live owner and an unknown owner left untouched, a torn file
reported, a journaled id never prepared again); and a stop carried through a
real launch, which on an interactive desktop checks the refusal and nothing
else. The pipe's tests create real pipes in the test's own account: the
same-account client is admitted, a second first instance is refused, a client
the list omits cannot connect, a client the list admits but the policy does not
list is disconnected unserved, a client that stays silent is dropped
unidentified, and the narrow entry alone admits the configured client.
`windows_service` also checks that no wait a client asks for is carried out
longer than a quarter of a second, running or ending.

**Not demonstrated by 4a** (4c's, or later):

- a run under the Service Control Manager, including boot without logon,
  logoff, stop, preshutdown and reboot, and forced service and child crashes;
- that the Service Control Manager delivers preshutdown to a service already
  stopping, and that the account the service runs as may set its own
  preshutdown limit;
- the pipe's access list as the service's real account and a different real
  client account, and a client that really connects from another machine;
- bounded SCM recovery configuration, which is installation's;
- the supervisor service loop over the pipe, a stand-in coordinator driver
  and the owner runbook, which are 4b's.

### Phase 4b, first part: the supervisor's pipe client and remote host runtime

Task `078703b3-dc8c-4153-92e8-b229d3b5d298` built these. Everything here is
**implemented** and tested on native Windows in the test's own account.
**No service was installed, started or configured, and nothing here is
machine-accepted.** The supervisor service role, the stand-in coordinator
driver and the owner runbook are the second part of 4b and do not exist yet.

**The pipe client** (`host_runtime::windows::pipe_client`). The supervisor's
side of the runner's pipe. It opens the pipe asking for exactly the access
the pipe's list grants a client, and lets the server identify it and no more
(identification level), so the runner can learn who connected and cannot act
as the supervisor. Before it writes a byte it asks Windows who **owns** the
pipe and refuses any owner but the configured runner account: only the
process that created a pipe owns it, so a pipe a stranger made under the
runner's name is never sent a request. Ownership is used, and not the token
of the server process, because one service account cannot open another's
process. An exchange is one request line and one answer line. An exchange
that fails leaves the client unusable, because the outcome of the request is
then unknown.

**The remote host runtime** (`host_runtime::windows::remote`). `RemoteHost`
implements the [host-runtime interface](#the-interface) for a supervisor that
holds no launch itself. Each operation is one typed request to the runner;
what a launch runs comes from a `Planner`, as with `WindowsHost`.

| Operation | Request | What an uncertain answer means |
| --- | --- | --- |
| `prepare` | `prepare` | The launch is not prepared; nothing is claimed |
| `start` | `start`, asked once: the command is forgotten before the request goes out | The start failed as far as the loop knows; it is never asked again |
| `observe` | `observe` with no wait | Running |
| `request_stop`, `terminate` | The same | Logged; the launch is not taken to have ended |
| `recover` | `recover`, then `observe` for a launch the runner holds | May be running |
| `cleanup` | `recover`, then `cleanup` or `reclaim` with `remove_directories`; a launch the journal does not name is reported as nothing to remove | Nothing is removed |

`recover` calls a recorded launch ended in three cases only: the runner's
journal does not name it and has no unreadable entry; Windows says an earlier
runner's launch ended; or the running runner holds it and it was never
started or its root process has exited. A runner that cannot be asked, a
refusal and an unreadable entry all leave the launch possibly running, so it
keeps what it holds and blocks new claims, as on Linux.

The runner's journal entry now records a launch's control and working
directories, and its `cleanup` and `reclaim` requests take
`remove_directories`: the directories belong to the runner's account, so the
supervisor's account cannot delete them. A launch the runner could not end
keeps its directories and its entry. The removal is `remove_dir_all` by the
runner's account inside a tree the launch could write; handle-relative
deletion there is still [unverified](#disposition).

**Tests.** The client's tests use real pipes: an exchange with a pipe its
runner owns, the refusal of a pipe owned by another account before anything
is sent, a missing pipe, and a runner that leaves in the middle of an answer.
The runtime's tests use a scripted stand-in runner for every uncertain case
above, and one real runner behind a real pipe, without a launch, so it runs
on an interactive desktop too.

**Not demonstrated**: a client and a runner in two different real accounts;
the ownership check against a pipe a second account really made; a launch
carried through the remote runtime under a role account.

### Phase 4b, second part: the supervisor service, the stand-in and the 4c runbook

Task `49aaca03-8d3b-4fe6-b0c4-4d0de68c7b1e` built these. Everything here is
**implemented** and tested without elevation. That task installed, started
and configured no service. Phase 4c has since followed the runbook below as
far as step 6; the [machine run](#phase-4c-machine-run-2026-11-11) records
what that showed. **Nothing here is machine-accepted.**

**The supervisor service** (`host_runtime::windows::supervisor_service`,
`service::supervisor`). `agentc-supervisor --config FILE windows-service
--role supervisor` runs the existing live loop (`run_loop::run`) under the
4a SCM binding. The loop already ends correctly once its driver says it is
stopping: it claims nothing more, asks a running launch to stop, terminates
it after `[run] drain_seconds`, releases its attempt and returns, and after a
restart it settles what it left through the host runtime's `recover`. The
service's driver, `LoopDriver`, only makes "stopping" follow the stop or
preshutdown the control handler recorded, and reports a rising checkpoint
while the loop drains. The service holds no launch: launches are requests to
the runner over its pipe.

Where the work comes from is one of two things. Neither contacts a
coordinator.

- **No harness** (the default). No provider harness is supported on
  Windows, so the service refuses every claim, says so in its heartbeat
  (`state: "stopped"`, with the reason) and never opens the runner's pipe.
- **The acceptance stand-in** (`service::stand_in`), off unless
  `[windows_supervisor.stand_in] enabled = true`. Work is a directory of
  queued items, one file each. Every launch runs one configured fixture
  command inside a less-privileged boundary with no capability, in two
  directories of its own.

**The ledger.** The stand-in appends every claim, renewal and release, and
every prepare, start, exit, stop request, termination, recovery verdict and
cleanup, to a ledger, one JSON object per line. Each line is flushed to disk
**before** the effect it records, and a claim or a start that cannot be
recorded does not happen. An item is offered until it is claimed and never
again. `agentc-supervisor stand-in-judge --ledger FILE` reads a ledger and
reports a task that was asked to start more than once, a release of a launch
that was started and is not known to have ended, and any unreadable line. It
exits non-zero for any of those and for a ledger with no event.

**A failed prepare** (decided in task `8981c84a`). The loop prepares a
launch before it claims the item, so a prepare the runner refuses claims
nothing and there is nothing to release; the item stays offered. In the
second 4c run that meant the same refusal at every 10-second poll, 26 times
in four minutes. The stand-in now **waits**: after a failed prepare it takes
nothing for 30 seconds, twice as long after each further failure in a row,
and at most ten minutes. A launch that starts ends the row. The wait is read
from the ledger's `prepare_result` lines, so a restart of the service does
not shorten it, and the heartbeat gives it as the reason nothing is claimed
(`the last N prepares in a row failed; nothing is taken for another S s`,
with the refusal's text). Releasing or dropping the item was not chosen: the
stand-in holds no claim to release, and an item that silently leaves the
queue would hide the refusal from an acceptance run. The loop itself is
unchanged, on Linux too; this is the stand-in's rule. It is unit-tested on
both platforms and **has not been seen under the Service Control Manager**:
the build that carries it made no failed prepare there.

A stand-in launch's environment is `SystemRoot` and `LOCALAPPDATA`. It
first held `SystemRoot` alone, and the first stand-in launch to reach
process creation failed with error 203
([record](#station-observation-and-step-6-2026-11-12)), as
[layer 5](#launch-boundary) says a block without `LOCALAPPDATA` does.

**The two 4a advisories.**

- At the drain deadline the runner now kills every remaining launch's job in
  one step and afterwards only looks whether each has emptied, so the waits
  share the service's one deadline and the service keeps reporting. A launch
  Windows never confirms is named in the log when the service gives up.
- The SCM binding makes its wake event before Windows can call the service's
  entry point, so that entry point always reports the service stopped once
  its control handler is registered. A failed start is written to the
  Windows application event log under the service's name, and the supervisor
  service sends its diagnostics to `service.log` in its state directory.

A start whose answer was lost may have happened. The remote runtime then asks
the runner to end the launch, because the loop is about to release the
attempt; the start is still reported as failed.

**Settings.** Every setting has an in-code default and is shown commented out
in `deploy/agentc/supervisor-windows.example.toml`.

| Setting | Default |
| --- | --- |
| `[windows_supervisor] name` | `agentc-supervisor` |
| `state_dir` | `C:\ProgramData\agentc\supervisor` (launch records, heartbeat, `service.log`) |
| `runner_pipe` | `agentc-runner` |
| `runner_sid` | empty: the service refuses to start, since no pipe could be trusted |
| `status_interval_seconds` | 3 |
| `[windows_supervisor.stand_in] enabled` | `false` |
| `queue_dir` | `C:\ProgramData\agentc\stand-in\queue` |
| `ledger` | `C:\ProgramData\agentc\stand-in\ledger.jsonl` |
| `fixture` | empty: the stand-in cannot be enabled without a command |
| `launch_root` | `C:\ProgramData\agentc\runner\stand-in-launches` |

**Tests.** The loop driver and the stand-in are platform neutral and run on
Linux and Windows against a scripted host runtime and a clock that only moves
when the loop waits: without a harness every poll is refused and the
heartbeat is written; the stand-in feeds the loop one item at a time; a stop
ends admission, drains the running launch and returns; and a supervisor that
dies while its launch runs is, after a restart, held back while the launch
may still run, then releases the attempt once it is judged ended and
launches only the other item, with a clean ledger. On Windows the service
command refuses from a console for both roles.

**Not demonstrated by 4b**: either service under the Service Control Manager
(the [4c run](#phase-4c-machine-run-2026-11-11) later started and stopped
both); a stand-in launch carried through a real runner; status reports
reaching Windows during a drain; the event-log trace as a service account
(the test writes one event as the test's own account).

#### Owner runbook for phase 4c

This is what the owner runs on MiniAir from an **elevated** session. Task
`62d9942f` followed it on 2026-11-11 until step 6 stopped the
[machine run](#phase-4c-machine-run-2026-11-11). Steps 1 to 6 and 9 below
are corrected from that run. Task `7b848696` followed it again on 2026-11-12
and was stopped by step 6 a second time
([second machine run](#phase-4c-second-machine-run-2026-11-12)); the
lifecycle rounds of step 7 were not run. Task `8981c84a` ran
steps 1 to 6 and 9 a third time on 2026-11-12, and
[step 6 passed](#station-observation-and-step-6-2026-11-12). Task
`4197c362` ran steps 1 to 7 and 9 on 2026-11-13
([third machine run](#phase-4c-third-machine-run-2026-11-13)): step 6
passed three times; the stop and supervisor-crash rounds passed, the
runner-stop round cannot run as written, the runner-crash round failed, and
the remaining rounds were not run. Stop at the first step that does not do
what it says; do not work around it.

Corrections from the third machine run:

- Step 7, runner stop: Windows refuses `sc.exe stop agentc-runner` while
  the supervisor runs (error 1051), because the supervisor is registered
  with `depend= agentc-runner`. The round cannot run as written; record it
  as not run until it is redefined.
- Step 7, runner crash: until task `76685e6b` is done, this round leaves the
  supervisor holding a launch it can neither settle nor drain, and the
  supervisor then does not stop (`STOP_PENDING`). Run it last; the removal
  then needs a restart first.
- Step 7, every round: `stand-in-judge` saying `ok: true` does not show that
  every claimed attempt was released. Check the round's trace for `release`
  and `cleanup`.
- Step 7, stop: the trace has `exited` (code 1) between `terminate` and
  `release`.
- Step 9: run after a restart with both services stopped, the removal
  removes the two service profiles at once; Windows has not loaded them.

Corrections from the third run of step 6:

- Step 6: a refusal's text now ends with what Windows shows about the
  station; record it whole. After a refusal the stand-in waits before it
  takes the item again, so the ledger no longer grows at every poll.
- An item that was claimed is never offered again, even when its launch
  failed to start; queue a new item for another try.
- Step 9: the two service profiles again stayed loaded after the services
  were deleted and need a restart before they can be removed.

Corrections from the second run:

- Step 6: a refusal's text may also say that Windows refused full access to
  the station (error 5), which is what a real service gets. After a refusal,
  stop the supervisor service before anything else: the stand-in takes the
  refused item again at every poll, so the ledger and the log grow until it
  is stopped.
- The steps were run from scripts that append each native command, its
  output and its exit code to a log file, without `Start-Transcript`. That
  recorded everything.
- Setting the preshutdown limit in step 4, before the first start, worked:
  both services read back 120 000 ms.

Corrections from the run:

- Step 3: the ledger is `stand-in\ledger.jsonl`, so the supervisor's account
  needs modify on `stand-in\` itself; `queue\` inherits it.
- Step 4 gains the preshutdown time limit. Each service tries to set its own
  at start and is refused (`OpenServiceW` error 5: a service's account has no
  `SERVICE_CHANGE_CONFIG` on its own service). That leaves the Windows
  default of 10 seconds, too short for the drain. `sc.exe` has no verb for
  it; the owner sets it through `ChangeServiceConfig2W`
  (`SERVICE_CONFIG_PRESHUTDOWN_INFO`).
- Record native command output to files. In the run, PowerShell 7's
  `Start-Transcript` dropped most of the output of `sc.exe` and `icacls`.
- Step 9: deleting the services leaves each virtual account's profile under
  `C:\Windows\ServiceProfiles\`. Windows kept both hives loaded after the
  services stopped, so the profiles can be removed only after a restart.

1. **Files.** Build `agentc-supervisor.exe` in release mode from the exact
   commit under test and record its SHA-256. Copy it and the probe fixture to
   `C:\Program Files\agentc\`. Copy the example configuration there as
   `supervisor.toml`.
2. **Register the services, stopped.** Each runs as its own virtual service
   account, which Windows creates with the service:

   ```text
   sc.exe create agentc-runner start= demand obj= "NT SERVICE\agentc-runner" binPath= "\"C:\Program Files\agentc\agentc-supervisor.exe\" --config \"C:\Program Files\agentc\supervisor.toml\" windows-service"
   sc.exe create agentc-supervisor start= demand depend= agentc-runner obj= "NT SERVICE\agentc-supervisor" binPath= "\"C:\Program Files\agentc\agentc-supervisor.exe\" --config \"C:\Program Files\agentc\supervisor.toml\" windows-service --role supervisor"
   sc.exe showsid agentc-runner
   sc.exe showsid agentc-supervisor
   ```

   Put the supervisor's SID in `[windows_service] client_sid` and the
   runner's in `[windows_supervisor] runner_sid`.
3. **Directories.** Create `C:\ProgramData\agentc\runner`,
   `...\runner\stand-in-launches`, `...\supervisor` and `...\stand-in\queue`.
   Grant the runner's account modify on the two runner directories and the
   supervisor's account modify on `...\supervisor` and `...\stand-in` (which
   holds the ledger and the queue), and nothing to anyone else beyond the
   system and administrators. Record `icacls` of each.
4. **Bounded recovery and the preshutdown limit.** `sc.exe failure
   agentc-runner reset= 86400 actions= restart/60000/restart/60000//` and the
   same for `agentc-supervisor`: two restarts, then none. Set both services'
   preshutdown time limit to `preshutdown_timeout_seconds` (120 by default)
   through `ChangeServiceConfig2W` and read it back with
   `QueryServiceConfig2W`.
5. **Start without the stand-in.** Start both. Expect: both `RUNNING`; the
   runner's `service.log` says `running`; the supervisor's
   `heartbeat.json` says `state: "stopped"` with the reason that no harness
   is supported; no pipe client appears in the runner's log. Record the
   token privileges and groups of both service processes.
6. **Can the runner launch at all as a service?** A service's logon
   session already has a window station. Since task `7f6f242d` the launcher
   takes it over once
   [Windows confirms it is the runner's alone](#the-station-of-a-runner-that-is-a-service);
   that needs no installation step and made one launch under the Service
   Control Manager on 2026-11-12. Enable the stand-in with the probe fixture, queue one item, and
   watch the ledger. Expect one `prepare`, a `start` and an exit. If the
   launch is refused with `station_not_ours`, record the `prepare_result`
   line, whose text names the fact Windows did not confirm and what Windows
   shows about the station, check that
   `stand-in-launches` is empty and that the `cleanup` line says
   `"removed": "nothing"`, and stop: that is a finding for a new task, not
   something to configure around.
7. **Lifecycle rounds**, each with fresh queued items, each ended by
   `stand-in-judge` on the ledger and by a check that no fixture process, no
   AppContainer profile of the run and no launch directory is left:

   | Round | Action | Expect |
   | --- | --- | --- |
   | Stop | `sc.exe stop agentc-supervisor` during a launch | Admission ends, the launch is asked to stop and ended at the deadline, `STOPPED` with exit code 0 |
   | Runner stop | `sc.exe stop agentc-runner` during a launch | The runner journals the end and stops clean; the supervisor releases the attempt. Windows refuses the stop with error 1051 while the supervisor runs ([third run](#phase-4c-third-machine-run-2026-11-13)) |
   | Supervisor crash | Kill the supervisor process during a launch | Windows restarts it; it claims nothing while the launch may run, then releases and goes on |
   | Runner crash | Kill the runner process during a launch | Windows ends the launch with the job; the restarted runner reclaims it; the supervisor releases |
   | Child crash | Kill the fixture's root process | The launch is seen exited and released; nothing is left in the job |
   | Logoff | Log the owner off and on during a launch | Nothing changes for either service |
   | Reboot | Restart Windows during a launch, with both services set to `auto` | Preshutdown drains within the limit; after boot, without logon, both run and the earlier launch is settled, not started again |
   | Power loss | Reset the machine without shutdown (a virtual machine, if MiniAir may not be reset) | After boot the earlier launch is settled from the journal and the ledger, not started again |

8. **Record** the ledger, both `service.log` files, the heartbeat, the
   application event log entries of both services and the `sc.exe qc` and
   `sc.exe qfailure` output as one pinned artifact, with the commit and the
   binary's hash.
9. **Remove.** `sc.exe delete` both services, delete the directories and the
   files under `Program Files`, and confirm no AppContainer profile named
   `agentc.standin.*` is left. After a restart, once Windows reports them
   unloaded, remove the two virtual accounts' profiles (`Win32_UserProfile`
   for each service SID).

Not covered by this runbook and still open after it: a real coordinator, any
provider harness, the reviewer side, brokers, the installer, and resource
caps and quiet hours, which need the real loop's admission to mean anything.

### Phase 4c machine run, 2026-11-11

Task `62d9942f-7b8c-4f0c-b300-bc5553c7aa15`. The owner ran every step that
registered, started, stopped or removed a service in an elevated PowerShell 7
session on MiniAir (Windows 11 Pro 10.0.26300); the agent session prepared
the scripts, read the results and wrote this record. `agentc-supervisor.exe`
was built in release mode from `1d2a73eb0ca4393aec8a781519eb9d19a84322a8`,
SHA-256 `57b5440d4b0f2f55b0ebaa14d789eebe69d94630cc706333cb9263f2f63d9336`;
the probe fixture's SHA-256 was
`ec304c355a25ff805b3dffe2756da8984e21ecc8d6991049c9b1834172448ab6`. The
evidence is coordinator artifact `8b0c1cca-fc30-49d1-9912-62f02ea216aa`, SHA-256
`84d3b5671279c1d8bb9c055f8fd61c3f57b05fea709530193bdf920e58583181`: one zip
of the run's logs, ledgers, transcripts and removal records with the owner
scripts, packed by registered job `37bb68a2-d98a-40d7-a9a9-98b6ea97cae6`.
All times are UTC.

**Install (runbook steps 1 to 4), 17:43.** Both services registered stopped,
demand start, as `NT SERVICE\agentc-runner` and `NT SERVICE\agentc-supervisor`,
the supervisor depending on the runner, each with two 60-second restarts and
a one-day reset. The service SIDs matched the ones in the configuration. The
directories' access lists were exactly as step 3 asks, with the correction
above. Both services' security descriptors grant service logon users only
query, interrogate and read-control, which is why neither service can set its
own preshutdown limit. Read back before it was set, the effective limit was
10 000 ms on both; the owner set 120 000 ms and it read back as set.

**Start without the stand-in (step 5), 18:20: passed as written.** Both
`RUNNING`. The runner's log said `start-up: 0 reclaimed, 0 may still run, 0
failed, 0 unreadable` and `running`, and showed no pipe client. The
supervisor's heartbeat said `state: "stopped"` with the reason that no
provider harness is supported on Windows. Both service tokens: their own
service SID as user, High integrity, session 0, `SERVICE`, `ALL SERVICES`,
`Users`, `Authenticated Users`, `CONSOLE LOGON` and their logon session; no
administrators group. Privileges: `SeChangeNotify`, `SeImpersonate` and
`SeCreateGlobal` enabled; `SeShutdown`, `SeUndock`,
`SeIncreaseWorkingSet` and `SeTimeZone` present and disabled.

**Can the runner launch as a service (step 6), 18:36: no.** With the stand-in
on and one item, `probe-1`, queued, the supervisor connected to the runner's
pipe and the runner served it only after checking its SID (`serving
S-1-5-80-3306791472-…`). Every prepare was then refused: *the logon session's
window station was made by another program; the launcher starts nothing in a
station it does not control*. The launcher only accepts the unnamed station
of its logon session if it creates it itself (`CreateWindowStationW` with
`CWF_CREATE_ONLY`); a service's logon session already has one when the
service starts, so creation fails with `ERROR_ALREADY_EXISTS`. The phase 2 and
3 role-account runs never met this because their logon session had no
station yet. The run stopped here as the task requires.

The same step showed a second defect. The runner makes a launch's `-control`
and `-work` directories before the station step, journals nothing when the
step fails and does not remove them. The remote runtime's cleanup then finds
no journal entry, removes nothing, and the ledger still records `cleanup` with
`ok: true`. The item is never claimed, so the loop prepares it again at every
poll: from 18:36:18 to 18:43:31 the ledger holds 44 prepares and 44 cleanups
and 88 launch directories were left. `stand-in-judge` found no problem in the
ledger, which is right for what it judges: nothing was started or released.
No fixture process and no AppContainer profile was left.

**Stop and removal, 18:43.** Both services stopped with exit code 0; the
runner logged `the client is gone` and `stopped: every launch is confirmed
ended`. The owner deleted both services, `C:\Program Files\agentc` and
`C:\ProgramData\agentc`, which removed the leftover directories. The two
virtual accounts' profiles stayed: Windows still had both hives loaded, and
a removal tried at 18:52 was refused for that reason. MiniAir was restarted
at 18:55; at 19:02 Windows reported both profiles unloaded and the owner
removed them. No service, profile or profile folder of either account is
left (`removal-profiles.json`).

| Item | Result |
| --- | --- |
| Registration under two virtual service accounts, failure actions, access lists | **Demonstrated** |
| Both services start, report running and stop with exit code 0 under the SCM | **Demonstrated** (demand start, stopped by `sc.exe stop`) |
| Supervisor without a harness claims nothing and says why | **Demonstrated** |
| The runner's pipe admits the supervisor by SID across two service accounts | **Demonstrated** |
| The services set their own preshutdown limit | **Failed**: refused with error 5; set by the owner instead |
| A launch made by the runner as a service | **Failed**: `station_not_ours`; follow-up task `7f6f242d` |
| A refused prepare leaves nothing behind | **Failed**: two directories per attempt; follow-up task `7f6f242d` |
| Rounds: supervisor stop, runner stop, supervisor crash, runner crash, child crash, logoff, reboot with preshutdown, power loss | **Not run**: no launch can be made |
| Boot without logon, orphan cleanup of launches | **Not run** |

### Phase 4c second machine run, 2026-11-12

Task `7b848696-e197-44b4-a273-e9b4a3c161e4`, on MiniAir (Windows 11 Pro
10.0.26300), after task `7f6f242d` put the window-station take-over on
`main`. `agentc-supervisor.exe` was built in release mode from
`d36ceffdbd2a34126084f148dc9afe869a970c3d`, SHA-256
`6436dd555dce83c9ea16bca3377b4abf632f6902255d572eceec3a921cc3afd0`; the
probe fixture's SHA-256 was
`d8f1cc8f1ce88b748c66413e6fb90cf0a5767375759145b40036d743a056231b`. The
machine had no `agentc` service, directory or service profile before the
run. The evidence is coordinator artifact
`ad0cb3e1-779d-4383-a2c1-95f838530fb7`, SHA-256
`ae9216cff929c30fe90556f103b648e2b23c87395af8b0d9757883e925ec76f7`: one zip
of the run's logs, ledgers, registration and removal records with the
scripts. All times are UTC; the run was on the evening of 2026-11-11 local
time.

Who ran what: the owner ran runbook steps 1 to 6 as one script in an
elevated PowerShell 7 session. The stop, the final evidence copy and the
removal were run by the agent session, which was elevated, after the owner
said in the session that it may run that script. The owner restarted
MiniAir; the profile removal after it was run by the agent session, again
elevated, when the owner told it to continue the task.

**Install (steps 1 to 4), 03:51.** Both services registered stopped, demand
start, under their virtual accounts, the supervisor depending on the runner,
with two 60-second restarts and a one-day reset. The service SIDs matched
the configuration. The directories' access lists were as step 3 asks: the
system and administrators with full control, the runner's account with
modify on `runner\` and `runner\stand-in-launches\`, the supervisor's with
modify on `supervisor\` and `stand-in\`. The owner's script set the
preshutdown limit of both services to 120 000 ms before the first start and
it read back as set. At start each service still tried to set its own and
logged the refusal (`OpenServiceW` error 5), as in the first run.

**Start without the stand-in (step 5), 03:51: passed as written.** Both
`RUNNING`; the runner logged `start-up: 0 reclaimed, 0 may still run, 0
failed, 0 unreadable` and `running`; the supervisor refused every claim
because no provider harness is supported on Windows. Both tokens were
recorded (`tokens.json`).

**Can the runner launch as a service (step 6), 03:52: no.** The supervisor
was restarted with the stand-in on and one item, `probe-1`, queued. The
runner served it after checking its SID. Every prepare was refused:

```text
the runner refused "prepare": the logon session's window station was made by another program (Windows refused full access to it with error 5); the launcher starts nothing in a station it does not control
```

The station exists, as in the first run. The take-over then opens it with
every right, and Windows denied that to `NT SERVICE\agentc-runner`. The
four facts of the
[take-over](#the-station-of-a-runner-that-is-a-service) were therefore
never checked, and the run recorded only the error code: the station's
owner and access list are not known. The run stopped here as the task
requires; follow-up task `8981c84a-8275-4cca-9018-b7a760b1f582`.

The refusal left nothing, which the first run did not show. From 03:52:29
to 03:56:41 the ledger holds 26 `prepare`, 26 `prepare_result` with `ok:
false` and the text above, and 26 `cleanup` with `ok: true` and `"removed":
"nothing"`. `stand-in-launches` was empty at both checks, no fixture process
and no AppContainer profile or folder of the run existed, and
`stand-in-judge` found no problem. The item was taken again at every
10-second poll, each time as a new session and without a claim line; whether
a refused prepare should back off is part of the follow-up task.

**Stop and removal, 03:56.** `sc.exe stop` ended the supervisor and then
the runner, both with exit code 0; the runner logged `the client is gone`
and `stopped: every launch is confirmed ended`. Both services,
`C:\Program Files\agentc` and `C:\ProgramData\agentc` were then deleted and
confirmed gone (`removal.json`). Windows still had both virtual accounts'
profiles loaded. After the owner's restart at 04:02, the profile removal at
04:08 removed both profiles, which the run had created at 03:51, and found
no `agentc` service, profile or profile folder left
(`removal-profiles.json`).

| Item | Result |
| --- | --- |
| Registration from the recorded commit and hash under two virtual service accounts, failure actions, access lists | **Demonstrated** again |
| Preshutdown limit set at installation by the owner and read back | **Demonstrated** (120 000 ms on both) |
| Both services start, run and stop with exit code 0 under the SCM; the pipe admits the supervisor by SID | **Demonstrated** again |
| A launch made by the runner as a service | **Failed**: Windows refuses the runner full access to its session's window station (error 5); follow-up task `8981c84a` |
| A refused prepare leaves nothing behind and the ledger says so | **Demonstrated** under the SCM: 26 refusals, nothing left |
| The four facts of the take-over under the SCM | **Not reached** |
| Rounds: supervisor stop, runner stop, supervisor crash, runner crash, child crash, logoff, reboot with preshutdown, power loss | **Not run**: no launch can be made |
| Boot without logon, orphan cleanup of launches | **Not run** |

### Station observation and step 6, 2026-11-12

Task `8981c84a-8275-4cca-9018-b7a760b1f582`, on MiniAir (Windows 11 Pro
10.0.26300), on the afternoon of 2026-11-12 local time. It is not the third
4c run: it repeats runbook steps 1 to 6 and 9 to read the station of a real
service and to try the take-over made from that reading. No lifecycle round
was run. The machine had no `agentc` service, directory or service profile
before the run. The evidence is coordinator artifact
`646343e9-1e07-455c-8a88-a9ae0afd000f`, SHA-256
`11df19cadfaa9917e6ebaf097308370b9f2cbf10214899576b05373b36037306`: one zip
of the run's logs, ledgers, registration and removal records with the
scripts, packed by registered job `b15bfb73-d120-4073-aa67-30822592fd67`.
All times are UTC.

Who ran what: every script was run by the agent session, which was
elevated, after the owner said in the session, each time it was asked, that
it may: the install and first step 6, each replacement of the binary, and
the removal. The owner has not restarted MiniAir, so the two service
profiles are still there.

Three release builds of `agentc-supervisor.exe` were used, each from a clean
checkout of the commit named; the probe fixture's SHA-256 was
`1d8adbcaeecdaeb19421f9c61d1fa628e8ed443ff65affd554b84be2c3a3e168`.

| Build | Commit | SHA-256 | What it adds |
| --- | --- | --- | --- |
| Diagnostic | `e0219df4908d4387e5d11f9f5ab51775fbf145f5` | `19cbccd8b008aac432e66126964963beed3209c3f9b62e37d1385781e9cd4d1f` | The refusal says what Windows shows |
| Take-over | `a08f29909c6072387f9f9be8ab887368ce81733d` | `20f7a68dc6214f4a99e17f77569893a45e4035b8bdf79e19e83ac0b7b01409b3` | The station is opened with the rights its owner is granted; the stand-in's wait |
| Environment | `ce61cc92fa2ac74a94a628a48130cc1d35eae4f7` | `03bfad6642afe286d6fc8295ec04648947b9819be79a2a703eb2a9fe71604520` | `LOCALAPPDATA` in a stand-in launch's environment |

**Install and start (steps 1 to 5), 19:00, diagnostic build.** As in the
second run: both services registered stopped under their virtual accounts,
the access lists as step 3 asks, the preshutdown limit set to 120 000 ms and
read back, both `RUNNING`, tokens recorded (`tokens.json`).

**Step 6 with the diagnostic build, 19:00: refused, and the refusal says
why.** With the stand-in on and `probe-1` queued, both prepares before the
supervisor was stopped were refused with:

```text
the runner refused "prepare": the logon session's window station was made by another program (Windows refused full access to it with error 5; Windows shows: this process is S-1-5-80-2505214551-3868131734-2396360996-2952680925-136825992 in Service-0x0-c86f2ae$; Service-0x0-c86f2ae$ has security "O:S-1-5-80-2505214551-3868131734-2396360996-2952680925-136825992G:S-1-5-80-2505214551-3868131734-2396360996-2952680925-136825992D:(A;;DCLCSWWPDTSDRCWDWO;;;S-1-5-80-2505214551-3868131734-2396360996-2952680925-136825992)(A;OINPIO;CCDCLCSWDTLOSDRCWDWO;;;S-1-5-80-2505214551-3868131734-2396360996-2952680925-136825992)(A;;CR;;;BA)(A;OINPIO;CCDTLO;;;BA)S:NO_ACCESS_CONTROL"; of the rights 0x000f037f it grants this process 0x000f006e (not WINSTA_ENUMDESKTOPS, WINSTA_WRITEATTRIBUTES, WINSTA_ENUMERATE, WINSTA_READSCREEN); other processes in the logon session: []); the launcher starts nothing in a station it does not control
```

What that means is set out under
[the station of a runner that is a service](#the-station-of-a-runner-that-is-a-service):
the runner owns its station and may rewrite its access list and label. Each
refusal left nothing: `cleanup` said `"removed": "nothing"`,
`stand-in-launches` was empty and `stand-in-judge` found no problem. This
build did not yet have the stand-in's wait, so the item was taken again
after 10 seconds.

**Step 6 with the take-over build, 19:08: the station was taken over; the
launch failed to start for another reason.** Both services were stopped,
the binary replaced and both started. The prepare succeeded, which it never
had under the Service Control Manager, `probe-1` was claimed, and the start
was refused: `CreateProcessW failed with Windows error 203`. The stand-in's
environment held only `SystemRoot`. The runner ended what it had made, the
attempt was released and `cleanup` said `"removed": "launch"`; nothing was
left.

**Step 6 with the environment build, 19:13: passed.** With a new item,
`probe-2`, the ledger holds `prepare`, `claim`, `start`, `start_result`
with `ok: true` (process 20048), `exited` with code 0 after 21 seconds (the
fixture sleeps 20), `release` and `cleanup` with `"removed": "launch"`.
`probe-1`, claimed before, was not offered again. `stand-in-launches` was
empty, no fixture process and no AppContainer profile or folder of the run
existed, and `stand-in-judge` found no problem.

**Stop and removal, 19:21.** Both services stopped; the runner logged
`stopped: every launch is confirmed ended` each time it was stopped. Both
services, `C:\Program Files\agentc` and `C:\ProgramData\agentc` were deleted
and confirmed gone (`removal.json`). Windows still had both virtual
accounts' profiles loaded, so they were **not removed**; `profiles.ps1` of
the kit removes them after a restart.

| Item | Result |
| --- | --- |
| The owner, access list, label and granted rights of a real service's window station | **Recorded** from the runner's own refusal |
| The refusal names those facts and leaves nothing behind | **Demonstrated** under the SCM |
| The take-over under the SCM: the open with the owner's rights, the four facts, the rewrite, the read-back and the open with every right | **Demonstrated** for one runner start and one launch |
| A launch made by the runner as a service: prepare, start, exit seen, release, cleanup | **Demonstrated** once (`probe-2`, exit code 0) |
| A failed start after a claim is released and cleaned up | **Demonstrated** once (`probe-1`, error 203) |
| The stand-in's wait after a refused prepare | **Not seen** under the SCM: no prepare failed with a build that has it |
| A second launch in the same station, a runner restarted into a station it rewrote | **Not run** as such. The runner was restarted between builds, each time into a new logon session |
| Rounds: supervisor stop, runner stop, supervisor crash, runner crash, child crash, logoff, reboot with preshutdown, power loss | **Not run**; the [third 4c run](#phase-4c-third-machine-run-2026-11-13) is its own task |
| Removal of the two service profiles | **Done** on 2026-11-13 at 02:24 UTC, after the owner restarted MiniAir, by task `4197c362` before its install |

### Phase 4c third machine run, 2026-11-13

Task `4197c362-2bd5-4ad0-8d66-170984a12094`, on MiniAir (Windows 11 Pro
10.0.26300). `agentc-supervisor.exe` and the probe fixture were built in
release mode (`--locked`) from a clean checkout of `main` at
`103e640a304355190e8f8be9c9d4ac8934629d3b`, which contains task
`8981c84a`'s result. The binary's SHA-256 was
`18d0204b8333e0bfee3256225b60f142a6473edf321776c9690a719e7daa8cd3`; the
fixture's was
`2c650d2906e137d9504650d343337f4007650ca084d9055690bd251229755a85`. Before
the install the machine had no `agentc` service, directory or service
profile. The evidence is coordinator artifact
`20c5927b-dfb6-4c12-9640-cfbbbc55c20e`, SHA-256
`033a333f4cd483c46cc3088cb6d1015c87dbabb985362c72a7d9adb140855817`: one zip
of the run's logs, ledgers, both service logs, heartbeats, the services'
application and Service Control Manager event log entries, `sc.exe qc` and
`qfailure` output, token privileges and groups, `icacls` output, the
manifest with the commit and hashes, and the scripts, packed by registered
job `7629a914-c0b7-4ba2-8cf9-37ca4e16ade6`. All times are UTC; the rounds
ran in the night of 2026-11-12 to 13 local time.

Who ran what: every script ran in an elevated session on MiniAir, each only
after the owner's go-ahead in the agent session. The owner restarted
MiniAir before the install and once more before the removal, and decided to
stop the rounds at the runner-crash finding.

**Install (steps 1 to 5), 05:14: passed as written.** The installed hashes
matched the build. Both services registered stopped, demand start, under
their virtual accounts, the supervisor depending on the runner, with two
60-second restarts and a one-day reset. The preshutdown limit was set to
120 000 ms on both before the first start and read back. The directories'
access lists were as step 3 asks. Both `RUNNING` with the stand-in off
(runner process 15212, supervisor 19036), tokens recorded (`tokens.json`),
and no fixture process, AppContainer profile or launch directory. As in the
earlier runs, every start of the runner logs `preshutdown time limit not
set: OpenServiceW failed with Windows error 5`; the limit set at
installation stays in place.

**Step 6, 05:23: passed three times.** With the stand-in on, each item
traced `prepare claim start start_result exited release cleanup`, its
cleanup saying `"removed": "launch"`. `s6-first` and `s6-same-station` were
made by runner process 15212, the second in the station the first had taken
over. Both services were then stopped and started through the Service
Control Manager, and the new runner process, 6528, made `s6-after-restart`.
`stand-in-judge` found no problem and nothing was left.

**Stop round, 06:17: passed.** `sc.exe stop agentc-supervisor` 11 seconds
into a launch. Trace: `prepare claim start start_result stop_requested
terminate exited release cleanup`. The fixture ignores a stop request, so it
was ended at the 30-second drain deadline and exited with code 1; the
release says the launch exited after a drain because the host supervisor
stopped. The supervisor was `Stopped` with exit code 0, the judge found no
problem and nothing was left.

**Runner-stop round, 06:18: not run as written.** `sc.exe stop
agentc-runner` during a launch was refused:

```text
[SC] ControlService FAILED 1051:
A stop control has been sent to a service that other running services are dependent on.
```

The supervisor depends on the runner, so Windows does not stop the runner
while the supervisor runs. Nothing was stopped. The round's 180-second
launch ran to its end with code 0 at 06:21:39 and was released and cleaned
up; a check at 06:21:48 found nothing left.

**Supervisor-crash round, 06:22: passed.** The supervisor's process
(17088) was killed during a launch and a second item was queued. Windows
restarted the supervisor about 60 seconds later (process 15380, connected
to the runner at 06:23:21). It then recorded `recover` with
`may_be_running` every 10 seconds while the fixture ran, eleven times,
`ended` at 06:25:11 when the 180-second fixture ended, then `release` and
`cleanup` with `"removed": "launch"`. Only then was the second item
claimed; it exited with code 0 and was released. The judge found no problem
and nothing was left.

**Runner-crash round, 06:28: failed.** The runner's process (6528) was
killed during launch `5a5fd655-c78f-461b-bdc0-248a2578f456`. The fixture was
gone within 3 seconds, and Windows restarted the runner about 60 seconds
later (process 15056), which logged `start-up: 1 reclaimed` and `reclaimed
5a5fd655-…`. The supervisor never released the attempt. Its log repeats,
about once a second:

```text
agentc-supervisor run: observe the launch: the runner refused "observe": launch 5a5fd655-c78f-461b-bdc0-248a2578f456 is not prepared
```

It renewed the dead launch's attempt every 60 seconds until its 240-minute
budget was spent at 10:28. Its drain then failed as well: the restarted
runner refused `request_stop` (`is not prepared`) and `terminate` (`was not
prepared by this runner`), and the supervisor went back to observing. Its
launch record (`released: false`, with the job of runner process 6528) and
the launch's `-control` and `-work` directories were left. No fixture
process and no AppContainer profile of the run was left. `stand-in-judge`
still found no problem: it judges what the ledger holds, and an attempt
that is never released breaks none of its rules.

**Child-crash, logoff, reboot and power-loss rounds: not run.** The owner
stopped the rounds at the runner-crash finding.

**Removal (steps 8 and 9), 18:36 and 18:54.** The first removal stopped at
its first action. `sc.exe stop agentc-supervisor` was accepted, but the
service stayed `STOP_PENDING` past 180 seconds with its checkpoint still
advancing: its stop waits on the same launch. Nothing was removed or worked
around; both service logs, the heartbeat, the ledger and `sc.exe queryex`
were saved. On the owner's decision MiniAir was restarted (a Restart, at
18:46); both services are demand start and stayed stopped. The removal run
again at 18:54 found both services stopped and recorded their
configuration. Its final check found no fixture process, the judge no
problem, and the two launch directories of `5a5fd655` still there. It
deleted both services, `C:\Program Files\agentc` and
`C:\ProgramData\agentc`, and removed both service profiles, which the run
had created at 05:14 and Windows had not loaded since the restart. No
service, directory, profile, profile folder or fixture process is left
(`removal.json`). The runner's hive was not loaded at the final check, so
its AppContainer mappings could not be listed; the hive went with the
runner's profile.

The findings are task `76685e6b-b701-4f61-a95a-e8289c4673a5`: settle a
launch the restarted runner no longer knows, let the supervisor stop while
such a launch is outstanding, and the runner's refused preshutdown limit.

| Item | Result |
| --- | --- |
| Registration from the recorded commit and hash under two virtual service accounts, failure actions, preshutdown limit, access lists | **Demonstrated** |
| Step 6 under the SCM: prepare, start, exit seen, release, cleanup | **Demonstrated** three times |
| Further launches in the same station by the same runner process | **Demonstrated**: `s6-same-station`, and every round after step 6 reused runner process 6528 |
| A launch after the runner was stopped and started through the SCM | **Demonstrated** once (`s6-after-restart`) |
| A launch by a runner the SCM restarted after a crash | **Not reached**: the runner-crash round failed first |
| Round: supervisor stop | **Passed** |
| Round: runner stop | **Not run as written**: Windows refuses with error 1051 while the supervisor runs |
| Round: supervisor crash | **Passed** |
| Round: runner crash | **Failed**: the launch ended and the restarted runner reclaimed it, but the supervisor never released the attempt; task `76685e6b` |
| Rounds: child crash, logoff, reboot with preshutdown, power loss | **Not run**: stopped at the runner-crash finding on the owner's decision |
| The supervisor stops on request while a launch it cannot settle is outstanding | **Failed**: `STOP_PENDING` past 180 seconds, cleared only by a restart; task `76685e6b` |
| `stand-in-judge` catches an attempt that is never released | **No**: it found no problem after the runner-crash round |
| The stand-in's wait after a refused prepare | **Not seen**: no prepare was refused |
| Boot without logon, orphan cleanup of launches | **Not run** |
| Removal of both services, both trees and both service profiles | **Demonstrated**, after one restart |

## Delivery phases and gates

Each phase is an independently reviewed task. A phase starts only when the gate
before it has passed; a blocked gate stops the phases behind it.

| Phase | Content | Gate to start | State |
| --- | --- | --- | --- |
| 1 | This design and the containment prototype | None | Delivered by task `a9126e52`; the prototype gate is **blocked** |
| 2 preparation | A role-account mode for the prototype and the owner runbook | Owner decision `df37b339` | Delivered by task `3787e738` |
| 2 | The same matrix with launches running as a dedicated role account, plus network policy loss | An owner-controlled elevated session on the target | **Complete.** The role-account matrix **passed** on 2026-11-09 after a fix to the prototype's window station. The network policy loss test (task `96d4927e`) **passed** in the owner's run with the prototype launcher on 2026-11-09; task `4baa4706` records it under [Network policy run](#network-policy-run-2026-11-09). The same runbook, followed with the Rust launcher in phase 3, passed |
| 3 | Shared host-runtime interface and a Windows launcher that reuses this boundary and fixture | Phase 2 gate passes | Started by task `37788fa5` on the owner's go-ahead of 2026-11-09, after the owner's network policy run that task `4baa4706` records. The interface, the Linux backend behind it and the Windows backend are written and tested. The acceptance run through the new launcher under a role account, with the two network rounds, **passed** on 2026-11-09. The phase is complete when the task's review and integration are |
| 4a | Service lifecycle core, SCM binding and the role runner as a service behind a secured named pipe | Phase 3 | Implemented by task `e0459185`; see [Phase 4a](#phase-4a-service-lifecycle-core-scm-binding-and-the-runner-service). **No service was installed and nothing is machine-accepted** |
| 4b | The supervisor's pipe client and remote host runtime; then the supervisor service loop on Windows over that pipe, a stand-in coordinator driver for acceptance and the owner runbook | 4a | Implemented. First part, task `078703b3`: the [pipe client and the remote host runtime](#phase-4b-first-part-the-supervisors-pipe-client-and-remote-host-runtime). Second part, task `49aaca03`: the [supervisor service, the stand-in and the proposed 4c runbook](#phase-4b-second-part-the-supervisor-service-the-stand-in-and-the-4c-runbook). **No service was installed and nothing is machine-accepted** |
| 4c | Owner-run machine acceptance: boot without logon, logoff, stop, preshutdown and reboot, forced service and child crashes, orphan cleanup, and its record | 4a and 4b | Run by task `62d9942f` on 2026-11-11 and **stopped at runbook step 6**: both services install, run and stop under the Service Control Manager, but the runner **cannot make a launch** as a service (`station_not_ours`). No lifecycle round was run. Task `7f6f242d` fixes the window station and a refused prepare's leftover directories; see [Phase 4c machine run](#phase-4c-machine-run-2026-11-11). Run again by task `7b848696` on 2026-11-12 with that fix and **stopped at step 6 again**: Windows refuses the runner's account full access to its window station, so the take-over refuses; a refused prepare now leaves nothing. See the [second machine run](#phase-4c-second-machine-run-2026-11-12); task `8981c84a` followed. Run a third time by task `4197c362` on 2026-11-13 from `103e640`: step 6 **passed** three times; the stop and supervisor-crash rounds **passed**, the runner-stop round **cannot run as written** (error 1051), and the runner-crash round **failed**: the supervisor never releases a launch the restarted runner reclaimed, and then does not stop. Child crash, logoff, reboot and power loss were not run. See the [third machine run](#phase-4c-third-machine-run-2026-11-13); task `76685e6b` follows. **Nothing is machine-accepted** |
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
- the take-over of a service's window station under the Service Control
  Manager beyond [one runner start and one launch](#station-observation-and-step-6-2026-11-12):
  no second launch, no restart into a rewritten station and no lifecycle
  round has run;
- behavior when the firewall service itself is stopped (the rounds covered
  disabled profiles only), and any launch-specific filter policy, which
  does not exist;
- native provider harness command paths and reviewer candidate interception;
- production egress and publication brokers;
- SCM lifecycle, crash recovery and reboot behavior: phases 4a and 4b
  implemented the lifecycle and both services. The
  [phase 4c run](#phase-4c-machine-run-2026-11-11) showed only install, start
  and clean stop under the Service Control Manager; it made no launch, so no
  lifecycle round, crash recovery or reboot has been run;
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
