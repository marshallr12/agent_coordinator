#!/usr/bin/env python3
"""Read-only GET against the coordinator with the bearer token from env. Never prints the token."""
import json, os, sys, urllib.request
BASE = "https://agents.sithbit.com"
def get(path):
    req = urllib.request.Request(BASE + path, method="GET",
        headers={"Authorization": "Bearer " + os.environ["AGENT_COORDINATOR_TOKEN"], "Accept": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")
if __name__ == "__main__":
    path, out = sys.argv[1], sys.argv[2]
    st, body = get(path)
    json.dump(body, open(out, "w"), indent=1)
    print(st, out, len(json.dumps(body)))
