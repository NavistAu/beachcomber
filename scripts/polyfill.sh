#!/bin/sh
# beachcomber polyfill — wraps `comb` with transparent fallback to native tools.
#
# If comb is installed, uses it. If not, falls back to the native tool for
# known keys. Enables integrations to adopt beachcomber without requiring it
# as a hard dependency.
#
# Install: source this file from your shell rc, or curl it down:
#   curl -fsSL https://beachcomber.sh/scripts/polyfill.sh | source /dev/stdin
#
# Usage (same as real comb):
#   comb g git.branch .
#   comb g hostname.name
#   comb g load.one
#
# https://beachcomber.sh

if command -v comb >/dev/null 2>&1; then
    # comb is installed — no polyfill needed.
    return 0 2>/dev/null || exit 0
fi

comb() {
    # Parse command: support "g", "get", "g.p" (plain text, the default)
    _cmd="$1"
    shift

    # Extract format suffix if present.
    case "$_cmd" in
        *.p|*.text)
            _cmd="${_cmd%%.*}"
            _fmt="text"
            ;;
        *.*)
            # Unsupported format suffix in polyfill — bail.
            echo "comb polyfill: format not supported without beachcomber" >&2
            return 1
            ;;
        *)
            _fmt="text"
            ;;
    esac

    case "$_cmd" in
        g|get)
            _key="$1"
            _path="$2"
            _comb_polyfill_get "$_key" "$_path"
            ;;
        *)
            echo "comb polyfill: command '$_cmd' requires beachcomber" >&2
            echo "Install: https://beachcomber.sh" >&2
            return 1
            ;;
    esac
}

_comb_polyfill_get() {
    _key="$1"
    _path="$2"

    case "$_key" in
        git.branch)
            git -C "${_path:-.}" rev-parse --abbrev-ref HEAD 2>/dev/null
            ;;
        git.dirty)
            if git -C "${_path:-.}" diff --quiet HEAD 2>/dev/null; then
                echo "false"
            else
                echo "true"
            fi
            ;;
        git.ahead)
            git -C "${_path:-.}" rev-list --count '@{upstream}..HEAD' 2>/dev/null || echo "0"
            ;;
        git.behind)
            git -C "${_path:-.}" rev-list --count 'HEAD..@{upstream}' 2>/dev/null || echo "0"
            ;;
        git.stash_count)
            git -C "${_path:-.}" stash list 2>/dev/null | wc -l | tr -d ' '
            ;;
        git.commit_summary)
            git -C "${_path:-.}" log -1 --format='%s' 2>/dev/null
            ;;
        hostname.name)
            hostname 2>/dev/null
            ;;
        hostname.short)
            hostname -s 2>/dev/null || hostname 2>/dev/null | cut -d. -f1
            ;;
        user.name)
            whoami 2>/dev/null
            ;;
        load.one)
            uptime 2>/dev/null | sed 's/.*load average[s]*: //' | cut -d, -f1 | tr -d ' '
            ;;
        load.five)
            uptime 2>/dev/null | sed 's/.*load average[s]*: //' | cut -d, -f2 | tr -d ' '
            ;;
        load.fifteen)
            uptime 2>/dev/null | sed 's/.*load average[s]*: //' | cut -d, -f3 | tr -d ' '
            ;;
        battery.percent)
            if [ "$(uname)" = "Darwin" ]; then
                pmset -g batt 2>/dev/null | grep -o '[0-9]*%' | tr -d '%'
            elif [ -f /sys/class/power_supply/BAT0/capacity ]; then
                cat /sys/class/power_supply/BAT0/capacity
            else
                echo ""
            fi
            ;;
        battery.charging)
            if [ "$(uname)" = "Darwin" ]; then
                pmset -g batt 2>/dev/null | grep -q 'AC Power' && echo "true" || echo "false"
            elif [ -f /sys/class/power_supply/BAT0/status ]; then
                grep -qi 'charging' /sys/class/power_supply/BAT0/status && echo "true" || echo "false"
            else
                echo ""
            fi
            ;;
        *)
            echo "comb polyfill: unknown key '$_key'" >&2
            return 1
            ;;
    esac
}
