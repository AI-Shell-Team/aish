#!/usr/bin/env bash
#
# End-to-end smoke test for the /model switcher surface (issue #592) against
# the real `aish` binary.
#
# Why tmux and not expect/DejaGnu: aish draws a slash-command popup as soon as
# the line starts with `/`. A burst of keystrokes is consumed by the popup
# redraw, and the popup's own terminal queries (ESC[6n) need a real terminal
# emulator to answer them. tmux provides both, so this script types character
# by character at human speed and reads the pane.
#
# What it asserts:
#   1. `/model <name>` on a writable config prints the saved-as-default result
#      and the config file carries the new model
#   2. `/model <name>` with an unwritable config *directory* (the atomic save
#      renames over the file, so the file's own mode cannot block it) prints
#      the session-only warning, and the file keeps the old model
#   3. `/model` picker lists the loopback fixture's models (fetch path works)
#   4. picker `s` (session-only switch) changes the live session but never
#      writes the config file
#   5. restoring write access makes a later `/model <name>` persist again
#
# Usage (from the repo root, after `cargo build`):
#   bash tests/dejagnu/model/model-switch-smoke.sh
#
# Environment overrides: AISH_BIN.

set -u

AISH_BIN="${AISH_BIN:-$PWD/target/debug/aish}"
SESSION="aish-model-smoke-$$"
TMP="$(mktemp -d /tmp/aish-model-smoke.XXXXXX)"
PASS=0
FAIL=0

# --- Loopback fixture endpoint ----------------------------------------------
# Serves GET /v1/models with a small catalogue; POSTs get a minimal chat
# response so a stray AI call cannot wedge the run.
FIXTURE_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()' 2>/dev/null || echo 39471)"
FIXTURE_LOG="$TMP/fixture.log"

python3 - "$FIXTURE_PORT" > "$FIXTURE_LOG" 2>&1 <<'PYEOF' &
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

port = int(sys.argv[1])

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            body = json.dumps({
                "data": [{"id": m} for m in
                         ["model-a", "model-b", "model-c"]]
            }).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        body = json.dumps({
            "id": "chatcmpl-smoke",
            "object": "chat.completion",
            "created": 0,
            "model": "model-a",
            "choices": [{"index": 0, "message": {"role": "assistant",
                         "content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1,
                      "total_tokens": 2},
        }).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        sys.stderr.write("%s\n" % (fmt % args))
        sys.stderr.flush()

HTTPServer(("127.0.0.1", port), Handler).serve_forever()
PYEOF
FIXTURE_PID=$!
for _ in $(seq 1 40); do
  if python3 -c "import socket; s=socket.socket(); s.settimeout(0.3); s.connect(('127.0.0.1', $FIXTURE_PORT))" 2>/dev/null; then
    break
  fi
  sleep 0.25
done

# --- Isolation ---------------------------------------------------------------
ISOLATED_XDG=(
  "XDG_CONFIG_HOME=$TMP/config"
  "XDG_DATA_HOME=$TMP/data"
  "XDG_STATE_HOME=$TMP/state"
  "XDG_CACHE_HOME=$TMP/cache"
)
mkdir -p "$TMP/config/aish" "$TMP/data" "$TMP/state" "$TMP/cache"

CONFIG="$TMP/config/aish/config.yaml"
CONFIG_DIR="$TMP/config/aish"
write_config() {
  cat > "$CONFIG" <<EOF
model: model-a
api_key: smoke-key
api_base: http://127.0.0.1:$FIXTURE_PORT/v1
check_update_on_startup: false
EOF
}
write_config
chmod 644 "$CONFIG"

tmux new-session -d -s "$SESSION" -x 150 -y 45 \
  "env ${ISOLATED_XDG[*]} TERM=xterm-256color $AISH_BIN"
sleep 7

ok()   { echo "PASS: $1"; PASS=$((PASS + 1)); }
bad()  { echo "FAIL: $1"; FAIL=$((FAIL + 1)); }
pane() { tmux capture-pane -p -t "$SESSION" 2>/dev/null; }

# Clear the line first (C-u): a previous command's leftovers or a half-open
# slash popup must never bleed into the next command.
type_line() {
  tmux send-keys -t "$SESSION" C-u
  sleep 0.3
  local text="$1" i
  for ((i = 0; i < ${#text}; i++)); do
    tmux send-keys -t "$SESSION" -l "${text:$i:1}"
    sleep 0.25
  done
  tmux send-keys -t "$SESSION" Enter
}

config_model() {
  grep -E '^model:' "$CONFIG" 2>/dev/null | head -1 | sed 's/^model:[[:space:]]*//'
}

cleanup() {
  tmux kill-session -t "$SESSION" >/dev/null 2>&1
  [ -n "${PANE_PID:-}" ] && kill "${PANE_PID}" 2>/dev/null
  kill "$FIXTURE_PID" 2>/dev/null
  chmod -R u+w "$CONFIG_DIR" 2>/dev/null
  echo "artifacts: $TMP"
}
trap cleanup EXIT

# 1. /model model-b -> saved as default, config file updated.
type_line "/model model-b"
sleep 3
if pane | grep -qa "model-b"; then
  ok "/model model-b produced a result message"
else
  bad "no result message for /model model-b"
fi
if pane | grep -qa "已设为默认\|saved as default\|als Standard\|デフォルトに保存\|enregistré par défaut\|guardado como predeterminado"; then
  ok "result message states the default was saved"
else
  bad "result message does not state the saved-as-default scope"
fi
if [ "$(config_model)" = "model-b" ]; then
  ok "config file now carries model-b"
else
  bad "config file model is '$(config_model)', expected model-b"
fi

# 2. Unwritable config DIRECTORY -> the atomic save (temp file + rename) is
#    blocked, the session keeps running, and the file keeps the old model.
chmod 555 "$CONFIG_DIR"
type_line "/model model-c"
sleep 3
chmod 755 "$CONFIG_DIR"
if pane | grep -qa "默认模型未保存\|NOT saved\|NICHT gespeichert\|未保存\|NON enregistre\|NO guardado"; then
  ok "unwritable config directory produced the session-only warning"
else
  bad "no session-only warning for unwritable config directory"
fi
if [ "$(config_model)" = "model-b" ]; then
  ok "failed save left the config file at model-b"
else
  bad "config file changed despite failed save: '$(config_model)'"
fi

# 3. Picker lists the fixture catalogue (fetch path works).
type_line "/model"
sleep 3
if pane | grep -qa "model-c"; then
  ok "picker shows fetched model-c from the fixture endpoint"
else
  bad "picker did not show fetched models"
fi

# 4. `s` on the highlighted (non-current) entry -> session-only switch, no
#    config write. The entry under the cursor after one Down is whichever
#    recent/fetched item comes first; only the scope semantics matter here.
BASELINE_MODEL="$(config_model)"
tmux send-keys -t "$SESSION" Down
sleep 0.5
tmux send-keys -t "$SESSION" s
sleep 3
if pane | grep -qa "本会话已切换\|this session only\|nur diese Sitzung\|このセッション\|cette session\|solo esta sesion"; then
  ok "picker s printed the session-only result"
else
  bad "picker s did not print the session-only result"
fi
if [ "$(config_model)" = "$BASELINE_MODEL" ]; then
  ok "picker s left the config file untouched"
else
  bad "picker s wrote the config file: '$(config_model)' (expected $BASELINE_MODEL)"
fi

# 5. Writable config restored -> a later direct command persists again.
#    (The picker already exits on `s`, so no Escape is needed here — a stray
#    Escape would land in the input line.)
type_line "/model model-a"
sleep 3
if [ "$(config_model)" = "model-a" ]; then
  ok "restored config persists a later /model command"
else
  bad "later /model did not persist: '$(config_model)'"
fi

echo "--- $PASS passed, $FAIL failed ---"
[ "$FAIL" -eq 0 ]
