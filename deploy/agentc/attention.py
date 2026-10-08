#!/usr/bin/env python3
"""Attention-budget helpers for one agentc host (autonomy plan P3b, U13).

    attention.py digest --url URL --project ID --token-file FILE [--hours 24]
                        [--mail-to ADDR --smtp-host HOST [--smtp-port 25]
                         [--mail-from ADDR]]
    attention.py canary --url URL --project ID --token-file FILE
                        --heartbeat /var/lib/agentc/heartbeat.json
                        --ntfy-topic TOPIC [--ntfy-url https://ntfy.sh]
                        [--state FILE] [--max-hri N]

`digest` reads `GET /api/v1/projects/ID/digest` and prints what the attention
budget did: reversible decisions that proceeded on their recommendation after
24 hours, the ones about to, and the human-required interventions (HRI: open
human-required integrator reports and stalled tasks). With --mail-to it also
mails the text through the given SMTP host.

`canary` probes the loop end to end and pages through ntfy when an SLO fails:
the service answers /healthz, the supervisor's own `next` call (implementer
role, authenticated) succeeds within its latency budget, the supervisor
heartbeat is fresh, and, with --max-hri, the HRI count is within bounds. A
failing check pages once; it pages again only after it has recovered and
failed again, or when a different check starts failing. The paged set lives
in --state (default: next to the heartbeat). Exit status: 0 healthy, 1 an SLO
failed, 2 the page could not be delivered (the next run retries it).

The token files hold one bearer token (and nothing else); tokens are never
printed. ntfy credentials come from NTFY_TOKEN when set. Standard library only.
"""
import argparse
import json
import os
from pathlib import Path
import smtplib
import sys
import time
import urllib.error
import urllib.request
from email.message import EmailMessage

sys.dont_write_bytecode = True

HEALTH_BUDGET_SECONDS = 5.0
NEXT_BUDGET_SECONDS = 10.0
HEARTBEAT_MAX_AGE_SECONDS = 300.0
DEFAULT_NTFY = "https://ntfy.sh"


def read_token(path):
    """The bearer token in `path`, whitespace trimmed."""
    token = Path(path).read_text().strip()
    if not token:
        raise SystemExit(f"attention: {path} holds no token")
    return token


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


def render_digest(data):
    """The digest body as plain text."""
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
    return "\n".join(lines) + "\n"


def read_digest(args, token):
    status, body, _ = fetch(project_url(args, f"digest?hours={args.hours}"), token)
    if status != 200 or not body or "data" not in body:
        raise SystemExit(f"attention: digest request failed with status {status}")
    return body["data"]


def run_digest(args):
    text = render_digest(read_digest(args, read_token(args.token_file)))
    print(text, end="")
    if args.mail_to:
        if not args.smtp_host:
            raise SystemExit("attention: --mail-to needs --smtp-host")
        message = EmailMessage()
        message["Subject"] = "agentc attention digest"
        message["From"] = args.mail_from
        message["To"] = args.mail_to
        message.set_content(text)
        with smtplib.SMTP(args.smtp_host, args.smtp_port, timeout=30) as smtp:
            smtp.send_message(message)
    return 0


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
        age = time.time() - beat["at_ms"] / 1000.0
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


PROBES = {"service": probe_service, "next": probe_next,
          "supervisor": probe_heartbeat, "hri": probe_hri}


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


def load_paged(path):
    try:
        return set(json.loads(path.read_text())["paged"])
    except (OSError, ValueError, KeyError, TypeError):
        return set()


def save_paged(path, paged):
    temp = path.with_name(path.name + ".tmp")
    temp.write_text(json.dumps({"paged": sorted(paged)}))
    os.replace(temp, path)


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
    save_paged(path, set(failures))
    return 1 if failures else 0


# --------------------------------------------------------------------- main


def parse(argv):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("digest", "canary"):
        p = sub.add_parser(name)
        p.add_argument("--url", required=True)
        p.add_argument("--project", required=True)
        p.add_argument("--token-file", required=True)
        p.add_argument("--hours", type=int, default=24)
    digest = sub.choices["digest"]
    digest.add_argument("--mail-to")
    digest.add_argument("--mail-from", default="agentc@localhost")
    digest.add_argument("--smtp-host")
    digest.add_argument("--smtp-port", type=int, default=25)
    canary = sub.choices["canary"]
    canary.add_argument("--heartbeat")
    canary.add_argument("--heartbeat-max-age", type=float, default=HEARTBEAT_MAX_AGE_SECONDS)
    canary.add_argument("--ntfy-topic", required=True)
    canary.add_argument("--ntfy-url", default=DEFAULT_NTFY)
    canary.add_argument("--state")
    canary.add_argument("--max-hri", type=int)
    return parser.parse_args(argv)


def main(argv=None):
    args = parse(argv)
    return {"digest": run_digest, "canary": run_canary}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
