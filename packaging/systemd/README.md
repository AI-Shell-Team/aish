# AI Shell Sandbox Systemd Units

The Rust package uses the main `aish` binary for both hidden sandbox runtime
entry points:

- `aish --sandbox-daemon` runs the privileged sandbox daemon.
- `aish --sandbox-worker` is spawned internally by the daemon and is hidden from
  normal CLI help.

`aish-sandbox.socket` owns `/run/aish/sandbox.sock` with `SocketMode=0666` so
regular users can request sandbox simulations. The daemon authenticates callers
with Unix peer credentials (`SO_PEERCRED`) and decides the payload identity from
that metadata; the socket does not trust user-supplied uid/gid fields.

The service runs as root because the worker needs mount namespace, overlayfs,
bind mount, and remount operations. Socket activation is preferred: systemd owns
the socket and passes fd 3 to `aish --sandbox-daemon` when the first client
connects.