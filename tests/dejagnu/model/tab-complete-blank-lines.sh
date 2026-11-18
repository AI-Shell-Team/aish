#!/usr/bin/env bash
#
# Regression smoke: Tab-completing a slash command must not leave blank lines
# in the terminal scrollback (fixed: slash popup viewport now matches the
# rendered row count instead of reserving 9 rows).
#
# Method: run the real binary under `script` (typescript capture) inside a
# controlled pty, type "/sett" + Tab (popup opens with 1 candidate -> Tab
# completes and dismisses), then Esc and exit. The captured typescript is
# scanned for runs of 3+ consecutive blank lines between the popup and the
# next prompt — the symptom being fixed.
#
# Usage: bash tests/dejagnu/model/tab-complete-blank-lines.sh [AISH_BIN]

set -u

AISH_BIN="${1:-$PWD/target/debug/aish}"
TMP="$(mktemp -d /tmp/aish-tab-smoke.XXXXXX)"
TS="$TMP/cast.typescript"

cleanup() {
  [ -n "${SCRIPT_PID:-}" ] && kill "$SCRIPT_PID" 2>/dev/null
  wait 2>/dev/null
  rm -rf "$TMP"
}
trap cleanup EXIT

# Isolated XDG root so the run does not touch the user's real config.
export XDG_CONFIG_HOME="$TMP/config"
export XDG_DATA_HOME="$TMP/data"
export XDG_STATE_HOME="$TMP/state"
export XDG_CACHE_HOME="$TMP/cache"
export AISH_CONFIG_DIR="$TMP/config/aish"
mkdir -p "$AISH_CONFIG_DIR" "$TMP/data" "$TMP/state" "$TMP/cache"

# Drive the binary inside a pty via `script`; keystrokes are fed with
# delays so the popup has time to open.
SCRIPT_PID=""
(
  sleep 3
  printf '/sett'
  sleep 1
  printf '\t'      # Tab: popup opens with 1 candidate, completes + dismisses
  sleep 1
  printf '\x1b'    # Esc: cancel back to a fresh prompt
  sleep 1
  printf '/setting\r'  # direct command: open settings panel
  sleep 3
  printf '\x1b'    # Esc: leave settings
  sleep 1
  printf '\x04'    # Ctrl-D: exit
  sleep 2
) | script -q -c "$AISH_BIN" "$TS" > /dev/null 2>&1 &
SCRIPT_PID=$!

# Wait for the session to end.
for _ in $(seq 1 60); do
  kill -0 "$SCRIPT_PID" 2>/dev/null || break
  sleep 1
done

# Scan the typescript for runs of 3+ consecutive blank lines. The typescript
# format prefixes each write with timing metadata; strip it with `script`'s
# -q output plus a plain-text filter (the replay format from util-linux writes
# raw bytes only, so a straight scan works).
python3 - "$TS" <<'PY'
import re, sys

raw = open(sys.argv[1], "rb").read().decode("utf-8", "replace")
# Strip ANSI escape sequences (colors, cursor moves, clears) for the scan.
text = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", raw)
text = re.sub(r"\x1b\][^\x07]*\x07", "", text)
text = re.sub(r"\x1b[()][B0]", "", text)

# Count runs of 3+ consecutive blank lines (empty after stripping CR).
runs = re.findall(r"(?:\r?\n[ \t]*\r?\n){2,}", text)
if runs:
    print(f"FAIL: found {len(runs)} run(s) of 3+ blank lines")
    sys.exit(1)
print("PASS: no blank-line runs in the transcript")
PY
