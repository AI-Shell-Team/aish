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

CONFIG_AISH_DIR="$TARGET_HOME/.config/aish"
USER_SKILLS_DIR="$CONFIG_AISH_DIR/skills"
mkdir -p "$USER_SKILLS_DIR"

seeded=0
for skill_path in "$SOURCE_SKILLS_DIR"/*/; do
    [[ -d "$skill_path" ]] || continue
    skill_name="$(basename "$skill_path")"
    target="$USER_SKILLS_DIR/$skill_name"

    # Replace any previous copy so package upgrades refresh product skills.
    if [[ -e "$target" ]]; then
        rm -rf "$target"
    fi

    cp -r "$skill_path" "$target"
    seeded=$((seeded + 1))
done

# mkdir/cp under sudo leave the tree root-owned. The user must own
# ~/.config/aish (not just skill leaves) so first-run can create config.yaml.
# Also repair ~/.config if we just created it as root.
if [[ -d "$TARGET_HOME/.config" ]]; then
    cfg_owner="$(stat -c '%u' "$TARGET_HOME/.config" 2>/dev/null || true)"
    if [[ "$cfg_owner" == "0" ]]; then
        chown "$TARGET_USER:" "$TARGET_HOME/.config" 2>/dev/null || true
    fi
fi
chown -R "$TARGET_USER:" "$CONFIG_AISH_DIR" 2>/dev/null || true

if [[ $seeded -gt 0 ]]; then
    echo "Seeded $seeded packaged skill(s) to $USER_SKILLS_DIR (overwrote same-name trees)."
fi
