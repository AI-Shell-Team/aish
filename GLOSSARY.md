# AISH

The interactive shell the user types into, and the state that session holds.

## Language

**Shell session**:
The long-lived interactive shell the user types into. Source of truth for shell state.
_Avoid_: PTY, persistent bash, backend

**Shell state**:
The shell session's current directory, exported environment, and directory stack.
_Avoid_: builtin state, prompt state, a second state owned by AISH
