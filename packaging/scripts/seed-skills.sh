#!/usr/bin/env bash
# Seed packaged skills into the invoking user's ~/.config/aish/skills/.
#
# Source is the skills tree shipped with the installer (bundle payload or
# repo `skills/`). Skills are never installed under /usr or /usr/local;
# this script writes only into the target user's config directory.
#
# Every skill in the source directory is copied into the user directory,
# replacing any existing tree of the same name. Packaged skills are
# product-owned; upgrades must land on disk so runtime behavior matches
# the installed aish version. User-authored skills (names not in the
# package) are left untouched.
#
# When invoked as root (e.g. sudo ./install.sh), filesystem operations in
# the target home are run as TARGET_USER so ownership is correct without
# chown, and symlink races in the user-controlled tree cannot be abused
# for root-owned writes.
#
# Usage: seed-skills.sh <source_skills_dir>
# Must be invoked with a readable source tree (e.g. bundle rootfs payload
# or the repo's skills/ directory).

set -euo pipefail

SOURCE_SKILLS_DIR="${1:-}"

if [[ -z "$SOURCE_SKILLS_DIR" ]] || [[ ! -d "$SOURCE_SKILLS_DIR" ]]; then
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

# Drop to TARGET_USER for all writes under their home when we are root.
run_as_target() {
    if [[ "${EUID}" -eq 0 ]] && [[ "$TARGET_USER" != "root" ]]; then
        if command -v runuser >/dev/null 2>&1; then
            runuser -u "$TARGET_USER" -- "$@"
        else
            sudo -u "$TARGET_USER" -- "$@"
        fi
    else
        "$@"
    fi
}

CONFIG_AISH_DIR="$TARGET_HOME/.config/aish"
USER_SKILLS_DIR="$CONFIG_AISH_DIR/skills"

# Repair leftover root-owned config from older installers (real dirs only;
# never follow a symlink that could point outside the user's tree).
if [[ "${EUID}" -eq 0 ]] && [[ "$TARGET_USER" != "root" ]]; then
    if [[ -d "$CONFIG_AISH_DIR" ]] && [[ ! -L "$CONFIG_AISH_DIR" ]]; then
        if [[ "$(stat -c '%u' "$CONFIG_AISH_DIR" 2>/dev/null || echo)" == "0" ]]; then
            chown -R "$TARGET_USER:" "$CONFIG_AISH_DIR"
        fi
    fi
fi

run_as_target mkdir -p "$USER_SKILLS_DIR"

seeded=0
for skill_path in "$SOURCE_SKILLS_DIR"/*/; do
    [[ -d "$skill_path" ]] || continue
    skill_name="$(basename "$skill_path")"
    target="$USER_SKILLS_DIR/$skill_name"

    # Replace any previous copy so package upgrades refresh product skills.
    if [[ -e "$target" ]]; then
        run_as_target rm -rf "$target"
    fi

    run_as_target cp -r "$skill_path" "$target"
    seeded=$((seeded + 1))
done

if [[ $seeded -gt 0 ]]; then
    echo "Seeded $seeded packaged skill(s) to $USER_SKILLS_DIR (overwrote same-name trees)."
fi
