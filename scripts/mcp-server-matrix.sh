#!/usr/bin/env bash
#
# MCP client test matrix for aish.
#
# Runs aish against a set of hand-written MCP servers built on the official
# Python and Node SDKs (see `scripts/mcp-servers/`), covering both transports,
# every tool/annotation kind, server-initiated requests and notifications,
# approval paths, timeouts, cancellation and process cleanup.
#
# The run is fully isolated: a throw-away XDG root holds the config, the
# session database and the caches, so the user's real `~/.config/aish` and
# `~/.local/share/aish/sessions.db` are never touched. Only processes this
# script started are ever killed.
#
# Usage:
#   scripts/mcp-server-matrix.sh check      # verify the SDK runners work offline
#   scripts/mcp-server-matrix.sh serve      # start the HTTP/SSE servers
#   scripts/mcp-server-matrix.sh stop       # stop them
#   scripts/mcp-server-matrix.sh config     # print the generated mcp: section
#   scripts/mcp-server-matrix.sh install    # write it into an isolated XDG root
#   scripts/mcp-server-matrix.sh shell      # interactive aish against that root
#   scripts/mcp-server-matrix.sh run        # run the whole tmux matrix
#
# Environment: AISH_BIN (aish binary), AISH_MCP_MATRIX_STATE (state directory),
# AISH_MCP_MATRIX_KEEP=1 (keep the isolated root for inspection).

set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATE="${AISH_MCP_MATRIX_STATE:-${TMPDIR:-/tmp}/aish-mcp-matrix}"
PY_DIR="$ROOT/scripts/mcp-servers/python"
NODE_DIR="$ROOT/scripts/mcp-servers/node"
NODE_MODULES_SRC="${AISH_MCP_NODE_MODULES:-$HOME/.npm/_npx/6ddf87659f2ad8a4/node_modules}"
AISH_BIN="${AISH_BIN:-$ROOT/target/debug/aish}"
# --no-project: the repo sits under a parent uv project, and without this flag
# uv tries to resolve *its* dependencies (offline resolution then fails).
VENV="$STATE/venv"
PY_BIN="$VENV/bin/python"

# One shared virtualenv for every python cell: `uv run --with …` re-resolves the
# environment on each start (and the stdio servers are started per connection),
# while a prepared interpreter starts a server in well under a second.
setup_venv() {
    if [ -x "$PY_BIN" ] && "$PY_BIN" -c "import mcp, uvicorn" >/dev/null 2>&1; then
        return 0
    fi
    mkdir -p "$STATE"
    say "preparing the shared python environment (offline)"
    uv venv --python 3.12 --quiet "$VENV" || die "uv venv failed"
    uv pip install --offline --quiet --python "$PY_BIN" "mcp==1.30.0" uvicorn \
        || die "installing mcp/uvicorn from the local cache failed"
}

HTTP_PY_TOOLS_PORT=39201
HTTP_PY_AUTH_PORT=39202
HTTP_PY_SSE_PORT=39203
HTTP_TS_TOOLS_PORT=39204
SSE_TOKEN="matrix-secret-token"

PY_TOOLS_URL="http://127.0.0.1:$HTTP_PY_TOOLS_PORT/mcp"
PY_AUTH_URL="http://127.0.0.1:$HTTP_PY_AUTH_PORT/mcp"
PY_SSE_URL="http://127.0.0.1:$HTTP_PY_SSE_PORT/sse"
TS_TOOLS_URL="http://127.0.0.1:$HTTP_TS_TOOLS_PORT/mcp"

say() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

# --- state helpers ---------------------------------------------------------

root_of_run() { printf '%s' "${RUN_ROOT:-$STATE/root}"; }

prepare_isolated_root() {
    local root
    root="$(root_of_run)"
    rm -rf "$root"
    mkdir -p "$root"/{config,data,state,cache,servers}
    # Keep uv's cache and interpreters reachable so the offline runs stay
    # offline.
    mkdir -p "$root/cache"
    ln -sfn "$HOME/.cache/uv" "$root/cache/uv" 2>/dev/null || true
    mkdir -p "$root/data"
    ln -sfn "$HOME/.local/share/uv" "$root/data/uv" 2>/dev/null || true
    printf '%s' "$root"
}

require_node_modules() {
    [ -d "$NODE_DIR/node_modules/@modelcontextprotocol/sdk" ] && return 0
    [ -d "$NODE_MODULES_SRC/@modelcontextprotocol/sdk" ] || die \
        "the cached TypeScript SDK is missing; set AISH_MCP_NODE_MODULES to a node_modules tree containing @modelcontextprotocol/sdk"
    ln -sfn "$NODE_MODULES_SRC" "$NODE_DIR/node_modules"
    say "linked $NODE_DIR/node_modules -> $NODE_MODULES_SRC"
}

# --- server process management ---------------------------------------------

pid_is_ours() {
    local pid="$1" want="$2"
    [ -n "$pid" ] || return 1
    kill -0 "$pid" 2>/dev/null || return 1
    tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null | grep -q -- "$want"
}

stop_pid() {
    local pid="$1" name="$2"
    [ -n "$pid" ] || return 0
    if kill -0 "$pid" 2>/dev/null; then
        kill "$pid" 2>/dev/null
        for _ in $(seq 1 20); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.2
        done
        kill -9 "$pid" 2>/dev/null
        say "stopped $name (pid $pid)"
    fi
    rm -f "$STATE/$name.pid"
}

start_server() {
    local name="$1" log="$2" ready="$3"
    shift 3
    rm -f "$ready"
    ( : >"$log" )
    ("$@" >/dev/null 2>>"$log" &
        echo $! >"$STATE/$name.pid")
    for _ in $(seq 1 100); do
        [ -s "$ready" ] && return 0
        sleep 0.2
    done
    warn "$name did not report READY; log tail:"
    tail -5 "$log" >&2 || true
    return 1
}

cmd_check() {
    say "== SDK availability =="
    require_node_modules
    (cd "$NODE_DIR" && node -e '
const { McpServer } = require("@modelcontextprotocol/sdk/server/mcp.js");
const { StdioServerTransport } = require("@modelcontextprotocol/sdk/server/stdio.js");
console.log("node sdk ok:", typeof McpServer, typeof StdioServerTransport);
') || die "node SDK is not loadable"
    setup_venv
    "$PY_BIN" - <<'PY' || die "python SDK is not loadable"
from mcp.server.fastmcp import FastMCP
import uvicorn
print("python sdk ok: FastMCP", hasattr(FastMCP, "run"), "| uvicorn", uvicorn.__version__)
PY
    say "== server files =="
    local missing=0 f
    for f in tools_server.py features_server.py auth_http_server.py slow_init_server.py \
        crash_once_server.py junk_names_server.py legacy_sse_server.py noisy_server.py; do
        [ -f "$PY_DIR/$f" ] || { warn "missing python/$f"; missing=1; }
    done
    for f in tools_server.cjs features_server.cjs auth_http_server.cjs; do
        [ -f "$NODE_DIR/$f" ] || { warn "missing node/$f"; missing=1; }
    done
    [ "$missing" -eq 0 ] || die "the server matrix is incomplete"
    say "all servers present"
}

cmd_serve() {
    require_node_modules
    setup_venv
    mkdir -p "$STATE"
    # A previous run may have left servers listening; starting on top of them
    # would only produce bind failures.
    cmd_stop >/dev/null 2>&1 || true
    local root
    root="$(root_of_run)"
    mkdir -p "$root/servers"

    start_server py-tools-http "$STATE/py-tools-http.log" "$STATE/py-tools-http.ready" \
        "$PY_BIN" "$PY_DIR/tools_server.py" --transport http --port "$HTTP_PY_TOOLS_PORT" \
        --ready-file "$STATE/py-tools-http.ready" --root "$root/servers" \
        || { cmd_stop; die "py-tools-http failed to start"; }

    start_server py-auth-http "$STATE/py-auth-http.log" "$STATE/py-auth-http.ready" \
        "$PY_BIN" "$PY_DIR/auth_http_server.py" --port "$HTTP_PY_AUTH_PORT" \
        --token "$SSE_TOKEN" --ready-file "$STATE/py-auth-http.ready" --root "$root/servers" \
        || { cmd_stop; die "py-auth-http failed to start"; }

    start_server py-legacy-sse "$STATE/py-legacy-sse.log" "$STATE/py-legacy-sse.ready" \
        "$PY_BIN" "$PY_DIR/legacy_sse_server.py" --port "$HTTP_PY_SSE_PORT" \
        --ready-file "$STATE/py-legacy-sse.ready" --root "$root/servers" \
        || { cmd_stop; die "py-legacy-sse failed to start"; }

    start_server ts-tools-http "$STATE/ts-tools-http.log" "$STATE/ts-tools-http.ready" \
        node "$NODE_DIR/tools_server.cjs" --transport http --port "$HTTP_TS_TOOLS_PORT" \
        --ready-file "$STATE/ts-tools-http.ready" --root "$root/servers" \
        || { cmd_stop; die "ts-tools-http failed to start"; }

    say "http servers up:"
    for f in "$STATE"/*.ready; do
        [ -f "$f" ] && printf '  %s: %s\n' "$(basename "$f" .ready)" "$(cat "$f")"
    done
}

cmd_stop() {
    local name
    for name in py-tools-http py-auth-http py-legacy-sse ts-tools-http; do
        stop_pid "$(cat "$STATE/$name.pid" 2>/dev/null)" "$name"
    done
    # Pid files only cover the latest start; a run whose files were overwritten
    # would leak its listeners, so sweep by the argv they were started with.
    local root pid
    root="$(root_of_run)"
    for pid in $(pgrep -f -- "--root $root/servers" 2>/dev/null); do
        [ "$pid" = "$$" ] && continue
        if tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null | grep -q -- "--root $root/servers"; then
            kill -9 "$pid" 2>/dev/null && say "swept stale server pid $pid"
        fi
    done
}

# --- configuration ---------------------------------------------------------

render_servers() {
    local root
    root="$(root_of_run)"
    cat <<YAML
    # --- python SDK, stdio: every tool and annotation kind -------------------
    - name: py-tools
      transport: stdio
      command: $PY_BIN
      args: ["$PY_DIR/tools_server.py", "--root", "$root/servers"]
      trust: allow
      side_effects: read
      expose: tools
    # same server, default ask/unknown: proves nothing runs without a yes
    - name: py-tools-ask
      transport: stdio
      command: $PY_BIN
      args: ["$PY_DIR/tools_server.py", "--root", "$root/servers"]
      trust: ask
      side_effects: unknown
      expose: gateway
    # same server again, with a tiny per-server output cap: proves the cap is
    # applied per server and not only through the global default
    - name: py-tools-tiny
      transport: stdio
      command: $PY_BIN
      args: ["$PY_DIR/tools_server.py", "--root", "$root/servers"]
      trust: allow
      side_effects: read
      max_output_bytes: 1024
    # --- node SDK, stdio (interop across SDKs) -------------------------------
    - name: ts-tools
      transport: stdio
      command: node
      args: ["$NODE_DIR/tools_server.cjs", "--root", "$root/servers"]
      trust: allow
      side_effects: read
      expose: tools
    # --- python SDK, Streamable HTTP ----------------------------------------
    - name: py-http
      transport: http
      url: $PY_TOOLS_URL
      trust: allow
      side_effects: read
      expose: tools
    - name: ts-http
      transport: http
      url: $TS_TOOLS_URL
      trust: ask
      side_effects: unknown
      expose: gateway
    # --- bearer-token HTTP, with and without the token ----------------------
    - name: py-auth
      transport: http
      url: $PY_AUTH_URL
      headers:
        Authorization: "Bearer $SSE_TOKEN"
      trust: allow
      side_effects: read
      expose: tools
    - name: py-auth-notoken
      transport: http
      url: $PY_AUTH_URL
      trust: ask
      side_effects: unknown
    # --- slow handshake, crash-once restart, junk names, noisy stderr --------
    - name: py-slow-init
      transport: stdio
      command: $PY_BIN
      args: ["$PY_DIR/slow_init_server.py", "--init-delay-ms", "6000", "--root", "$root/servers"]
      request_timeout_ms: 30000
      trust: allow
      side_effects: read
    - name: py-crash-once
      transport: stdio
      command: $PY_BIN
      args: ["$PY_DIR/crash_once_server.py", "--root", "$root/servers"]
      trust: allow
      side_effects: read
    - name: py-junk-names
      transport: stdio
      command: $PY_BIN
      args: ["$PY_DIR/junk_names_server.py", "--root", "$root/servers"]
      trust: allow
      side_effects: read
    - name: py-noisy
      transport: stdio
      command: $PY_BIN
      args: ["$PY_DIR/noisy_server.py", "--root", "$root/servers"]
      trust: allow
      side_effects: read
    # --- resources/prompts + list_changed + server-initiated requests --------
    - name: py-features
      transport: stdio
      command: $PY_BIN
      args: ["$PY_DIR/features_server.py", "--root", "$root/servers"]
      trust: allow
      side_effects: read
    - name: ts-features
      transport: stdio
      command: node
      args: ["$NODE_DIR/features_server.cjs", "--root", "$root/servers"]
      trust: ask
      side_effects: read
    # --- legacy HTTP+SSE: aish supports only Streamable HTTP -----------------
    - name: py-legacy-sse
      transport: http
      url: $PY_SSE_URL
      trust: ask
      side_effects: unknown
YAML
}

cmd_config() {
    printf 'mcp:\n  enabled: true\n  request_timeout_ms: 30000\n  servers:\n'
    render_servers
}

install_config() {
    local root config
    root="$(root_of_run)"
    config="$root/config/aish/config.yaml"
    mkdir -p "$(dirname "$config")"
    # Start from the user's real config so the model credentials work, then
    # replace the mcp section with the matrix servers.
    if [ -f "$HOME/.config/aish/config.yaml" ]; then
        cp "$HOME/.config/aish/config.yaml" "$config"
    else
        printf 'mcp:\n  servers: []\n' >"$config"
    fi
    MCP_SERVERS="$(render_servers)" python3 - "$config" <<'PY'
import os, sys, yaml
path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    document = yaml.safe_load(handle) or {}
body = "mcp:\n  enabled: true\n  request_timeout_ms: 30000\n  servers:\n" + os.environ["MCP_SERVERS"]
section = yaml.safe_load(body)["mcp"]
document["mcp"] = section
with open(path, "w", encoding="utf-8") as handle:
    yaml.safe_dump(document, handle, sort_keys=False, allow_unicode=True)
print(f"installed {len(section['servers'])} matrix servers into {path}")
PY
}

# Create the isolated root and write the matrix config into it.
cmd_install() {
    local root
    root="$(prepare_isolated_root)"
    say "isolated root: $root"
    install_config
}

# Start an interactive aish against the isolated root (Ctrl-D / /exit to leave).
cmd_shell() {
    [ -x "$AISH_BIN" ] || die "aish binary not found: $AISH_BIN (build it first)"
    local root
    root="$(root_of_run)"
    [ -f "$root/config/aish/config.yaml" ] || install_config
    say "starting $AISH_BIN with XDG_CONFIG_HOME=$root/config"
    env TERM=xterm-256color \
        XDG_CONFIG_HOME="$root/config" XDG_DATA_HOME="$root/data" \
        XDG_STATE_HOME="$root/state" XDG_CACHE_HOME="$root/cache" \
        MCP_FIXTURE_ROOT="$root/servers" \
        "$AISH_BIN"
}

# --- tmux driving ----------------------------------------------------------

SESSION="${AISH_MCP_MATRIX_SESSION:-aish-mcp-matrix}"
PANES=0
PASSED=0
FAILED=0
FAILURES=()

stop_session() { tmux kill-session -t "$SESSION" 2>/dev/null || true; }

sweep_isolated() {
    local root
    root="$(root_of_run)"
    local pid
    for pid in $(pgrep -f "aish-mcp-matrix|$root" 2>/dev/null); do
        [ "$pid" = "$$" ] && continue
        if tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null | grep -q -- "$root"; then
            kill -9 "$pid" 2>/dev/null && say "swept leftover pid $pid"
        fi
    done
}

pane() { tmux capture-pane -p -S -4000 -t "$SESSION" 2>/dev/null; }

# True when the shell is idle: the input line is the last line, nothing is
# thinking, no panel and no picker is open.
#
# The input line is drawn even while a turn runs, so the last-line check alone
# would report idle mid-turn (a spinner line sits above the prompt).
shell_is_idle() {
    local last
    last="$(pane | sed -e 's/[[:space:]]*$//' | grep -v '^$' | tail -1)"
    printf '%s' "$last" | grep -qaE '^◆ aish' || return 1
    pane | tail -8 | grep -qaE '思考中' && return 1
    pane | tail -25 | grep -qaE '你的选择|1-9 quick select' && return 1
    return 0
}

# Wait until the shell is idle. Every action waits for this first, so a slow
# model turn or an open picker can never swallow the next line.
wait_idle() {
    local timeout="${1:-90}" i
    for i in $(seq 1 "$((timeout * 5))"); do
        shell_is_idle && return 0
        sleep 0.2
    done
    return 1
}

# If a plain-looking line was taken as a natural-language question, answer yes.
answer_nl_prompt() {
    if new_lines | grep -qa '是否让 AI 解答'; then
        tmux send-keys -t "$SESSION" -l "y"
        sleep 0.2
        tmux send-keys -t "$SESSION" Enter
    fi
}

# Wait until the output produced after the last `mark` matches the pattern.
wait_for() {
    local pattern="$1" timeout="${2:-60}" i
    for i in $(seq 1 "$((timeout * 5))"); do
        new_lines | grep -aEq "$pattern" && return 0
        sleep 0.2
    done
    say "        (waited ${timeout}s for: $pattern)"
    return 1
}

# Output produced after the last `mark`.
new_lines() {
    local line
    if [ "$MARK_SEQ" -gt 0 ]; then
        line="$(pane | grep -aFnx "AISH-MARK-$MARK_SEQ" | tail -1 | cut -d: -f1)"
        if [ -n "$line" ]; then
            pane | tail -n "+$line"
            return
        fi
    fi
    pane
}

# Wait until any of the patterns appears in the new output. Used wherever the
# next event may legitimately be one of several things (an approval panel or a
# tool card), so the driver never wastes a timeout on the branch not taken.
wait_for_any() {
    local timeout="$1"
    shift
    local i pattern
    for i in $(seq 1 "$((timeout * 5))"); do
        for pattern in "$@"; do
            new_lines | grep -aEq "$pattern" && return 0
        done
        sleep 0.1
    done
    return 1
}

# Assert a shell-level condition (not pane text) and print PASS/FAIL.
assert_true() {
    local name="$1"
    shift
    if "$@"; then
        PASSED=$((PASSED + 1))
        printf '  PASS  %s\n' "$name"
    else
        FAILED=$((FAILED + 1))
        FAILURES+=("$name")
        printf '  FAIL  %s\n' "$name"
    fi
}

# Assert that the new output does NOT contain the pattern.
check_absent() {
    local name="$1" pattern="$2"
    if new_lines | grep -aEq "$pattern"; then
        FAILED=$((FAILED + 1))
        FAILURES+=("$name")
        printf '  FAIL  %s (unexpected match: %s)\n' "$name" "$pattern"
        new_lines | sed -e 's/[[:space:]]*$//' | grep -v '^$' | tail -8 | sed 's/^/        /'
    else
        PASSED=$((PASSED + 1))
        printf '  PASS  %s\n' "$name"
    fi
}

# Plant a sentinel line in the shell so the next assertion only looks at output
# produced after it. Line arithmetic on a scrolling pane is not reliable; a
# token echoed by the shell itself is.
MARK_SEQ=0
mark() {
    MARK_SEQ=$((MARK_SEQ + 1))
    local token="AISH-MARK-$MARK_SEQ"
    tmux send-keys -t "$SESSION" -l "echo $token"
    sleep 0.2
    tmux send-keys -t "$SESSION" Enter
    wait_for "^${token}$" 30 || true
}

# Assert that the output produced since the last mark matches the pattern.
# Usage: check <name> <pattern>
check() {
    local name="$1" pattern="$2"
    if new_lines | grep -aEq "$pattern"; then
        PASSED=$((PASSED + 1))
        printf '  PASS  %s\n' "$name"
    else
        FAILED=$((FAILED + 1))
        FAILURES+=("$name")
        printf '  FAIL  %s (pattern: %s)\n' "$name" "$pattern"
        printf '        --- last 12 lines ---\n'
        pane | sed -e 's/[[:space:]]*$//' | grep -v '^$' | tail -12 | sed 's/^/        /'
    fi
}

# Same, but the assertion window starts from the beginning of the pane (used
# for `/mcp status` style output that is printed in one go).
check_full() {
    local name="$1" pattern="$2"
    if pane | grep -aEq "$pattern"; then
        PASSED=$((PASSED + 1))
        printf '  PASS  %s\n' "$name"
    else
        FAILED=$((FAILED + 1))
        FAILURES+=("$name")
        printf '  FAIL  %s (pattern: %s)\n' "$name" "$pattern"
        printf '        --- last 12 lines ---\n'
        pane | sed -e 's/[[:space:]]*$//' | grep -v '^$' | tail -12 | sed 's/^/        /'
    fi
}

# Type an AI prompt: the whole line in one burst, because the leading `;`
# switches the shell into AI mode and per-character typing can drop it.
ask_line() {
    tmux send-keys -t "$SESSION" -l "; $1"
    sleep 0.4
    tmux send-keys -t "$SESSION" Enter
    sleep 3
    answer_nl_prompt
}

# Type a slash/shell line one character at a time: the slash-command popup
# swallows burst input.
type_line() {
    local text="$1" i
    for ((i = 0; i < ${#text}; i++)); do
        tmux send-keys -t "$SESSION" -l "${text:$i:1}"
        sleep 0.1
    done
    tmux send-keys -t "$SESSION" Enter
}

approve() {
    tmux send-keys -t "$SESSION" -l "y"
    sleep 0.3
    tmux send-keys -t "$SESSION" Enter
}

# "remember this session" in the approval panel.
remember() {
    tmux send-keys -t "$SESSION" -l "a"
    sleep 0.3
    tmux send-keys -t "$SESSION" Enter
}

# Ask the model to call one tool and wait for the tool card to settle.
# Drive one model turn: approve every panel that appears along the way and wait
# until the shell is idle again. Looking for "a card appeared" was not enough —
# the model often lists tools first, and an approval panel left open eats the
# next step's keystrokes as its answer.
ask_tool() {
    local prompt="$1" waited=0
    wait_idle 60 || true
    mark
    ask_line "$prompt"
    while [ "$waited" -lt 600 ]; do
        if panel_is_open; then
            approve
            sleep 1
            waited=$((waited + 4))
            continue
        fi
        if shell_is_idle; then
            break
        fi
        sleep 0.3
        waited=$((waited + 1))
    done
    sleep 0.5
}

# Ask the same question until the model actually did the thing (it sometimes
# only narrates). `pattern` is matched against the output of the last attempt.
ask_tool_until() {
    local pattern="$1" prompt="$2" tries="${3:-3}" attempt
    for attempt in $(seq 1 "$tries"); do
        ask_tool "$prompt"
        if new_lines | grep -aEq "$pattern"; then
            return 0
        fi
        say "        (attempt $attempt did not produce '$pattern'; asking again)"
    done
    return 1
}

# True while an approval/confirm panel is on screen.
panel_is_open() {
    pane | tail -30 | grep -qa '你的选择'
}

cmd_run() {
    [ -x "$AISH_BIN" ] || die "aish binary not found: $AISH_BIN (build it first)"
    command -v tmux >/dev/null || die "tmux is required"
    require_node_modules
    setup_venv
    mkdir -p "$STATE"
    say "== isolated root =="
    prepare_isolated_root >/dev/null
    say "root: $(root_of_run)"
    say "== starting servers =="
    cmd_serve
    say "== isolated config =="
    install_config
    # Every run must exercise the crash path, so clear the once-marker.
    rm -f "$(root_of_run)/servers/crash-once.marker"

    local root
    root="$(root_of_run)"
    stop_session
    tmux new-session -d -s "$SESSION" -x 200 -y 60 \
        "env TERM=xterm-256color \
             XDG_CONFIG_HOME=$root/config XDG_DATA_HOME=$root/data \
             XDG_STATE_HOME=$root/state XDG_CACHE_HOME=$root/cache \
             MCP_FIXTURE_ROOT=$root/servers \
             $AISH_BIN"
    say "== waiting for the shell =="
    wait_for '◆ aish' 90 || die "aish did not start; pane: $(pane | tail -5)"

    # Servers with a real handshake need a moment (py-slow-init deliberately
    # takes 6s).
    sleep 12

    say ""
    say "== 1. connection and status =="
    wait_idle 60 || true
    mark
    # The python stdio servers need a moment (py-slow-init takes 6s on purpose),
    # so poll the command instead of sleeping a fixed amount.
    local attempt
    for attempt in 1 2 3 4; do
        type_line "/mcp status"
        wait_for '已连接 1[23]/15' 20 && break
        sleep 1
    done
    check "expected servers connected, broken ones rejected" '已连接 1[23]/15'
    check_full "legacy SSE server reported a classified failure" 'py-legacy-sse \[http\] failed'
    check_full "legacy SSE failure carries its reason" '失败: .*(http|connect|40[45])'
    check_full "py-slow-init connected after its slow handshake" 'py-slow-init \[stdio\] connected'
    check_full "py-junk-names connected" 'py-junk-names \[stdio\] connected'
    check_full "py-noisy connected despite its stderr flood" 'py-noisy \[stdio\] connected'
    check_full "bearer-token HTTP server connected" 'py-auth \[http\] connected'
    check_full "misconfigured (token-less) HTTP server failed cleanly" 'py-auth-notoken \[http\] failed'
    check_full "http transports connected" 'py-http \[http\] connected'
    check_full "ts-http connected" 'ts-http \[http\] connected'

    say ""
    say "== 2. tool catalogue and annotations =="
    wait_idle 60 || true
    mark
    type_line "/mcp tools py-tools"
    wait_for 'echo' 30 || true
    for tool in echo add slow fail big image structured unicode write_note delete_all; do
        check "py-tools exposes $tool" "(^|[^a-z_])$tool\b"
    done
    wait_idle 60 || true
    mark
    type_line "/mcp tool py-tools write_note"
    wait_for 'destructive|write' 30 || true
    check "write_note detail shows destructive + verdict" 'destructive|write'
    wait_idle 60 || true
    mark
    type_line "/mcp tools py-junk-names"
    wait_for 'echo' 30 || true
    check "junk names filtered, valid name listed" 'echo'
    check_absent "invalid tool names are absent from the catalogue" 'bad|n{20,}'
    wait_idle 60 || true
    mark
    type_line "/mcp tool py-tools delete_all"
    wait_for 'write' 30 || true
    check "delete_all reports the open-world destructive hint" 'open|destructive|write'

    say ""
    say "== 3. calls through the model over both SDKs and both transports =="
    # One turn per group: a turn is what costs wall time, and the model issues
    # several tool calls per turn, so the checks stay independent while the run
    # stays short.
    ask_tool_until 'matrix-echo' "依次调用这些 MCP 工具，每个都调用一次，然后简短汇总每一步的结果：\n1) server py-tools 的 echo，text 为 matrix-echo\n2) server py-tools 的 add，a 为 2、b 为 3\n3) server py-tools 的 structured（汇总里写出 count 的值）\n4) server py-tools 的 unicode（原样保留文本）\n5) server py-tools 的 image（说明是否收到图片）" || true
    check "python SDK: echo returned the text" 'matrix-echo'
    check "python SDK: add returned 5" '(^|[^0-9])5([^0-9]|$)'
    check "python SDK: structuredContent reached the model" '(count|3)'
    check "python SDK: unicode round-tripped" '你好'
    check "python SDK: image content was reported" '(图片|image|是的)'
    ask_tool_until 'node-echo' "依次调用这些 MCP 工具，每个都调用一次，然后简短汇总每一步的结果：\n1) server ts-tools 的 echo，text 为 node-echo\n2) server py-http 的 echo，text 为 via-http\n3) server py-auth 的 echo，text 为 auth-ok" || true
    check "node SDK server answered" 'node-echo'
    check "Streamable HTTP call worked" 'via-http'
    check "bearer-token HTTP call worked" 'auth-ok'
    # ts-http is configured `trust: ask`, so it is called on its own: the panel
    # must appear and the call must run after approving it.
    ask_tool_until 'ts-http-echo' "调用 MCP server ts-http 的 echo 工具，text 为 ts-http-echo，只回答结果。" || true
    check "node HTTP server answered after approval" 'ts-http-echo'

    say "== 4. truncation, failure, cancellation =="
    # `py-tools-tiny` caps one call at 1024 bytes, so the marker has to be in
    # the rendered result itself (independent of what the model chooses to say).
    ask_tool "调用 MCP server py-tools-tiny 的 big 工具，kb 设为 300，然后回答：返回内容里是否出现了 truncated 字样？只回答 是 或 否。"
    # The marker is what the model actually received, so read it from the
    # isolated session database instead of relying on the model's wording (the
    # card collapses long results, which can hide the last line).
    local isolated_db="$root/data/aish/sessions.db"
    assert_true "the per-server cap marked the truncated output for the model" bash -c "
grep -q 'truncated at 1024 bytes' <(sqlite3 '$isolated_db' \"select content from ai_messages where role='tool'\" 2>/dev/null) 2>/dev/null
"
    check "the model was told the output was truncated" '(是|truncated)' 
    ask_tool "调用 MCP server py-tools 的 fail 工具，并告诉我它的报错内容。"
    check "tool-level error surfaced" '(exit nonzero|fixture failure)'
    wait_idle 60 || true
    mark
    ask_line "调用 MCP server py-tools 的 slow 工具，ms 设为 30000，然后回答。"
    wait_for 'mcp__|需要确认' 60 || true
    sleep 1
    tmux send-keys -t "$SESSION" Escape
    mark
    wait_for '已中断' 30 || true
    check "long call cancelled from the keyboard" '已中断' 

    say ""
    say "== 5. approval paths =="
    wait_idle 60 || true
    mark
    ask_line "调用 MCP server py-tools-ask 的 echo 工具，text 设为 ask-path，只回答结果。"
    # The flash model sometimes only narrates the call, so retry until the panel
    # (or the card) really shows up.
    local attempt
    for attempt in 1 2 3; do
        wait_for_any 45 '需要确认' 'mcp__py_tools_ask \(\{"action":"call"' || true
        if new_lines | grep -aq '需要确认'; then
            break
        fi
        say "        (no panel yet; asking the model again)"
        ask_line "必须真正调用 MCP server py-tools-ask 的 echo 工具（text 为 ask-path），不要只解释。"
    done
    check "untrusted server asks before running" '需要确认'
    check "panel shows the exact arguments" 'text=ask-path'
    remember
    wait_for 'ask-path' 60 || true
    check "remembered call ran" 'ask-path'
    wait_idle 60 || true
    mark
    ask_line "调用 MCP server py-tools 的 write_note 工具，path 为 note.txt、content 为 hi，只回答路径。"
    wait_for_any 60 '需要确认' 'mcp__' || true
    check "destructive tool asks even on a read-only trust:allow server" '需要确认'
    check "panel explains the side-effect narrowing" 'write|destructive'
    tmux send-keys -t "$SESSION" -l "n"
    sleep 0.3
    tmux send-keys -t "$SESSION" Enter
    wait_for '拒绝|denied|换方案' 30 || true
    check "denial reported back to the model" '拒绝|denied|换方案'

    say ""
    say "== 6. reuse and re-approval by exact arguments =="
    wait_idle 60 || true
    mark
    ask_line "调用 MCP server py-tools-ask 的 echo 工具，text 设为 ask-path，只回答结果。"
    # Either the remembered call runs (card with the text) or it asks again;
    # never burn a timeout waiting for the branch that cannot happen.
    wait_for_any 45 'ask-path' '需要确认' || true
    if new_lines | grep -aq '需要确认'; then
        check "a remembered approval must not ask again" '需要确认'
        approve
    else
        check "a remembered approval must not ask again" 'ask-path'
    fi
    wait_idle 60 || true
    mark
    ask_line "调用 MCP server py-tools-ask 的 echo 工具，text 设为 changed-args，只回答结果。"
    wait_for '需要确认' 60 || true
    check "changed arguments ask again (approval is bound to the arguments)" '需要确认'
    check "the panel shows the new arguments" 'text=changed-args'
    approve
    wait_for 'changed-args' 60 || true
    check "the re-approved call ran" 'changed-args'

    say ""
    say "== 7. slow handshake and crash recovery =="
    wait_idle 60 || true
    mark
    type_line "/mcp test py-slow-init"
    # Do not wait for the server name: the echoed command line contains it.
    wait_for '正常|失败|超时|可用工具' 40 || true
    check "bounded /mcp test against a slow server" '(正常|失败|超时|可用工具)' 
    wait_idle 60 || true
    mark
    type_line "/mcp reconnect py-crash-once"
    wait_for '已重连|重连失败' 40 || true
    check "manual reconnect succeeds" '已重连' 
    ask_tool "调用 MCP server py-crash-once 的 echo 工具，text 设为 after-crash，只回答结果。"
    check "call works after the crash/reconnect cycle" 'after-crash'

    say ""
    say "== 8. server-initiated requests and notifications =="
    ask_tool "调用 MCP server py-features 的 list_added 工具，然后回答 added。"
    check "list_added call succeeded" 'added'
    wait_idle 60 || true
    mark
    type_line "/mcp tools py-features"
    wait_for 'added_after_change|ask_client' 30 || true
    check "tools/list_changed added the new tool to the live catalogue" 'added_after_change'
    ask_tool "调用 MCP server py-features 的 ask_client 工具，然后回答它返回的文本。"
    check "ask_client call completed" '(roots|sampling|client)'
    check "the tool result reports both exchanges" \
        'client answers: roots=ok, sampling=error -32601'
    local answers="$root/servers/ask-client.json"
    assert_true "the server recorded the client answers in ask-client.json" test -f "$answers"
    assert_true "client answered roots/list with a result" python3 -c "
import json, sys
data = json.load(open(sys.argv[1], encoding='utf-8'))
roots = data.get('roots') or {}
sys.exit(0 if isinstance(roots, dict) and 'result' in roots else 1)
" "$answers"
    assert_true "client rejected sampling/createMessage with -32601" python3 -c "
import json, sys
data = json.load(open(sys.argv[1], encoding='utf-8'))
sampling = data.get('sampling') or {}
sys.exit(0 if ((sampling.get('error') or {}).get('code') == -32601) else 1)
" "$answers"

    say ""
    say "== 9. cleanup =="
    wait_idle 60 || true
    # Find this run's own shell first: the assertion below is about *aish*
    # reaping its children when it exits, not about the harness cleaning up.
    local aish_pid
    aish_pid="$(for pid in $(pgrep -f -- "$AISH_BIN" 2>/dev/null); do
        tr '\0' '\n' <"/proc/$pid/environ" 2>/dev/null | grep -q "XDG_CONFIG_HOME=$root/config" && echo "$pid"
    done | head -1)"
    mark
    # Leave any AI/picker state first: a slash command typed into AI mode never
    # reaches the shell.
    tmux send-keys -t "$SESSION" Escape
    sleep 0.5
    type_line "/exit"
    local waited=0
    while [ -n "$aish_pid" ] && kill -0 "$aish_pid" 2>/dev/null && [ "$waited" -lt 40 ]; do
        sleep 0.5
        waited=$((waited + 1))
    done
    if [ -n "$aish_pid" ] && kill -0 "$aish_pid" 2>/dev/null; then
        say "aish did not exit on /exit; retrying once"
        tmux send-keys -t "$SESSION" Escape
        sleep 0.5
        type_line "/exit"
        waited=0
        while [ -n "$aish_pid" ] && kill -0 "$aish_pid" 2>/dev/null && [ "$waited" -lt 20 ]; do
            sleep 0.5
            waited=$((waited + 1))
        done
    fi
    if [ -n "$aish_pid" ] && kill -0 "$aish_pid" 2>/dev/null; then
        say "aish did not exit on /exit; pane tail:"
        pane | sed -e 's/[[:space:]]*$//' | grep -v '^$' | tail -6 | sed 's/^/        /'
        kill -9 "$aish_pid" 2>/dev/null
    fi
    sleep 1
    # Count only aish's own stdio children: the HTTP servers this harness started
    # carry the same --root and must not be mistaken for leaked children.
    local leftover=0 pid harness_pids
    harness_pids="$(cat "$STATE"/*.pid 2>/dev/null | sort -u)"
    for pid in $(pgrep -f -- "--root $root/servers" 2>/dev/null); do
        printf '%s\n' "$harness_pids" | grep -qx "$pid" && continue
        tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null | grep -q -- "--transport http\|--port " && continue
        leftover=$((leftover + 1))
        say "        leaked child: $(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null | cut -c1-110)"
    done
    assert_true "every stdio child was reaped when aish exited" test "$leftover" -eq 0
    sweep_isolated
    stop_session
    cmd_stop
    local real_db="$HOME/.local/share/aish/sessions.db"
    if [ -f "$real_db" ]; then
        local leaked
        leaked="$(sqlite3 "$real_db" "select count(*) from sessions where model like '%matrix%';" 2>/dev/null || echo 0)"
        if [ "$leaked" = "0" ]; then
            PASSED=$((PASSED + 1))
            printf '  PASS  the real session database was not written\n'
        else
            FAILED=$((FAILED + 1))
            FAILURES+=("real session database untouched")
            printf '  FAIL  %s matrix sessions leaked into the real database\n' "$leaked"
        fi
    fi

    if [ "${AISH_MCP_MATRIX_KEEP:-0}" != "1" ]; then
        rm -rf "$root"
    else
        say "kept the isolated root: $root"
    fi

    say ""
    say "===== matrix summary: $PASSED passed, $FAILED failed ====="
    if [ "$FAILED" -gt 0 ]; then
        printf 'failed checks:\n'
        printf '  - %s\n' "${FAILURES[@]}"
        exit 1
    fi
}

case "${1:-run}" in
check) cmd_check ;;
install) cmd_install ;;
shell) cmd_shell ;;
serve) cmd_serve ;;
stop) cmd_stop ;;
config) cmd_config ;;
run) cmd_run ;;
all)
    cmd_check
    cmd_serve
    cmd_run
    ;;
*)
    die "unknown subcommand: ${1} (use check|install|shell|serve|stop|config|run|all)"
    ;;
esac
