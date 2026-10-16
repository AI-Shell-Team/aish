#!/usr/bin/env bash
#
# End-to-end smoke test for the MCP client surface (issue #564) against the
# real `aish` binary.
#
# Why tmux and not expect/DejaGnu: aish draws a slash-command popup as soon as
# the line starts with `/`. A burst of keystrokes (what `send -- "/mcp status"` is)
# is consumed by the popup redraw, and the popup's own terminals queries
# (`ESC[6n`) need a real terminal emulator to answer them. tmux provides both, so
# this script types character by character at human speed and reads the pane.
#
# What it asserts:
#   1. the real binary reads `mcp.servers`, spawns the server and completes the
#      MCP handshake + `tools/list` (verified on the wire through a `tee` wrapper)
#   2. `/status` carries the MCP summary line
#   3. `/mcp status` reports the server as connected with its tool count
#   4. `/mcp tools <server>` lists the discovered catalogue
#   5. `/exit` shuts the shell down and reaps the server child processes
#   6. teardown kills this run's processes only (a foreign fixture survives)
#   7. the run stays inside its temporary XDG root: no row is appended to the
#      user's real `~/.local/share/aish/sessions.db`
#
# Usage (from the repo root, after `cargo build`):
#   bash tests/dejagnu/mcp/mcp-smoke.sh
#
# Environment overrides: AISH_BIN, AISH_MCP_FIXTURE.

set -u

AISH_BIN="${AISH_BIN:-$PWD/target/debug/aish}"
FIXTURE="${AISH_MCP_FIXTURE:-$PWD/target/debug/aish-mcp-fixture}"
SESSION="aish-mcp-smoke-$$"
TMP="$(mktemp -d /tmp/aish-mcp-smoke.XXXXXX)"
PASS=0
FAIL=0

# The user's real session database, resolved before anything is redirected:
# the run must not append a single row to it (checked in step 9). The baseline
# is taken now, before aish starts.
REAL_DB="${XDG_DATA_HOME:-$HOME/.local/share}/aish/sessions.db"
BEFORE_DB=""
if command -v sqlite3 >/dev/null 2>&1 && [ -f "$REAL_DB" ]; then
  BEFORE_DB="$(sqlite3 "$REAL_DB" \
    "select count(*) from sessions where model = 'smoke-model';" 2>/dev/null)"
fi

# --- Isolation ---------------------------------------------------------------
# aish takes its session database and history from XDG_DATA_HOME, its config,
# logs and markers from XDG_CONFIG_HOME, its builtin-skill cache from
# XDG_CACHE_HOME and other state from XDG_STATE_HOME. Redirecting only
# XDG_CONFIG_HOME left every run writing real sessions and history rows into
# ~/.local/share/aish. HOME itself stays real so that node/npx keep finding
# their ~/.npm/_npx package cache.
ISOLATED_XDG=(
  "XDG_CONFIG_HOME=$TMP/config"
  "XDG_DATA_HOME=$TMP/data"
  "XDG_STATE_HOME=$TMP/state"
  "XDG_CACHE_HOME=$TMP/cache"
)
mkdir -p "$TMP/config/aish" "$TMP/data" "$TMP/state" "$TMP/cache"
# `uvx`/`npx` resolve their package caches through XDG_CACHE_HOME: keep the uv
# cache reachable, otherwise an offline run would try to download everything.
if [ -d "$HOME/.cache/uv" ] && [ ! -e "$TMP/cache/uv" ]; then
  ln -s "$HOME/.cache/uv" "$TMP/cache/uv"
fi

if ! command -v tmux >/dev/null 2>&1; then
  echo "SKIP: tmux is required to drive the interactive surface"
  exit 0
fi
for bin in "$AISH_BIN" "$FIXTURE"; do
  if [ ! -x "$bin" ]; then
    echo "SKIP: missing binary $bin (run cargo build first)"
    exit 0
  fi
done

# True when $1 is a live process whose command line mentions $2.
pid_is_binary() {
  [ -n "${1:-}" ] && [ -r "/proc/$1/cmdline" ] \
    && tr '\0' ' ' < "/proc/$1/cmdline" 2>/dev/null | grep -qaF "$2"
}

# TERM, then KILL, one recorded pid. Only ever a process this script started.
kill_pid() {
  local pid="${1:-}"
  [ -n "$pid" ] || return 0
  kill "$pid" 2>/dev/null
  sleep 0.3
  kill -9 "$pid" 2>/dev/null
}

# Kill $1 and everything below it (the shell tmux ran aish in, and aish's
# own children: the stdio fixture, uvx/node servers).
kill_tree() {
  local pid="${1:-}" child
  [ -n "$pid" ] || return 0
  for child in $(pgrep -P "$pid" 2>/dev/null); do kill_tree "$child"; done
  kill -9 "$pid" 2>/dev/null
}

# Pids of this run's shell and MCP servers: they are the ones whose command line
# names our binaries *and* whose environment carries this run's isolated root, so
# the sweep can never match the user's shell, another session's aish or a
# fixture started by someone else.
run_pids() {
  local pid
  for pid in $(pgrep -u "$(id -u)" -f "$AISH_BIN" 2>/dev/null) \
             $(pgrep -u "$(id -u)" -f "$FIXTURE" 2>/dev/null); do
    [ "$pid" = "$$" ] && continue
    grep -qaF "$TMP/" "/proc/$pid/environ" 2>/dev/null && echo "$pid"
  done
}

# Pids of the fixture processes this run spawned (stdio child, HTTP fixture).
fixture_pids() {
  local pid
  for pid in $(pgrep -u "$(id -u)" -f "$FIXTURE" 2>/dev/null); do
    grep -qaF "$TMP/" "/proc/$pid/environ" 2>/dev/null && echo "$pid"
  done
}

# Stop everything this run started. Idempotent: runs before the final checks
# and again from the EXIT trap.
teardown() {
  tmux kill-session -t "$SESSION" >/dev/null 2>&1
  [ -n "${PANE_PID:-}" ] && kill_tree "$PANE_PID"
  if pid_is_binary "${HTTP_FIXTURE_PID:-}" "$(basename "$FIXTURE")"; then
    kill_pid "$HTTP_FIXTURE_PID"
  fi
  local pid
  for pid in $(run_pids); do kill -9 "$pid" 2>/dev/null; done
}

cleanup() {
  teardown
  if pid_is_binary "${DECOY_PID:-}" "$(basename "$FIXTURE")"; then
    kill_pid "$DECOY_PID"
  fi
  echo "artifacts: $TMP"
}
trap cleanup EXIT

ok()   { echo "PASS: $1"; PASS=$((PASS + 1)); }
bad()  { echo "FAIL: $1"; FAIL=$((FAIL + 1)); }
pane() { tmux capture-pane -p -t "$SESSION" 2>/dev/null; }

# Type one command at human speed; the slash popup eats fast bursts.
type_line() {
  local text="$1" i
  for ((i = 0; i < ${#text}; i++)); do
    tmux send-keys -t "$SESSION" -l "${text:$i:1}"
    sleep 0.25
  done
  tmux send-keys -t "$SESSION" Enter
}

# A free port for a fixture endpoint (the fixture prints its port, but the MCP
# transport owns that pipe, so the script picks one up front).
free_port() {
  python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()' 2>/dev/null || echo 39487
}
HTTP_PORT="$(free_port)"

# The HTTP transport is client-only: aish never spawns the remote server, so
# the fixture runs here as a standalone MCP endpoint for the smoke run.
"$FIXTURE" --mode http --port "$HTTP_PORT" --session > "$TMP/http-fixture.log" 2>&1 &
HTTP_FIXTURE_PID=$!
for _ in $(seq 1 40); do
  grep -qa "LISTENING" "$TMP/http-fixture.log" 2>/dev/null && break
  sleep 0.25
done

# A second fixture standing in for the endpoint another session (or the user)
# left running: it starts without this run's isolated environment. Teardown must
# not touch it (step 7).
DECOY_PORT="$(free_port)"
env -u XDG_CONFIG_HOME -u XDG_DATA_HOME -u XDG_STATE_HOME -u XDG_CACHE_HOME \
  "$FIXTURE" --mode http --port "$DECOY_PORT" --session > "$TMP/decoy.log" 2>&1 &
DECOY_PID=$!
for _ in $(seq 1 40); do
  grep -qa "LISTENING" "$TMP/decoy.log" 2>/dev/null && break
  sleep 0.25
done

cat > "$TMP/config/aish/config.yaml" <<EOF
model: smoke-model
api_key: smoke-key
api_base: http://127.0.0.1:9/v1
check_update_on_startup: false
mcp:
  enabled: true
  request_timeout_ms: 8000
  servers:
    - name: fixture
      transport: stdio
      command: /bin/sh
      args: ["-c", "exec tee -a $TMP/rx.jsonl | $FIXTURE | tee -a $TMP/tx.jsonl"]
      trust: ask
      side_effects: read
    - name: fixture-http
      transport: http
      url: http://127.0.0.1:$HTTP_PORT/mcp
      trust: ask
      side_effects: read
EOF

tmux new-session -d -s "$SESSION" -x 150 -y 45 \
  "env ${ISOLATED_XDG[*]} TERM=xterm-256color $AISH_BIN"
PANE_PID="$(tmux list-panes -t "$SESSION" -F '#{pane_pid}' 2>/dev/null | head -1)"
sleep 7

# 1. Wire evidence: handshake + discovery happened for real.
if grep -qa '"method":"initialize"' "$TMP/rx.jsonl" 2>/dev/null \
  && grep -qa 'notifications/initialized' "$TMP/rx.jsonl" 2>/dev/null \
  && grep -qa '"method":"tools/list"' "$TMP/rx.jsonl" 2>/dev/null; then
  ok "client sent initialize + notifications/initialized + tools/list"
else
  bad "handshake frames missing from $TMP/rx.jsonl"
fi
if grep -qa '"serverInfo"' "$TMP/tx.jsonl" 2>/dev/null \
  && grep -qa '"name":"echo"' "$TMP/tx.jsonl" 2>/dev/null; then
  ok "server negotiated a session and returned its tool catalogue"
else
  bad "no initialize result / tools list on the wire"
fi

# 2. /status summary line (two servers: stdio + http).
type_line "/status"
sleep 3
if pane | grep -qa "MCP：已连接 2/2\|MCP: 2/2"; then
  ok "/status shows both servers connected"
else
  bad "/status did not report 2/2 connected servers"
fi

# 3. /mcp status.
type_line "/mcp status"
sleep 3
if pane | grep -qa "fixture \[stdio\] connected"; then
  ok "/mcp status reports the stdio server as connected"
else
  bad "/mcp status did not report the stdio server"
fi
if pane | grep -qa "fixture-http \[http\] connected"; then
  ok "/mcp status reports the http server as connected"
else
  bad "/mcp status did not report the http server"
fi

# 4. /mcp tools <server>.
type_line "/mcp tools fixture"
sleep 3
if pane | grep -qa "echo（read）\|echo (read)"; then
  ok "/mcp tools lists the discovered catalogue"
else
  bad "/mcp tools did not list the catalogue"
fi

# 5. Bare `/mcp` opens the interactive menu (status first, then the picker).
type_line "/mcp"
sleep 3
if pane | grep -qa "MCP 菜单\|MCP menu"; then
  ok "bare /mcp opened the interactive menu"
else
  bad "bare /mcp did not open the menu"
fi
if pane | grep -qa "添加 MCP server\|Add MCP server"; then
  ok "menu offers the add wizard row"
else
  bad "menu is missing the add row"
fi
tmux send-keys -t "$SESSION" Escape
sleep 1

# 6. Management commands: quick add, test, disable/enable, remove.
type_line "/mcp add smoke --transport stdio --side-effects read -- $FIXTURE"
sleep 3
if grep -qa "name: smoke" "$TMP/config/aish/config.yaml" 2>/dev/null; then
  ok "/mcp add wrote the new server into config.yaml"
else
  bad "/mcp add did not persist the server"
fi
if pane | grep -qa "已添加\|Added"; then
  ok "/mcp add reported the result"
else
  bad "/mcp add printed nothing useful"
fi

type_line "/mcp test smoke"
sleep 6
if pane | grep -qa "mcp-fixture"; then
  ok "/mcp test connected and reported the server"
else
  bad "/mcp test did not report the server"
fi

type_line "/mcp disable smoke"
sleep 3
type_line "/mcp status"
sleep 3
if pane | grep -qa "smoke \[stdio\] disabled"; then
  ok "/mcp disable keeps the server visible as disabled"
else
  bad "/mcp disable hid the server"
fi

type_line "/mcp remove smoke"
sleep 3
if grep -qa "name: smoke" "$TMP/config/aish/config.yaml" 2>/dev/null; then
  bad "/mcp remove left the server in config.yaml"
else
  ok "/mcp remove dropped the server from config.yaml"
fi

# 7. Clean shutdown reaps the child (`/exit` needs a confirming second Enter).
CHILD_PIDS="$(fixture_pids)"
type_line "/exit"
sleep 1.5
tmux send-keys -t "$SESSION" Enter
sleep 3
if [ -n "$CHILD_PIDS" ]; then
  ALIVE=""
  for pid in $CHILD_PIDS; do
    kill -0 "$pid" 2>/dev/null && ALIVE="$ALIVE $pid"
  done
  if [ -z "$ALIVE" ]; then
    ok "/exit reaped the MCP server child processes"
  else
    bad "MCP server children survived /exit:$ALIVE"
  fi
else
  bad "no MCP server child process was running"
fi

# 8. Teardown kills this run's processes only: the decoy fixture stands in for
#    an endpoint another session left behind and must still be alive.
teardown
sleep 1
if kill -0 "$DECOY_PID" 2>/dev/null; then
  ok "teardown left an unrelated fixture process (pid $DECOY_PID) running"
else
  bad "teardown killed an unrelated fixture process (pid $DECOY_PID)"
fi

# 9. Isolation: the run wrote its sessions under $TMP, never into the user's
#    real database (which is what leaked before XDG_DATA_HOME was redirected).
ISOLATED_DB="$TMP/data/aish/sessions.db"
if [ -z "${BEFORE_DB:-}" ]; then
  echo "SKIP: sqlite3 or $REAL_DB unavailable; real database not inspected"
elif [ "$(sqlite3 "$REAL_DB" \
      "select count(*) from sessions where model = 'smoke-model';" 2>/dev/null)" = "$BEFORE_DB" ]; then
  ok "no smoke session was written to the real database ($REAL_DB: $BEFORE_DB rows)"
else
  bad "the run leaked into the real database ($REAL_DB: was $BEFORE_DB rows)"
fi
if [ ! -f "$ISOLATED_DB" ]; then
  bad "no session database under $TMP/data: the run is not isolated"
elif [ -z "${BEFORE_DB:-}" ]; then
  ok "the run recorded its session in the isolated database ($ISOLATED_DB)"
elif [ "$(sqlite3 "$ISOLATED_DB" \
      "select count(*) from sessions where model = 'smoke-model';" 2>/dev/null)" -ge 1 ]; then
  ok "the run recorded its session in the isolated database ($ISOLATED_DB)"
else
  bad "the isolated database $ISOLATED_DB holds no smoke session"
fi

echo "--- $PASS passed, $FAIL failed ---"
[ "$FAIL" -eq 0 ]
