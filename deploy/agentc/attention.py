#!/usr/bin/env python3
"""Attention-budget helpers for one agentc host (autonomy plan P3b, U13).

    attention.py digest --url URL --project ID --token-file FILE [--hours 24]
                        [--mail-to ADDR --smtp-host HOST [--smtp-port N]
                         [--smtp-tls starttls|tls|none] [--smtp-user USER
                          --smtp-password-file FILE] [--mail-from ADDR]]
    attention.py canary --url URL --project ID --token-file FILE
                        --heartbeat /var/lib/agentc/heartbeat.json
                        --ntfy-topic TOPIC [--ntfy-url https://ntfy.sh]
                        [--state FILE] [--max-hri N] [--neglect-days N]

`digest` reads `GET /api/v1/projects/ID/digest` and prints what the attention
budget did: reversible decisions that proceeded on their recommendation after
24 hours, the ones about to, and the human-required interventions (HRI: open
human-required integrator reports and stalled tasks). With --mail-to it also
mails the text through the given SMTP host, with a signed "I read this" link
that records the read (the digest printed to the journal carries no link).
Opening the digest in the dashboard records a read too; reading it here does not.

The mail goes through an authenticated TLS relay when one is configured:
--smtp-tls (ATTENTION_SMTP_TLS) is `starttls` (port 587), `tls` (implicit TLS,
port 465) or `none` (port 25), defaulting to `starttls` when a password file is
set and `none` otherwise; --smtp-port overrides the port. --smtp-user logs in
with the password read from --smtp-password-file, a file only its owner may
read, never from the environment file or the command line. A failed delivery
or login exits non-zero and prints the error's type and code, never the
server's reply or the password.

`canary` probes the loop end to end and pages through ntfy when an SLO fails:
the service answers /healthz, the supervisor's own `next` call (implementer
role, authenticated) succeeds within its latency budget, the supervisor
heartbeat is fresh, with --max-hri the HRI count is within bounds, and the
digest has not gone unread for more than --neglect-days days (default 3; 0
turns the check off). A digest never read counts from the canary's first run. A
failing check pages once; it pages again only after it has recovered and
failed again, or when a different check starts failing. The paged set lives
in --state (default: next to the heartbeat). Exit status: 0 healthy, 1 an SLO
failed, 2 the page could not be delivered (the next run retries it).

The token files hold one bearer token (and nothing else); tokens are never
printed. ntfy credentials come from NTFY_TOKEN when set. Standard library only.

Every option defaults from an ATTENTION_* environment variable (the names are
in parse() below, and host-setup.sh's attention.env lists them), so the systemd units host-setup.sh installs carry no arguments;
an empty variable counts as unset, and a command-line option wins.
"""
import argparse
import json
import os
from pathlib import Path
import smtplib
import ssl
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime
from email.message import EmailMessage

sys.dont_write_bytecode = True

HEALTH_BUDGET_SECONDS = 5.0
NEXT_BUDGET_SECONDS = 10.0
HEARTBEAT_MAX_AGE_SECONDS = 300.0
DEFAULT_NTFY = "https://ntfy.sh"
DEFAULT_URL = "https://agents.sithbit.com"
DEFAULT_PROJECT = "fe95a6c5-2aad-463f-8446-4366d9a281c7"
DEFAULT_HEARTBEAT = "/var/lib/agentc/heartbeat.json"
DEFAULT_TOKEN_FILE = "/etc/agentc/attention-token"
DEFAULT_MAIL_FROM = "agentc@localhost"
SMTP_TLS_MODES = ("starttls", "tls", "none")
SMTP_PORTS = {"starttls": 587, "tls": 465, "none": 25}
DEFAULT_NEGLECT_DAYS = 3
DAY_SECONDS = 86400.0


def now():
    """Seconds since the epoch; tests replace this with a fake clock."""
    return time.time()


def read_token(path):
    """The bearer token in `path`, whitespace trimmed; the file must be private."""
    try:
        mode = os.stat(path).st_mode
        token = Path(path).read_text().strip()
    except OSError as error:
        raise SystemExit(f"attention: cannot read the token file ({type(error).__name__})")
    if mode & 0o077:
        raise SystemExit(f"attention: {path} is readable by other users (use root:root 0400)")
    if not token:
        raise SystemExit(f"attention: {path} holds no token")
    return token


def read_password(path):
    """The SMTP password in `path` (trailing whitespace trimmed); the file must be private."""
    try:
        mode = os.stat(path).st_mode
        password = Path(path).read_text().strip()
    except OSError as error:
        raise SystemExit(f"attention: cannot read the SMTP password file ({type(error).__name__})")
    if mode & 0o077:
        raise SystemExit(f"attention: {path} is readable by other users (use root:root 0400)")
    if not password:
        raise SystemExit(f"attention: {path} holds no password")
    return password


def fetch(url, token=None, timeout=15.0):
    """(status, parsed JSON or None, seconds) for a GET; transport errors raise."""
    request = urllib.request.Request(url)
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    started = time.monotonic()
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            status, raw = response.status, response.read()
    except urllib.error.HTTPError as error:
        status, raw = error.code, error.read()
    elapsed = time.monotonic() - started
    try:
        body = json.loads(raw)
    except ValueError:
        body = None
    return status, body, elapsed


def project_url(args, route):
    return f"{args.url.rstrip('/')}/api/v1/projects/{args.project}/{route}"


# ------------------------------------------------------------------- digest


def render_digest(data, ack_url=None):
    """The digest body as plain text, with the acknowledgement link when given."""
    hri = data["hri"]
    lines = [f"Attention digest, last {data['window_hours']}h (until {data['until']})", ""]
    lines.append(f"Proceeded on their recommendation ({len(data['proceeded_decisions'])}):")
    for item in data["proceeded_decisions"]:
        lines.append(f"  - {item['question']} -> {item['proceeded_with']} "
                     f"at {item['proceeded_at']} (decision {item['decision_id']}; reopen it to undo)")
    lines += ["", f"Will proceed on their own ({len(data['pending_reversible_decisions'])}):"]
    for item in data["pending_reversible_decisions"]:
        lines.append(f"  - {item['question']} -> {item['recommendation']} "
                     f"at {item['proceeds_at']} (decision {item['decision_id']})")
    held = data.get("held_agent_tasks") or []
    if held:
        lines += ["", f"Agent tasks held by the weekly budget ({len(held)}):"]
        for item in held:
            lines.append(f"  - {item['title']} (task {item['task_id']}; edit it to release it)")
    lines += ["", f"Human-required interventions: {hri['count']} "
                  f"({hri['stalled_tasks']} stalled tasks)"]
    for item in hri["items"]:
        lines.append(f"  - {item.get('code')}: {item.get('title') or item.get('summary') or item}")
    if ack_url:
        lines += ["", f"I read this (records the read; it does nothing else): {ack_url}"]
    return "\n".join(lines) + "\n"


def read_digest(args, token, ack_link=False):
    query = f"digest?hours={args.hours}" + ("&ack_link=true" if ack_link else "")
    status, body, _ = fetch(project_url(args, query), token)
    if status != 200 or not body or "data" not in body:
        raise SystemExit(f"attention: digest request failed with status {status}")
    return body["data"]


def run_digest(args):
    data = read_digest(args, read_token(args.token_file), ack_link=bool(args.mail_to))
    print(render_digest(data), end="")
    if args.mail_to:
        if not args.smtp_host:
            raise SystemExit("attention: --mail-to needs --smtp-host")
        text = render_digest(data, (data.get("ack_link") or {}).get("url"))
        message = EmailMessage()
        message["Subject"] = "agentc attention digest"
        message["From"] = args.mail_from
        message["To"] = args.mail_to
        message.set_content(text)
        send_mail(args, message)
    return 0


def send_mail(args, message):
    """Send `message` through the configured relay; failures exit without the server's reply."""
    mode = args.smtp_tls or ("starttls" if args.smtp_password_file else "none")
    port = args.smtp_port or SMTP_PORTS[mode]
    if args.smtp_user and not args.smtp_password_file:
        raise SystemExit("attention: --smtp-user needs --smtp-password-file")
    if args.smtp_password_file and not args.smtp_user:
        raise SystemExit("attention: --smtp-password-file needs --smtp-user")
    if mode == "none" and args.smtp_password_file:
        raise SystemExit("attention: refusing to log in without TLS (set ATTENTION_SMTP_TLS)")
    password = read_password(args.smtp_password_file) if args.smtp_password_file else None
    try:
        if mode == "tls":
            smtp = smtplib.SMTP_SSL(args.smtp_host, port, timeout=30, context=tls_context())
        else:
            smtp = smtplib.SMTP(args.smtp_host, port, timeout=30)
        with smtp:
            if mode == "starttls":
                smtp.starttls(context=tls_context())
            if password:
                smtp.login(args.smtp_user, password)
            smtp.send_message(message)
    except smtplib.SMTPResponseException as error:
        raise SystemExit(f"attention: mail not sent ({type(error).__name__} {error.smtp_code})")
    except (smtplib.SMTPException, OSError) as error:
        raise SystemExit(f"attention: mail not sent ({type(error).__name__})")


def tls_context():
    """The TLS settings for the relay: certificate and host name verified; tests replace this."""
    return ssl.create_default_context()


# ------------------------------------------------------------------- canary


def probe_service(args, token):
    status, _, elapsed = fetch(f"{args.url.rstrip('/')}/healthz")
    if status != 200:
        return f"/healthz answered {status}"
    if elapsed > HEALTH_BUDGET_SECONDS:
        return f"/healthz took {elapsed:.1f}s (budget {HEALTH_BUDGET_SECONDS:.0f}s)"
    return None


def probe_next(args, token):
    status, body, elapsed = fetch(project_url(args, "next?role=implementer"), token)
    if status != 200 or not body or "data" not in body:
        return f"next answered {status}"
    if elapsed > NEXT_BUDGET_SECONDS:
        return f"next took {elapsed:.1f}s (budget {NEXT_BUDGET_SECONDS:.0f}s)"
    return None


def probe_heartbeat(args, token):
    if not args.heartbeat:
        return None
    try:
        beat = json.loads(Path(args.heartbeat).read_text())
        age = now() - beat["at_ms"] / 1000.0
    except (OSError, ValueError, KeyError, TypeError) as error:
        return f"supervisor heartbeat unreadable ({type(error).__name__})"
    if age > args.heartbeat_max_age:
        return f"supervisor heartbeat is {age:.0f}s old (limit {args.heartbeat_max_age:.0f}s)"
    return None


def probe_hri(args, token):
    if args.max_hri is None:
        return None
    count = read_digest(args, token)["hri"]["count"]
    if count > args.max_hri:
        return f"{count} human-required interventions (limit {args.max_hri})"
    return None


def parse_time(text):
    """Seconds since the epoch for an RFC 3339 timestamp such as 2026-10-08T00:00:00.000Z."""
    return datetime.fromisoformat(text.replace("Z", "+00:00")).timestamp()


def probe_digest(args, token):
    """Fails once the digest has gone unread for more than --neglect-days days.

    The clock starts at the last read, or at the canary's first run while the
    digest has never been read (kept in the state file).
    """
    if not args.neglect_days:
        return None
    last_read = read_digest(args, token).get("last_read_at")
    started = parse_time(last_read) if last_read else digest_baseline(args)
    unread_days = (now() - started) / DAY_SECONDS
    if unread_days > args.neglect_days:
        return f"digest unread for {int(unread_days)} days (limit {args.neglect_days})"
    return None


def digest_baseline(args):
    """When the canary first saw the digest unread, recorded on first use."""
    path = state_path(args)
    state = load_state(path)
    if "digest_baseline" not in state:
        state["digest_baseline"] = now()
        save_state(path, state)
    return state["digest_baseline"]


PROBES = {"service": probe_service, "next": probe_next,
          "supervisor": probe_heartbeat, "hri": probe_hri, "digest": probe_digest}


def run_probes(args, token):
    """{check name: failure text} for every failing check."""
    failures = {}
    for name, probe in PROBES.items():
        try:
            failure = probe(args, token)
        except (OSError, SystemExit) as error:
            failure = f"{name} probe failed ({type(error).__name__})"
        if failure:
            failures[name] = failure
    return failures


def state_path(args):
    if args.state:
        return Path(args.state)
    base = Path(args.heartbeat).parent if args.heartbeat else Path.cwd()
    return base / "canary-state.json"


def load_state(path):
    """The canary state: {"paged": [...], "digest_baseline": seconds}, or {} when unreadable."""
    try:
        state = json.loads(path.read_text())
    except (OSError, ValueError):
        return {}
    return state if isinstance(state, dict) else {}


def save_state(path, state):
    temp = path.with_name(path.name + ".tmp")
    temp.write_text(json.dumps(state))
    os.replace(temp, path)


def load_paged(path):
    paged = load_state(path).get("paged")
    return set(paged) if isinstance(paged, list) else set()


def page(args, failures):
    """One ntfy message naming every failing check."""
    body = "\n".join(f"{name}: {text}" for name, text in sorted(failures.items()))
    request = urllib.request.Request(
        f"{args.ntfy_url.rstrip('/')}/{args.ntfy_topic}", data=body.encode(), method="POST")
    request.add_header("Title", "agentc canary failed")
    request.add_header("Priority", "high")
    request.add_header("Tags", "rotating_light")
    ntfy_token = os.environ.get("NTFY_TOKEN")
    if ntfy_token:
        request.add_header("Authorization", f"Bearer {ntfy_token}")
    with urllib.request.urlopen(request, timeout=15) as response:
        response.read()


def run_canary(args):
    token = read_token(args.token_file)
    failures = run_probes(args, token)
    path = state_path(args)
    paged = load_paged(path)
    for name, text in sorted(failures.items()):
        print(f"canary: {name}: {text}", file=sys.stderr)
    fresh = {name: text for name, text in failures.items() if name not in paged}
    if fresh:
        try:
            page(args, failures)
        except (OSError, urllib.error.URLError) as error:
            print(f"canary: ntfy page not delivered ({type(error).__name__})", file=sys.stderr)
            return 2
    state = load_state(path)
    state["paged"] = sorted(failures)
    save_state(path, state)
    return 1 if failures else 0


# --------------------------------------------------------------------- main


def env(name, default=None, kind=str):
    """The environment variable `name` as `kind`, or `default` when unset or empty."""
    value = os.environ.get(name, "").strip()
    return kind(value) if value else default


def parse(argv):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("digest", "canary"):
        p = sub.add_parser(name)
        p.add_argument("--url", default=env("ATTENTION_URL", DEFAULT_URL))
        p.add_argument("--project", default=env("ATTENTION_PROJECT", DEFAULT_PROJECT))
        p.add_argument("--token-file", default=env("ATTENTION_TOKEN_FILE", DEFAULT_TOKEN_FILE))
        p.add_argument("--hours", type=int, default=env("ATTENTION_HOURS", 24, int))
    digest = sub.choices["digest"]
    digest.add_argument("--mail-to", default=env("ATTENTION_MAIL_TO"))
    digest.add_argument("--mail-from", default=env("ATTENTION_MAIL_FROM", DEFAULT_MAIL_FROM))
    digest.add_argument("--smtp-host", default=env("ATTENTION_SMTP_HOST"))
    digest.add_argument("--smtp-port", type=int, default=env("ATTENTION_SMTP_PORT", None, int))
    digest.add_argument("--smtp-tls", choices=SMTP_TLS_MODES, default=env("ATTENTION_SMTP_TLS"))
    digest.add_argument("--smtp-user", default=env("ATTENTION_SMTP_USER"))
    digest.add_argument("--smtp-password-file", default=env("ATTENTION_SMTP_PASSWORD_FILE"))
    canary = sub.choices["canary"]
    canary.add_argument("--heartbeat", default=env("ATTENTION_HEARTBEAT", DEFAULT_HEARTBEAT))
    canary.add_argument("--heartbeat-max-age", type=float,
                        default=env("ATTENTION_HEARTBEAT_MAX_AGE", HEARTBEAT_MAX_AGE_SECONDS, float))
    canary.add_argument("--ntfy-topic", default=env("ATTENTION_NTFY_TOPIC"))
    canary.add_argument("--ntfy-url", default=env("ATTENTION_NTFY_URL", DEFAULT_NTFY))
    canary.add_argument("--state", default=env("ATTENTION_STATE"))
    canary.add_argument("--max-hri", type=int, default=env("ATTENTION_MAX_HRI", None, int))
    canary.add_argument("--neglect-days", type=float,
                        default=env("ATTENTION_NEGLECT_DAYS", DEFAULT_NEGLECT_DAYS, float))
    args = parser.parse_args(argv)
    if args.command == "canary" and not args.ntfy_topic:
        parser.error("canary needs --ntfy-topic or ATTENTION_NTFY_TOPIC")
    if args.command == "digest" and args.smtp_tls not in (None, *SMTP_TLS_MODES):
        parser.error(f"ATTENTION_SMTP_TLS must be one of {', '.join(SMTP_TLS_MODES)}")
    return args


def main(argv=None):
    args = parse(argv)
    return {"digest": run_digest, "canary": run_canary}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
