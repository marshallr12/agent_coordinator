# Native Windows supervisor: design and prototype

Status: **proposed, incomplete, not approved for unattended execution**.
Task `7011c55f-652e-47d3-a409-7d46420c7eb6` activates a native Windows
replacement for the abandoned WSL host approach. This chapter defines the
delivery gates; it does not claim that Windows services or a production Windows
sandbox are implemented. The [current implementation](implementation-status.md)
remains authoritative. The Linux backend remains supported.

## Threat model and authority

Treat repository files, build scripts, tests, package hooks, model output and
every worker command as hostile. Assume an attacker can execute arbitrary native
code under its worker token and knows all paths, pipe names, process IDs and
launch IDs. A random name is not an access check. Protect the workstation user,
service processes, provider/coordinator credentials, Git publication key, role
state, other launches (including launches of the same role), immutable tools,
policy and mirrors. Protect reviewer credentials from the reviewed candidate.

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

The initial **evaluation target** is MiniAir, native Windows 11 Pro x86-64,
build 26300 (10.0.26300), observed 2026-10-25. This is an observation, not a
supported production OS assertion. No Windows edition/build or harness/role
combination is admitted until its exact combination has an acceptance report.
Windows Server, ARM64, Windows Home, older Windows builds and WSL are not
covered by this prototype. A different build needs the same acceptance gates.

| Component | Evaluation choice | Admission state |
| --- | --- | --- |
| Python | Native CPython 3.14, stdlib ctypes probe | Experiment driver only |
| Initial native workload | Windows .NET Framework64 v4.0.30319 `csc.exe`, synthetic C# fixture | Must actually compile and execute inside the boundary |
| Rust/Cargo | Pinned workspace toolchain and MSVC linker/SDK, isolated caches | Untested inside the Windows boundary |
| Git | Native pinned Git, no hooks, disabled direct push | Not yet exercised in a Windows role launch |
| Claude implementer/reviewer | Native pinned release with all command paths examined | Unsupported; no Windows interception evidence |
| Codex implementer/reviewer | Native pinned release with all command paths examined | Unsupported; no Windows interception evidence |
| Browser, MCP and auxiliary tools | Individually pinned and brokered | Unsupported until alternate execution paths are tested |

Record full executable paths, SHA-256, reported versions, OS revision and source
commit/tree in each acceptance report. An available executable or a generic
AppContainer test does not qualify a harness. Do not substitute API billing for
the owner's provider enrollment model. The [earlier Codex evaluation](codex-reviewer-isolation.md)
is historical Linux evidence, not Windows permission to launch or a WSL fallback.

## Service identities and filesystem

Use separate least-privilege virtual service accounts for `AgentcSupervisor`,
`AgentcImplementer`, `AgentcReviewer`, `AgentcEgress` and `AgentcPublication`.
None is an administrator or LocalSystem worker. Deny interactive logon; grant
only the service and object rights each component actually needs. Inventory token
privileges and groups during installation and acceptance; never assume the
account name proves its privilege level. A privileged installer/policy helper
may install SCM/WFP policy, but cannot run caller-supplied programs.

Keep administrator-owned binaries/configuration under a dedicated Program Files
directory and protected state under a dedicated ProgramData directory. Each
role has its own protected credential/state subtree. The supervisor owns durable
launch manifests and coordinator mutation journals; workers cannot edit them.
The publication service alone reads the signing key. Egress has no publication
key. Credentialed provider harness state is outside candidate-readable grants.

Each worker gets a unique disposable AppContainer package SID, including
concurrent workers under the same service account. Evaluate a less-privileged
AppContainer (LPAC) with `ALL_APPLICATION_PACKAGES` access opted out. Ordinary
AppContainer grants to all application packages can expose host resources;
neither ordinary AppContainer nor LPAC is accepted without hostile probes.
Grant only required read/execute tool roots and the launch's own writable roots.
Do not grant access to an entire user profile or ProgramData for compatibility.

Create the process with explicit application path, bounded arguments, a fresh
allowlisted environment, no inherited handles and immutable security attributes.
Assign its unnamed Job Object atomically using the process attribute job list;
start suspended and resume only after setup succeeds. Require kill-on-job-close,
no breakaway, bounded process count and memory, and explicit CPU/time budgets.
Job Objects control lifetime/resources; AppContainer tokens, ACLs and network
policy enforce access. Reject unsupported attributes; never retry uncontained.

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

## Delivery gates and current experiment

1. Review this threat model, support matrix, parity map and broker interfaces.
2. Run the disposable containment prototype on the exact target; then expand it
   to every mandatory parity row and native harness command path. Require real
   build/test success and positive controls before interpreting denied access.
3. Implement the shared runtime boundary and SCM services only after containment
   is demonstrated; retain Linux validation and deny unsupported combinations.
4. Add reviewer routing, egress/publication brokers, then the idempotent installer
   and update/rollback/uninstall. Protect enrollment and credentials outside repos.
5. Record full host acceptance, logs/artifacts and exact-source Linux, Windows,
   smoke and documentation checks. Independent review and guarded integration
   complete the task. No intermediate phase is production acceptance.

`deploy/agentc/windows-containment-probe.py` is an initial experiment. It creates
unique ordinary AppContainer and LPAC profiles with **no capabilities**, starts
processes in jobs atomically, compiles a synthetic C# fixture inside the boundary
and checks own writes, synthetic host-file access, host process access and child
token inheritance. It uses an allowlisted environment and no inherited handles.
The regular and LPAC experiments are independent comparisons, never fallback
choices. A failed compiler, timeout or missing observation fails the experiment.

Run from the source checkout using native Python (replace `NEW_REPORT` with a
new absolute path outside the repository):

```text
python -I deploy/agentc/windows-containment-probe.py --output NEW_REPORT
```

The script retains its new temporary directory and report for inspection. It
changes only that directory's ACLs, deletes only its generated profiles, and
never installs services, firewall rules, provider logins or a host admission
receipt. A pass still reports `host_admitted: false`: networking, IPC, path races,
role identities, actual harnesses and lifecycle remain unverified. This experiment
is not a substitute for a Rust/MSVC build under an admitted worker profile.

## Primary references

- [Launching AppContainer and LPAC processes](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer)
- [AppContainer isolation](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation)
- [AppContainer for legacy applications](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-for-legacy-applications-)
- [Process creation attributes](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute)
- [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)
- [Windows Filtering Platform](https://learn.microsoft.com/en-us/windows/win32/fwp/windows-filtering-platform-start-page)
- [Service control handlers](https://learn.microsoft.com/en-us/windows/win32/services/service-control-handler-function)
- [Interactive service limitations](https://learn.microsoft.com/en-us/windows/win32/services/interactive-services)
