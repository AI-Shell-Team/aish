# aish bash rc wrapper
# This file is used as rcfile for interactive bash

# Enable readline for interactive use
set -o emacs

# Source user's bashrc if exists
if [ -f ~/.bashrc ]; then
    source ~/.bashrc
fi

# Source system bashrc if exists
if [ -f /etc/bash.bashrc ]; then
    source /etc/bash.bashrc
fi

case ":${HISTCONTROL:-}:" in
    *:ignorespace:*|*:ignoreboth:*)
        ;;
    "::")
        HISTCONTROL="ignorespace"
        ;;
    *)
        HISTCONTROL="${HISTCONTROL}:ignorespace"
        ;;
esac

# Set up exit code tracking
__aish_last_exit_code=0
__AISH_PROTOCOL_VERSION=1
__AISH_CONTROL_FD="${AISH_CONTROL_FD:-}"
__AISH_AT_PROMPT=0

__aish_json_escape() {
    local value="$1"
    value=${value//\\/\\\\}
    value=${value//\"/\\\"}
    value=${value//$'\n'/\\n}
    value=${value//$'\r'/\\r}
    value=${value//$'\t'/\\t}
    printf '%s' "$value"
}

__aish_emit_control_line() {
    local payload="$1"
    if [[ ! "$__AISH_CONTROL_FD" =~ ^[0-9]+$ ]]; then
        return 0
    fi

    printf '%s\n' "$payload" >&${__AISH_CONTROL_FD} 2>/dev/null || true
}

__aish_emit_session_ready() {
    local ts cwd_json payload
    ts=$(date +%s)
    cwd_json=$(__aish_json_escape "$PWD")
    printf -v payload \
        '{"version":%s,"type":"session_ready","ts":%s,"shell_pid":%s,"cwd":"%s","shlvl":%s}' \
        "$__AISH_PROTOCOL_VERSION" "$ts" "$$" "$cwd_json" "${SHLVL:-0}"
    __aish_emit_control_line "$payload"
}

__aish_emit_prompt_ready() {
    local exit_code="$1"
    local ts cwd_json interrupted command_seq payload
    ts=$(date +%s)
    cwd_json=$(__aish_json_escape "$PWD")
    interrupted=false
    if [[ "$exit_code" == "130" ]]; then
        interrupted=true
    fi

    command_seq=null
    if [[ -n "${__AISH_ACTIVE_COMMAND_SEQ:-}" ]]; then
        command_seq="${__AISH_ACTIVE_COMMAND_SEQ}"
    fi

    printf -v payload \
        '{"version":%s,"type":"prompt_ready","ts":%s,"command_seq":%s,"exit_code":%s,"cwd":"%s","shlvl":%s,"interrupted":%s}' \
        "$__AISH_PROTOCOL_VERSION" "$ts" "$command_seq" "$exit_code" "$cwd_json" "${SHLVL:-0}" "$interrupted"
    __aish_emit_control_line "$payload"
    unset __AISH_ACTIVE_COMMAND_SEQ
    unset __AISH_ACTIVE_COMMAND_TEXT
}

__aish_emit_command_started() {
    local command="$1"
    local ts command_json command_seq payload
    ts=$(date +%s)
    command_json=$(__aish_json_escape "$command")

    command_seq=null
    if [[ -n "${__AISH_ACTIVE_COMMAND_SEQ:-}" ]]; then
        command_seq="${__AISH_ACTIVE_COMMAND_SEQ}"
    fi

    printf -v payload \
        '{"version":%s,"type":"command_started","ts":%s,"command_seq":%s,"command":"%s","cwd":"%s","shlvl":%s}' \
        "$__AISH_PROTOCOL_VERSION" "$ts" "$command_seq" "$command_json" "$(__aish_json_escape "$PWD")" "${SHLVL:-0}"
    __aish_emit_control_line "$payload"
}

__aish_rewrite_last_history_entry() {
    local seq="${__AISH_ACTIVE_COMMAND_SEQ:-}"
    local original_command="${__AISH_ACTIVE_COMMAND_TEXT:-}"
    local history_line history_index history_command

    if [[ -z "$seq" ]]; then
        return 0
    fi

    history_line=$(builtin history 1 2>/dev/null || true)
    if [[ "$history_line" =~ ^[[:space:]]*([0-9]+)[[:space:]]+(.*)$ ]]; then
        history_index="${BASH_REMATCH[1]}"
        history_command="${BASH_REMATCH[2]}"

        if [[ "$history_command" == __AISH_ACTIVE_COMMAND_SEQ=* ]]; then
            builtin history -d "$history_index" 2>/dev/null || true
            if [[ -z "$original_command" ]]; then
                original_command="${history_command#*; }"
            fi
        fi
    fi

    if [[ "$seq" == -* ]]; then
        return 0
    fi

    if [[ -z "$original_command" ]]; then
        return 0
    fi

    builtin history -s "$original_command" 2>/dev/null || true
}

__aish_emit_shell_exiting() {
    local exit_code="$1"
    local ts payload
    ts=$(date +%s)
    printf -v payload \
        '{"version":%s,"type":"shell_exiting","ts":%s,"exit_code":%s}' \
        "$__AISH_PROTOCOL_VERSION" "$ts" "$exit_code"
    __aish_emit_control_line "$payload"
}

__aish_on_exit() {
    local exit_code=$?
    __aish_emit_shell_exiting "$exit_code"
}

__aish_on_debug() {
    if [[ "${__AISH_AT_PROMPT:-0}" != "1" ]]; then
        return 0
    fi

    case "$BASH_COMMAND" in
        __aish_prompt_command*|__aish_on_debug*|__aish_emit_*|__aish_json_escape*|trap* )
            return 0
            ;;
        __AISH_ACTIVE_COMMAND_SEQ=* )
            return 0
            ;;
        __AISH_ACTIVE_COMMAND_TEXT=* )
            return 0
            ;;
    esac

    __AISH_AT_PROMPT=0
    __aish_emit_command_started "$BASH_COMMAND"
    return 0
}

__aish_prompt_command() {
    local exit_code=$?
    __aish_last_exit_code=$exit_code
    __aish_rewrite_last_history_entry
    # Call original PROMPT_COMMAND if it exists
    if [[ -n "$__AISH_ORIGINAL_PROMPT_COMMAND" ]]; then
        eval "$__AISH_ORIGINAL_PROMPT_COMMAND"
    fi
    # Keep PS1 empty — prompt rendering is handled by the Python frontend.
    PS1=''
    __AISH_AT_PROMPT=1
    __aish_emit_prompt_ready "$exit_code"
}

# Save original PROMPT_COMMAND before we override it
__AISH_ORIGINAL_PROMPT_COMMAND="$PROMPT_COMMAND"

# Keep the backend prompt silent by default; only enable the custom aish
# prompt when AISH_ENABLE_CUSTOM_PROMPT=1 is set.
PROMPT_COMMAND='__aish_prompt_command'

trap '__aish_on_exit' EXIT
trap '__aish_on_debug' DEBUG

# Disable terminal echo — the frontend (aish) handles all display.
# This prevents the PTY line discipline from echoing user input back
# through master_fd, which would cause commands to appear twice.
stty -echo -echonl 2>/dev/null || true

__aish_emit_session_ready
