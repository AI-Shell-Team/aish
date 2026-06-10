#!/usr/bin/env bash
# Shell tests for __aish_complete control-pipe protocol.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WRAPPER="${ROOT}/src/bash_rc_wrapper.sh"
TEST_TMP="${ROOT}/.completion_test_tmp"
mkdir -p "$TEST_TMP"
export TMPDIR="$TEST_TMP"

if [[ ! -f "$WRAPPER" ]]; then
  echo "missing wrapper: $WRAPPER" >&2
  exit 1
fi

# Direct invocation with mocked control fd writer.
source_wrapper_and_complete() {
  local line="$1"
  local cursor="$2"
  local request_id="${3:-1}"
  local fifo
  fifo="$(mktemp -u)"
  mkfifo "$fifo"
  exec {CTL_FD}<> "$fifo"
  rm -f "$fifo"

  export AISH_CONTROL_FD=$CTL_FD
  bash --norc --noprofile -c "source '$WRAPPER'; __aish_complete $request_id $(printf '%q' "$line") $cursor" \
    >/dev/null 2>/dev/null &
  local pid=$!

  local json="" buf=""
  while IFS= read -r -t 3 buf <&$CTL_FD; do
    if [[ "$buf" == *'"type":"completion_result"'* ]]; then
      json="$buf"
      break
    fi
  done
  wait "$pid" 2>/dev/null || true
  exec {CTL_FD}<&-
  printf '%s' "$json"
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local msg="${3:-}"
  if [[ "$haystack" != *"$needle"* ]]; then
    echo "ASSERT FAIL: ${msg:-expected '$needle' in output}" >&2
    echo "  got: $haystack" >&2
    exit 1
  fi
}

echo "=== completion shell tests ==="

json=$(source_wrapper_and_complete "gi" 2)
assert_contains "$json" '"type":"completion_result"' "gi emits completion_result"
assert_contains "$json" '"replacement":"git "' "gi includes git candidate"

json=$(source_wrapper_and_complete "ls /ho" 6)
assert_contains "$json" '/home/' "ls /ho completes /home/"

json=$(source_wrapper_and_complete "/ho" 3)
assert_contains "$json" '/home/' "/ho at word 0 completes /home/"

json=$(source_wrapper_and_complete "/home/" 7)
assert_contains "$json" '"display":"' "/home/ lists directory children"

json=$(source_wrapper_and_complete "/us" 3)
assert_contains "$json" '/usr/' "/us at word 0 completes /usr/"

json=$(source_wrapper_and_complete "/usr/" 5)
assert_contains "$json" '"display":"' "/usr/ lists directory children"
assert_contains "$json" '/usr/' "/usr/ has child replacement paths"

json=$(source_wrapper_and_complete "/usr/" 5)
if [[ "$json" == *'"replacement":"/usr/"'* ]]; then
  echo "ASSERT FAIL: /usr/ should not offer itself as candidate" >&2
  exit 1
fi

if [[ -d /home ]]; then
  json=$(source_wrapper_and_complete "ls /home/" 9)
  assert_contains "$json" '"display":"' "ls /home/ has display fields"
  assert_contains "$json" '/home/' "ls /home/ has replacement paths"
  if [[ "$json" == *'"/home/"'* && "$json" != *'lixin'* && "$json" != *'test'* ]]; then
    echo "WARN: ls /home/ may only list self — check path-like bypass" >&2
  fi
fi

json=$(source_wrapper_and_complete "cd /home/" 9)
assert_contains "$json" '/home/' "cd /home/ completes children"

json=$(source_wrapper_and_complete "" 0)
assert_contains "$json" '"candidates":[]' "empty line returns no candidates"

if [[ -d /usr/bin ]]; then
  json=$(source_wrapper_and_complete "ls /usr/bin" 11)
  assert_contains "$json" '/usr/bin' "ls /usr/bin uses native bash completion"
  n=$(echo "$json" | grep -o '"display"' | wc -l)
  if (( n > 100 )); then
    echo "ASSERT FAIL: ls /usr/bin exceeded completion limit ($n)" >&2
    exit 1
  fi

  json=$(source_wrapper_and_complete "ls /usr/bin/" 12)
  assert_contains "$json" '"display":"' "ls /usr/bin/ lists children"
fi

SORT_DIR="${ROOT}/.completion_test_tmp/sorttest"
mkdir -p "$SORT_DIR"/{zebra,alpha,beta}
json=$(source_wrapper_and_complete "ls ${SORT_DIR}/" $(( ${#SORT_DIR} + 4 )) )
if [[ "$json" == *'"display":"alpha/'* ]] && [[ "$json" == *'"display":"beta/'* ]] && [[ "$json" == *'"display":"zebra/'* ]]; then
  alpha_pos=${json%%\"display\":\"alpha/\"*}
  beta_pos=${json%%\"display\":\"beta/\"*}
  zebra_pos=${json%%\"display\":\"zebra/\"*}
  if (( ${#alpha_pos} >= ${#beta_pos} || ${#beta_pos} >= ${#zebra_pos} )); then
    echo "ASSERT FAIL: directory children should be sorted like bash (alpha beta zebra), got $json" >&2
    exit 1
  fi
else
  echo "ASSERT FAIL: expected alpha/beta/zebra candidates, got $json" >&2
  exit 1
fi

echo "All completion shell tests passed."
