#!/bin/sh
# smoke_luau.sh - end-to-end smoke test for tern-yazi Luau plugin in an isolated session.
set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
TERN=${TERN:-tern}
IT=$(mktemp -d /tmp/tern-yazi-smoke.XXXXXX)
export TERN_CONFIG_DIR="$IT/config"

cleanup() {
	"$TERN" plugin unlink tern-yazi >/dev/null 2>&1 || true
	rm -f "${XDG_RUNTIME_DIR:-/tmp}/tern-yazi/state-9999.json"
	rm -rf "$IT"
}
trap cleanup EXIT

say() { printf '\033[1m== %s\033[0m\n' "$*"; }
fail() { echo "SMOKE FAIL: $*" >&2; exit 1; }

say "1. Set up isolated environment"
mkdir -p "$TERN_CONFIG_DIR"

say "2. Link tern-yazi plugin"
"$TERN" plugin link "$REPO" >/dev/null

say "3. Verify plugin loads and reports ready"
LIST_OUT=$("$TERN" plugin list)
echo "$LIST_OUT"
if ! echo "$LIST_OUT" | grep -q "tern-yazi.*ready"; then
	fail "tern-yazi plugin did not report ready in plugin list"
fi

say "4. Simulate Yazi state file emission"
SDIR="${XDG_RUNTIME_DIR:-/tmp}/tern-yazi"
mkdir -p "$SDIR"
cat << 'EOF' > "$SDIR/state-9999.json"
{
  "seq": 100,
  "client_id": 9999,
  "cwd": "/tmp/smoke-test",
  "files": ["file1.txt", "file2.lua", "dirA"],
  "selected": 2,
  "hovered": {
    "url": "/tmp/smoke-test/file1.txt",
    "dir": false,
    "size": 1024,
    "mtime": 1791511000
  },
  "tasks": {
    "total": 3,
    "succ": 1,
    "fail": 0,
    "running": 2
  }
}
EOF

say "5. Reload plugin with active state file"
"$TERN" plugin reload >/dev/null
LIST_RELOAD=$("$TERN" plugin list)
if ! echo "$LIST_RELOAD" | grep -q "tern-yazi.*ready"; then
	fail "tern-yazi failed to reload after state file was written"
fi

say "6. Unlink plugin"
"$TERN" plugin unlink tern-yazi >/dev/null

say "All Luau plugin smoke checks passed successfully!"
