#!/usr/bin/env bash
#
# Test MCP servers for aish.
#
# Builds the bundled Rust fixture server, can run it as a Streamable HTTP
# endpoint, prepares a scratch git repository for the official git server, and
# prints `mcp.servers` entries you can paste into `~/.config/aish/config.yaml`
# (or add through the `/mcp add` wizard / menu).
#
# Usage:
#   scripts/mcp-test-servers.sh build            # build the fixture server
#   scripts/mcp-test-servers.sh scratch-repo     # create the scratch git repo
#   scripts/mcp-test-servers.sh start-http [port]# run the HTTP endpoint (default: random port)
#   scripts/mcp-test-servers.sh stop-http
#   scripts/mcp-test-servers.sh status
#   scripts/mcp-test-servers.sh config           # print the YAML to paste
#   scripts/mcp-test-servers.sh selftest         # isolated dry run: every server must connect
#   scripts/mcp-test-servers.sh install          # write those servers into the real config.yaml
#   scripts/mcp-test-servers.sh stop             # stop the HTTP endpoint and clean up
#
# `install` rewrites the `mcp:` section of `~/.config/aish/config.yaml` (other
# sections and unknown keys are preserved) after saving a timestamped backup
# next to it, and prints the backup path; restore it to undo the change.
#
# `selftest` runs aish against a throw-away XDG root (config, data, state and
# cache), so it never touches `~/.local/share/aish/sessions.db`. Teardown only
# ever kills processes this script started: the recorded HTTP pid and the
# processes that carry the isolated root of a run in their environment.
#
# Environment: AISH_BIN, AISH_MCP_TEST_STATE (move the scratch/pid directory).

set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# AISH_MCP_TEST_STATE redirects the scratch directory; the harness test uses it
# so it can exercise `stop` without touching the endpoint the user's config
# points at.
STATE="${AISH_MCP_TEST_STATE:-${TMPDIR:-/tmp}/aish-mcp-test}"
FIXTURE="$ROOT/target/debug/aish-mcp-fixture"
SCRATCH_REPO="${TMPDIR:-/tmp}/aish-mcp-scratch"
AISH_BIN="${AISH_BIN:-$ROOT/target/debug/aish}"
PID_FILE="$STATE/http.pid"
PORT_FILE="$STATE/http.port"
ROOTS_FILE="$STATE/env-roots"

mkdir -p "$STATE"

say() { printf '%s\n' "$*"; }

fixture_built() { [ -x "$FIXTURE" ]; }

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

# Kill $1 and everything below it: the shell tmux ran aish in, plus the MCP
# servers aish spawned (fixture, uvx, node).
kill_tree() {
  local pid="${1:-}" child
  [ -n "$pid" ] || return 0
  for child in $(pgrep -P "$pid" 2>/dev/null); do kill_tree "$child"; done
  kill -9 "$pid" 2>/dev/null
}

# Kill the leftovers of the isolated run rooted at $1. A process counts as ours
# only when its command line names our binaries *and* its environment carries that
# root, so the sweep can never match the user's shell, the tmux server or another
# session's servers.
sweep_isolated() {
  local root="$1" pid
  for pid in $(pgrep -u "$(id -u)" -f "$AISH_BIN" 2>/dev/null) \
             $(pgrep -u "$(id -u)" -f "$FIXTURE" 2>/dev/null); do
    grep -qaF "$root/" "/proc/$pid/environ" 2>/dev/null && kill -9 "$pid" 2>/dev/null
  done
}

# Create the XDG root an isolated aish runs against and record it so that `stop`
# can clean up an interrupted run. `uvx` resolves its environments through
# XDG_CACHE_HOME, so the uv cache is linked back in; otherwise a machine without
# network access could not start the Python servers at all.
prepare_isolated_root() {
  local root="$1"
  mkdir -p "$root/config/aish" "$root/data" "$root/state" "$root/cache"
  if [ -d "$HOME/.cache/uv" ] && [ ! -e "$root/cache/uv" ]; then
    ln -s "$HOME/.cache/uv" "$root/cache/uv"
  fi
  if [ -d "$HOME/.local/share/uv" ] && [ ! -e "$root/data/uv" ]; then
    ln -s "$HOME/.local/share/uv" "$root/data/uv"
  fi
  printf '%s\n' "$root" >> "$ROOTS_FILE"
}

# The user's real session database and the rows the selftest config would leak
# into it (`probe-model` is not a model anyone runs for real).
real_db_path() { printf '%s\n' "${XDG_DATA_HOME:-$HOME/.local/share}/aish/sessions.db"; }

real_db_probe_sessions() {
  local db; db="$(real_db_path)"
  [ -f "$db" ] || { printf '0\n'; return 0; }
  sqlite3 "$db" "select count(*) from sessions where model = 'probe-model';" 2>/dev/null
}

cmd_build() {
  say "== building the fixture MCP server =="
  (cd "$ROOT" && cargo build -p aish-mcp --bin aish-mcp-fixture) || return 1
  say "built: $FIXTURE"
}

cmd_scratch_repo() {
  if [ ! -d "$SCRATCH_REPO/.git" ]; then
    mkdir -p "$SCRATCH_REPO"
    git -C "$SCRATCH_REPO" init -q
    git -C "$SCRATCH_REPO" config user.email mcp-test@example.com
    git -C "$SCRATCH_REPO" config user.name "MCP Test"
    printf 'hello from the scratch repo\n' > "$SCRATCH_REPO/README.md"
    git -C "$SCRATCH_REPO" add README.md
    git -C "$SCRATCH_REPO" commit -qm "initial commit"
  fi
  say "scratch repo: $SCRATCH_REPO"
}

cmd_start_http() {
  fixture_built || cmd_build || return 1
  cmd_stop_http >/dev/null 2>&1
  local port="${1:-0}"
  # `--session` exercises `Mcp-Session-Id` (the fixture echoes and validates it).
  "$FIXTURE" --mode http --port "$port" --session >"$STATE/http.log" 2>&1 &
  local pid=$!
  echo "$pid" > "$PID_FILE"
  for _ in $(seq 1 40); do
    if grep -qa "LISTENING" "$STATE/http.log" 2>/dev/null; then break; fi
    sleep 0.25
  done
  local bound
  bound="$(sed -n 's/^LISTENING \([0-9]*\).*/\1/p' "$STATE/http.log" | head -1)"
  if [ -z "$bound" ]; then
    say "failed to start the HTTP endpoint; log:"
    tail -5 "$STATE/http.log"
    return 1
  fi
  echo "$bound" > "$PORT_FILE"
  say "http endpoint: http://127.0.0.1:$bound/mcp (pid $pid)"
}

cmd_stop_http() {
  if [ -f "$PID_FILE" ]; then
    local pid; pid="$(cat "$PID_FILE")"
    if pid_is_binary "$pid" "aish-mcp-fixture"; then
      kill "$pid" 2>/dev/null
      sleep 0.3
      kill -9 "$pid" 2>/dev/null
      say "stopped http endpoint"
    else
      say "http endpoint pid $pid is gone (stale pid file)"
    fi
    rm -f "$PID_FILE" "$PORT_FILE"
  else
    say "no http endpoint running"
  fi
}

cmd_status() {
  say "fixture:  $(fixture_built && echo "$FIXTURE" || echo 'not built (run: build)')"
  say "aish:     $([ -x "$AISH_BIN" ] && echo "$AISH_BIN" || echo "missing: $AISH_BIN")"
  say "repo:     $SCRATCH_REPO $([ -d "$SCRATCH_REPO/.git" ] && echo '(ready)' || echo '(run: scratch-repo)')"
  if pid_is_binary "$(cat "$PID_FILE" 2>/dev/null)" "aish-mcp-fixture" && [ -f "$PORT_FILE" ]; then
    say "http:     http://127.0.0.1:$(cat "$PORT_FILE")/mcp (pid $(cat "$PID_FILE"))"
  else
    say "http:     not running (run: start-http)"
  fi
}

yaml_http_url() {
  if pid_is_binary "$(cat "$PID_FILE" 2>/dev/null)" "aish-mcp-fixture" && [ -f "$PORT_FILE" ]; then
    echo "http://127.0.0.1:$(cat "$PORT_FILE")/mcp"
  else
    echo "http://127.0.0.1:39487/mcp   # start it with: $0 start-http 39487"
  fi
}

cmd_config() {
  fixture_built || say "# note: the fixture is not built yet — run: $0 build"
  [ -d "$SCRATCH_REPO/.git" ] || say "# note: run '$0 scratch-repo' for the git server"
  say "# Paste under \`mcp:\` in ~/.config/aish/config.yaml (servers: [...]), or add them"
  say "# through the interactive menu: /mcp -> 1. add MCP server"
  say "#"
  say "# The first four need no network. browsermcp/playwright are the npm packages"
  say "# already cached on this machine (calls need the browser side; discovery works)."
  cat <<EOF
  servers:
    # 1. Bundled fixture server: read/write tools, slow/fail/big/structured/image.
    #    Good for approvals, timeouts, cancellation, truncation and audit tests.
    - name: fixture
      transport: stdio
      command: $FIXTURE
      trust: ask
      side_effects: read

    # 2. Same fixture over Streamable HTTP (start it with: $0 start-http 39487).
    - name: fixture-http
      transport: http
      url: $(yaml_http_url)
      trust: ask
      side_effects: read

    # 3. Official Python time server (uvx downloads it on first use).
    - name: time
      transport: stdio
      command: uvx
      args: ["mcp-server-time"]
      trust: allow
      side_effects: read          # get_current_time/convert_time are read-only

    # 4. Official Python fetch server: fetches a URL (network side effects).
    - name: fetch
      transport: stdio
      command: uvx
      args: ["mcp-server-fetch"]
      trust: ask
      side_effects: network

    # 5. Official Python git server on a scratch repository (12 git tools).
    #    It annotates every tool (readOnlyHint for git_status/git_log/…,
    #    destructive for git_reset), so side_effects: read + trust: allow
    #    auto-approves the read-only ones while git_commit/git_add/git_reset
    #    still ask. expose: tools puts each tool's real schema (git_status
    #    needs repo_path) in front of the model.
    - name: git
      transport: stdio
      command: uvx
      args: ["mcp-server-git", "--repository", "$SCRATCH_REPO"]
      trust: allow
      side_effects: read
      expose: tools

    # 6/7. Real-world npm servers already cached here (no network needed to start).
    - name: browsermcp
      transport: stdio
      command: node
      args: ["$(ls -d "$HOME"/.npm/_npx/*/node_modules/@browsermcp/mcp/dist/index.js 2>/dev/null | head -1)"]
      trust: ask
      side_effects: unknown

    # 7. Playwright MCP (cached too) needs Node >= 20; this machine has
    #    $(node -v 2>/dev/null || echo 'no node'), where it exits with an explicit
    #    version error. Uncomment after upgrading Node.
    # - name: playwright
    #   transport: stdio
    #   command: node
    #   args: ["$(ls -d "$HOME"/.npm/_npx/*/node_modules/@playwright/mcp/cli.js 2>/dev/null | head -1)"]
    #   trust: ask
    #   side_effects: unknown

    # 8. Failure UX (uncomment to see a failed state): missing command and a
    #    server that exits right after the handshake.
    # - name: broken
    #   transport: stdio
    #   command: /nonexistent/mcp-server
    #   trust: ask
    #   side_effects: unknown
    # - name: crashy
    #   transport: stdio
    #   command: $FIXTURE
    #   args: ["--behavior", "crash-after-init"]
    #   trust: ask
    #   side_effects: unknown
EOF
}

cmd_selftest() {
  fixture_built || cmd_build || return 1
  cmd_scratch_repo >/dev/null
  if [ ! -f "$PORT_FILE" ]; then cmd_start_http >/dev/null || return 1; fi
  local root; root="$(mktemp -d "${TMPDIR:-/tmp}/aish-mcp-selftest.XXXX")"
  prepare_isolated_root "$root"
  local real_db before_db
  real_db="$(real_db_path)"
  before_db="$(real_db_probe_sessions)"
  local body; body="$(cmd_config | sed -n '/^  servers:/,$p')"
  {
    echo "model: probe-model"
    echo "api_key: probe-key"
    echo "api_base: http://127.0.0.1:9/v1"
    echo "check_update_on_startup: false"
    echo "mcp:"
    echo "  enabled: true"
    echo "  request_timeout_ms: 60000"
    printf '%s\n' "$body"
  } > "$root/config/aish/config.yaml"
  local session="aish-mcp-selftest"
  local session_pid=""
  local isolated_env="XDG_CONFIG_HOME=$root/config XDG_DATA_HOME=$root/data XDG_STATE_HOME=$root/state XDG_CACHE_HOME=$root/cache"
  tmux kill-session -t "$session" 2>/dev/null
  # aish must run with every XDG directory inside $root: with only
  # XDG_CONFIG_HOME redirected it still opened and wrote the user's real
  # ~/.local/share/aish/sessions.db.
  tmux new-session -d -s "$session" -x 150 -y 48 \
    "env $isolated_env TERM=xterm-256color $AISH_BIN"
  session_pid="$(tmux list-panes -t "$session" -F '#{pane_pid}' 2>/dev/null | head -1)"
  # Slash commands must be typed character by character: the completion popup
  # swallows a burst of keystrokes.
  type_line() {
    local text="$1" i
    for ((i = 0; i < ${#text}; i++)); do
      tmux send-keys -t "$session" -l "${text:$i:1}"
      sleep 0.12
    done
    tmux send-keys -t "$session" Enter
  }
  sleep 25   # uvx / node servers need their first start
  type_line "/mcp status"
  sleep 5
  local pane; pane="$(tmux capture-pane -p -t "$session" 2>/dev/null)"
  local connected=(fixture fixture-http time fetch git browsermcp)
  local failing=()
  local ok=0 fail=0
  say "== per-server state =="
  for name in "${connected[@]}"; do
    line="$(printf '%s\n' "$pane" | grep -a "^$name \[" | tail -1 | sed -e 's/^[[:space:]]*//')"
    if printf '%s' "$line" | grep -qa connected; then
      say "  PASS $line"; ok=$((ok + 1))
    else
      say "  FAIL $name: ${line:-no status line}"
      fail=$((fail + 1))
    fi
  done
  for name in "${failing[@]}"; do
    line="$(printf '%s\n' "$pane" | grep -a "^$name \[" | tail -1 | sed -e 's/^[[:space:]]*//')"
    if printf '%s' "$line" | grep -qa failed; then
      say "  PASS $line (expected failure)"; ok=$((ok + 1))
    else
      say "  FAIL $name: ${line:-no status line} (expected a failed state)"
      fail=$((fail + 1))
    fi
  done
  say "== isolation =="
  if [ -z "$before_db" ]; then
    say "  SKIP real session database not inspected (need sqlite3 and $real_db)"
  else
    local after_db; after_db="$(real_db_probe_sessions)"
    if [ "$after_db" = "$before_db" ]; then
      say "  PASS real session database untouched ($real_db: $after_db probe-model rows)"
      ok=$((ok + 1))
    else
      say "  FAIL the run wrote into the real session database ($real_db: $before_db -> $after_db rows)"
      fail=$((fail + 1))
    fi
  fi
  if [ -f "$root/data/aish/sessions.db" ]; then
    say "  PASS session database written under the isolated root"
    ok=$((ok + 1))
  else
    say "  FAIL no session database under $root/data: the run is not isolated"
    fail=$((fail + 1))
  fi
  tmux kill-session -t "$session" 2>/dev/null
  [ -n "$session_pid" ] && kill_tree "$session_pid"
  # Backstop for an unclean exit; membership is proven by the isolated root.
  sweep_isolated "$root"
  # The HTTP endpoint is intentionally left running: it is a standalone server
  # the user may keep for manual testing (stop it with `stop-http`).
  say "(isolated root kept for inspection: $root)"
  say "== $ok as expected, $fail unexpected =="
  [ "$fail" -eq 0 ]
}

cmd_install() {
  fixture_built || cmd_build || return 1
  [ -d "$SCRATCH_REPO/.git" ] || cmd_scratch_repo >/dev/null
  if [ ! -f "$PORT_FILE" ]; then cmd_start_http 39487 >/dev/null || return 1; fi
  local target="${AISH_CONFIG:-$HOME/.config/aish/config.yaml}"
  if [ ! -f "$target" ]; then say "no config at $target"; return 1; fi
  local backup="${target}.bak-$(date +%Y%m%d%H%M%S)"
  cp "$target" "$backup"
  local body; body="$(cmd_config | sed -n '/^  servers:/,$p')"
  if ! AISH_MCP_BODY="$body" AISH_MCP_TARGET="$target" \
    python3 "$ROOT/scripts/mcp-test-install.py"; then
    say "failed to write $target; restoring the backup"
    cp "$backup" "$target"
    return 1
  fi
  say "backup: $backup"
  say "next: start aish, then run /mcp (interactive menu) or /mcp status"
  say "note: the http endpoint must stay up for \`fixture-http\`; restart it with: $0 start-http 39487"
}

cmd_stop() {
  cmd_stop_http
  # Kill only what an isolated run left behind: the HTTP pid handled above and
  # the roots `selftest` recorded. The old `pkill -9 -f <fixture>` matched every
  # fixture on the machine, including the user's own servers and shell helpers.
  if [ -f "$ROOTS_FILE" ]; then
    local root
    while read -r root; do
      case "$root" in
        */aish-mcp-selftest.*)
          sweep_isolated "$root"
          rm -rf "$root"
          ;;
      esac
    done < "$ROOTS_FILE"
    : > "$ROOTS_FILE"
  fi
  say "cleaned up"
}

case "${1:-help}" in
  build) cmd_build ;;
  scratch-repo) cmd_scratch_repo ;;
  start-http) shift; cmd_start_http "$@" ;;
  stop-http) cmd_stop_http ;;
  status) cmd_status ;;
  config) cmd_config ;;
  selftest) cmd_selftest ;;
  install) cmd_install ;;
  stop) cmd_stop ;;
  *)
    sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    ;;
esac
