#!/usr/bin/env bash
# neenee interrupt/turn notification hook.
#
# Drop-in example: wire it to PermissionRequest / UserQuestion / Turn in your
# config.toml so a long-running task that goes unattended still grabs your
# attention when it blocks on you — or when a turn finishes.
#
#   [[hooks]]
#   event   = "PermissionRequest"   # agent blocked on an approval prompt
#   matcher = "bash"                # optional: only bash approvals
#   command = ".neenee/hooks/notify.sh"
#
#   [[hooks]]
#   event   = "UserQuestion"        # agent blocked on ask_user
#   command = ".neenee/hooks/notify.sh"
#
#   [[hooks]]
#   event   = "Turn"                # a tool round finished
#   command = ".neenee/hooks/notify.sh"
#
# The hook context arrives as JSON on stdin (fields vary by event). We pull a
# human title out of it and fire the best notification the host supports:
#   - notify-send on Linux desktops
#   - osascript on macOS
#   - terminal bell (\x07) + a line on stderr as a universal fallback
#
# Observe-only by design: this script only emits a notification; it never prints
# a deny/decision, so it cannot gate or alter the turn regardless of event.

set -u

# Read the JSON context from stdin (best-effort; tolerate empty).
ctx="$(cat)"

# Derive a short title + body from the event. `jq` is optional — fall back to a
# generic message if it is missing or the field is absent.
pick() {
    local field="$1" fallback="$2"
    if command -v jq >/dev/null 2>&1; then
        printf '%s' "$ctx" | jq -r --arg f "$field" '.[$f] // empty' 2>/dev/null \
            | tr -d '\n' | grep -q . && \
            printf '%s' "$ctx" | jq -r --arg f "$field" '.[$f] // empty' 2>/dev/null \
            || printf '%s' "$fallback"
    else
        printf '%s' "$fallback"
    fi
}

event="$(printf '%s' "$ctx" | jq -r '.event // empty' 2>/dev/null || true)"
case "$event" in
    PermissionRequest)
        title="neenee: needs approval"
        body="$(pick tool "a tool") — $(pick label "approval required")"
        ;;
    UserQuestion)
        title="neenee: asked a question"
        body="$(pick questions "waiting for your answer")"
        ;;
    Turn)
        title="neenee: turn finished"
        body="round $(pick turn "?") complete"
        ;;
    *)
        title="neenee: ${event:-notification}"
        body="$(pick description "")"
        ;;
esac

# Best-available notifier, with graceful fallback. Each path is independent so a
# missing tool degrades cleanly.
if command -v notify-send >/dev/null 2>&1; then
    notify-send -a neenee "$title" "$body" >/dev/null 2>&1 || true
elif command -v osascript >/dev/null 2>&1; then
    # macOS: sanitize for AppleScript string literals.
    t="${title//\"/\\\"}"; b="${body//\"/\\\"}"
    osascript -e "display notification \"$b\" with title \"$t\"" >/dev/null 2>&1 || true
fi

# Universal fallback: terminal bell + stderr line. The bell works in almost
# every terminal and pairs well with a long-running task in a background pane.
printf '\a' >&2
printf '[neenee] %s — %s\n' "$title" "$body" >&2

exit 0
