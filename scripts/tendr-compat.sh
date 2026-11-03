#!/usr/bin/env bash
# Check that a tendr build still drives sio the way README.md and the skill say.
#
#   scripts/tendr-compat.sh                      # tendr on PATH, sio built here
#   scripts/tendr-compat.sh path/to/tendr        # a release or a local build
#   scripts/tendr-compat.sh path/to/tendr path/to/sio
#
# Run it against each tendr release (or tendr's main before one). It uses the
# loopback device, so no hardware is needed, and its own tendr namespace,
# pruned on exit. Needs jq. Exits 1 on any failed check.
set -uo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
TENDR="${1:-tendr}"
if [[ -n "${2:-}" ]]; then
    SIO="$2"
else
    cargo build --quiet --manifest-path "$repo/Cargo.toml" || exit 1
    SIO="$repo/target/debug/sio"
fi
NS="sio-compat-$$"
failed=0

t() { "$TENDR" "$1" --namespace "$NS" "${@:2}"; }
cleanup() {
    t kill serial >/dev/null 2>&1
    t kill con >/dev/null 2>&1
    t prune serial con >/dev/null 2>&1
    rm -rf -- "${work:-}"
}
trap cleanup EXIT
work="$(mktemp -d)"

check() { # name, want, got
    if [[ "$3" == "$2" ]]; then
        echo "ok   $1"
    else
        echo "FAIL $1: want [$2] got [$3]"
        failed=1
    fi
}
reply() { jq -r ".stdout | fromjson | $1" <<<"$2" 2>/dev/null; }

version="$("$TENDR" --version | awk '{print $2}')"
at_least() { [[ "$(printf '%s\n%s\n' "$1" "$version" | sort -V | head -1)" == "$1" ]]; }
echo "tendr $version with $("$SIO" --version)"
if ! at_least 0.5.0; then
    echo "FAIL tendr $version is older than 0.5.0, the oldest sio supports"
    exit 1
fi

# Session protocol through the ndjson exec target (tendr 0.5.0 or newer).
t start --stdin --exec-target ndjson serial -- "$SIO" stdio --loopback >/dev/null
out="$(t exec serial -- '{"cmd":"open","port":"loop://"}')"
check "exec exits 0 for ok:true" 0 "$?"
check "open" true "$(reply .ok "$out")"

out="$(t exec serial -- '{"cmd":"write","text":"hello\n"}')"
check "write" 6 "$(reply .result.bytes "$out")"
out="$(t exec serial -- '{"cmd":"expect","pattern":"hel","timeout_ms":2000}')"
check "expect" match "$(reply .result.end "$out")"
out="$(t exec serial -- '{"cmd":"readline","timeout_ms":2000}')"
check "bytes after a match stay buffered" '"lo\n"' "$(reply '.result.text | @json' "$out")"

out="$(t exec serial -- '{"cmd":"write","text":"x","bits":9}')"
check "exec exits 1 for an error reply" 1 "$?"
check "error code" invalid_request "$(reply .error.code "$out")"

# A wait tendr stops waiting for, then cancelled by its request_id.
out="$(t exec --timeout 1 serial -- '{"cmd":"expect","pattern":"never","timeout_ms":30000}' 2>/dev/null)"
check "exec exits 124 when tendr stops waiting" 124 "$?"
rid="$(jq -r '.request_id // empty' <<<"$out")"
check "timed-out exec reports request_id" yes "$([[ -n "$rid" ]] && echo yes || echo no)"
start="$(date +%s)"
out="$(t exec --timeout 5 serial -- "{\"cmd\":\"cancel\",\"target\":\"$rid\"}")"
check "cancel stops the wait" true "$(reply .result.cancelled "$out")"
check "stopped wait's reply arrives in unmatched" cancelled "$(jq -r '
    [.unmatched[]? | (if type == "string" then fromjson else . end) | .result.end?]
    | map(select(. == "cancelled")) | first // "none"' <<<"$out")"
check "cancel takes under 2 s" yes "$([[ $(( $(date +%s) - start )) -le 2 ]] && echo yes || echo no)"
out="$(t exec serial -- '{"cmd":"close"}')"
check "close" true "$(reply .ok "$out")"

# Watching a sio terminal console without control (tendr 0.7.0 or newer).
if at_least 0.7.0; then
    t start --pty con -- "$SIO" terminal --loopback --port loop:// >/dev/null
    sleep 0.5
    t attach --read-only con >"$work/watch" 2>"$work/watch.err" &
    watcher=$!
    sleep 1
    printf 'ping-from-push' | t push con >/dev/null
    sleep 1
    t kill con >/dev/null 2>&1
    wait "$watcher"
    check "read-only watcher exits 0 with the complete output" 0 "$?"
    check "read-only watcher sees the console" yes \
        "$(grep -q ping-from-push "$work/watch" && echo yes || echo no)"
else
    echo "skip read-only watching: needs tendr 0.7.0"
fi

exit "$failed"
