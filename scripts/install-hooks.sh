#!/usr/bin/env bash
# Install git hooks for OpenQuant.
#
# Usage: ./scripts/install-hooks.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "Installing git hooks..."

# Pre-push hook
ln -sf "../../scripts/pre-push-benchmark.sh" "$PROJECT_ROOT/.git/hooks/pre-push"
chmod +x "$PROJECT_ROOT/scripts/pre-push-benchmark.sh"

echo "  pre-push -> scripts/pre-push-benchmark.sh"
echo ""
echo "Done. Hooks installed."
echo ""
echo "To uninstall: rm .git/hooks/pre-push"
