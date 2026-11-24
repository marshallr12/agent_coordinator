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
| Claude reviewer | Native pinned release with all command paths examined | Unsupported for a host; **one contained run passed**. Phase 5 [observed the command paths](#native-claude-code-command-paths-2026-11-14) of 2.1.325 uncontained. Its Bash tool runs Git Bash, which [does not start in an AppContainer](#git-bash-does-not-start-in-an-appcontainer-2026-11-14); a pinned busybox-w32 shell [does](#a-posix-shell-that-starts-in-an-appcontainer-2026-11-16). On 2026-11-17 build 2.1.326 ran [inside a less-privileged reviewer launch](#native-claude-code-in-a-contained-reviewer-launch-2026-11-17), and its one Bash command went through the prefix and the shim into the candidate. The capability record admits only 2.1.326 by SHA-256, with only routed tools and a ready candidate runner, but no Windows driver calls it, so no product launch uses it. Provider network, credentials and the harness's own hash check are not product paths yet |
| Claude implementer | Native pinned release with all command paths examined | Unsupported. Its Bash tool needs the same shell; no implementer launch has run |
| Codex implementer/reviewer | Native pinned release with all command paths examined | Unsupported; no Windows interception evidence |
| Browser, MCP and auxiliary tools | Individually pinned and brokered | Unsupported until alternate execution paths are tested |

Record full executable paths, SHA-256, reported versions, OS revision and source
commit/tree in each acceptance report. An available executable or a generic
AppContainer test does not qualify a harness. Do not substitute API billing for
the owner's provider enrollment model. The [earlier Codex evaluation](codex-reviewer-isolation.md)
is historical Linux evidence, not Windows permission to launch or a WSL fallback.

### Native Claude Code command paths, 2026-11-14

Phase 5 (task `61fe9ca2`) first observed how native Claude Code runs
commands on MiniAir, before building anything on it. The harness was
`C:\Users\marsh\.local\bin\claude.exe`, version 2.1.325, SHA-256
`87e0cb6ae0f4775da388cd3043f4d8097ef961dcddbf55fe90bc61ffc3f57a30`, run as
the owner from an ordinary (not contained) session with a logging
`CLAUDE_CODE_SHELL_PREFIX` script in a scratch directory. These are
**observations of one version as the owner**, not a contained launch.

| Question | Observed |
| --- | --- |
| How the prefix is called | Through Git Bash (`/usr/bin/bash`, GNU bash 5.3 for Cygwin/MSYS), with exactly one argument: the whole wrapped command, `source <shell snapshot> ... && eval '<command>' < /dev/null && pwd -P >\| %TEMP%\claude-XXXX-cwd`. This is the form the Linux candidate shell already expects. The prefix must run it with `bash -c`; executing the argument as a program fails with status 127 |
| Background commands | A `run_in_background` Bash call reaches the prefix the same way |
| Hooks | Every hook command (user and plugin `SessionStart`, `PreToolUse`, `Stop`, `SessionEnd`) also runs through the prefix. A reviewer launch must therefore run with no hooks of its own, or they run as candidate commands |
| The native PowerShell tool | **Bypasses the prefix**: a PowerShell call ran in its own process with no prefix call. On Windows it is a candidate route outside the boundary unless it is not offered |
| Tool allowlist | `--tools Read Glob Grep Bash`, the Linux reviewer allowlist (`role_settings::allow`), leaves exactly those four tools; `--disallowedTools PowerShell` removes only the one named |
| The Linux reviewer flags | With `--safe-mode --setting-sources user --strict-mcp-config --permission-mode dontAsk --permission-prompts none --tools Read Glob Grep Bash` the harness reported tools `Bash, Glob, Grep, Read`, no MCP server (the account's claude.ai connectors did not attach), no user hook ran, and the one Bash call went through the prefix |
| What still loaded | Plugins (language servers among them) and skills installed in the owner's profile. A role account's profile holds none; a launch must still be checked for them |
| The tracked working directory | The cwd file is written by the wrapped command, inside the prefix. As on Linux, the candidate context must not be able to rewrite it after the command, so the shim has to own it |

What this settles: the prefix carries every Bash route that was observed,
and the one route that bypasses it, the PowerShell tool, is not offered under
the reviewer allowlist. What it does not settle: whether any other route
(language-server plugins, file tools reading candidate files with the
harness's rights) executes candidate code, and how the shim behaves under a
contained reviewer token. A Claude reviewer on Windows stays refused until
the candidate runner exists and passes its fixture checks.

### Git Bash does not start in an AppContainer, 2026-11-14

Step 5c found a **blocker** for this proposal and for any contained native
Claude Code launch. On MiniAir, the Git for Windows `bash.exe` (GNU bash
5.3.15, MSYS2 runtime `msys-2.0.dll`), started through the launcher in a
full AppContainer and in a less-privileged one, ends during start-up with
status `0xC0000142` (DLL initialisation failed) after printing

```text
*** fatal error - NtCreateDirectoryObject(\BaseNamedObjects\msys-2.0S5-<key>): 0xC0000022
```

`0xC0000022` is access denied. The runtime makes its shared objects in a
fixed directory under `\BaseNamedObjects` with a native call. Windows moves
an AppContainer's ordinary named objects into the container's own namespace,
but not a native call naming that path, so the token is refused. No setting
of the runtime was found that changes the path. The test
`git_bash_does_not_start_in_an_appcontainer` in
`crates/supervisor/tests/windows_candidate.rs` asserts this, so it will say
when a later runtime starts.

What it means:

- **The candidate** cannot run Git Bash, so the proposal's candidate shell
  needs another shell that starts in an AppContainer.
- **The reviewer harness** cannot either. Native Claude Code runs every Bash
  command, and the shell prefix itself, through Git Bash
  ([observed](#native-claude-code-command-paths-2026-11-14)), so a Claude
  Code launch inside the launch boundary has no working Bash tool. That holds
  for an implementer as much as for a reviewer.

A Claude reviewer and a Claude implementer on Windows stay refused. The
capability record already refuses both, since nothing reports a ready
candidate runner.

### A POSIX shell that starts in an AppContainer, 2026-11-16

Task `d8ae17f5` looked for a shell that does start, on MiniAir, through the
same launcher. Two shells were tried. A look at `PATH` and at the usual places (Git for
Windows, scoop, chocolatey, MSYS2, Cygwin) found no other POSIX shell on the
host, except that Git for Windows also ships `dash.exe` (SHA-256
`a5c7d5d3fd27a5ad356e275f200e49ca277a066001f58dfd709dbc615007e1e6`), which
uses the same `msys-2.0.dll` and was **not tried**.

| Shell | Version and SHA-256 | In an AppContainer through the launcher |
| --- | --- | --- |
| Git for Windows `bash.exe` | GNU bash 5.3.15(2), Git 2.56.0.windows.4; `bash.exe` `5490d0da5e7cf9d92068cc48fcc590f2bcf8564add8ff91c3b5fe541eb2d72e3`, `msys-2.0.dll` `2a89b7c31b323c42e2fb37900af087aeb27417694e630a210bc75b0369410bc7` | **Does not start**, full or less privileged: the [failure above](#git-bash-does-not-start-in-an-appcontainer-2026-11-14), seen again on 2026-11-16 |
| busybox-w32 `sh` | BusyBox 1.39.0, release FRP-6354-g5f09fb446 of 2026-10-27, file `busybox-w64u-FRP-6354-g5f09fb446.exe` (64-bit, Unicode), 683,520 bytes, `36b266923fc5765b42f5f981319f39568498fba60b53ccf26e8fc1e575e2a9a9` | **Starts and runs**, full and less privileged alike |

The busybox-w32 file was downloaded from the publisher's
[download directory](https://frippery.org/files/busybox/) with the owner's
approval of that exact file, and its hash matches the publisher's
`SHA256SUM`. The publisher's signature was not checked. It is one program
with no runtime library of its own, and it runs the shell when its file is
named `sh.exe`. It is kept outside the repository; the tests find it through
`AGENTC_TEST_POSIX_SHELL`, read it once, refuse bytes with another hash and
run only the bytes they checked. Where the variable is unset they say on
standard error that nothing was checked, and the test harness still counts
them as passed. The pin is the tests' own: the runner starts whatever shell
its driver names, and nothing in the product checks a hash yet.

**What ran.** `the_pinned_shell_runs_in_an_appcontainer` in
`crates/supervisor/tests/windows_candidate.rs` copies the file as `sh.exe`
into a directory only the launch's package may read, and starts it with
`-c` and this script, once in a full and once in a less-privileged
AppContainer (`registryRead`, no network capability). The launch sets
`LOCALAPPDATA` and `TMP` to its working directory, sets no `TEMP`, and sets
`PROBE_KEPT` to `as given`:

```text
echo started; echo abc | tr a-c A-C | cat; (exit 3); echo "sub $?";
x=$(echo inner | sed s/inner/captured/); echo "$x";
(sleep 1; echo late > bg) & echo early; wait; echo "waited $?"; cat bg;
case "$LOCALAPPDATA" in *\\Packages\\*\\AC) echo local-replaced;; *) echo local-kept;; esac;
case "$TMP" in *\\AC\\Temp) echo tmp-replaced;; *) echo tmp-kept;; esac;
case "$TEMP" in *\\AC\\Temp) echo temp-added;; *) echo "temp=$TEMP";; esac;
[ -d "$TMP" ] && echo temp-directory || echo no-temp-directory;
echo "other=$PROBE_KEPT";
export TMP=C:/zz; sh -c 'echo "child=$TMP"'; cmd /c "echo cmd=%TMP%"; exit 7
```

Both runs ended with status 7 and printed exactly

```text
started
ABC
sub 3
captured
early
waited 0
late
local-replaced
tmp-replaced
temp-added
no-temp-directory
other=as given
child=C:/zz
cmd=C:/zz
```

So a pipeline, a subshell, a command substitution and a background job that
is waited for all work there, and a program that is not the shell
(`cmd.exe`) starts from it.

**Three things the run showed about the launch, not the shell.**

- **Windows replaces three variables.** A process started in an AppContainer
  does not get the `LOCALAPPDATA`, `TEMP` and `TMP` it was given. It gets
  `<the given LOCALAPPDATA>\Packages\<profile>\AC` and that directory's
  `Temp`, which nothing makes, and `TEMP` appears even where none was set.
  The probe asserts each of these. A variable Windows has no use for arrived
  as given, and so did `PATH` and `SystemRoot` in the one full listing that
  was read by hand and not kept. A value the shell exports does reach its
  children. Step 5c's "exact environment" was therefore never exact for
  these three; no command had run to show it. The candidate's command file
  now sets the three again before the command.
- **The job is not empty the moment the shell ends.** It still counts a
  process for a short time, so the first command of all was reported as
  having left processes running. Which process that is was **not
  identified**; a console host is the likely one. A command now counts as
  having left something only if the job has not emptied after two seconds.
- **The shell does not take Git Bash's options or paths.** It refuses
  `--noprofile` (status 2), so the candidate starts it with the command file
  alone; a shell that is neither interactive nor a login shell reads no
  start-up file. It writes paths as `C:/a/b` and does not know `/c/a/b`.
  Its own applets come before programs on `PATH`: `whoami` ran the applet,
  not `whoami.exe`.

**The candidate's native tests.** With those three changes
`a_reviewers_candidate_runs_commands_in_its_own_package_and_job` and
`a_candidate_command_past_its_time_limit_is_stopped_and_repeats_are_refused`
are no longer ignored and **pass** on MiniAir with this shell: output and
status return, the environment is the candidate's own and holds no
credential of the runner, a background process is reported and ended, the
candidate reads the source and cannot write it, cannot read the reviewer's
control directory, a command past its limit is stopped and the third is
refused, and cleanup removes the candidate's two trees, the source and the
reviewer's working directory. As before, they run in a new logon
session of the developer's account and are a development check, not
acceptance evidence.

**Native Claude Code with this shell.** Observed with the same harness as
[above](#native-claude-code-command-paths-2026-11-14) (2.1.325, SHA-256
`87e0cb6ae0f4775da388cd3043f4d8097ef961dcddbf55fe90bc61ffc3f57a30`), again as the owner from an ordinary session,
**not contained**, with the Linux reviewer flags and a logging prefix.

| Question | Observed |
| --- | --- |
| Whether the harness accepts it | Yes. With `CLAUDE_CODE_GIT_BASH_PATH` naming a copy of the file called `bash.exe`, the harness started and ran its Bash tool through it (`uname -s` in the prefix said `Windows_NT`). Another file name was not tried |
| How the prefix is called | As under Git Bash: one argument, the same wrapped command, with the snapshot and the cwd file still written as `/c/Users/...` |
| The wrapped command run as it is | **Fails.** A prefix that ran its argument with `sh -c` ended with status 2 and no output. Run piece by piece, `source <a file that is not there> 2>/dev/null \|\| true` ends this shell with status 2, as POSIX allows for a special built-in, and the shell opens no `/c/...` path, to read or to write, so neither the snapshot nor the cwd file can work. `\builtin` is not found, which the form already tolerates. The snapshot file named in a later run did not exist once that run had ended; whether the harness wrote and removed it was not observed |
| The command through a prefix that does what the shim does | **Works.** A prefix that kept only the eval'd command, ran that in a fresh shell and wrote the cwd file itself returned `MARKER-42` for `echo marker-$((6*7)) \| tr a-z A-Z`, status 5 and `sub` for `(echo sub; exit 5)`, and ran a `run_in_background` call whose file appeared. A `cd ..` in one call did not carry to the next, since the prefix wrote its own directory |

What this settles: the recorded command form reaches the prefix unchanged
under this shell, and the [proposed shim](#proposed-candidate-runner), which
never runs the snapshot or the harness's own redirection, is what makes it
usable. Without such a prefix the harness's Bash tool does not work under
this shell. **Unverified:** the harness itself running inside an
AppContainer with this shell, hooks under it, whether the harness uses the
shell for anything besides the Bash tool and the prefix, and every release
other than 2.1.325.

A shell therefore **qualifies** for the candidate, and the blocker for the
candidate is gone. Nothing else changed: the capability record still refuses
every Claude launch on Windows, because no candidate runner reports ready.
Steps 5d to 5f were not done by that task; see
[phase 5 steps](#phase-5-steps) for what followed.

### Native Claude Code in a contained reviewer launch, 2026-11-17

Step 5f (task `9cf04a5c`) ran native Claude Code inside a reviewer launch
once, with the owner at the keyboard, at 19:58 UTC.
The harness was a copy of `C:\Users\marsh\.local\bin\claude.exe`, version
**2.1.326**, SHA-256
`44e85ac6d3e36b924a0d631d3f69c13a6907491115d71e97c8eb6160c82b2aed`, kept
outside the repository so that the host's self-update cannot change it. The
recorded 2.1.325 is no longer on the host. The shell was the pinned
busybox-w32 above. The test is
`native_claude_code_runs_a_bash_command_in_its_candidate` in
`crates/supervisor/tests/windows_candidate.rs`. It runs only where both
`AGENTC_TEST_CLAUDE` and `AGENTC_TEST_CLAUDE_LOGIN` are set, so it never
starts unattended, and like the other native tests it runs in a new logon
session of the owner's account.

**The launch.** The runner prepared a reviewer launch with its candidate,
both less privileged. On the owner's one-off decision, the reviewer was given
the `internetClient` capability besides `registryRead`, and a copy of the
owner's `~/.claude/.credentials.json` in `claude-config` in its working
directory, as `CLAUDE_CONFIG_DIR`. The candidate kept `registryRead` alone,
with no network capability. The reviewer's tool directory, which only its
package could read, held the harness, the shell as `sh.exe` and as
`bash.exe` (`CLAUDE_CODE_GIT_BASH_PATH`), the supervisor program as the shim,
and a prefix (`CLAUDE_CODE_SHELL_PREFIX`) that appended its argument count,
`TEMP` and argument to a log in the working directory before it ran the
runner's `candidate-shell`. The settings were the Linux reviewer's
permission rules with Claude's own sandbox off. The command line was the
Linux reviewer flags (`-p --safe-mode --setting-sources user
--permission-mode dontAsk --permission-prompts none --strict-mcp-config
--tools Read Glob Grep Bash`) with `--model sonnet`, asked to run
`echo ran > "$TEMP/harness-ran"; echo marker-$((6*7)) | tr a-z A-Z` once with
the Bash tool. The runner gave the shim, as the reviewer's temp, the `TEMP`
Windows substitutes in an AppContainer (see
[the three variables](#a-posix-shell-that-starts-in-an-appcontainer-2026-11-16)):
`<working directory>\Packages\<the reviewer's profile>\AC\Temp`. The test
made that directory first.

| Question | Observed |
| --- | --- |
| Whether the harness starts and works there | Yes. It ended by itself with status 0 after 12.9 seconds for the whole test, and printed `MARKER-42` |
| Prefix calls | Exactly one, with one argument |
| The `TEMP` the harness exported | The substituted one, `...\reviewer-work\Packages\agentc.test.610b7c00174444fbba7bdc5564599401\AC\Temp`, which is the reviewer's temp the runner gave the shim, so the cwd file was inside it |
| The argument | The recorded form, unchanged: `source /c/.../claude-config/shell-snapshots/snapshot-bash-1794945505866-k1aa7a.sh 2>/dev/null \|\| true && export TEMP='<that temp>' TMP='<that temp>' && <the three resets> && eval 'echo ran > "$TEMP/harness-ran"; echo marker-$((6*7)) \| tr a-z A-Z' < /dev/null && pwd -P >\| /c/.../AC/Temp/claude-16f7-cwd`. `form::parse` accepted it, since a refusal would have ended the call with status 126 and no output |
| Where the command ran | In the candidate: `harness-ran` appeared in the candidate's temp, `candidate-work\tmp`, which the reviewer's package cannot write, so the candidate's own `TEMP` was the one the command saw |
| Identities | The owner's account in a new logon session. The reviewer ran as the AppContainer profile `agentc.test.610b7c00174444fbba7bdc5564599401`, and its candidate as a second, separate profile. Their package SIDs were not printed |
| The credential | The copy was unchanged at the end (no refresh). It and the configuration directory were removed before cleanup, and cleanup deleted both profiles and every tree. The owner's own login still worked afterwards |

What this settles: the command form of 2.1.326 matches the recorded form,
and inside a contained reviewer launch the harness's Bash tool reaches the
candidate only through the prefix and the shim. A Windows reviewer whose
temp is the substituted one passes the shim's cwd check when the runner is
given that temp, which closes the gap noted under step 5d. So
`capability::WINDOWS_CLAUDE` now records 2.1.326 by SHA-256, and only that
build. 2.1.325 is dropped, since its form was seen only outside a launch.

**Unverified:**

- Hooks, plugins and other harness routes inside the launch. `--safe-mode`
  loaded none here, and the profile held none.
- A command that fails, a background command, and more than one call.
- The candidate's reach of the real credential. The step 5e fixture shows a
  synthetic one out of its reach; this run did not probe it.
- Network egress of either launch. The reviewer reached the provider through
  `internetClient`; the candidate's lack of network is its spec, not probed
  in this run.
- A driver that hashes the harness, gives the reviewer the substituted temp
  and makes that directory. The test did those things itself.

### Proposed candidate runner

**Mostly a proposal.** It was blocked by
[Git Bash in an AppContainer](#git-bash-does-not-start-in-an-appcontainer-2026-11-14)
until task `d8ae17f5` found
[a shell that starts there](#a-posix-shell-that-starts-in-an-appcontainer-2026-11-16).
Phase 5 builds it in the steps under
[phase 5 steps](#phase-5-steps). Steps 5a and 5b, the parser of the command
form, the capability record and pipe admission by package, are implemented,
and nothing calls them on Windows yet. It reuses the launch boundary:
the candidate is one more launch of the same runner, with its own package,
and each command is one start in a fresh job.

**The shim.** A reviewer launch's `CLAUDE_CODE_SHELL_PREFIX` names
`candidate-shell`, a script the runner writes into the launch's control
directory, which the launch can read and not change. It runs
`agentc-supervisor windows-candidate-shell` with the launch's pipe name, clone
and temp. The shim runs as the reviewer, with the reviewer's token, and does
four things.

1. It matches its one argument against the form recorded above for the pinned
   version: the snapshot `source ... 2>/dev/null || true`, the `TEMP`/`TMP`
   export, the shell option and alias resets, then
   `eval '<command>' < /dev/null && pwd -P >| <cwd file>`. Anything else,
   hook commands among them, is refused with status 126 before anything runs.
   The cwd file must be in the reviewer's temp and named as the Linux
   `candidate::cwd_guard` allows.
2. It sends only the eval'd command to the pipe. The snapshot, the reviewer's
   `TEMP` and the cwd file never reach the candidate. Nothing it needs is
   lost: the snapshot is already allowed to fail, and the candidate gets its
   own environment.
3. It writes the candidate's output and status back to the harness.
4. It writes the clone's path into the cwd file. The shim owns that file, and
   the candidate cannot reach the reviewer's temp, so it cannot rewrite it.

**The pipe.** Each reviewer launch gets `agentc-candidate-<launch>`. Its
access list names the runner's account in full and the reviewer's package
with the client rights of the [runner pipe](#phase-4a-service-lifecycle-core-scm-binding-and-the-runner-service).
The reviewer and the candidate run as the same account, so the account cannot
tell them apart. Admission therefore reads the package SID from the
connection's token and requires it to be exactly the reviewer's. A process of
the account with no package, the candidate's package and another launch's
package are each refused, and the access list keeps the last two from
connecting at all. Background calls also arrive through the shim and may run
beside a foreground one, so the pipe has a small fixed number of instances,
each served by its own thread. A call beyond that number waits, and is
refused with a message saying why once its wait runs out.

**The candidate context.** When the runner prepares a reviewer launch it also
prepares a candidate launch: a second disposable AppContainer profile
(less privileged, the tool capability, `registryRead`, no network
capability), its own control and working directories and its own desktop in
the runner's station. Each command is one start in a fresh job with the
limits of a launch and the time limit of the Linux shell (`CALL_SECONDS`, and
`STUCK_CALLS` before further commands are refused). It runs the pinned
shell (first Git Bash with `--noprofile --norc`, now the pinned busybox-w32
`sh` on the command file alone), from the clone, with an exact environment:
`PATH` to the pinned tools and toolchain, and `HOME`, `TEMP`, `TMP` and
`CARGO_HOME` in the candidate's working directory, `LOCALAPPDATA` and the
system variables Windows needs. Windows replaces `LOCALAPPDATA`, `TEMP` and
`TMP` at the start, so the command file sets them again. Nothing is
inherited, so no variable of the Linux `UNSET` list can reach it. Output and error go to one log the runner
creates and the candidate only inherits. The runner returns at most a fixed
number of bytes of it and says when it cut the rest. When the command ends the
job is ended, so background processes die with the call, as on Linux, and the
shim says so when the job still held any.

**Grants.** The clone gets read access for both packages. The candidate may
write only its own working directory (build output, Cargo home, home and
temp). The reviewer's control directory, temp, state and pipe grant the
candidate's package nothing, so the AppContainer access check refuses it.
Credentials stay out of the registry, because `registryRead` reads the hive
of the account the launch runs as; a key granted only to the reviewer's
package is part of the fixture.

**The capability record.** `candidate::applies` and `candidate::unsupported`
stop testing the harness name alone. A record per host, role, harness and
version says whether candidate code can be isolated and how. Linux keeps its
answers unchanged: a Claude reviewer through Bubblewrap, a Codex reviewer
refused. On Windows a Claude reviewer is admitted only when the harness binary
is a version with recorded evidence (2.1.326, by SHA-256, from the
[contained run](#native-claude-code-in-a-contained-reviewer-launch-2026-11-17)) and the
candidate runner answers a readiness check. A Codex reviewer, any other
version and a missing or broken candidate runner are refused, each with its
reason. The reviewer's command line must hold the Linux reviewer flags and
exactly the tool allowlist `Read Glob Grep Bash`, so the PowerShell tool is
never offered.

**Recovery.** The reviewer launch's journal entry records the candidate
profile too. A launch that ended takes its candidate with it. Reclaim deletes
both profiles. A restarted runner never serves a candidate pipe for a
journaled launch.

**Still open.** Whether a language-server plugin or another harness route
runs candidate code (a role profile holds no plugins, and the launch must
check that it does not). Whether the reviewer's file tools can be pointed at
host files through a junction the candidate makes in its writable tree.
Junctions need no privilege on Windows. The same question exists on Linux
with symbolic links and is not settled there either.

#### Phase 5 steps

Each step builds, passes its tests and is committed on its own.

| Step | Content | Test |
| --- | --- | --- |
| 5a | Parser of the recorded command form and the candidate command it becomes; the capability record, with Linux unchanged | Unit tests on every platform: accepted and refused forms, quoting, cwd-file layouts, the record's answers |
| 5b | Pipe admission by package SID | Native: the reviewer's package is served; no package, another package and the candidate's package are refused |
| 5c | The candidate launch: profile, directories, desktop, grants, journal and reclaim; one start per command with the exact environment, the limits and bounded output | Native: a command runs in the candidate package and job, its output and status return, a timed-out command is stopped |
| 5d | The candidate pipe's threads and the shim subcommand | Native: a harness-shaped command through the shim, the cwd file reset, refusals before anything runs |
| 5e | Fixture checks | Native: a synthetic credential (environment, file, process, handle, registry) is out of the candidate's reach; it cannot open the reviewer's process, state or pipe, or another launch's; a missing or broken runner refuses the launch |
| 5f | A real Bash command from native Claude Code (2.1.325 planned; 2.1.326 on the owner's decision of 2026-11-17, since 2.1.325 left the host) through the prefix in the candidate context; the support matrix, evidence and phases tables | Owner-attended run on MiniAir, the Linux gate on the exact candidate, mdBook build and link checks |

Step 5a is implemented in `crates/supervisor/src/candidate/form.rs` and
`crates/supervisor/src/candidate/capability.rs`. `form::parse` accepts the
recorded argument and keeps only its command; it decodes both
quote-in-quote escapes (`'\''` and `'"'"'`), which is broader than the one
quote-free command that was observed. `form::cwd_file` maps the cwd file to
Windows form and admits only the Linux layouts inside the reviewer's temp.
`capability::decide` is the record; `candidate::applies` and
`candidate::unsupported` are now its Linux answers, and their tests are
unchanged.

Step 5b is implemented in `service::pipe_policy` and the Windows `pipe`.
`Pipe::for_package` makes a launch's candidate pipe with
`package_descriptor`'s access list (the runner's account in full, the
package with the client rights, a low label so a low-integrity
AppContainer client can write). The pipe opens the connection's token once,
while impersonating, and reads its account and package after the revert.
`Admitted::package` serves only the runner's account carrying exactly that
package. The portable tests cover the right package, another package, no
package, an unreadable package, forged SID text and another account. The
native test shows a process of the runner's own account with no package
connecting, being refused and getting no answer. An AppContainer client
being served, and another package being kept out by the access list, need a
real launch and are tested in step 5d.

Step 5c is implemented and **stopped by the blocker**.
`host_runtime::windows::candidate` prepares a reviewer launch's candidate
with it: a second profile (less privileged, refused otherwise), its
directories and desktop, its writable `home`, `tmp`, `cargo`, `target` and
`local`, and the source tree, made new with `sddl::shared_source`, which both
packages may read and neither may change. It is then detached from the
launcher (`Launcher::detach`, `Launcher::retire`) so that pipe threads can
use it. `Candidate::run` writes the command to a file in the candidate's
control directory and starts the shell on that file in a fresh job, with the
exact environment, the log as the only inherited handle, `CALL_SECONDS` and
`STUCK_CALLS` as on Linux, output cut at 1 MiB, and a note when processes were
left running. The runner prepares it from `prepare`'s `candidate` field,
journals it (`CandidateRecord`) and retires, removes and reclaims it with the
launch. Unit tests cover the environment, the output bound, the refusal, the
access list and the journal. The native tests that run commands through it,
`a_reviewers_candidate_runs_commands_in_its_own_package_and_job` and
`a_candidate_command_past_its_time_limit_is_stopped_and_repeats_are_refused`,
were written and ignored, because their shell, Git Bash, does not start.

Step 5d is implemented (task `9cf04a5c`, 2026-11-17).

- **The pipe's threads.** `host_runtime::windows::candidate_pipe` gives each
  reviewer launch the pipe `agentc-candidate-<launch>` with four instances
  (`Pipe::package_instances`), each served by its own thread. An instance
  reads one request line holding only the command, runs it with
  `Candidate::run` and writes one reply line: the status, the bounded output
  and what the reviewer is told besides it, or why nothing ran. A refused
  client is written nothing. The runner starts the threads when it prepares
  the launch and ends them when it cleans the launch up: a command still
  running is ended, the candidate runs no more, and the candidate is retired
  only once every thread has ended. No pipe is made for a journaled launch
  of an earlier runner.
- **The owner of the pipe.** The pipe's access list now names the runner's
  account as owner. In an elevated session Windows otherwise makes the
  Administrators group the owner, and the shim, which uses only a pipe its
  own account owns, refused it (seen on MiniAir).
- **The shim.** `agentc-supervisor windows-candidate-shell` is
  `host_runtime::windows::candidate_shell` over the portable
  `candidate::shim`. It takes exactly one argument, matches it with
  `form::parse`, requires the cwd file inside the reviewer's temp and
  refuses everything else with status 126 before the pipe is opened. It
  sends the command, writes the output to standard output and the notes to
  standard error, and writes the clone's path into the cwd file in the
  `/c/...` form the harness wrote the file's own name in. A pipe with every
  instance taken is tried again for 60 seconds; then the call is refused and
  says so.
- **The prefix script.** The runner writes `candidate-shell` into the
  reviewer's control directory. It runs under the pinned shell and hands its
  arguments unchanged to the shim, with the launch's pipe, the reviewer's
  temp and the clone. `CandidateSpec` gained `shim`, the supervisor program
  the reviewer's package can read, and `reviewer_temp`.

The native test
`a_harness_shaped_command_runs_through_the_shim_and_others_are_refused`
**passes** on MiniAir with the pinned shell. A reviewer launch, less
privileged, runs the pinned shell, which calls the prefix script the way
the harness does. Observed: a command in the recorded form printed its
output and error and returned status 3; the cwd file then held the clone's
path, and still did after a command that changed directory; a hook-shaped
argument, a cwd file outside the reviewer's temp and a second argument were
each refused with status 126, and the file the refused command would have
written was not there; with four commands running, a fifth call given one
second to wait was refused with the message about command slots. The shim
started from the candidate's own package and from another launch's package
ended with `Access is denied. (os error 5)` when it opened the pipe, so the
access list keeps both out, and the reviewer's package was served. Like the
other native tests it runs in a new logon session of the developer's
account and is a development check, not acceptance evidence. **Not
observed:** the real harness calling the prefix inside the launch (step
5f), and a reviewer whose temp is the one Windows substitutes (see
[the three variables](#a-posix-shell-that-starts-in-an-appcontainer-2026-11-16)):
the test passes the temp it names in the argument.

The same run showed that Windows refuses to start a process in an
AppContainer whose environment holds no `LOCALAPPDATA` (`CreateProcessW`
error 203).

Step 5e is implemented (task `9cf04a5c`, 2026-11-17).

- **The readiness check.** `Candidate::ready` is the check the capability
  record asks for. It hashes the candidate's shell and requires a shell
  with recorded evidence (`capability::WINDOWS_SHELLS`, the pinned
  busybox-w32 by SHA-256), then runs one command in the candidate and
  requires its output and status. Anything else is `Broken` with the
  reason. The runner answers it as the request `candidate_ready`: `ready`,
  `broken` with the reason, or `missing` for a launch prepared without a
  candidate. The hash is taken when the check is asked, not at every
  command; the shell's directory can be changed only by the runner's
  account.
- **A broken runner refuses the launch.** The runner refuses to start a
  launch whose candidate is broken, before the start is journaled. A launch
  prepared without a candidate is not a reviewer launch to the runner; the
  driver's capability record refuses a reviewer whose runner is missing.

The native test
`a_reviewers_credential_and_processes_are_out_of_its_candidates_reach`
**passes** on MiniAir with the pinned shell. It prepares two reviewer
launches, each with its candidate, all four less privileged. Before the
candidate's shell exists the readiness check says broken, the reviewer's
start is refused and nothing is started; a launch without a candidate
answers missing; `capability::decide` refuses a reviewer for both. Then a
synthetic credential is put in the reviewer's environment, in a file in
its control directory and one in its working directory, in a file only the
runner's account can open, whose handle the reviewer inherits, and in a
registry key of the account's hive that only the account and the
reviewer's package may read. One probe program then runs twice, in the
reviewer and in its candidate, and tries the same list:

| Target | From the reviewer | From its candidate |
| --- | --- | --- |
| The credential file in the reviewer's control directory | read | error 5 |
| The credential file in the reviewer's working directory | read | error 5 |
| The registry key granted to the reviewer's package | read | error 5 |
| The reviewer's candidate pipe | opened | error 5 |
| The reviewer's root process, to query, to read its memory, to take a handle out | opened, each | error 5, each |
| The file only the runner's account can open, by path | error 5 | error 5 |
| A handle to that file among the process's own handles | present | absent |
| The other launch's two credential files, its pipe and its root process | error 5, each | error 5, each |
| The runner's process | error 5 | error 5 |

Error 5 is access denied. The candidate's own handles were its command
file, its log and its starting directory, and its environment did not hold
the credential. The reviewer's column is the control: the same program
reaches what is the reviewer's own, so the candidate's refusals are not a
probe that cannot work. As with the other native tests this runs in a new
logon session of the developer's account, which is elevated here, and is a
development check, not acceptance evidence. **Not checked:** a credential
in the reviewer's memory other than through opening its process, window
messages between the two desktops, and the junction question left open
under [the proposed runner](#proposed-candidate-runner).

The probe first guessed handle values, and a process in an AppContainer
that does so ends with an invalid-handle exception (`0xC0000008`); it now
reads its own handle table.

**Toward step 5f (2026-11-17).** The host updated its native Claude Code to
2.1.326 (SHA-256
`44e85ac6d3e36b924a0d631d3f69c13a6907491115d71e97c8eb6160c82b2aed`) on
2026-11-16, and the recorded build 2.1.325 is no longer on it. Build
2.1.326 starts in a full and in a less-privileged AppContainer, from a
copy only the launch's package may read, and prints its version with
status 0 (`native_claude_code_reports_its_version_inside_a_launch`, which
runs only where `AGENTC_TEST_CLAUDE` names the file). It was asked for
nothing else: no session, no network, no credential.

Step 5f is done (task `9cf04a5c`, 2026-11-17). In the owner-attended
[contained run](#native-claude-code-in-a-contained-reviewer-launch-2026-11-17),
2.1.326 sent its one Bash command in the recorded form through the prefix
script and the shim, and the command ran in the candidate. The harness
exported the `TEMP` Windows substitutes. Because the runner was given that
temp as `reviewer_temp`, the shim's cwd check passed, which closes the gap
noted under step 5d. `capability::WINDOWS_CLAUDE` now holds 2.1.326 by
SHA-256 and nothing else. The unit tests admit it and refuse 2.1.325, a
wrong hash, the PowerShell tool and a missing or broken runner.

**Still open after phase 5:**

- **The process a job still counts after the shell ends.** It was never
  identified (a console host is the likely one). The candidate waits up to
  two seconds before it reports processes left running.
- **The harness's hash and temp in the product.** The readiness check
  hashes the candidate's shell (step 5e). Nothing in the product hashes the
  harness executable, gives the reviewer the substituted temp or makes that
  directory: the test does all three. No Windows driver calls
  `capability::decide` yet, so no product launch is admitted by it.
- **Provider network and credentials for a reviewer launch.** Both were given
  for this one run only. Brokered egress and protected provider state are
  phase 6 and later.
- **The junction question**, the other harness routes, and the cases listed
  as unverified under the run.

On the owner's decision of 2026-11-15, phase 5 **closed at step 5c**. Task `d8ae17f5` then found
[a shell that starts inside an AppContainer](#a-posix-shell-that-starts-in-an-appcontainer-2026-11-16)
and that native Claude Code accepts behind a shim-like prefix. With it, and
with three corrections to the candidate launch that its first real commands
showed (the shell's start, three variables Windows replaces, and when a job
counts as not empty), the two tests **pass** on MiniAir and are no longer
ignored. `CandidateSpec`'s `bash` field is now `shell`. The owner chose on
2026-11-17 to do the remaining steps before phase 6, as task `9cf04a5c`.
Steps 5d, 5e and 5f are done as described above.

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

### Phase 7 steps

Phase 7 turns what the owner did by hand in the runs of tasks `ec6d901c`,
`daa42f71` and `f71179f5` (with `sc.exe`, `icacls` and a script around
`ChangeServiceConfig2W`) into an installer. Two owner decisions of
2026-11-23 shape it:

- **Phase 7 is the installer followed by a gap report.** The real
  coordinator loop on Windows, a provider harness driver, a WFP policy,
  resource caps and quiet hours are a separately planned phase 8. Phase 7
  therefore ends with a host that is installed and described, not with a
  host that is admitted.
- **The installer is a Rust subcommand of `agentc-supervisor`**, not a
  script: `agentc-supervisor windows-host`. It calls the Service Control
  Manager and the file system directly, so no command line is built for
  another program to parse and nothing a caller supplies is ever run.

| Step | Content | Task | State |
| --- | --- | --- | --- |
| 7a | The installer core: a desired-state model, `windows-host install` and `windows-host status` | `ddeb5d76` | Implemented; see below. **No service the installer registered has been started** |
| 7b | `windows-host update`, `rollback` and `uninstall`, each stage journaled before it is acted on | `2064cf54` | Implemented; see [step 7b](#step-7b-update-rollback-and-uninstall-2026-11-24). Driven natively with a runner in the test process; the run against registered, started services is gated on the owner's consent |
| 7c | Owner-attended installer acceptance on MiniAir, using only the `windows-host` subcommands, with the stand-in as the workload and the phase 4c lifecycle rounds | `012ff9f2` | Not started |
| 7d | The host acceptance gap report against the [parity map](#linux-p2-parity-and-acceptance-map) and the phase 8 plan | `748f6c3a` | Not started |

The operator's view of the installer is the
[Windows host installation](windows-installation.md) chapter. This section
records the design and what is and is not shown.

#### Step 7a: the installer core (2026-11-23)

Step 7a is implemented in the platform-neutral `windows_installer` module
and the Windows `host_runtime::windows::installer` module.

**A model, an observation and their difference.** `windows_installer::model`
builds the desired state of a host from a layout (the program directory,
the state root and the service name prefix, which default to
`C:\Program Files\agentc`, `C:\ProgramData\agentc` and `agentc`) and the
owner's inputs (the remote, the push App's ids, whether its key is
installed, the start type and the stand-in's command). `observed` holds what
a host reports. `plan` compares the two and returns either the reasons the
host must be left alone or a list of changes, one kind per kind of drift:
a changed command line, start type, dependency, account, recovery setting,
preshutdown limit or privilege list each gets its own change, a missing
directory is made, a directory or key whose access list differs gets its
list set again, and a configuration whose managed entries differ is
rewritten. None of this calls Windows, so it is compiled and unit-tested on
Linux as well, against a host kept in memory.

**What an installed host has.**

| Part | Desired state |
| --- | --- |
| Binary | `agentc-supervisor.exe` in the program directory, with the SHA-256 of the binary that ran `install` |
| Services | `agentc-egress`, `agentc-publication`, `agentc-runner`, `agentc-supervisor`: own process, each under its own virtual account `NT SERVICE\<name>`, registered stopped; start on demand unless the owner asks for automatic start; the supervisor depends on the runner |
| Command line | The installed binary with `--config` naming the installed configuration, `windows-service` and the role. Nothing else is ever registered |
| Recovery | Two restarts 60 s apart, then none; the failure count resets after one day; no failure program and no restart message |
| Preshutdown limit | 120 000 ms, set by the installer because a service's own account may not set it |
| Required privileges | `SeChangeNotifyPrivilege` and `SeImpersonatePrivilege` and no others |
| Program directory | Protected: the system and administrators in full, `BUILTIN\Users` read and execute |
| State root, `installer\` | Protected: the system and administrators only |
| `runner\`, `runner\stand-in-launches\` | The same, and the runner's SID may modify |
| `supervisor\`, `stand-in\`, `stand-in\queue\` | The same, and the supervisor's SID may modify |
| `egress\`, `publication\` | The same, and the broker's own SID in full, which it needs to set its directory's list again at each start |
| `publication-key\` and `push-app.pem` | Only with a push App: the publication SID may read, the system and administrators have full control, nobody else has anything |
| `supervisor.toml` | Each service is given the others' service SIDs, the broker control pipes and the remote; see below |
| `installer\manifest.json` | The owner's inputs, so `status` and a later `install` need no arguments |

A service SID is `S-1-5-80-` followed by the SHA-1 of the upper-cased
service name; the model computes it, and a unit test pins the four SIDs
`sc.exe showsid` printed on MiniAir on 2026-11-22. Every directory and the
key are created owned by the Administrators group, whichever administrator
installs.

**Access lists are compared entry by entry.** A list is its protected flag
and the set of its entries (type, SID, mask, inheritance flags and whether
the entry was inherited), read with `GetNamedSecurityInfoW` and `GetAce`.
SDDL text is only written, never compared, because Windows spells one list
in several ways and keeps entries in an order of its own. Only the modeled
directories and the key file are inspected: what a service makes below its
own directory (`service.log`, the journal, `publication\work`) is not drift.
The two broker directories are given exactly the list each broker sets on
its own directory at start, so a started broker leaves nothing for `install`
to repair.

**Refusals come before any change.** `install` and `status` refuse a
session that is not elevated before reading anything else. A modeled
directory that is a link or any other reparse point, a file where a
directory belongs, and a directory owned by anyone but the system,
administrators or the service it belongs to are refused, and nothing below
such a directory is looked at, the manifest included. A key file that is
missing, empty, larger than 64 KiB, not a plain file or a link is refused,
as is a wanted key with no key file to copy it from. A configuration the
services could not load is refused rather than overwritten. A binary that
differs from the installed one is not put under running services: that is
an update, step 7b's.

**One change at a time, each on a fresh observation.** `install` observes,
plans, applies the first change and starts again, until the plan is empty.
A refusal that appears part-way therefore stops it, and a change is never
applied to a host that was only checked earlier. A change that was applied
and is still needed ends the run with an error instead of looping. Each
change is written to `installer\journal.jsonl` before it is applied and
again after, flushed; the first lines wait in memory until the installer
has made that directory. A run that fails part-way leaves the changes it
applied recorded, and the next run plans from the host as it then is and
makes the rest.

**The configuration.** The installer manages a fixed set of entries: each
service's name, state directory and pipe names, the SIDs each service
trusts, the remote, the push App's ids and key path, the egress broker's
`must_not_open` list and the stand-in table. It writes one only when its
value is not the in-code default, so a default installation's file holds
the service SIDs, the two control pipes the runner admits launches through,
the remote and the App's ids. Every other entry is the owner's and is kept
across a rewrite; comments are not. Unit tests load the generated text
through the same structures the services use and run each service's own
validation on it.

**The key.** The model holds the key file's path and never its bytes. The
key is copied (never moved) to a new file created with its final access
list and owner from its first byte, then renamed into place; an error names
a path and what Windows said. A later run needs no key file: it checks the
installed key's access list. A different key file given later replaces the
installed key whole.

**Token inventory.** `install` and `status` report each service's token
against the required-privileges list: `not_registered`, `stopped`,
`inventoried` (with the privileges held, those it may not hold and those it
lacks) or `not_inventoried` with the reason. A stopped service has no token
and is reported as stopped, never as a pass. `status` exits non-zero for a
running service whose token holds a privilege outside the list.

**Tests.** The neutral suite (`windows_installer::tests`) covers the full
plan of a clean host, the empty plan of an installed one, each kind of
drift with exactly its repair, every refusal with the host compared before
and after, a failure at the fourteenth change followed by a run that makes
the rest, the manifest, and the generated configuration. Native tests on
MiniAir, which register nothing, cover reading and setting real access
lists (a list written and read back, a widened list detected and restored,
a junction), the key copy with its list and a refused link, reading a
service Windows itself registers, a service that does not exist, this
process's elevation, and the token of a service running under another
account. `tests/windows_installer.rs` runs the built binary: `status`
refuses a junction and a directory it does not trust. One test in that file
runs `install` for real, with a random service prefix and a temporary tree
beside any real host, checks the registrations against `sc.exe qc`,
`qfailure` and `qprivs`, the preshutdown limit and `icacls`, installs
again, drifts a start type, an access list and a configuration entry, and
scans everything printed and written for a synthetic key. It registers four
services, so it is ignored and also refuses to run unless
`AGENTC_INSTALLER_REGISTERS_SERVICES=1` is set: a machine's owner asks for
it.

**The native install run on MiniAir, 2026-11-23.** With the owner's consent
that test was run from an elevated development session, with the debug
binary of the task's branch. Its first run stopped at the first
registration: `CreateServiceW` refused the virtual account with error 1057
because an empty password was passed where Windows wants none. The fifteen
directory, file and key changes before it had been applied and journaled,
nothing was registered, and the test removed its tree. With no password
passed, the second run passed. The test was then extended with the service
drift and the junction rows below and passed twice more, the last time on
the committed source:

| Observation | Result |
| --- | --- |
| `install` on a tree that does not exist, with a synthetic key file | Exit 0; 31 changes applied: 11 directories, the binary, the manifest, the configuration, the key and 4 changes for each of the 4 services |
| Each service, from `sc.exe qc`, `query`, `qfailure` and `qprivs` | Own process, demand start, `NT SERVICE\<name>`, the installed binary with `--config` and its role, stopped; the supervisor depends on the runner and no other service on anything; reset period 86 400 s and two restarts after 60 000 ms; exactly the two required privileges |
| Each service's preshutdown limit, read back through `QueryServiceConfig2W` | 120 s |
| The key, from `icacls` | The publication account read, the system and administrators full control, three entries and no more; the key file given was left in place |
| The state root, `runner\` and `egress\`, from `icacls` | Two entries; the runner's SID modify; the egress SID full control |
| Every service's token in the report | `stopped`: none was ever started |
| `install` again, with no arguments | Exit 0, nothing applied; `status` exits 0 |
| The runner set to automatic start with `sc.exe config`, Everyone granted on `egress\` with `icacls`, the remote edited in the configuration | `status` exits 1 and names `set_access_list`, `write_config` (entry `windows_publication.remote`) and `set_start_type`, and changes none of them; `install` applies exactly those three; `status` exits 0 |
| With `sc.exe`: the egress service's command line replaced and `SeDebugPrivilege` put on its privilege list in place of `SeImpersonatePrivilege`; the publication service's account set to LocalSystem; the runner's recovery set to one restart with a failure program; the supervisor's dependency removed | `status` exits 1 and names `set_bin_path`, `set_privileges`, `set_account`, `set_recovery` and `set_dependencies`; `install` applies exactly those five; `sc.exe` then prints the designed registration again, with no failure program; `status` exits 0 |
| `install` with a key file that does not exist; `install` onto a state directory given to another account | Each refused with exit 1; no service registered, no program directory made, the state directory left empty |
| `install` with the program directory a junction, and with the state directory a junction | Each refused with exit 1 as a link; no service registered, the other directory not made, nothing made in the junction's target |
| Everything the binary printed, refusals included, the journal, the manifest and the configuration | None holds the synthetic key's body; the installed key does |
| After the test | No `agentc` service is registered; the temporary tree is gone |

The services carried a random prefix (`agentc-t` and ten hex digits) and
lived under a temporary directory, so this is the installer on the real
Service Control Manager and file system of MiniAir but not an installation
of the product's four names in `C:\Program Files\agentc` and
`C:\ProgramData\agentc`. That is step 7c's.

**What 7a leaves unverified:**

- No service the installer registered has been started. The effect of the
  required-privileges list on a service's token is therefore not observed:
  Windows applies it when it starts the service. That is step 7c's.
- `SeImpersonatePrivilege` stays in the list because every service
  identifies its pipe client by impersonating it. Whether impersonation at
  identification level, which needs no privilege, would be enough was not
  tested.
- The refusal of a session that is not elevated is shown with the host in
  memory. The development session on MiniAir is elevated, so the native
  tests there take the elevated branch.
- Paths are checked by name, not by an opened handle. Once the two roots
  are secured only the system and administrators can change the tree, and
  each change is decided on a fresh observation, but a change of a path
  between that observation and its use is not excluded.
- The preshutdown limit is a fixed 120 000 ms for all four services. An
  owner who raises the runner's `preshutdown_timeout_seconds` gets the
  runner's own warning at start, not a longer limit.
- The service SID type and each service's own security descriptor are left
  as Windows makes them.
- The program directory's protected access list is new: the kits left that
  directory inheriting from `Program Files`. No service has started from it
  and no launch has run against it, so that the services can read it and
  that a launch cannot are both expected from the list, not observed.
- The directories the acceptance kits staged for their probes (`probe`,
  `probe-drop`, `runner\probe-launches`) are not part of the model.
- Update, rollback and uninstall are [step 7b](#step-7b-update-rollback-and-uninstall-2026-11-24).

#### Step 7b: update, rollback and uninstall (2026-11-24)

Step 7b adds `windows-host update`, `rollback` and `uninstall`. The
decisions are in the neutral `windows_installer::update`, `uninstall`,
`release` and `control` modules; the Windows backend gains the calls that
start, stop and delete a service (`StartServiceW`, `ControlService`,
`DeleteService`) and that read and delete a service account's profile
(the profile list, `HKEY_USERS` and `DeleteProfileW`). The operator's view is
the [installation chapter](windows-installation.md#update).

**Releases.** A release is a binary named by its SHA-256 and its source
commit. The installed binary keeps its one path, so the services' command
lines never change. Each release a host keeps has its binary and the
configuration it ran with in `releases\<sha256>\` below the program
directory; `installer\releases.json` names the current release, the
previous one and every release that was rolled back. `install` now records
the current release, and refuses a different binary over an installed one
whether or not the services run: replacing a release is an update, which
keeps the one it replaces.

**An update is run by the new release.** The owner states the SHA-256 and the
commit; the update is refused unless the running binary has that digest and
was built from that commit from a clean tree, and it copies itself into its
release directory. Nothing a caller supplies is run to learn what it is, and
a binary inside the program directory is refused, since it would replace or
remove itself.

**One record, one stage at a time.** Before anything changes, the update
writes `installer\update.json` and flushes it; after every stage it
rewrites it, naming the stage that is done. The stages are: stage the new
binary, keep the installed binary and configuration, stop the services
(supervisor, runner, then the brokers; Windows refuses to stop the runner
under the supervisor that depends on it), switch the binary and the
configuration, record each log's length, start all four (brokers first),
check them, and stop again those that were not running before. A service
is healthy when Windows reports it running within 60 s, its log gained its
start line after the length recorded before the start, and it still runs
5 s later; the supervisor's and the brokers' lines carry no time, so an
old line in the log never counts. The checks passing is the commit point.
A failure before it turns the record to undoing itself: stop all, put the
kept binary and configuration back, start those that ran before and check
them. A release undone after it was put in place is recorded as rolled
back, and an update to it is refused until the owner gives `--retry`. A
runner that stops with its own code for launches whose end Windows did not
confirm counts as a failed stop, so nothing is switched under a launch that
may still run.

**Interruptions.** Each stage checks what is already in place, so it may be
done again. The next `update` or `rollback`, whatever it was asked, first
finishes a record past the commit point and undoes any other. `install` and
`status` refuse while a record is open. The neutral tests kill an update in
both windows of every stage (the record of a stage written and its work not
done, and its work done and the next record not written) and check that the
next run leaves the old or the new release fully in place, with the four
services running, never a mixture. `install`'s own writes are now flushed
before they are renamed into place.

**Rollback** is the same record with the kept previous release as its
target and that release's own configuration, not a merge. It records the
release it left as rolled back and keeps no previous release, so a second
rollback is refused.

**Uninstall** stops each registered service, deletes the four, removes both
trees and deletes the four service accounts' profiles. Each step looks at
the host as it is, so it is safe to run again. A root that is a link, has
another owner or holds anything the installer did not make is refused
before anything changes. The removal never descends a link or junction
(the runner's account could plant one in its own directory) and takes no
ownership; what cannot be removed is reported. The AppContainer profiles
the launches made were made by the runner for its own account
(`CreateAppContainerProfile` acts on the caller's store), so they live in
the runner's service profile, not in the administrator's; `uninstall`
reads their names from the runner's journal before the tree goes and they
go with the runner's profile. A profile whose hive Windows still keeps
loaded is reported as pending; the next run computes the four accounts from
the service names and removes it, so no record has to outlive the tree.
Nothing touches WSL.

**Tests.** The neutral suite (`windows_installer::tests_update`, on Linux
and Windows) covers an update with its stop and start order, a failed check
and the rolled-back memory, `--retry`, the kill at every stage, the unclean
runner stop, services that were stopped before, rollback and its refusal,
every refusal of a binary that is not the release named, and uninstall
with a loaded profile, a link inside the tree, a file that cannot be
removed, foreign content, a link as a root and the push key's note. On
MiniAir, `tests/windows_update.rs` runs, with nothing registered:

| Test | What it shows |
| --- | --- |
| The update over real files with the role runner in the test process | The runner refuses new launches as soon as the stop is asked; its drain ends a launch that ignores the stop at the 1 s deadline and journals `ended`; the services come back on the second build's binary with the old one kept; the restarted runner reclaims the launch an ended runner left, leaves the drained one and one whose jobs' owner still runs, and refuses to prepare either of those again |
| `uninstall` through the built binary on a temporary tree | Both trees go; a junction planted in `runner\` is removed and its target kept; a read-only file is removed; a second run removes nothing and exits 0; the WSL configuration files and the distribution list are the same before and after |

The test that installs a throwaway host, updates it to a second build,
rolls it back and uninstalls it, with its four services registered and
started by Windows, is ignored and gated on
`AGENTC_INSTALLER_STARTS_SERVICES=1`.

**What 7b leaves unverified:**

- The in-process runner is the runner's own code, but not the runner
  service under the Service Control Manager: the drained launch is reclaimed
  by a service's next start because the old runner process is gone, which a
  runner in the test process cannot show. The gated test and step 7c run the
  services themselves.
- An update keeps the services' registrations as they are. A release whose
  model registers a service differently needs `install` after the update.
- A kill or power loss is shown against the host in memory, not against
  the machine.
- The health check is the start line and 5 s of running; a release that
  fails later is not caught by the update. `rollback` is the owner's answer.
- Removing a service profile was not run against a real one in the ungated
  tests: no service had run under those names.

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
launch cannot open the pipe at all. Step 6a gave each launch's broker pipe a
grant with a role and a generation, and step 6c checks the coordinator
authority of each publication request by asking the supervisor ([phase 6
steps](#phase-6-steps)). Since [step 6d](#step-6d-the-brokers-as-services-2026-11-20)
both brokers run as services under their own virtual accounts: each control
pipe is owned by its broker's service SID and serves only the configured
runner account, the runner uses a control pipe only when the configured
broker account owns it, and the supervisor's authority pipe serves only the
publication broker's service SID. The owner-attended run of step 6d
exercised those checks with the owner's account standing in for the runner
and the supervisor. In the [run of 2026-11-22](#step-6d-follow-up-the-services-own-launches-2026-11-22) the installed
runner and supervisor services were on the other end, each under its own
service SID.

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

### Phase 6 steps

Phase 6 adds the egress broker and the publication broker, which a launch
reaches over named pipes of its own. The owner decided on 2026-11-17 to split
it into the steps below, to check a launch's coordinator authority by
asking the supervisor service over its own pipe, and to leave a stopped
firewall service untested. Each step builds, passes its tests and is committed
on its own.

| Step | Content | Test |
| --- | --- | --- |
| 6a | Byte streams on pipe sessions; a broker pipe per launch whose identity comes from the client's token and the broker's registry; the launch's role on the Windows prepare path | Native: a launch is served its own grant whatever its request claims; another package, the same account without the package and the remote form of the path are refused; closing a launch ends its session and removes its pipe |
| 6b | The egress broker over the pipe, and a native probe of loopback inside one AppContainer package | Native: an allowed route works through the broker; the refusals in the acceptance map |
| 6c | The publication broker, which asks the supervisor service over its pipe whether each request still has coordinator authority; the CLI's Windows candidate-checkpoint client | Native: a reviewer call, a foreign launch and a request without current authority are refused |
| 6d | Both brokers as virtual service accounts with SCM entry points; the push key readable only by the publication account, with per-connection scoped tokens; a protected broker work directory; the secret scan refusing the credential digests a launch is admitted with, which nothing supplies yet (task `daa42f71`) | Native, and an owner-attended run on MiniAir: each installed service serves a launch under its own account; the push key is refused to the egress account and to three launches; a push to the real remote uses a token revoked afterwards |
| 6d follow-up | The publication broker removes a launch's repository at close, at stop and at start; the supervisor's `prepare` sends the digests of the coordinator credentials a plan gives a launch; probes of the push key and the broker's work directory from implementer, reviewer and candidate launches (task `f71179f5`) | Native, and an owner-attended run on MiniAir: the installed supervisor and runner services make a stand-in launch that both installed brokers admit; launches made by the installed runner are refused the key and the broker's directories after controls inside the launch |

Step 6a is implemented in the Windows `pipe` and `broker` modules and in
`service::pipe_policy`. A pipe session reads a byte stream
(`Session::read`), bytes already read first, and splits into a `Reader` and
a `Writer` that a reading thread and a writing thread use at once; reads and
writes each have their own event. `broker_descriptor` writes a broker pipe's access list: the
broker's account owns it, the launch's account and its package each get the
client rights, the NETWORK SID is denied, the list is protected and carries
the low label. `Admitted::launch` serves only the launch's account carrying
exactly its package. A `Broker` keeps each launch's `Grant` (launch, role,
account, package, generation) in a registry keyed by package SID, serves the
launch's pipe with one thread per instance and hands each admitted session
to its handler with the grant the registry holds for the token's package.
Opening a launch again gives it a higher generation; closing it ends a
session still going and removes the pipe's name. The runner's `prepare`
takes an optional role and journals it; the supervisor's remote host sends
the run loop's launches as implementers, and sends no attempt or generation
because its launches carry none.

What 6a demonstrates on MiniAir, with launches running as the owner in a new
logon session (`crates/supervisor/tests/windows_broker.rs`): a launch whose
request claims another launch, role and generation is answered with its own
grant; another launch's package is denied the pipe (error 5) while its own
pipe serves it; and closing a launch ends its session that is still going and
removes the pipe's name. The `broker` module's tests show that the owner's
account without the package is refused, that the remote form of the pipe's
path (`\\localhost\pipe\...`) is denied while a pipe that accepts remote
clients is reachable that way, and that opening a launch again raises its
generation. Nothing in 6a checks a grant against the coordinator, no broker
runs as a service account, and no egress or publication broker exists.

Step 6b is implemented in the Windows `egress_broker`, `broker_control` and
`egress_admission` modules. The egress broker's handler reads one HTTP
request head from a launch's session and applies the Linux proxy's policy
(`egress`): only `CONNECT host:443` to a host on the allowlist is honoured,
the broker resolves the name and contacts only a publicly routable address,
and everything else is answered 403 with nothing contacted. An honoured
request becomes a byte tunnel over the split session; whichever side ends
first ends the other, and closing the launch ends it at once. No wait of
the handler outlasts a close: the destination's socket is non-blocking and
waited on a quarter second at a time, so a destination that stops reading
holds a tunnel only until then; name resolution and the connect run on a
thread of their own that a close abandons; and a client that reads slowly
holds the destination back for as long as it takes, which only a close or
the client's leaving ends. After its last bytes the handler waits up to
five seconds for the client to leave, because disconnecting a pipe
discards what the client has not read. Each decision is a JSON line naming
the launch and the role from the grant. The public-address check shared
with the Linux proxy also judges an IPv6 address by the IPv4 address it
carries (IPv4-mapped, IPv4-translated, IPv4-compatible, the NAT64
well-known prefix and 6to4), refuses the local-use NAT64 prefix
64:ff9b:1::/48, and refuses 198.18.0.0/15 and 240.0.0.0/4. A host whose
resolver hands out fake addresses from 198.18.0.0/15 therefore gets no
egress through either proxy.

The runner reaches a broker over the broker's control pipe, which the
broker's account owns and only the runner's account may open; the runner
refuses the pipe when another account owns it. An `open` names the launch,
its role, its package and a generation, and the launch's account is the
account Windows reports for the control client, not a request field. When
`windows_service.egress_control_pipe` and `egress_broker_sid` are set, the
runner has every launch admitted before it starts and passes the launch its
pipe's path in `AGENTC_EGRESS_PIPE`. A missing broker, a pipe another
account owns, a launch with no role and a refusal each keep the launch from
starting, with no fallback. The runner service serves everything on one
thread, so each exchange with the broker (reaching its pipe, the request
and the answer) is bounded to one second on a thread of its own, and an
exchange still running then is cancelled: a broker that is busy, wedged or
gone refuses the launch rather than holding the runner. A launch whose
release failed, or whose admission ended without an answer, is kept for
release and asked again before the next admission and after the next
successful release, whether or not its journal entry is still there. The
control pipe has one instance; a client that finds it busy waits for it
within that bound rather than failing at once. The two settings are empty
by default, and then no launch is offered egress at all.

What 6b demonstrates on MiniAir, with the broker and launches running as the
owner in a new logon session (`crates/supervisor/tests/windows_egress.rs`,
2026-11-18):

| Probe from inside the launch | Result |
| --- | --- |
| CONNECT to the allowed name over the egress pipe | 200; bytes echoed through the tunnel by the test's listener on loopback |
| CONNECT to another host, to port 22, an absolute-URL GET, a CONNECT without a version | 403 each; the broker contacted nothing |
| Direct TCP to the same listener over IPv4 and IPv6 loopback | Error 10013; only the broker's tunnel reached it |
| UDP to a receiver on loopback | Error 10013; nothing arrived, while the host's own datagram did |
| Resolving a name the host resolves | Error 11003 |
| A tunnel held open while the launch is closed through the control pipe | Ended at once (error 233, the pipe broken); opening the pipe again gives error 2 |
| Binding a listener on IPv4 loopback inside the launch | **Error 10013**: the probe of loopback within one package stops here |

The last row is the probe the owner asked for. A launch whose token has no
network capability cannot even listen on loopback, so a shim inside a
launch's own package cannot offer a harness a loopback proxy address
without a network capability, which this design does not allow as a
workaround. How harness provider traffic reaches the broker is left to a
later phase. The `egress_broker` and `egress_admission` module tests show
that the public upstream never reaches a local address, that a head that is
too long gets no answer, that a destination's last bytes still arrive, that
a close ends a tunnel whose destination stopped reading and a session still
waiting for its destination within two seconds, that a client pausing for
eleven seconds still gets every byte, that a hundred back-to-back admissions
and releases on the one control pipe instance all arrive, that a failed
release is kept for the next one, that a broker that never answers holds
the caller for about a second, and that a missing broker, a control pipe
another account owns and a launch with no role are each refused. The
native runner test drives a real runner: a start with no broker serving is
refused and the launch stays prepared, a start the broker admits runs with
its pipe in `AGENTC_EGRESS_PIPE`, a cleanup while the broker is gone keeps
the launch for release, and the next admission releases it.

Step 6c is implemented in the Windows `publication_broker` and
`authority_pipe` modules, in `service::authority`, and in the CLI's
candidate-checkpoint client. The publication broker serves the candidate
push protocol of the Linux helper (`coordinator_local::candidate_push`) on
an implementer launch's own broker pipe. The request names a commit and its
tree and never a ref, a remote or a task: the broker publishes only to
`refs/agent-coordinator/candidates/<task>/<launch>` (or that launch's
create-only WIP refs) on the one remote it is configured with, taking the
task and the launch from its grant, which the runner's `open` fills from the
launch's journal entry. The commit must carry the tree the request names,
refs inside the bundle are ignored, and one launch publishes one candidate
commit, as on Linux. A reviewer launch, and a launch whose grant names no
task, are refused before anything is read; a second session of a launch
while one of its pushes is under way is refused, since a launch's pushes
share one private repository. After its reply the broker reads and discards
what the client still sends until it leaves or is quiet for five seconds,
because disconnecting a pipe discards what the client has not read.

How current coordinator authority is checked per request, as the owner
decided: the broker asks the supervisor service over the supervisor's
authority pipe, once the request line is read and before any bundle byte
is, and again after the import and the secret scan, just before the remote
is written (`serve_one_gated`). The supervisor's account owns that pipe and
only the configured publication broker account is served; the broker
refuses a pipe another account owns. The supervisor answers from
`service::authority`: its loop driver holds each launch it claimed an
attempt for, with the task and the attempt's id and generation, from the
claim until just before it releases the attempt, and every question reads
the attempt again. A launch is allowed only while its attempt is `active`
at the generation it was claimed at. A launch the supervisor does not hold
(another supervisor's, one from before a restart, one whose attempt it
released), another task, any other state or generation, a failed read, a
supervisor that is not serving and an answer that does not come within one
second are each refused with `not_authorized`, with fixed text to the
launch and the reason in the broker's log. On Windows the supervisor
service has no coordinator but the acceptance stand-in, so the attempt is
read from the stand-in's ledger; a Windows provider harness would read it
from the coordinator through the same `AttemptReader`. The Linux helper
fixes its launch at spawn and keeps `serve_one`, whose gate always passes.

The runner admits implementer launches to a configured publication broker
before they start, as with egress, sending the task the supervisor's
`prepare` now carries, and passes the launch its pipe in
`AGENT_COORDINATOR_CANDIDATE_PUSH_SOCKET`; reviewer launches are never
admitted. The CLI's candidate-checkpoint client reads the same variable on
Windows, accepts only a local pipe path (`\\.\pipe\<name>`), bundles the
candidate before it opens the pipe (a broker pipe drops a client that sends
nothing for ten seconds after it connects, so a slow bundle must not be made
while connected), opens it for identification only, so the broker can learn who the client is but never
act as it, and maps `not_authorized` to a non-retryable state conflict.
The settings are empty by default: `windows_service.publication_control_pipe`
and `publication_broker_sid`, `windows_supervisor.authority_pipe` (default
`agentc-supervisor-authority`) and `publication_broker_sid`.

What 6c demonstrates on MiniAir, with the supervisor's authority pipe, the
broker and the launches running as the owner in a new logon session
(`crates/supervisor/tests/windows_publication.rs`, 2026-11-18), pushing a
bundle the host made to a bare repository in the test's directory:

| Probe from inside the launch | Result |
| --- | --- |
| An implementer launch the supervisor holds sends its commit with another commit's tree | `revision_mismatch`; nothing published |
| The same launch sends its commit with its tree, then the same commit again | Accepted to `refs/agent-coordinator/candidates/task-1/<launch>`, both times |
| The remote's refs afterwards | Only that ref under `refs/agent-coordinator/`; the ref the bundle carried (`refs/heads/evil`) was never written |
| An implementer launch the supervisor holds no attempt for | `not_authorized` |
| A reviewer launch, held and admitted with a task | `not_authorized`, before the broker read anything |
| Another launch opens the first launch's pipe | Error 5 |
| The first launch's pipe once the runner closed it | Error 2 |
| A held launch whose attempt the coordinator then shows released | `not_authorized` |
| A held launch while the supervisor is not serving its authority pipe | `not_authorized` |
| The remote after those two | No ref under `refs/agent-coordinator/` |

The `authority_pipe`, `publication_broker` and `service::authority` tests
show that a question about a launch not held, another task, an attempt that
ended or moved to another generation, and one that cannot be read are
refused; that a missing supervisor, one whose pipe another account owns and
one that never answers refuse within the bound; and that a reviewer, a
launch without a task and a second push of a launch at once are refused.
The CLI's tests publish through a pipe served by the real push protocol and
show that a refusal for lost authority is exit 5 with its code, and that a
pipe path that is not local is refused.

What 6c leaves unverified: the broker runs as the owner, not as a virtual
service account, and its work directory is not protected (on Windows the
push protocol's private-directory protection is a no-op); no push key or
GitHub App token minting exists on Windows, so the broker has published
only to a local repository; the broker does not know an implementer's
coordinator credential digests, so its secret scan refuses only what the
CLI's own scan refuses before sending; the authority pipe has one instance,
so concurrent questions wait up to half a second for it and are refused
after that; a launch whose agent submitted keeps its authority record (and
is refused, since its attempt is no longer active) until the service
restarts; a close that arrives while a push is importing, scanning or
writing the remote waits for that Git work; and no harness has published
through it. Step 6d, task `daa42f71`, took the service accounts, the push
key and its token minting, the protected work directory and the broker's
half of the credential digests; the rest of that list still stands.

#### Step 6d: the brokers as services (2026-11-20)

Step 6d is implemented in the Windows `broker_service`, `state_directory`,
`token_inventory` and `publication_credentials` modules, in
`service::broker_settings`, and in `coordinator_local::private_directory`.

**Service entry points.** `agentc-supervisor windows-service --role egress`
and `--role publication` run a broker under the Service Control Manager;
from a console both fail at once. Each takes its account from Windows, never
from a setting. In order, the service validates its settings, secures its
state directory, sends its diagnostics to `service.log` there, logs its
token's user, groups and privileges as one JSON line, builds its handler,
creates its control pipe for the runner's account and only then reports
running. A failure at any step stops it. A stop or preshutdown request ends
the control loop within a quarter second, and closing the broker ends every
launch's sessions and removes its pipe before the service reports stopped.
`[windows_egress]` and `[windows_publication]` give every entry a default
except the accounts and the publication remote, so neither broker starts
from an empty configuration; `deploy/agentc/supervisor-windows.example.toml`
shows them.

**State directories.** A missing state directory is made with a protected
access list for the broker's account, the system and administrators. Every
default sits below `C:\ProgramData`, where any local user may create a
directory and own it, so an existing directory is used only when it is a
plain directory (no link or reparse point) that the broker's account, the
system or administrators own, and its list is then set again. The
publication broker's work root is secured the same way.

**Protected work directory.** `private_directory` was a no-op on Windows.
It now gives each launch's private repository a protected access list
naming only Owner Rights and the system, inherited below it, and refuses a
link or junction instead of following it. Administrators are not on that
list: removing a launch's repository as an administrator needs ownership
taken first.

**Credential digests.** The runner's `prepare` accepts the digests of the
coordinator credentials a launch is given (`known_digests`), checks their
form and journals them; the publication admission's `open` carries them,
the grant holds them and the broker's secret scan refuses a candidate that
adds one of those credentials. Step 6d sent none. Since task `f71179f5`
the supervisor's remote host sends them: a `Planner` names the coordinator
credentials its plan gives a launch (`coordinator_credentials`), and
`prepare` carries their SHA-256 digests. **No Windows launch is given a
coordinator credential**: the supervisor service has only the acceptance
stand-in, whose planner names none. On the path the services run today the
list is therefore still empty, and the scan refuses only what the CLI's own
scan refuses; see the [follow-up](#step-6d-follow-up-the-services-own-launches-2026-11-22).

**Push credentials.** With `[windows_publication] app_id` and
`installation_id` set (both 0 by default: no credentials, which only a
local remote accepts), the broker reads the push App's key at start and
signs with it, and stops if it cannot. For each push it reads the request
preamble first, so an empty or malformed session costs no mint; asks the
supervisor whether the launch may publish; spends one mint from that
launch's budget; asks GitHub for an installation token narrowed to the
remote's repository with `contents: write`; pushes through the gated
protocol; and revokes the token when the push ends, whatever its outcome,
logging that it did. Git reaches the token only through `GIT_ASKPASS`:
`agentc-supervisor.exe` answers one github.com prompt when
`AGENTC_PUSH_ASKPASS_TOKEN` is set. The askpass mode, the mint budget and
the preamble moved into the integrator library, so the Linux helper
`agentc-push` and the Windows broker share them. The service removes
inherited `GIT_*`, proxy and CA-bundle variables before any thread starts;
Windows needs the rest of its environment, so this is a list of names to
remove, where the Linux helper keeps only an allowed few. `[windows_egress]
must_not_open` (default: the push key's path) lists files the egress
account must not be able to open: the service tries each at start, refuses
to run if one opens and logs each refusal; a file that is not there is
logged as not checked.

**The authority pipe keeps one instance.** The publication broker is its
only client and each question is one short exchange, so the pipe was left
as it is. Concurrent pushes of several launches were not measured against
it: a question that waits longer than half a second is refused, which
fails closed.

**What 6d demonstrates on MiniAir.** The owner-attended run of 2026-11-20
installed the release binary built from `a074874` as two demand-start
services, `agentc-egress` and `agentc-publication`, each under its own
virtual account (`NT SERVICE\agentc-egress`, `NT SERVICE\agentc-publication`),
from an elevated session. A new private key of the existing push App, made
for MiniAir, was copied to `C:\ProgramData\agentc\publication-key\push-app.pem`
with inheritance removed: the publication account may read it, the system
and administrators have full control. Two ignored native tests
(`windows_egress` and `windows_publication`) then played the runner and the
supervisor as the owner's account, which both services were configured to
trust, with launches running as the owner in a new logon session. The
results below are those of the last run of each test, at `7d41049`, which
differs from `a074874` only in the publication test; the egress test had
passed with the same result at `a074874`. The run's log does not record
the commit the binary was built from; it was built in a clean worktree at
`a074874`.

| Observation | Result |
| --- | --- |
| Both services start, report running and stop under the SCM | Running after `sc.exe start`; stopped with exit code 0 after `sc.exe stop` |
| Each service's token, from its own log | User: the service's SID. Groups: Everyone, `BUILTIN\Users`, `NT AUTHORITY\SERVICE`, console logon, Authenticated Users, This Organization, LOCAL, its logon session and `NT SERVICE\ALL SERVICES`, at high integrity; not Administrators. Privileges enabled: `SeChangeNotify`, `SeImpersonate`, `SeCreateGlobal`; present but disabled: `SeShutdown`, `SeUndock`, `SeIncreaseWorkingSet`, `SeTimeZone`. Both tokens are alike |
| State directories the services made | `egress`, `publication` and `publication\work`: the service's account, the system and administrators, full control, nothing inherited |
| The egress service opens the push key at start | Refused, "Access is denied" (error 5); the service then ran |
| A launch the egress service admitted: CONNECT to github.com:443 | 200 |
| The same launch: CONNECT to example.com:443 | 403 |
| The same launch: direct TCP to a public address; resolving github.com | Error 10013; error 11003 |
| That launch's egress pipe once the runner closed it | Error 2 |
| A held implementer launch pushes a candidate commit made on `origin/main` | Accepted at `refs/agent-coordinator/candidates/<task>/<launch>` on `https://github.com/marshallr12/agent_coordinator.git`; `git ls-remote`, run by hand before the refs were deleted, listed the ref at that commit |
| The publication service's log for that push | `"token":"revoked"`, then the accepted outcome with the launch, task, role and revision |
| Three further launches, named implementer, reviewer and candidate, open the push key | Error 5 each |

The first publication run stopped after its push was accepted: the test
reused the launch that had pushed for a key probe, and a launch starts
once. The push now has a launch of its own. Three candidate refs were
published across the runs and all three were deleted from the remote
afterwards. Both services, `C:\Program Files\agentc` and
`C:\ProgramData\agentc` (the installed key with it) were removed; the two
accounts' service profiles were still loaded and stay until a restart.
The first removal stopped after deleting the services: an administrator is
denied a launch's repository below the work directory, which three launches
had left there, owned by the publication account. The removal took
ownership of the work tree and then finished. The key file the owner
downloaded was only copied, and the key stays registered with the App
until the owner removes it.

The native suites, run as the owner, show the rest: `broker_service::open`
is the start-up without the SCM; a state directory another user owns is
refused; a launch admitted with a token's digest whose candidate adds that
token is refused `secret_detected` with nothing published; `git credential
fill` asks the binary's askpass mode and gets the token; an App without a
readable key stops the publication handler from being built; a launch
without authority never causes a mint; and a spent budget and a failed
mint are refused as the Linux helper words them.

What 6d leaves unverified:

- The tests stood in for the runner and the supervisor as the owner's
  account. The brokers have not served the installed `agentc-runner` and
  `agentc-supervisor` services, and no harness has used either broker.
- The three key probes are alike in everything but their names: each is a
  less-privileged AppContainer launch with a profile of its own, running as
  the owner. None was admitted with a role, none came from the candidate
  runner, and none ran under a role account. The probe has no positive
  control inside the launch, so error 5 shows that such a package cannot
  open the key, not that the key's access list is what refuses it. The
  list itself, as `icacls` printed it, names only the publication account,
  the system and administrators.
- No credential digest reaches the broker on the services' own path: the
  supervisor sent none at this step. The refusal was shown only with a
  digest the native test supplied.
- No launch or role account tried to open the publication broker's work
  directory or a launch's repository in it. Their access lists name no
  worker: the work root's was printed by `icacls` at install, and a
  launch's repository was seen only to deny an administrator and to be
  owned by the publication account. The protection against the worker
  roles is those lists, not an observed refusal.
- The token's narrowing is what the broker asks GitHub for (one repository,
  `contents: write`), and the App itself holds only `contents: write` and
  `metadata: read` on a selected-repositories installation. The run did not
  read the minted token's scope back or try the token after its revocation;
  the log line follows GitHub's accepted revocation call.
- The installed services were asked only for the accepted paths and the key
  refusals. Reviewer, foreign-launch, wrong-tree, lost-authority and
  credential-digest refusals were shown with the broker running as the
  owner.
- A closed launch's repository stays below the publication broker's work
  directory: nothing removes it at close, at stop or at the next start, so
  the directory grows with every launch that pushes.
- The services' tokens keep the default service privileges, including
  `SeImpersonate`; no required-privilege list or bounded recovery was set
  for the two broker services, and nothing installs them. Phase 7's
  installer owns the registration, the key's access list and removal.
- The authority pipe under concurrent pushes, as above.

Follow-up task `f71179f5` took the launch repositories left behind, the
missing digests, key and work-directory probes from real worker launches,
and a run with the installed runner and supervisor services. The next
section records it and says which items of this list still stand.

#### Step 6d follow-up: the services' own launches (2026-11-22)

Task `f71179f5` changed the publication broker and the supervisor's remote
host, added probes, and ran all four services together on MiniAir.

**A launch's repository is removed.** A broker now tells its handler when a
launch is closed and when the broker itself goes (`Handler::closed`,
`Handler::stopped`). The publication handler removes the closed launch's
private repository, sweeps its work root when the broker stops, and the
service sweeps the work root when it starts, before it serves any launch:
whatever is there then was left by a broker that crashed or was killed. A
launch's turn is held while its repository is removed, so one whose push is
under way is kept and no push begins in a repository that is going. A
launch that the runner opens again while it is open keeps its repository:
the record of the commit it published is in it, and without that record
the launch's next push would be refused as a lease conflict. The sweep
removes a link or junction itself and never what it points to.

**The supervisor sends credential digests.** `Planner::coordinator_credentials`
names the coordinator credentials (token text) a plan gives a launch, none
by default. The remote host's `prepare` sends their SHA-256 digests as
`known_digests`, in order and without repeats. An empty credential, or more
than the sixteen a grant may carry, refuses the launch before the runner is
asked, rather than leaving a credential out of the scan. The stand-in's
planner gives a launch `SystemRoot` and `LOCALAPPDATA` and no credential;
tests pin that it names none and that such a launch's `prepare` carries no
`known_digests` field. Whoever writes a planner that gives a Windows launch
a coordinator credential must name it there.

**Probes.** The dependency-free probe fixture
(`deploy/agentc/windows-probe-fixture`) has a `brokers` mode. Inside a
launch it records the account and package it runs as, two positive
controls (a file it must be able to read and a directory it must be able
to list), each file and directory its spec says it must not reach, the
status line of each request over the launch's egress pipe, the reply to
one candidate push over its publication pipe and, after that push, whether
it reaches its own repository below the broker's work directory. A refusal
is recorded as the Windows error; `allowed` would be a finding.

**Native tests** (as the owner, in a new logon session where a launcher is
needed): a real push's repository, protected as `private_directory` leaves
it, is there while its launch is open and gone once the runner closes the
launch, and a stopped broker leaves its work root empty; a repository whose
launch holds its turn survives a close and a sweep and goes at the next
one; a starting service removes repositories given the private access
list; a broker's handler hears of each close and of the stop, and not of a
reopening. For the digests, the remote host prepares two launches through a
runner in the test's process: the one whose plan gives it a credential is
refused `secret_detected` by the broker when its candidate adds that
credential, and the same candidate from the launch given no credential is
published. For the probes, an implementer launch, a reviewer launch and the
reviewer's candidate (a command the reviewer hands its prefix script, so
the candidate runner makes it) each read their controls and are refused a
protected key file and the broker's work directory with error 5.

**The owner-attended run.** On 2026-11-22 the release binary built from
`b531e6f` (SHA-256 `a99be108...aeef48`) was installed from an elevated
session as four demand-start services, each under its own virtual account:
`agentc-egress`, `agentc-publication`, `agentc-runner` and
`agentc-supervisor`. Each was configured with the others' service SIDs and
with nothing of the owner's: the brokers serve only the runner's SID, the
runner uses a control pipe only when its broker's SID owns it, the
publication broker asks only the supervisor's SID, and the supervisor's
authority pipe serves only the publication broker's SID. The push App's
key was installed as in step 6d. The probes' files were put in a directory
that the runner's account and the restricted-packages group may read, and
their rows went to one both may write: a launch reads nothing below
`Program Files`.

*Stand-in round.* The supervisor's stand-in was enabled with the `brokers`
probe as its fixture and one item was queued. The installed supervisor had
the installed runner make the launch; nothing ran as the owner.

| Observation | Result |
| --- | --- |
| The launch's account, from its own token | `S-1-5-80-2505214551-...`, the SID of `agentc-runner`; a less-privileged AppContainer in a job, at low integrity |
| Controls inside the launch: read a file, list a directory | Both worked |
| Open the push key; list the key's directory, the publication broker's state directory and its work directory | Error 5, four times |
| CONNECT to `github.com:443` and to `example.com:443` over the egress pipe | `HTTP/1.1 200 Connection Established`; `HTTP/1.1 403 Forbidden` |
| The publication broker asks the supervisor about the launch | The supervisor's log has `"current":true` three times, for that launch and task |
| The launch pushes a candidate commit made on `origin/main` | Accepted at `refs/agent-coordinator/candidates/<task>/<launch>`; `git ls-remote` listed the ref at that commit; the broker's log has `"token":"revoked"` and then the accepted outcome |
| The launch then lists its own repository below the work directory and opens its `HEAD` | Error 5, twice |
| The work directory once the launch was closed | Empty; the broker's log has `"repository":"removed"` for the launch |
| The stand-in's ledger | prepare, claim, start, exit with code 0, release, cleanup; `stand-in-judge` reports no problem |

*Role round.* The installed supervisor only ever asks for stand-in
implementer launches. For a reviewer and its candidate the runner was
configured to serve the owner's account in place of the supervisor's, and
an ignored native test asked for the launches over the runner's pipe. The
launches were still made by the installed runner service, ran as its
account and were admitted by the installed brokers. The supervisor was
stopped.

| Launch | Account | Controls | Key, key directory, broker state and work directories | Egress | Push |
| --- | --- | --- | --- | --- | --- |
| Implementer, no capability | The runner's SID | Both worked | Error 5, four times | 200 and 403 | `not_authorized`: no supervisor was serving the authority pipe |
| Reviewer, `registryRead` | The runner's SID | Both worked | Error 5, four times | 200 and 403 | No publication pipe is offered to a reviewer |
| The reviewer's candidate, `registryRead` | The runner's SID | Both worked | Error 5, four times | No egress pipe is offered to a candidate | No publication pipe |

Each of the three ran in a package of its own. The first attempt at this
round stopped after the reviewer's own probe: the test's driver asked the
runner once to wait two minutes for the launch, the runner service
shortens every such wait, and the driver took the shortened answer for the
launch's end. The driver now asks again until the launch ends
(`7ce1720`, which differs from `b531e6f` only in that test), and the
second attempt passed. The runner, restarted between the attempts,
reclaimed the two launches the first one left.

*Removal.* All four services stopped with exit code 0. The work directory
was empty, and `C:\ProgramData\agentc` and `C:\Program Files\agentc` were
removed as they were, with no ownership taken. The published ref was
deleted from the remote. The service profiles of `agentc-runner` and
`agentc-supervisor` were still loaded and stay until a restart; those of
the two brokers date from the run of 2026-11-20 and were left for the same
removal.

**Which items of the step 6d list this closes.** The brokers have served
the installed runner and supervisor services. The key probes now come from
an admitted implementer launch, a reviewer launch and a candidate made by
the candidate runner, under the account the runner service gives its
launches, each after controls that worked inside the launch. Launches have
tried the publication broker's work directory, and one its own repository.
A closed launch's repository is removed. The supervisor's `prepare` sends
digests when a plan names a credential.

**What this leaves unverified:**

- No Windows launch is given a coordinator credential, so the installed
  services sent no digest. The refusal on the supervisor's own path was
  shown with a runner in the test's process, as the owner.
- In the role round the runner served the owner's account. The supervisor
  service has asked the runner only for stand-in implementer launches; no
  supervisor asks for a reviewer launch on Windows yet.
- The probes open a file for reading and list a directory. They do not try
  to write, delete or rename a target, to reach it through another name,
  or to find an inherited handle.
- Only the stand-in implementer pushed, so only it tried a repository of
  its own. The reviewer and the candidate tried the work directory.
- The start-up sweep was shown natively, as the owner, with repositories
  made for the test. The installed publication service was never killed
  while a repository existed, and at its stop the work directory was
  already empty.
- A close waits for the launch's sessions to end before the repository is
  removed, so the guard for a push under way was shown only by holding the
  launch's turn in a unit test.
- No harness has used either broker.
- From the step 6d list these still stand as written: the minted token's
  scope and its use after revocation, the installed services' reviewer,
  foreign-launch, wrong-tree and credential-digest refusals, the services'
  default privileges and missing recovery settings, the installer, and the
  authority pipe under concurrent pushes. The lost-authority refusal was
  seen from the installed publication service in the role round, for want
  of a supervisor.

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
| `check_egress` | Partial. Direct TCP over IPv4 and IPv6, loopback, the local network, UDP and DNS are denied to a token with no network capability, and the authorized pipe route works. With the firewall profiles disabled the refusals held in the [launcher's role-account run](#role-account-run-through-the-launcher-2026-11-09). Since [step 6b](#phase-6-steps) an allowlist egress broker serves each launch over its pipe: an allowed CONNECT is tunnelled, and another host, another port, a request that is not a CONNECT and a malformed one get 403 with nothing contacted, while a direct connection from the launch to the broker's own destination fails. The broker runs as the owner in that test. In [step 6d](#step-6d-the-brokers-as-services-2026-11-20) the installed service `agentc-egress`, under its own virtual account, answered a launch's CONNECT to github.com:443 with 200 and another host with 403, while the launch's direct TCP to a public address and its name resolution failed. No WFP policy exists, and a stopped firewall service is untested |
| `check_claude_launch`, `check_relay_probe`, shell execution | Partial. Native Claude Code 2.1.326 ran in a less-privileged reviewer launch, and its Bash command went through the prefix and the shim into the candidate ([step 5f](#native-claude-code-in-a-contained-reviewer-launch-2026-11-17)). The network and the credential were given for that one owner-attended run, and no harness has run through the egress broker. The candidate runner's readiness check refuses a broken runner (step 5e). With an egress broker configured, the runner refuses to start a launch the broker does not admit or whose broker is missing (step 6b) |
| `check_claude_token`, `check_candidate_token`, reviewer isolation | Partial. A synthetic credential file is denied, and implementer and reviewer packages cannot reach each other. In step 5e a synthetic credential in the reviewer's environment, files, inherited handle and registry was out of its candidate's reach, and so were the reviewer's process, its pipe, another launch and the runner. Whether the candidate can reach the real credential the harness held in step 5f was not probed |
| `check_push_kill`, `check_push_launch`, `check_live_helper` | Partial. A per-launch pipe admits only its package, a foreign launch is denied, and the broker takes identity from the client token. Since [step 6c](#phase-6-steps) a publication broker publishes only an implementer launch's own candidate ref, refuses a reviewer launch, a wrong tree and a launch without current coordinator authority (asked of the supervisor before each push), and a closed launch's pipe is gone. In [step 6d](#step-6d-the-brokers-as-services-2026-11-20) the broker ran as the service `agentc-publication` under its own virtual account and published a held implementer launch's candidate to the real remote under that launch's own ref, with a token minted for that push and revoked after it. The refusals above were shown with the broker running as the owner; the installed service was asked only for the accepted push. A closed launch's repository stays in the broker's work directory, and no crash sweep exists |
| `check_push_key`, `check_attention_token` | Partial. The synthetic credential stand-in is denied to every launch. In [step 6d](#step-6d-the-brokers-as-services-2026-11-20) the real push App key sat in a directory whose access list names the publication service's account (read), the system and administrators: the publication service read it and signed with it, the egress service's account was refused it at start (error 5), and three less-privileged launches, named implementer, reviewer and candidate but otherwise alike, were each refused it (error 5). Those launches ran as the owner with no positive control; the role accounts of phase 4 and a real candidate launch were not probed, and there is no attention token on Windows |
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
| `observe` | Looks whether the launch ended | `Observation`: running, exited with a code, or gone (the host no longer knows the launch, so no exit was seen) |
| `request_stop` | Asks the launch to end by itself | Nothing |
| `terminate` | Ends the launch and everything it started | Nothing |
| `recover` | Judges a saved launch record without starting anything | `Recovery`: may be running, or ended |
| `cleanup` | Removes the launch's tree and run directory | Nothing |

The loop writes the `StartIntent` into the launch record before the start and
adds the `LaunchIdentity` after it. A failure to look is "running", and
"ended" needs evidence: a lost observer proves nothing. "Gone" is evidence
too: only a host that asked where the launch stands and was told it no
longer exists says it ([task `76685e6b`](#settling-a-launch-the-runner-no-longer-holds)).

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
| `startup` | Judges every journaled launch: one Windows says ended is reclaimed (its profile, its control and working directories and its entry; the directories since task `76685e6b`), one that may still run is left untouched and reported, a failed reclaim and an unreadable file are reported. Its journal interface has no way to start a launch |
| `quiet` | Which log lines a service writes while one failure repeats: the first, then the same line once a minute with how often it was held back (task `76685e6b`) |
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
manager. The preshutdown time limit is the **installer's** to set (runbook
step 4). Until task `76685e6b` the runner tried to set its own from
`preshutdown_timeout_seconds` at every start, and under the SCM its virtual
account was refused every time (`OpenServiceW` error 5: it lacks the right to
change its own service's configuration), which logged a warning about a limit
the installer had already set. The runner now only **reads the limit back**
(`QueryServiceConfig2W`, which needs the right to read the configuration) and
logs it: `preshutdown time limit is 120s`, or that it is less than
`preshutdown_timeout_seconds` and the installer must set it, or why it could
not be read. None of these stops the service. The setting still bounds the
drain: it must exceed the preshutdown grace plus the termination patience.

**The runner as a service** (`host_runtime::windows::runner_service`). It
starts in this order and reports running only at the end: validate the
settings, make the state directory, read back the preshutdown limit, create the
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
| `observe` | `observe` with no wait; if that fails, `recover` | Running, unless the journal no longer names the launch or says an earlier runner's launch ended: then gone ([task `76685e6b`](#settling-a-launch-the-runner-no-longer-holds)) |
| `request_stop`, `terminate` | The same | Logged once, then once a minute with a count; the launch is not taken to have ended |
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

### Settling a launch the runner no longer holds

Task `76685e6b` changed this after the runner-crash round of the third 4c
machine run (task `4197c362`, MiniAir, 2026-11-13). Everything here is
**implemented** and tested on native Windows and, where it is platform
neutral, on Linux. The
[fourth machine run](#phase-4c-fourth-machine-run-2026-11-14) (task
`ec6d901c`, 2026-11-14) ran it under the Service Control Manager: in its
runner-crash round the supervisor traced `gone release cleanup` and kept
running, the restarted runner had already removed the launch's directories,
and every start of the runner read back its preshutdown limit. The other
rules below were not specifically exercised there.

**What the run showed.** The fixture ended within 3 seconds of the runner's
death. Windows restarted the runner, and at start it judged the launch ended
and reclaimed it, which removed the launch's journal entry. From then on the
runner answered every request about that launch with "is not prepared" or
"was not prepared by this runner". The supervisor counted each refusal as
"running", as the rules above say. It asked once a second and logged every
refusal: 43,847 lines, 5.8 MB in 12 hours. It renewed the dead launch's lease
until the 240-minute budget ran out and drained it. The runner refused the
stop request and the termination as well, so the supervisor went back to
waiting. A service stop then waited on the same launch, and the service stayed
`STOP_PENDING` until the machine was restarted. The supervisor's launch record
(`released: false`) and the launch's two directories were left behind, and
`stand-in-judge` still said ok.

**The rules now.**

- **A launch the runner no longer holds is gone.** When `observe` fails,
  `RemoteHost` asks the runner's journal where the launch stands, as
  `recover` does. If the journal no longer names it and has no unreadable
  entry, or says an earlier runner's launch ended, `observe` answers
  `Observation::Gone`. Otherwise it answers running, as before: a failed
  exchange, a refusal about a launch the runner still holds, a torn entry
  and a journal that cannot be read prove nothing. On `Gone` the loop ends
  `supervise` as if the launch had exited, with no exit code. It releases the
  attempt with a handoff that says the launch ended unseen and its exit code
  is unknown. It reports exit code -1 (`UNSEEN_EXIT_CODE`) and then cleans up
  as usual. The supervisor only observes a launch after its start succeeded,
  so a journal without it means the runner reclaimed it. A runner reclaims a
  launch only when Windows says both its root process and its job's owner
  are gone.
- **The restarted runner removes the directories.** The startup reclaim now
  removes the ended launch's control and working directories as well as its
  profile and entry. The entry is the only record of those directories, and
  nothing reads a launch's directories once it has ended. The supervisor's
  later `cleanup` finds nothing to remove and says so. If removing the
  directories fails, the entry stays for the next start, and the supervisor's
  `cleanup` sends `reclaim` with `remove_directories`.
- **A drain cannot wait for ever.** Once a drain has terminated the launch, the
  loop waits at most `SETTLE_AFTER_TERMINATE` (10 seconds) to see it end. A
  drain can come from a stop request, a spent budget or an attempt the
  service ended. If the launch still has not been seen to end, the loop gives
  up: the launch is **unsettled**. It may still run, so its attempt is **not
  released** and its record stays. On the next pass, `record::recover` judges
  it as it judges a launch left by a crashed supervisor, and the host claims
  nothing new until the launch is judged ended. A service stop is therefore
  honored within `[run] drain_seconds` plus 10 seconds, even while the runner
  cannot be asked at all. The same bound applies on Linux, where a killed
  process group is normally seen to end at the next look.
- **A repeated failure is logged once a minute.** `service::quiet` writes the
  first line of each kind (`observe`, `request_stop`, `terminate`,
  `recover`). It holds back the same line until a minute has passed, then
  writes it with the count held back (`... (repeated 60 more times in the
  last 60 s)`). A different line first says how often the previous one was
  repeated. An answer from `observe` resets its kind.
- **The judge reports what was left.** `stand-in-judge` now also reports a
  launch that was asked to start and was never released, or never cleaned up
  (a cleanup that found nothing counts). It counts a `gone` ledger line as an
  end. It is meant for a ledger whose run has ended; a launch still running
  counts as never released. On the third run's ledger it would have reported
  the runner-crash launch on both counts.

**Tests.** `remote::crash_tests` reproduces the runner-crash sequence
through the supervisor's own loop. The first runner journals a prepared,
then started, launch with two directories and "crashes". A real `Runner`
over the same journal is settled as the runner service settles it at
start, and it answers every later request. The loop sees the launch gone,
releases it with the unseen-exit handoff and cleans it up. The ledger has
`gone`, `release` and `cleanup` (`removed: nothing`) and the judge finds
nothing. The supervisor's record, the two directories and the journal entry
are all gone. `remote`'s scripted tests cover every standing that is and is
not "gone". `service::supervisor` has a stop while a launch can never be
seen to end: it ends within the drain plus the settle window, nothing is
released, the record stays and the judge reports the launch. It also has a
launch the host no longer knows at the first look. The judge, `quiet` and the
preshutdown read-back (`scm`, against the Event Log service) have unit tests.

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
restart it settles what it left through the host runtime's `recover`. If the
terminated launch is still not seen to end within 10 seconds, the loop returns
without releasing it and leaves it to that recovery
([task `76685e6b`](#settling-a-launch-the-runner-no-longer-holds)). The
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
that was started and is not known to have ended, a launch that was started
and never released or never cleaned up (since task `76685e6b`), and any
unreadable line. It exits non-zero for any of those and for a ledger with no
event.

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
the remaining rounds were not run. Task `ec6d901c` ran every step on
2026-11-14 ([fourth machine run](#phase-4c-fourth-machine-run-2026-11-14)):
step 6 and every round of step 7 passed, the reboot round on its second
attempt, and step 9 removed everything after one restart. Stop at the first
step that does not do what it says; do not work around it.

Corrections from the fourth machine run:

- Step 7, reboot and power loss: before the first half, turn off Windows'
  automatic restart sign-on, either with the
  `DisableAutomaticRestartSignOn` policy or with the owner's own setting
  (Settings > Accounts > Sign-in options, "Use my sign-in info to
  automatically finish setting up"). With it on, a Start-menu restart signs
  the owner in seconds after boot and shows only the lock screen, so waiting
  at that screen does not keep the machine signed out. The sign-in screen
  must ask for a PIN or password.
- Step 7, reboot and power loss: Windows' own Window Manager (`DWM-n`) and
  Font Driver Host (`UMFD-n`) sessions are interactive logon sessions that
  start at every boot. They are not a sign-in; count only a real account's
  session.
- Step 7, reboot and power loss: the logon type and the process that made
  the sign-in do not show who made it. On MiniAir the owner's own PIN
  sign-in and automatic restart sign-on both log type 11 by `svchost.exe`.
  Check instead that automatic restart sign-on was still off and that the
  first sign-in came at least the waiting time (three minutes) after boot.

Corrections from the third machine run:

- Step 7, runner stop: Windows refuses `sc.exe stop agentc-runner` while
  the supervisor runs (error 1051), because the supervisor is registered
  with `depend= agentc-runner`. Task `ec6d901c` redefined the round rather
  than the dependency: the refusal is what keeps a launch's runner from
  being stopped under its supervisor, so the round now expects it (see the
  table below).
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
   | Runner stop | `sc.exe stop agentc-runner` during a launch | Windows refuses the stop with error 1051, because the supervisor depends on the runner; both services keep their process ids; the launch runs to its own end and is released and cleaned up as usual (redefined after the [third run](#phase-4c-third-machine-run-2026-11-13)) |
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

### Phase 4c fourth machine run, 2026-11-14

Task `ec6d901c-1277-40bb-93b7-babaf51d2cc5`, on MiniAir (Windows 11 Pro
10.0.26300). `agentc-supervisor.exe` and the probe fixture were built in
release mode (`--locked`, rustc 1.99.1) from a clean checkout of `main` at
`29d97e9c0b31b013c5778de8604f2cded53d7d9a`, which contains task
`76685e6b`'s result (`bfb5046`). The binary's SHA-256 was
`3525bddf3fa556f486c493c9175eeae46fd24b2c7c959b3d47e62df64499069d`; the
fixture's was
`fc037078de664908708c640d9ae124ff564efecdb868b8033b9b6150e09d4a15`. Before
the install the machine had no `agentc` service, directory or service
profile. The evidence is coordinator artifact
`d8804bf9-597a-4f15-a118-a7d9aee69b0c`, SHA-256
`eacb8d164ed50e928a1e17e1bad669dd4cefc94e42908271fb513b578a774b91`: one zip
of the run's logs, ledgers, both service logs, heartbeats, the services'
application and Service Control Manager event log entries, the System log
events of the reboot and power-loss rounds, the sign-in diagnosis,
`sc.exe qc` and `qfailure` output, token privileges and groups, the
manifest with the commit and hashes, and the scripts, packed by registered
job `523f361b-0481-4854-9fee-0e32ca643559`. It was packed from a copy of
the kit in which the owner's account name is replaced with `OWNER`; its
`SANITIZED.txt` lists the three files that changed. All times are UTC; the
run was on the evening of 2026-11-13 local time.

Who ran what: on the owner's instruction, the agent ran every step it could
from the owner's elevated Claude Code session, without a go-ahead per step.
The logoff, reboot and power-loss rounds end that session; only the owner
signs out, restarts or cuts power.

The kit was the third run's, with two changes. The runner-stop round was
redefined (see the corrections above the runbook), and every round now
writes whether the session's trace holds `release` and `cleanup`, because
`stand-in-judge` saying `ok: true` does not show that the attempt was
released.

**Install (steps 1 to 5), 01:28: passed as written.** The preshutdown limit
read back 120 000 ms for both services. The runner's `service.log` says
`start-up: 0 reclaimed, 0 may still run, 0 failed, 0 unreadable` and
`running`; the supervisor's heartbeat says `state: "stopped"` with the
reason that no provider harness is supported on Windows; no pipe client
appeared.

**Step 6, 01:29 to 01:31: passed three times.** `s6-first`,
`s6-same-station` (same runner process) and `s6-after-restart` (after both
services were stopped and started) each traced `prepare claim start
start_result exited release cleanup` with `removed: launch`.

**Rounds that need no session change, 01:31 to 01:44: all passed.**

| Round | Result |
| --- | --- |
| Stop | **Passed**: `... start_result stop_requested terminate exited release cleanup`; the supervisor `STOPPED` with exit code 0; the fixture gone |
| Runner stop, as redefined | **Passed**: `sc.exe stop agentc-runner` refused with 1051; both services kept their process ids; the launch ran to its own end and traced `exited release cleanup` |
| Supervisor crash | **Passed**: Windows restarted the supervisor after 60 s (pid 1100 to 9292); the trace shows `recover:may_be_running` while the fixture ran, then `release` and `cleanup`; the second item was claimed only after that |
| Runner crash | **Passed**: the fixture was gone within 3 s of the runner's death; Windows restarted the runner (pid 20256 to 13516); the supervisor traced `gone release cleanup` (`removed: nothing`, the restarted runner had reclaimed the directories) and kept running. Task `76685e6b`'s fix holds on the machine |
| Child crash | **Passed**: `exited` with code 1, `release`, `cleanup` with `removed: launch` |

After every round `stand-in-judge` found no problem and no fixture process,
AppContainer mapping or folder, or launch directory was left.

**Logoff, 01:46 to 01:54: passed.** The owner signed out and in again at
01:51. Afterwards the runner (pid 13516) and the supervisor (pid 13492) had
the same process ids and the fixture was still running; an ordinary
supervisor stop then traced `stop_requested terminate exited release
cleanup`. The judge found no problem and nothing was left.

**Reboot, first attempt, 01:57 to 02:09: failed.** Both services were set to
`auto` and a launch was started at 01:57:46. The owner restarted from the
Start menu at 01:58:51. Preshutdown asked the launch to stop at 01:58:58
and ended it at 01:59:28, and Windows shut down at 01:59:47, well within
the 120 000 ms limit. Windows booted at 01:59:56. After boot the supervisor
traced `recover:ended release cleanup` (`removed: nothing`); the item was
asked to start once, the fixture was gone, the judge found no problem and
nothing was left.

The round still does not show what it must: that both services run *without
logon*. The owner waited three minutes at the screen after boot, as asked,
but Windows had already signed the owner's account in at 02:00:10, 14 s
after boot (logon type 11, started by `svchost.exe`). With the owner still
waiting, that can only be automatic restart sign-on, which then shows only
the lock screen. The second attempt showed that the logon type and process
alone do not prove it. The runner started
at 02:00:15 and the supervisor at 02:00:16, after that sign-in. The kit's
own check was wrong too: it counted Windows' `DWM-1`, `UMFD-0` and `UMFD-1`
sessions (02:00:05) as the first sign-in, so its "before sign-in" could
never have been true. Both are recorded as corrections above the runbook.
The kit was fixed during the run: `session-before.ps1` refuses the reboot
and power-loss rounds while automatic restart sign-on is on, and
`session-after.ps1` counts only a real account's session and logs which
process made it. The reboot round is run again.

**Reboot, second attempt, 02:17 to 02:29: passed.** The owner turned
automatic restart sign-on off ("Use my sign-in info to automatically finish
setting up"), and `session-before.ps1` read it back as off (owner opt-out 1).
Both services were set to `auto` and a launch was started at 02:17:38. The
owner restarted from the Start menu at 02:19:20. Preshutdown asked the
launch to stop at 02:19:26 and ended it at 02:19:56, and Windows shut down
at 02:20:23, within the 120 000 ms limit. Windows booted at 02:20:38. The
runner started at 02:20:55 and the supervisor at 02:21:05. The owner waited
at a sign-in screen that asked for a PIN and signed in at 02:27:28, 6 min
50 s after boot. That was the only interactive sign-in since boot. At
02:21:05, before the sign-in, the supervisor traced `recover:ended release
cleanup` (`removed: nothing`). No System log error named an `agentc`
service. The item was asked to start once, the fixture was
gone, the judge found no problem and nothing was left. Both services went
back to `demand`.

The kit's own check called this a failure: it said that a sign-in made by
`svchost.exe` means Windows signed in by itself. That rule was wrong. The
owner's PIN sign-in was logged exactly like the first attempt's automatic
one: a `4624` event of type 11 by `svchost.exe`, logon process `User32`,
package `Negotiate`, and the same Winlogon events. The owner confirmed
having signed in by hand. Only the time after boot, with automatic restart
sign-on off, tells the two apart. On the owner's decision the round is
recorded as passed and the check as a finding. `session-after.ps1` now logs
whether automatic restart sign-on is still off and how many seconds after
boot the first sign-in came, which must be at least 180.

**Power loss, 02:38 to 02:47: passed.** `session-before.ps1` read
automatic restart sign-on back as off (owner opt-out 1), both services were
set to `auto`, and a launch was started at 02:38:55 (fixture pid 12540,
runner pid 4928, supervisor pid 8384). The owner then held the power button
until the machine went off, with no shutdown, and powered it on again.
Windows booted at 02:40:47; the System log has `41` (Kernel-Power, rebooted
without cleanly shutting down) and `6008` (previous shutdown unexpected).
The `6008` event gives 02:20:53 as the time of that shutdown, which is
Windows' last recorded "alive" time, not the moment the power was cut. No
System log error named an `agentc` service. The runner started at 02:41:06
and the supervisor at 02:41:13. At 02:41:13, with no preshutdown having run,
the supervisor traced `recover:ended release cleanup` (`removed: nothing`):
the launch had ended with the machine and was settled from the journal and
the ledger. The owner waited at a sign-in screen that asked for a PIN and
signed in at 02:46:04, 317 s after boot, with automatic restart sign-on
still off, so both services ran and settled the launch before any sign-in.
The item was asked to start once, the fixture was gone, the judge found no
problem and nothing was left. Both services went back to `demand`.

**Record and removal (steps 8 and 9), 02:49 to 02:50: as written.** Both
services stopped with exit code 0, supervisor first. The final check found
no problem in the ledger and no fixture process, AppContainer mapping or
folder, or launch directory left. Both services were deleted, and
`C:\Program Files\agentc` and `C:\ProgramData\agentc` were removed. As the
third run's correction to step 9 says, both service profiles were still
loaded and wait for `profiles.ps1` after a restart.

**Profile removal (step 9), 03:01: as written.** The owner restarted from
the Start menu at 02:56:52; Windows booted at 02:57:18. `profiles.ps1`
removed `C:\WINDOWS\ServiceProfiles\agentc-runner` and
`C:\WINDOWS\ServiceProfiles\agentc-supervisor`, both created by this run's
install at 01:28, and found no profile, profile folder or service of either
account left (`removal-profiles.json`).

| Item | Result |
| --- | --- |
| Registration from the recorded commit and hash under two virtual service accounts, failure actions, preshutdown limit, access lists | **Demonstrated** |
| The runner reads its preshutdown limit back | **Demonstrated**: every start of the runner logs `preshutdown time limit is 120s`; the third run's error 5 is gone (task `76685e6b`) |
| Step 6 under the SCM: prepare, start, exit seen, release, cleanup | **Demonstrated** three times |
| Further launches in the same station by the same runner process | **Demonstrated** (`s6-same-station`) |
| A launch after the runner was stopped and started through the SCM | **Demonstrated** (`s6-after-restart`) |
| A launch by a runner the SCM restarted after a crash | **Demonstrated**: the child-crash and logoff launches were made by runner process 13516, which Windows started after the runner-crash round |
| Round: supervisor stop | **Passed** |
| Round: runner stop, as redefined | **Passed**: refused with 1051, both process ids unchanged, the launch ran to its end and was released and cleaned up |
| Round: supervisor crash | **Passed** |
| Round: runner crash | **Passed**: the supervisor released the launch the restarted runner had reclaimed and kept running; task `76685e6b`'s fix holds |
| Round: child crash | **Passed** |
| Round: logoff | **Passed** |
| Round: reboot with preshutdown | **Passed** on the second attempt. The first attempt **failed**: automatic restart sign-on signed the owner in before either service started |
| Round: power loss | **Passed** |
| Preshutdown drains within the limit | **Demonstrated** twice: the launch was asked to stop 6 to 7 s after the restart began, ended at the 30-second drain deadline, and Windows shut down 56 to 63 s after the restart began, within 120 000 ms |
| Boot without logon | **Demonstrated** twice, with automatic restart sign-on off: after the second reboot and after the power loss both services started and settled the earlier launch before the first sign-in, 410 s and 317 s after boot |
| Orphan cleanup of launches after a reboot and a power loss | **Demonstrated**: each time the restarted runner reclaimed the launch (`start-up: 1 reclaimed`), the supervisor traced `recover:ended release cleanup`, the item was asked to start once, and nothing was left |
| Every claimed attempt released and cleaned up, checked in the trace | **Demonstrated** for step 6 and every round |
| The kit's sign-in check for the reboot and power-loss rounds | **Failed** twice and fixed during the run: it counted Windows' own sessions as a sign-in, and it took a sign-in made by `svchost.exe` to be automatic. Both are corrections above the runbook |
| Removal of both services, both trees and both service profiles | **Demonstrated**, the profiles after one restart |

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
| 4c | Owner-run machine acceptance: boot without logon, logoff, stop, preshutdown and reboot, forced service and child crashes, orphan cleanup, and its record | 4a and 4b | Run by task `62d9942f` on 2026-11-11 and **stopped at runbook step 6**: both services install, run and stop under the Service Control Manager, but the runner **cannot make a launch** as a service (`station_not_ours`). No lifecycle round was run. Task `7f6f242d` fixes the window station and a refused prepare's leftover directories; see [Phase 4c machine run](#phase-4c-machine-run-2026-11-11). Run again by task `7b848696` on 2026-11-12 with that fix and **stopped at step 6 again**: Windows refuses the runner's account full access to its window station, so the take-over refuses; a refused prepare now leaves nothing. See the [second machine run](#phase-4c-second-machine-run-2026-11-12); task `8981c84a` followed. Run a third time by task `4197c362` on 2026-11-13 from `103e640`: step 6 **passed** three times; the stop and supervisor-crash rounds **passed**, the runner-stop round **cannot run as written** (error 1051), and the runner-crash round **failed**: the supervisor never releases a launch the restarted runner reclaimed, and then does not stop. Child crash, logoff, reboot and power loss were not run. See the [third machine run](#phase-4c-third-machine-run-2026-11-13); task `76685e6b` follows. Run a fourth time by task `ec6d901c` on 2026-11-14 from `29d97e9`, which holds that fix: install, step 6 and **every lifecycle round passed**, the reboot round on its second attempt after automatic restart sign-on was turned off, and both services, trees and service profiles were removed. See the [fourth machine run](#phase-4c-fourth-machine-run-2026-11-14). The phase is complete when that task's review and integration are; the runbook does not cover a real coordinator or any provider harness |
| 5 | Reviewer candidate isolation on real native harness command paths | Phase 3 | **Steps 5a to 5f done**; the phase is complete when task `9cf04a5c`'s review and integration are. It first **closed at step 5c, blocked**, by task `61fe9ca2` on the owner's decision of 2026-11-15. The [command paths](#native-claude-code-command-paths-2026-11-14) of native Claude Code 2.1.325 are observed; the [candidate runner](#proposed-candidate-runner) is a proposal, built in [steps](#phase-5-steps). Steps 5a (command-form parser and capability record) and 5b (pipe admission by package) are implemented, and 5c (the candidate launch) is implemented. It was **blocked** because [Git Bash does not start in an AppContainer](#git-bash-does-not-start-in-an-appcontainer-2026-11-14). Task `d8ae17f5` on 2026-11-16 found that a pinned busybox-w32 shell [starts there](#a-posix-shell-that-starts-in-an-appcontainer-2026-11-16) and that native Claude Code 2.1.325 accepts it behind a shim-like prefix (observed uncontained); with it step 5c's native tests **pass**. Task `9cf04a5c` on 2026-11-17 implemented step 5d (the candidate pipe's threads and the shim) and step 5e (the readiness check and the fixture checks), whose native tests pass. In step 5f, an owner-attended run [inside a contained reviewer launch](#native-claude-code-in-a-contained-reviewer-launch-2026-11-17) sent a Bash command from native Claude Code 2.1.326 in the recorded form through the prefix and the shim into the candidate. The capability record now admits only that build. No Windows driver calls the record, so no product Claude launch runs on Windows. The [items still open](#phase-5-steps) include the process a job still counts after the shell ends |
| 6 | Egress and publication brokers | Phase 3 | Steps 6a, 6b and 6c implemented (task `13ae81c1`); step 6d implemented and run by the owner on MiniAir on 2026-11-20 (task `daa42f71`): both brokers served a launch as services under their own virtual accounts, and publication pushed to the real remote with a per-push token. Task `f71179f5` followed on 2026-11-22: the installed supervisor and runner services made a stand-in launch that both installed brokers admitted, launches made by the installed runner were refused the push key and the publication broker's directories, and a closed launch's repository is removed ([follow-up](#step-6d-follow-up-the-services-own-launches-2026-11-22)). No harness has used either broker; see [phase 6 steps](#phase-6-steps) |
| 7 | Idempotent installer, update/rollback/uninstall, then a host acceptance gap report; the real coordinator loop, a harness driver, WFP, resource caps and quiet hours are a later phase 8 (owner decision of 2026-11-23) | Phases 4 to 6 | Step 7a implemented (task `ddeb5d76`): `agentc-supervisor windows-host install` and `status`, a desired-state model with a platform-neutral plan, and a Windows backend over the Service Control Manager. **No service the installer registered has been started and nothing is machine-accepted.** Step 7b implemented (task `2064cf54`): `update`, `rollback` and `uninstall`, each stage journaled, driven natively with the runner in the test process. Steps 7c (owner-attended acceptance) and 7d (gap report) are not started; see [phase 7 steps](#phase-7-steps) |

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
- [busybox-w32](https://frippery.org/busybox/)
