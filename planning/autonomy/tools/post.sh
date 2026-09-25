#!/usr/bin/env bash
# Append a message to the public discussion transcript under an exclusive lock.
# Usage: post.sh "<Speaker name>" <message-file>
set -euo pipefail
T="$(cd "$(dirname "$0")/.." && pwd)/discussion-transcript.md"
exec 9>"$T.lock"
flock -x 9
{ printf '\n---\n\n### %s — %s\n\n' "$1" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"; cat "$2"; printf '\n'; } >> "$T"
echo "posted $(wc -l < "$T") lines total"
