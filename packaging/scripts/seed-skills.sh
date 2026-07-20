#!/usr/bin/env bash
# Seed built-in skills into the invoking user's ~/.config/aish/skills/.
#
# Every skill shipped under the system skills directory is copied into the
# user directory, replacing any existing tree of the same name. Packaged
# skills are product-owned; upgrades must land on disk so runtime behavior
# matches the installed aish version. User-authored skills (names not in
# the package) are left untouched.
#
# Usage: seed-skills.sh <system_skills_dir>
# Must be invoked AFTER the system-level skills directory has been populated
# (e.g. by `make install` or install-bundle.sh).

set -euo pipefail

SYSTEM_SKILLS_DIR="${1:-/usr/local/share/aish/skills}"

if [[ ! -d "$SYSTEM_SKILLS_DIR" ]]; then
    exit 0
fi

# Resolve the target user's home directory.
#   - Under sudo (SUDO_USER set and not root): use that user's passwd entry.
#   - Otherwise: $HOME if it's not /root.
#   - If neither applies (bare root shell, no SUDO_USER), skip seeding entirely.
if [[ -n "${SUDO_USER:-}" ]] && [[ "${SUDO_USER}" != "root" ]]; then
    TARGET_HOME="$(getent passwd "$SUDO_USER" | cut -d: -f6)"
    TARGET_USER="$SUDO_USER"
elif [[ -n "${HOME:-}" ]] && [[ "$HOME" != "/root" ]]; then
    TARGET_HOME="$HOME"
    TARGET_USER="$(id -un)"
else
    exit 0
fi

if [[ -z "$TARGET_HOME" ]] || [[ ! -d "$TARGET_HOME" ]]; then
    exit 0
fi

USER_SKILLS_DIR="$TARGET_HOME/.config/aish/skills"
mkdir -p "$USER_SKILLS_DIR"

seeded=0
for skill_path in "$SYSTEM_SKILLS_DIR"/*/; do
    [[ -d "$skill_path" ]] || continue
    skill_name="$(basename "$skill_path")"
    target="$USER_SKILLS_DIR/$skill_name"

    # Replace any previous copy so package upgrades refresh product skills.
    if [[ -e "$target" ]]; then
        rm -rf "$target"
    fi

    cp -r "$skill_path" "$target"
    # cp -r does not preserve ownership, so the copy is owned by root (the sudo caller);
    # restore ownership (user and group) so the target user can edit their seeded skills.
    chown -R "$TARGET_USER:" "$target" 2>/dev/null || true
    seeded=$((seeded + 1))
done

if [[ $seeded -gt 0 ]]; then
    echo "Seeded $seeded packaged skill(s) to $USER_SKILLS_DIR (overwrote same-name trees)."
fi
