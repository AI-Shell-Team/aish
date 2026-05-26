# aish bash rc wrapper
# This file is used as rcfile for interactive bash

# Disable readline — the frontend (aish) handles all display and editing.
# Without readline, bash uses the simple line reader which does not emit
# extra newlines on Enter, preventing spurious blank lines in PTY output.
set +o emacs
set +o vi

# Enable job control so Ctrl+Z suspends foreground jobs
set -m

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

    # Re-enable echo for interactive session commands (ssh, telnet, etc.)
    # so that the remote PTY inherits normal terminal settings.  The
    # local PTY has -echo set by this wrapper, and SSH propagates these
    # settings to the remote server, which can confuse the remote shell's
    # readline.
    local __aish_cmd_name="${BASH_COMMAND%% *}"
    __aish_cmd_name="${__aish_cmd_name##*/}"
    case "$__aish_cmd_name" in
        ssh|telnet|mosh|nc|netcat|ftp|sftp)
            stty echo 2>/dev/null || true
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
    # Re-disable echo in case a session command (ssh, telnet) re-enabled
    # it via the DEBUG trap.  The frontend handles all display itself.
    stty -echo -echonl 2>/dev/null || true
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

# ---------------------------------------------------------------------------
# Helper: append '/' to entries that are directories.
# Reads from stdin, one candidate per line.
# ---------------------------------------------------------------------------
__aish_mark_dirs() {
    local entry
    while IFS= read -r entry; do
        [[ -z "$entry" ]] && continue
        if [[ "$entry" == */ ]]; then
            printf '%s\n' "$entry"
        elif [[ -d "$entry" ]]; then
            printf '%s/\n' "$entry"
        else
            printf '%s\n' "$entry"
        fi
    done
}

# ---------------------------------------------------------------------------
# Completion query function for aish frontend.
# Usage: __aish_query_completions "command line" cursor_position
# Outputs one completion candidate per line.
# ---------------------------------------------------------------------------
__aish_query_completions() {
    local cmd_line="${1:-}"
    local cursor="${2:-0}"
    local -a words=()
    local word=""
    local i ch

    # Parse command line into words (simple whitespace split).
    for (( i=0; i<${#cmd_line}; i++ )); do
        ch="${cmd_line:$i:1}"
        if [[ "$ch" == " " || "$ch" == $'\t' ]]; then
            if [[ -n "$word" ]]; then
                words+=("$word")
                word=""
            fi
        else
            word+="$ch"
        fi
    done
    if [[ -n "$word" ]]; then
        words+=("$word")
    fi

    # If cursor is after a trailing space, append an empty word so that
    # argument completion (not command-name completion) is triggered.
    if (( cursor > 0 )) && [[ "${cmd_line:$((cursor-1)):1}" == " " ]]; then
        words+=("")
    fi

    # Determine COMP_CWORD: index of the word under the cursor.
    local cword=0
    local pos=0
    for (( i=0; i<${#words[@]}; i++ )); do
        pos=$(( pos + ${#words[$i]} + 1 ))
        if (( pos > cursor )); then
            cword=$i
            break
        fi
        cword=$i
    done

    local cmd="${words[0]:-}"
    local cur="${words[$cword]:-}"
    local prev="${words[$((cword-1))]:-}"

    # --- Empty line: list all commands ---
    if [[ ${#words[@]} -eq 0 ]]; then
        compgen -c 2>/dev/null
        return 0
    fi

    # --- First word (command name) ---
    if (( cword == 0 )); then
        compgen -c -- "$cur" 2>/dev/null
        return 0
    fi

    # --- Argument completion: use bash-completion if available ---
    # Attempt to load the completion function for this command.
    _completion_loader "$cmd" 2>/dev/null || true

    local comp_spec func_name=""
    comp_spec=$(complete -p "$cmd" 2>/dev/null) || true
    if [[ "$comp_spec" =~ -F[[:space:]]+([^[:space:]]+) ]]; then
        func_name="${BASH_REMATCH[1]}"
    fi

    if [[ -n "$func_name" ]]; then
        # Call the registered completion function with the standard
        # bash signature: func COMMAND CURRENT_WORD PREVIOUS_WORD
        COMPREPLY=()
        COMP_WORDS=("${words[@]}")
        COMP_CWORD=$cword
        COMP_LINE="$cmd_line"
        COMP_POINT=$cursor
        "$func_name" "$cmd" "$cur" "$prev" 2>/dev/null || true

        # Deduplicate and output, marking directories with /.
        if [[ ${#COMPREPLY[@]} -gt 0 ]]; then
            printf '%s\n' "${COMPREPLY[@]}" 2>/dev/null | __aish_mark_dirs
            return 0
        fi
    fi

    # --- Fallback: file/directory completion ---
    compgen -f -- "$cur" 2>/dev/null | __aish_mark_dirs
}

__aish_emit_session_ready
