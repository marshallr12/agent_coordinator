# Windows host installation

`agentc-supervisor windows-host install` makes a Windows machine a supervised
host: it registers the four services, makes their state tree, writes their
configuration and, when there is a push App, installs its key. `windows-host
status` reports how a machine differs from an installed host and changes
nothing.

**What this is not.** No provider harness runs on Windows yet: the installed
supervisor service claims nothing unless the acceptance stand-in is enabled.
An installed host is not an admitted host. Update, rollback and uninstall
arrive with step 7b of the [design](windows-host-design.md#phase-7-steps);
until then a host is removed by hand. The installer has been exercised by its
tests only; the owner-attended acceptance run is step 7c.

## Prerequisites

- Windows 11 on x86-64. The one evaluated machine runs Windows 11 Pro.
- An **elevated** session of a member of the Administrators group. Both
  subcommands refuse anything else before they read anything.
- `agentc-supervisor.exe`, built in release mode from the exact commit you
  mean to run, with its SHA-256 recorded:

  ```text
  cargo build --release --locked -p agentc-supervisor
  Get-FileHash target\release\agentc-supervisor.exe -Algorithm SHA256
  ```

  `install` copies the binary it is run from. Run it from the build output or
  from any copy; do not copy it into `C:\Program Files\agentc` yourself.
- The remote candidates are published to. With a push App it must be
  `https://github.com/<owner>/<repo>.git`.
- Optionally a GitHub App that may push: its App id, its installation id on
  that repository and a PEM private key. The App needs `contents: write` and
  `metadata: read` on the selected repository and nothing else.
- Git for Windows on the machine's `PATH`, for the publication service. The
  installer does not install or check it; see
  [the pinned native tools](#the-pinned-native-tools).

No configuration file is needed beforehand, and none is read: the installer
writes it.

## Install

The first run needs what has no default: the remote, and the push App if
there is one.

```text
agentc-supervisor.exe windows-host install ^
    --remote https://github.com/OWNER/REPO.git ^
    --app-id 123456 --installation-id 7654321 ^
    --key-source C:\Users\you\Downloads\private-key.pem
```

Without a push App, give `--remote` alone: the publication service then has
no credentials, which only a local remote accepts.

| Argument | Meaning | Default |
| --- | --- | --- |
| `--remote URL` | The one remote candidates are published to | None; required on the first run |
| `--app-id N`, `--installation-id N` | The push App and its installation, given together and with `--key-source` | No App |
| `--key-source PATH` | The App's PEM private key. It is copied, never moved, shown or logged | No key |
| `--start demand\|auto` | When Windows starts the services | `demand` |
| `--stand-in-fixture ARG...` | Enables the acceptance stand-in with this command. The installer records the command and never runs it. Give it last: everything after it is the command | Stand-in off |
| `--no-stand-in` | Turns the stand-in off again | |
| `--program-dir PATH` | Where the binary and the configuration go | `C:\Program Files\agentc` |
| `--state-dir PATH` | The root of the services' state | `C:\ProgramData\agentc` |

Everything `install` is told is recorded in `installer\manifest.json` below
the state root. A later run needs an argument only to change something:

```text
agentc-supervisor.exe windows-host install
agentc-supervisor.exe windows-host install --start auto
```

`install` prints one JSON report and exits 0 when the machine is an installed
host, and 1 otherwise:

- `applied` lists the changes this run made, in order, each with what was
  found and what was wanted. On an installed host it is empty: a second run
  changes nothing.
- `refusals` lists why nothing was changed, or nothing more.
- `tokens` lists each service's token against the privileges it may hold.

The services are registered **stopped**. Start them yourself, the brokers
first:

```text
sc.exe start agentc-egress
sc.exe start agentc-publication
sc.exe start agentc-runner
sc.exe start agentc-supervisor
```

## What install refuses

A refusal changes nothing. Fix what it names and run `install` again.

| Refusal | Why |
| --- | --- |
| The session is not elevated | Nothing can be registered or secured without it |
| A directory is a link or reparse point | The installer never follows one, and looks at nothing below it |
| A directory is owned by someone other than the system, administrators or its own service | `C:\ProgramData` lets any local user create a directory and own it; such a directory is never adopted |
| A file is where a directory belongs, or the reverse for the key | |
| The key file cannot be used | It is missing, empty, larger than 64 KiB, not a plain file or a link |
| A key is wanted, none is installed and no `--key-source` was given | |
| The configuration cannot be loaded | The services could not start from it either; it is left for you to fix or remove, never overwritten |
| The installed binary differs while services run from it | Replacing a running release is an update, not an install |

If a change fails part-way, `install` stops with an error. The changes it
made are in `installer\journal.jsonl`; run `install` again and it makes the
rest.

## Identities and access lists

Each service runs as its own virtual account, which Windows creates with the
service. None is an administrator and none is LocalSystem.

| Service | Account | Role | Depends on |
| --- | --- | --- | --- |
| `agentc-egress` | `NT SERVICE\agentc-egress` | Forwards a launch's HTTPS requests to the allowed hosts | |
| `agentc-publication` | `NT SERVICE\agentc-publication` | Publishes an implementer launch's candidate to its own ref | |
| `agentc-runner` | `NT SERVICE\agentc-runner` | Makes and contains each launch | |
| `agentc-supervisor` | `NT SERVICE\agentc-supervisor` | Runs the loop and answers the publication broker | `agentc-runner` |

Every service has the same registration besides its name and role:

| Setting | Value |
| --- | --- |
| Type | Its own process |
| Start | On demand, or automatic with `--start auto` |
| Recovery | Restart after 60 s, twice, then nothing; the count resets after a day. No failure program |
| Preshutdown time limit | 120 s |
| Required privileges | `SeChangeNotifyPrivilege`, `SeImpersonatePrivilege`. Windows documents that it removes every other privilege from the service's token when it starts it; no service the installer registered has been started yet, so this is not yet observed here |

A service's SID is derived from its name (`sc.exe showsid agentc-runner`
prints it) and is the same on every machine. Each service is configured with
the others' SIDs and with nobody else's: the brokers serve only the runner,
the runner serves only the supervisor and uses a broker's pipe only when that
broker's SID owns it, and the supervisor's authority pipe serves only the
publication broker.

Every directory has a complete, protected access list: nothing is inherited
from `C:\ProgramData` or `C:\Program Files`. The system and administrators
have full control of all of them.

| Directory | Who else | Holds |
| --- | --- | --- |
| `C:\Program Files\agentc` | `BUILTIN\Users`: read and execute | `agentc-supervisor.exe`, `supervisor.toml` |
| `C:\ProgramData\agentc` | Nobody | The state tree |
| `...\installer` | Nobody | `manifest.json`, `journal.jsonl` |
| `...\runner`, `...\runner\stand-in-launches` | The runner: modify | The launch journal, `service.log`, stand-in launch directories |
| `...\supervisor` | The supervisor: modify | Launch records, `heartbeat.json`, `service.log` |
| `...\stand-in`, `...\stand-in\queue` | The supervisor: modify | The stand-in's ledger and queue |
| `...\egress` | The egress broker: full control | `service.log` |
| `...\publication` | The publication broker: full control | `service.log`, and each launch's repository below `work\` |
| `...\publication-key` | The publication broker: read and execute | `push-app.pem` (only with a push App) |

The key file itself is readable by the publication account, the system and
administrators and by nobody else. The egress service checks at every start
that it cannot open it and refuses to run if it can. The file you gave
`--key-source` is left where it was: delete it, or keep it somewhere safe,
once the host works. The key stays registered with the App until you remove
it there.

A launch is meant to read nothing below `C:\Program Files\agentc`: it runs
in an AppContainer, which a grant to Users does not reach. No launch has run
against a program directory with this access list yet, so that too is not
yet observed.

## The pinned native tools

The installer installs one program, `agentc-supervisor.exe`, and nothing
else. The other native tools a host uses are the owner's to put on the
machine, and the supervisor admits them by content, not by name:

| Tool | Used for | What is recorded |
| --- | --- | --- |
| Git for Windows | The publication service's fetch and push | Found on `PATH`. The evaluated version is 2.56.0.windows.4. It is not pinned by hash |
| busybox-w32 `sh` | The POSIX shell of a reviewer's candidate commands; Git Bash does not start in an AppContainer | BusyBox 1.39.0, release FRP-6354-g5f09fb446, SHA-256 `36b266923fc5765b42f5f981319f39568498fba60b53ccf26e8fc1e575e2a9a9` |
| Claude Code | A reviewer harness | 2.1.326, SHA-256 `44e85ac6d3e36b924a0d631d3f69c13a6907491115d71e97c8eb6160c82b2aed`, kept outside the self-updating installation |

The shell and the harness are admitted only when their SHA-256 matches the
recorded one; another build is refused. No Windows driver starts a provider
harness yet, so an installed host uses neither of them today. The design
records how each was
[evaluated](windows-host-design.md#a-posix-shell-that-starts-in-an-appcontainer-2026-11-16).

## The generated configuration and its defaults

`install` writes `C:\Program Files\agentc\supervisor.toml`. Every setting has
an in-code default, and the installer writes only what has none. On a
default installation with a push App the file is, after three comment lines
that say who wrote it:

```toml
[windows_egress]
runner_sid = "S-1-5-80-2505214551-3868131734-2396360996-2952680925-136825992"

[windows_publication]
app_id = 123456
installation_id = 7654321
remote = "https://github.com/OWNER/REPO.git"
runner_sid = "S-1-5-80-2505214551-3868131734-2396360996-2952680925-136825992"
supervisor_sid = "S-1-5-80-3306791472-2437056731-757402864-3670258214-3591831055"

[windows_service]
client_sid = "S-1-5-80-3306791472-2437056731-757402864-3670258214-3591831055"
egress_broker_sid = "S-1-5-80-1897209490-1139552037-2514284598-2277624500-1663993593"
egress_control_pipe = "agentc-egress-control"
publication_broker_sid = "S-1-5-80-2610527295-3093505124-3895824955-3994243965-1923114604"
publication_control_pipe = "agentc-publication-control"

[windows_supervisor]
publication_broker_sid = "S-1-5-80-2610527295-3093505124-3895824955-3994243965-1923114604"
runner_sid = "S-1-5-80-2505214551-3868131734-2396360996-2952680925-136825992"
```

`[windows_service]` is the runner. The two control pipes are written because
their default is empty, which offers a launch no broker at all.

The installer **manages** these entries, in every table: the service names,
state directories and pipe names, the SIDs, the remote, the App's ids and key
path, the egress broker's `must_not_open` list and the whole
`[windows_supervisor.stand_in]` table. `status` reports a managed entry
whose value differs and `install` puts it back; a managed entry is left out
of the file when its value is the default.

Every other entry is yours: the grace periods and time limits, the egress
allow-list (`egress_allow_extra`), `[run]`, and anything else
`deploy/agentc/supervisor-windows.example.toml` in the repository shows with
its default. `install` keeps them when it rewrites the file. It does not keep
comments.

The preshutdown time limit Windows enforces is the installer's 120 s. If you
raise `preshutdown_timeout_seconds` above it, the runner says so in its log
at start and Windows still ends it at 120 s.

## Status and repair

```text
agentc-supervisor.exe windows-host status
```

`status` takes no inputs: it reads the manifest. It prints the same report
as `install`, with the changes `install` would make under `pending`, and
exits 0 only when there are none, nothing is refused, and every running
service's token could be read and holds exactly the privileges on the list. On a machine with no
manifest it reports that nothing is installed.

Each kind of drift has its own repair, so `install` on a drifted host
changes only what drifted:

| Drift | Change |
| --- | --- |
| A service is not registered | `create_service`, then its recovery, preshutdown limit and privileges |
| Command line, start type, dependencies or account | `set_bin_path`, `set_start_type`, `set_dependencies`, `set_account` |
| Recovery, preshutdown limit or required privileges | `set_recovery`, `set_preshutdown`, `set_privileges` |
| A directory is missing | `create_directory` |
| A directory's or the key's access list differs in any entry | `set_access_list` |
| A managed configuration entry differs | `write_config`, naming the entries |
| The key is missing, or a different `--key-source` is given | `install_key` |

In `tokens`, `stopped` means the service has no process and so no token to
read. It is not a pass: start the service and run `status` again to see what
its token holds.

## Logs and diagnostics

| Where | What |
| --- | --- |
| `C:\ProgramData\agentc\installer\journal.jsonl` | One line before and one after every change `install` made, with the run, the time and any failure |
| `C:\ProgramData\agentc\installer\manifest.json` | What `install` was told |
| `C:\ProgramData\agentc\<service>\service.log` | Each service's own log. A broker logs its token's user, groups and privileges at start; the runner logs its preshutdown limit |
| `C:\ProgramData\agentc\supervisor\heartbeat.json` | The supervisor loop's state and, when it claims nothing, why |
| `C:\ProgramData\agentc\stand-in\ledger.jsonl` | Every claim, launch and release of the acceptance stand-in |
| Windows Application event log, source = the service's name | Why a service failed to start or stopped with an error |

Windows' own tools read the installer's work back independently:

```text
sc.exe qc agentc-runner
sc.exe qfailure agentc-runner
sc.exe qprivs agentc-runner
sc.exe showsid agentc-runner
icacls C:\ProgramData\agentc\publication-key\push-app.pem
```

Reading the state tree needs an elevated session. Nothing the installer
prints, journals or records contains key or credential bytes.
