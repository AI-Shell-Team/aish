#!/usr/bin/env bash
#
# Focused test for the teardown of `scripts/mcp-test-servers.sh`: `stop` must
# kill only the processes that script started.
#
# Why it matters: teardown used to end in `pkill -9 -f <fixture>`, which killed
# *every* `aish-mcp-fixture` on the machine — the endpoint the user keeps around
# for their real config, another session's servers, and any stray fixture.
#
# What it asserts:
#   1. `stop` kills the HTTP fixture it started itself (recorded in http.pid)
#   2. `stop` kills a leftover process of an isolated run, identified by the
#      isolated root recorded in `env-roots` (the interrupted-`selftest` case)
#   3. `stop` leaves a fixture from "elsewhere" running, even though its command
#      line names the same binary
#   4. `stop` is idempotent
#
# Usage (from the repo root, after `cargo build -p aish-mcp --bin aish-mcp-fixture`):
#   bash tests/dejagnu/mcp/mcp-harness-test.sh
#
# Environment overrides: AISH_BIN, AISH_MCP_FIXTURE.

set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
# AISH_MCP_TEST_HARNESS points the test at a copy of the script; it exists so the
# test can be re-run against a mutated copy to confirm it still discriminates.
SCRIPT="${AISH_MCP_TEST_HARNESS:-$ROOT/scripts/mcp-test-servers.sh}"
FIXTURE="${AISH_MCP_FIXTURE:-$ROOT/target/debug/aish-mcp-fixture}"
AISH_BIN="${AISH_BIN:-$ROOT/target/debug/aish}"
# A scratch state directory: the test must never touch the pid/port files the
# user's own HTTP endpoint is tracked in.
STATE="$(mktemp -d /tmp/aish-mcp-harness-test.XXXXXX)"
PASS=0
FAIL=0

ok()   { echo "PASS: $1"; PASS=$((PASS + 1)); }
bad()  { echo "FAIL: $1"; FAIL=$((FAIL + 1)); }
alive() { kill -0 "${1:-}" 2>/dev/null; }

free_port() {
  python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()' 2>/dev/null || echo 39487
}

# Start a fixture the harness does not know about and wait until it listens.
start_fixture() { # $1=logfile $2..=extra env (VAR=value)
  local log="$1"; shift
  local port; port="$(free_port)"
  env "$@" "$FIXTURE" --mode http --port "$port" --session > "$log" 2>&1 &
  local pid=$!
  for _ in $(seq 1 40); do
    grep -qa "LISTENING" "$log" 2>/dev/null && break
    sleep 0.25
  done
  printf '%s\n' "$pid"
}

cleanup() {
  local pid
  for pid in ${FOREIGN_PID:-} ${LEFTOVER_PID:-} ${STARTED_PID:-}; do
    [ -n "$pid" ] && kill -9 "$pid" 2>/dev/null
  done
  [ -f "$STATE/http.pid" ] && kill -9 "$(cat "$STATE/http.pid")" 2>/dev/null
  rm -rf "$STATE"
}

if [ ! -x "$FIXTURE" ]; then
  echo "SKIP: missing binary $FIXTURE (run cargo build first)"
  exit 0
fi

trap cleanup EXIT

harness() { AISH_MCP_TEST_STATE="$STATE" AISH_BIN="$AISH_BIN" bash "$SCRIPT" "$@"; }

# 1. A fixture that belongs to someone else: same binary, no isolated root in its
#    environment — this is what `stop` must never kill.
FOREIGN_PID="$(start_fixture "$STATE/foreign.log" -u XDG_CONFIG_HOME -u XDG_DATA_HOME \
  -u XDG_STATE_HOME -u XDG_CACHE_HOME)"
alive "$FOREIGN_PID" || bad "could not start the foreign fixture (see $STATE/foreign.log)"

# 2. A leftover of an interrupted `selftest`: its environment carries the root
#    recorded in env-roots.
LEFTOVER_ROOT="$STATE/aish-mcp-selftest.sim"
mkdir -p "$LEFTOVER_ROOT/config"
LEFTOVER_PID="$(start_fixture "$STATE/leftover.log" \
  "XDG_CONFIG_HOME=$LEFTOVER_ROOT/config" "XDG_DATA_HOME=$LEFTOVER_ROOT/data" \
  "XDG_STATE_HOME=$LEFTOVER_ROOT/state" "XDG_CACHE_HOME=$LEFTOVER_ROOT/cache")"
alive "$LEFTOVER_PID" || bad "could not start the leftover fixture (see $STATE/leftover.log)"
printf '%s\n' "$LEFTOVER_ROOT" >> "$STATE/env-roots"

# 3. The HTTP endpoint the script starts itself, recorded in http.pid.
harness start-http >/dev/null || bad "start-http failed"
STARTED_PID="$(cat "$STATE/http.pid" 2>/dev/null)"
if alive "$STARTED_PID"; then
  ok "start-http launched a fixture (pid $STARTED_PID)"
else
  bad "start-http did not launch a fixture"
fi

# 4. Teardown.
STOP_OUT="$(harness stop 2>&1)"
printf '%s\n' "$STOP_OUT" | sed 's/^/    /'

if alive "$STARTED_PID"; then
  bad "stop left the fixture it started running (pid $STARTED_PID)"
else
  ok "stop killed the fixture it started (pid $STARTED_PID)"
fi
if alive "$LEFTOVER_PID"; then
  bad "stop left an isolated-run leftover running (pid $LEFTOVER_PID)"
else
  ok "stop killed the leftover of an isolated run (pid $LEFTOVER_PID)"
fi
[ -d "$LEFTOVER_ROOT" ] && bad "stop kept the isolated root $LEFTOVER_ROOT" \
  || ok "stop removed the isolated root of the cleaned-up run"
if alive "$FOREIGN_PID"; then
  ok "stop left an unrelated fixture running (pid $FOREIGN_PID)"
else
  bad "stop killed an unrelated fixture (pid $FOREIGN_PID)"
fi
[ -f "$STATE/http.pid" ] && bad "stop kept the stale pid file" || ok "stop removed the pid file"

# 5. Idempotent.
if harness stop >/dev/null 2>&1; then
  ok "stop is idempotent (second run succeeds)"
else
  bad "second stop run failed"
fi
alive "$FOREIGN_PID" && ok "the unrelated fixture is still running after the second stop" \
  || bad "the unrelated fixture died during the second stop"

echo "--- $PASS passed, $FAIL failed ---"
[ "$FAIL" -eq 0 ]
