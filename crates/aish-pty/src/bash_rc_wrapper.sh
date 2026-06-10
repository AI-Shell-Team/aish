# aish bash rc wrapper
# This file is used as rcfile for interactive bash

# Disable readline — the frontend (aish) handles all display and editing.
# Without readline, bash uses the simple line reader which does not emit
# extra newlines on Enter, preventing spurious blank lines in PTY output.
# Tab forwarding temporarily re-enables readline via `set -o emacs` in the PTY layer.
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
        __aish_prompt_command*|__aish_on_debug*|__aish_emit_*|__aish_json_escape*|__aish_complete*|_aish_*|trap* )
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
# Tab completion for aish frontend (bash is the single source of truth).
# Usage: __aish_complete REQUEST_ID "command line" CURSOR_POINT
# Emits completion_result JSON on the control pipe.
# ---------------------------------------------------------------------------

__AISH_COMP_DISPLAY=()
__AISH_COMP_REPLACEMENT=()

_aish_reset_candidates() {
    __AISH_COMP_DISPLAY=()
    __AISH_COMP_REPLACEMENT=()
}

_aish_add_candidate() {
    local raw="$1"
    local cur="${2:-}"
    local as_command="${3:-0}"
    local display="" replacement="" base="" existing

    [[ -z "$raw" ]] && return 0

    if [[ "$raw" == */ ]]; then
        replacement="$raw"
    elif [[ -d "$raw" ]]; then
        replacement="${raw}/"
    else
        replacement="$raw"
        [[ "$replacement" != *' ' ]] && replacement+=" "
    fi

    if [[ -n "$cur" && "$replacement" == "$cur" ]]; then
        return 0
    fi
    for existing in "${__AISH_COMP_REPLACEMENT[@]}"; do
        if [[ "$existing" == "$replacement" ]]; then
            return 0
        fi
    done

    if [[ "$as_command" == 1 ]]; then
        display="${raw% }"
    else
        base="${raw%/}"
        base="${base##*/}"
        [[ -z "$base" ]] && base="${raw%/}"
        if [[ "$raw" == */ || -d "$raw" ]]; then
            display="${base}/"
        else
            display="$base"
        fi
    fi

    __AISH_COMP_DISPLAY+=("$display")
    __AISH_COMP_REPLACEMENT+=("$replacement")
}

__aish_emit_completion_result() {
    local request_id="$1"
    local word_start="$2"
    local parts=() i payload candidates_json

    for (( i=0; i<${#__AISH_COMP_DISPLAY[@]}; i++ )); do
        parts+=("{\"display\":\"$(__aish_json_escape "${__AISH_COMP_DISPLAY[$i]}")\",\"replacement\":\"$(__aish_json_escape "${__AISH_COMP_REPLACEMENT[$i]}")\"}")
    done

    if ((${#parts[@]} > 0)); then
        local IFS=,
        candidates_json="[${parts[*]}]"
    else
        candidates_json="[]"
    fi

    printf -v payload \
        '{"version":%s,"type":"completion_result","request_id":%s,"word_start":%s,"candidates":%s}' \
        "$__AISH_PROTOCOL_VERSION" "$request_id" "$word_start" "$candidates_json"
    __aish_emit_control_line "$payload"
}

_aish_is_path_like() {
    local token="$1"
    [[ -z "$token" ]] && return 1
    [[ "$token" == /* || "$token" == ./* || "$token" == ../* || "$token" == ~* || "$token" == */ ]] && return 0
    return 1
}

_aish_parse_line() {
    local comp_line="$1"
    local cursor="$2"
    local -n _pl_words="$3"
    local -n _pl_starts="$4"

    _pl_words=()
    _pl_starts=()
    local i=0 word="" start=0 ch=""

    for (( i=0; i<${#comp_line}; i++ )); do
        ch="${comp_line:$i:1}"
        if [[ "$ch" == " " || "$ch" == $'\t' ]]; then
            if [[ -n "$word" ]]; then
                _pl_words+=("$word")
                _pl_starts+=("$start")
                word=""
            fi
        else
            if [[ -z "$word" ]]; then
                start=$i
            fi
            word+="$ch"
        fi
    done
    if [[ -n "$word" ]]; then
        _pl_words+=("$word")
        _pl_starts+=("$start")
    fi

    if (( cursor > 0 )) && [[ "${comp_line:$((cursor-1)):1}" == " " ]]; then
        _pl_words+=("")
        _pl_starts+=("$cursor")
    fi
}

# Maximum candidates emitted per completion (large dirs like /usr/bin/).
__AISH_COMPLETION_LIMIT=100

# readline sorts matches before display; COMPREPLY/compgen keep readdir order.
_aish_sort_compreply() {
    ((${#COMPREPLY[@]} < 2)) && return 0
    mapfile -t COMPREPLY < <(
        printf '%s\n' "${COMPREPLY[@]}" \
            | LC_COLLATE="${LC_COLLATE:-${LC_ALL:-C}}" sort
    )
}

__aish_load_bash_completion() {
    [[ -n "${__AISH_BASH_COMP_LOADED:-}" ]] && return 0
    if [[ -f /usr/share/bash-completion/bash_completion ]]; then
        # shellcheck source=/dev/null
        source /usr/share/bash-completion/bash_completion
    elif [[ -f /etc/bash_completion ]]; then
        # shellcheck source=/dev/null
        source /etc/bash_completion
    fi
    __AISH_BASH_COMP_LOADED=1
}

_aish_resolve_compreply() {
    local item="$1" cur="$2" dir=""

    [[ -z "$item" ]] && return 0
    if [[ "$item" == /* || "$item" == ~* || "$item" == ./* || "$item" == ../* ]]; then
        printf '%s' "$item"
        return 0
    fi

    if [[ "$cur" == */ ]]; then
        dir="$cur"
    elif [[ "$cur" == */* ]]; then
        dir="${cur%/*}/"
    elif [[ -n "$cur" && -d "$cur" ]]; then
        dir="${cur}/"
    else
        dir="./"
    fi
    printf '%s' "${dir}${item}"
}

_aish_set_comp_context() {
    local comp_line="$1"
    local cursor="$2"
    local -n _sc_words="$3"
    local -n _sc_starts="$4"
    local -n _sc_cword="$5"

    _aish_parse_line "$comp_line" "$cursor" _sc_words _sc_starts

    COMP_LINE="$comp_line"
    COMP_POINT="$cursor"
    COMP_WORDS=("${_sc_words[@]}")

    local pos=0 i=0
    _sc_cword=0
    for (( i=0; i<${#_sc_words[@]}; i++ )); do
        pos=$(( pos + ${#_sc_words[i]} + 1 ))
        if (( pos > cursor )); then
            _sc_cword=$i
            break
        fi
        _sc_cword=$i
    done
    COMP_CWORD=$_sc_cword
}

_aish_invoke_native_completion() {
    local cmd="$1"
    local cword="$2"
    local cur="$3"
    local comp_spec="" func_name=""

    COMPREPLY=()

    if (( cword == 0 )); then
        local entry
        if _aish_is_path_like "$cur"; then
            while IFS= read -r entry; do
                [[ -z "$entry" ]] && continue
                COMPREPLY+=("$entry")
            done < <(compgen -f -- "$cur" 2>/dev/null | __aish_mark_dirs)
        else
            while IFS= read -r entry; do
                [[ -z "$entry" ]] && continue
                COMPREPLY+=("$entry")
            done < <(compgen -c -- "$cur" 2>/dev/null)
        fi
        return 0
    fi

    if declare -F _comp_load &>/dev/null; then
        _comp_load -- "$cmd" 2>/dev/null || true
    fi
    if ! complete -p "$cmd" &>/dev/null && declare -F _completion_loader &>/dev/null; then
        _completion_loader "$cmd" 2>/dev/null || true
    fi
    comp_spec=$(complete -p "$cmd" 2>/dev/null || true)

    if [[ "$comp_spec" =~ -F[[:space:]]+([^[:space:]]+) ]]; then
        func_name="${BASH_REMATCH[1]}"
        "$func_name" "$cmd" 2>/dev/null || true
    elif [[ "$comp_spec" =~ -C[[:space:]]+([^[:space:]]+) ]]; then
        func_name="${BASH_REMATCH[1]}"
        COMPREPLY=( $(compgen -W "$(eval "$func_name")" -- "$cur") )
    elif declare -F _comp_compgen_filedir &>/dev/null; then
        local wcur="" wprev="" wwords=() wcword=0
        _comp_get_words -n "<>&" wcur wprev wwords wcword 2>/dev/null || true
        _comp_compgen_filedir 2>/dev/null || true
    else
        local entry
        while IFS= read -r entry; do
            [[ -z "$entry" ]] && continue
            COMPREPLY+=("$entry")
        done < <(compgen -f -- "$cur" 2>/dev/null | __aish_mark_dirs)
    fi
}

_aish_compreply_to_candidates() {
    local cur="$1"
    local item resolved count=0

    for item in "${COMPREPLY[@]}"; do
        if _aish_is_path_like "$cur" || _aish_is_path_like "$item" || \
           [[ "$item" == */* || "$item" == /* ]]; then
            resolved=$(_aish_resolve_compreply "$item" "$cur")
            _aish_add_candidate "$resolved" "$cur"
        else
            _aish_add_candidate "$item" "$cur" 1
        fi
        count=$(( count + 1 ))
        if (( count >= __AISH_COMPLETION_LIMIT )); then
            break
        fi
    done
}

__aish_complete() {
    local request_id="${1:-0}"
    local comp_line="${2:-}"
    local cursor="${3:-0}"
    local -a words=()
    local -a word_starts=()
    local cword=0 cmd="" cur="" word_start=0

    _aish_reset_candidates
    __aish_load_bash_completion

    _aish_set_comp_context "$comp_line" "$cursor" words word_starts cword

    if ((${#words[@]} == 0)); then
        __aish_emit_completion_result "$request_id" 0
        return 0
    fi

    word_start=${word_starts[$cword]:-0}
    cmd="${words[0]:-}"
    cur="${words[$cword]:-}"

    _aish_invoke_native_completion "$cmd" "$cword" "$cur"
    _aish_sort_compreply
    _aish_compreply_to_candidates "$cur"

    __aish_emit_completion_result "$request_id" "$word_start"
}

__aish_emit_session_ready
