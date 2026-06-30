#!/usr/bin/env bash
# Seed built-in skills into the invoking user's ~/.config/aish/skills/.
#
# Skills that already exist in the user directory are preserved as-is, so any
# local edits the user has made win over the packaged version. New skills that
# the package ships but the user does not yet have are copied across.
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
skipped=0
for skill_path in "$SYSTEM_SKILLS_DIR"/*/; do
    [[ -d "$skill_path" ]] || continue
    skill_name="$(basename "$skill_path")"
    target="$USER_SKILLS_DIR/$skill_name"

    if [[ -e "$target" ]]; then
        # Preserve any user edits — never overwrite.
        skipped=$((skipped + 1))
        continue
    fi

    cp -r "$skill_path" "$target"
    # cp -r does not preserve ownership, so the copy is owned by root (the sudo caller);
    # restore ownership to the target user so they can edit their seeded skills.
    chown -R "$TARGET_USER" "$target" 2>/dev/null || true
    seeded=$((seeded + 1))
done

if [[ $seeded -gt 0 ]]; then
    echo "Seeded $seeded skill(s) to $USER_SKILLS_DIR ($skipped already present)."
fi
