# Native Windows supervisor: design and prototype

Status: **proposed; the containment prototype gate is blocked; not approved for
unattended execution**. Task `7011c55f-652e-47d3-a409-7d46420c7eb6` activates a
native Windows replacement for the abandoned WSL host approach. Its first
phase, task `a9126e52-654c-4b26-8a7d-7748261d2c4a`, delivers this design and the
containment prototype. This chapter does not claim that Windows services or a
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
window station shares its clipboard, atoms and window messages.

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
4. **Private window station and desktop (demonstrated).** Each launch gets its
   own, with an access list naming only the service owner and that package.
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
address or the resolver. What happens when the firewall service or its policy
is lost is untested and needs a privileged session.

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
| `check_codex_sandbox`, `check_claude_sandbox` | Partial. Own writes work; host and cross-launch files, processes, pipes, events and the interactive window station are denied. **The owner's registry is not denied**: `host.registry_environment_open` fails |
| `check_egress` | Partial. Direct TCP over IPv4 and IPv6, loopback, the local network, UDP and DNS are denied to a token with no network capability, and the authorized pipe route works. No WFP policy, allowlist broker or policy-loss test exists |
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

## Delivery phases and gates

Each phase is an independently reviewed task. A phase starts only when the gate
before it has passed; a blocked gate stops the phases behind it.

| Phase | Content | Gate to start | State |
| --- | --- | --- | --- |
| 1 | This design and the containment prototype | None | Delivered by task `a9126e52`; the prototype gate is **blocked** |
| 2 | The same matrix with launches running as dedicated role accounts, plus network policy loss | An owner-controlled elevated session on the target | Not started; needs the owner |
| 3 | Shared host-runtime interface and a Windows launcher that reuses this boundary and fixture | Phase 2 gate passes | Blocked |
| 4 | SCM services, lifecycle and recovery acceptance | Phase 3 | Blocked |
| 5 | Reviewer candidate isolation on real native harness command paths | Phase 3 | Blocked |
| 6 | Egress and publication brokers | Phase 3 | Blocked |
| 7 | Idempotent installer, update/rollback/uninstall, full host acceptance report | Phases 4 to 6 | Blocked |

No intermediate phase is production acceptance. Independent review and guarded
integration complete each task; only the final acceptance can admit a host.

## Containment prototype

`deploy/agentc/windows-containment-probe.py` is a disposable experiment with
three parts.

- `windows-containment-probe.py` is the driver. It holds the accepted value of
  every row, creates the host-side canaries and positive controls, and derives
  the verdict.
- `windows-probe-native.py` is the Windows API layer: the launcher described
  under [Launch boundary](#launch-boundary), the broker stub, the listeners and
  the handle-based reader.
- `windows-probe-fixture/` is a Rust crate with no dependencies. It is the
  workload and the attacker.

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

### Disposition

**NO-GO for Windows supervised execution.** The prototype gate is blocked by
one guarantee: a launch running as the owner can read the owner's registry.
Everything else the prototype can test passed for the proposed boundary.

The blocked gate stops phase 3 and everything behind it. Phase 2 is the next
step and needs the owner: an elevated session on the target to create role
accounts, run this matrix with launches under them, and test network policy
loss. Until that passes, no runtime, service, broker or installer work starts.

Still unverified, and never to be read as passed:

- role service identities, since every launch here ran as the invoking account;
- privileged network policy and behavior when the firewall service or its
  policy is lost;
- native provider harness command paths and reviewer candidate interception;
- production egress and publication brokers;
- SCM lifecycle, crash recovery and reboot behavior;
- handle-relative creation and deletion by host brokers inside worker-writable
  trees;
- COM, RPC and broker surfaces beyond the Task Scheduler.

No service, firewall policy, provider login, WSL configuration or production
admission was changed.

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
