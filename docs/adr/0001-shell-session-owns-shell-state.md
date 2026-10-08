# The shell session owns shell state

AISH used to update the current directory, exported environment, and directory stack itself, then send the same line to the shell session. The two copies diverged as soon as the line contained shell syntax or quoting. The shell session is the source of truth. After a line that can change shell state, AISH replaces its copy from the session, including when the line's exit code is not 0. A failed read leaves the previous copy in place.
