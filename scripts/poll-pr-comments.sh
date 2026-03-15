#!/usr/bin/env bash
# Poll a PR for new review comments and display them.
#
# Usage:
#   ./scripts/poll-pr-comments.sh <PR_NUMBER>              # one-shot
#   ./scripts/poll-pr-comments.sh <PR_NUMBER> --watch 300   # poll every 5 min
#
# Designed to run after posting a PR. Checks for new unresolved
# review comments and presents them for action.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

PR_NUMBER="${1:?Usage: $0 <PR_NUMBER> [--watch <seconds>]}"
WATCH_MODE=false
POLL_INTERVAL=300  # default 5 minutes

shift
while [[ $# -gt 0 ]]; do
    case "$1" in
        --watch)
            WATCH_MODE=true
            if [[ -n "$2" && "$2" != --* ]]; then
                POLL_INTERVAL="$2"
                shift
            fi
            ;;
    esac
    shift
done

SEEN_FILE=$(mktemp /tmp/oq-pr-comments-XXXXXX)
trap "rm -f $SEEN_FILE" EXIT
touch "$SEEN_FILE"

fetch_and_display() {
    local new_count=0

    # Fetch review comments (file-level)
    local comments
    comments=$(gh api "repos/{owner}/{repo}/pulls/$PR_NUMBER/comments" \
        --jq '.[] | {id: .id, user: .user.login, path: .path, line: .line, body: .body, created: .created_at}' 2>/dev/null) || return

    while IFS= read -r comment; do
        [ -z "$comment" ] && continue

        local id
        id=$(echo "$comment" | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])" 2>/dev/null) || continue

        # Skip if already seen
        if grep -q "^${id}$" "$SEEN_FILE" 2>/dev/null; then
            continue
        fi

        echo "$id" >> "$SEEN_FILE"
        new_count=$((new_count + 1))

        local user path line body
        user=$(echo "$comment" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d['user'])")
        path=$(echo "$comment" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d['path'])")
        line=$(echo "$comment" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('line') or '?')")
        body=$(echo "$comment" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d['body'])")

        echo ""
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "  NEW COMMENT on $path:$line"
        echo "  By: $user"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "$body"
        echo ""
    done < <(echo "$comments" | python3 -c "
import json, sys
raw = sys.stdin.read().strip()
if not raw:
    sys.exit(0)
# Handle newline-delimited JSON objects
for line in raw.split('\n'):
    line = line.strip()
    if line:
        try:
            json.loads(line)
            print(line)
        except:
            pass
" 2>/dev/null)

    # Also check issue-level comments
    local issue_comments
    issue_comments=$(gh api "repos/{owner}/{repo}/issues/$PR_NUMBER/comments" \
        --jq '.[] | {id: .id, user: .user.login, body: .body, created: .created_at}' 2>/dev/null) || return

    while IFS= read -r comment; do
        [ -z "$comment" ] && continue

        local id
        id=$(echo "$comment" | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])" 2>/dev/null) || continue

        if grep -q "^${id}$" "$SEEN_FILE" 2>/dev/null; then
            continue
        fi

        echo "$id" >> "$SEEN_FILE"
        new_count=$((new_count + 1))

        local user body
        user=$(echo "$comment" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d['user'])")
        body=$(echo "$comment" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d['body'])")

        echo ""
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "  NEW COMMENT (general)"
        echo "  By: $user"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "$body"
        echo ""
    done < <(echo "$issue_comments" | python3 -c "
import json, sys
raw = sys.stdin.read().strip()
if not raw:
    sys.exit(0)
for line in raw.split('\n'):
    line = line.strip()
    if line:
        try:
            json.loads(line)
            print(line)
        except:
            pass
" 2>/dev/null)

    if [ "$new_count" -eq 0 ]; then
        echo "[$(date +%H:%M:%S)] No new comments on PR #$PR_NUMBER"
    else
        echo ""
        echo "[$(date +%H:%M:%S)] Found $new_count new comment(s) on PR #$PR_NUMBER"
    fi
}

# Main
echo "============================================"
echo "  OpenQuant PR Comment Poller"
echo "  PR #$PR_NUMBER"
if [ "$WATCH_MODE" = true ]; then
    echo "  Polling every ${POLL_INTERVAL}s (Ctrl+C to stop)"
fi
echo "============================================"

if [ "$WATCH_MODE" = true ]; then
    while true; do
        fetch_and_display
        sleep "$POLL_INTERVAL"
    done
else
    fetch_and_display
fi
