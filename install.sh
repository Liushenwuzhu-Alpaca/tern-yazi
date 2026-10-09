#!/bin/sh
# tern-yazi installer v1
set -efu
umask 077

usage() {
    printf '%s\n' 'Usage: sh install.sh [--prefix DIR] [--real ORIGINAL_YAZI] [--uninstall]'
}
fail() { printf 'tern-yazi: %s\n' "$*" >&2; exit 1; }

prefix=${HOME:?HOME is required}/.local
real=
uninstall=0
while [ "$#" -gt 0 ]; do
    case $1 in
        --prefix) [ "$#" -ge 2 ] || fail '--prefix needs a directory'; prefix=$2; shift 2 ;;
        --real) [ "$#" -ge 2 ] || fail '--real needs the original Yazi executable'; real=$2; shift 2 ;;
        --uninstall) uninstall=1; shift ;;
        --help|-h) usage; exit 0 ;;
        *) usage >&2; fail "unknown option: $1" ;;
    esac
done
case $prefix in /*) ;; *) prefix=$PWD/$prefix ;; esac
bin=$prefix/bin
lib=$prefix/libexec
wrapper=$bin/yazi
helper=$lib/tern-yazi-launch
original=$lib/tern-yazi-launch.real
manifest=$lib/tern-yazi-launch.owned
marker='# tern-yazi owned launcher wrapper v1'

hash() {
    sum=$(sha256sum < "$1") || return 1
    printf '%s\n' "${sum%% *}"
}
owned_marker() {
    [ -f "$1" ] && [ ! -L "$1" ] || return 1
    { IFS= read -r first && IFS= read -r second; } < "$1" || return 1
    [ "$second" = "$marker" ]
}
read_manifest() {
    [ -f "$manifest" ] && [ ! -L "$manifest" ] || fail 'ownership manifest missing or unsafe; refusing to overwrite/remove files'
    {
        IFS= read -r tag && IFS= read -r wrapper_hash && IFS= read -r helper_hash && IFS= read -r original_hash
    } < "$manifest" || fail 'incomplete ownership manifest'
    [ "$tag" = 'tern-yazi owned installation v1' ] || fail 'unrecognized ownership manifest'
    for digest in "$wrapper_hash" "$helper_hash" "$original_hash"; do
        case $digest in *[!0-9a-f]*|'') fail 'invalid ownership digest' ;; esac
        [ "${#digest}" -eq 64 ] || fail 'invalid ownership digest length'
    done
}
check_file() {
    file=$1
    expected=$2
    if [ -e "$file" ] || [ -L "$file" ]; then
        [ -f "$file" ] && [ ! -L "$file" ] || fail "refusing unsafe owned-file path: $file"
        [ "$(hash "$file")" = "$expected" ] || fail "file changed since installation; refusing to overwrite/remove: $file"
    fi
}

if [ "$uninstall" -eq 1 ]; then
    if [ ! -e "$manifest" ] && [ ! -L "$manifest" ] && [ ! -e "$wrapper" ] && [ ! -L "$wrapper" ] && [ ! -e "$helper" ] && [ ! -L "$helper" ] && [ ! -e "$original" ] && [ ! -L "$original" ]; then
        printf '%s\n' 'tern-yazi launcher is not installed at this prefix.'
        exit 0
    fi
    read_manifest
    check_file "$wrapper" "$wrapper_hash"
    check_file "$helper" "$helper_hash"
    check_file "$original" "$original_hash"
    # The real Yazi path is data, never an uninstall target.
    rm -f -- "$wrapper" "$helper" "$original" "$manifest"
    printf 'Removed the owned tern-yazi launcher from %s; original Yazi is unchanged.\n' "$prefix"
    exit 0
fi

for dir in "$prefix" "$bin" "$lib"; do
    [ ! -L "$dir" ] || fail "refusing symlink installation directory: $dir"
done
mkdir -p -- "$bin" "$lib"
previous=0
if [ -e "$wrapper" ] || [ -L "$wrapper" ] || [ -e "$helper" ] || [ -L "$helper" ] || [ -e "$original" ] || [ -L "$original" ] || [ -e "$manifest" ] || [ -L "$manifest" ]; then
    read_manifest
    check_file "$wrapper" "$wrapper_hash"
    check_file "$helper" "$helper_hash"
    check_file "$original" "$original_hash"
    [ -f "$wrapper" ] && [ -f "$helper" ] && [ -f "$original" ] || fail 'incomplete prior installation; use --uninstall before reinstalling'
    owned_marker "$wrapper" || fail 'existing yazi is not the owned wrapper'
    previous=1
    if [ -z "$real" ]; then IFS= read -r real < "$original" || fail 'cannot read original Yazi path'; fi
fi

if [ -z "$real" ]; then
    old_ifs=$IFS
    IFS=:
    for directory in $PATH; do
        [ -n "$directory" ] || directory=.
        candidate=$directory/yazi
        [ -f "$candidate" ] && [ -x "$candidate" ] || continue
        resolved=$(readlink -f -- "$candidate") || continue
        target=$(readlink -m -- "$wrapper") || fail 'cannot resolve wrapper destination'
        [ "$resolved" != "$target" ] || continue
        owned_marker "$resolved" && continue
        real=$resolved
        break
    done
    IFS=$old_ifs
fi
[ -n "$real" ] || fail 'original Yazi was not found on PATH; pass --real /path/to/yazi'
case $real in *'
'*) fail 'original executable path cannot contain a newline' ;; esac
real=$(readlink -f -- "$real") || fail 'cannot resolve original Yazi'
[ -f "$real" ] && [ -x "$real" ] || fail 'original Yazi must be an executable file'
[ "$real" != "$(readlink -m -- "$wrapper")" ] && [ "$real" != "$(readlink -m -- "$helper")" ] || fail 'original Yazi cannot be the installed launcher'
owned_marker "$real" && fail 'original Yazi cannot be another tern-yazi wrapper'

repo=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
command -v cargo >/dev/null 2>&1 || fail 'Cargo is required to build the native Rust launcher'
[ -f "$repo/launcher/Cargo.lock" ] || fail 'launcher/Cargo.lock is missing; run cargo generate-lockfile --manifest-path launcher/Cargo.toml first'
cargo build --locked --release --manifest-path "$repo/launcher/Cargo.toml" --target-dir "$repo/launcher/target"

tmp_helper=$(mktemp "$lib/.tern-yazi-launch.XXXXXX")
tmp_original=$(mktemp "$lib/.tern-yazi-original.XXXXXX")
tmp_wrapper=$(mktemp "$bin/.tern-yazi-wrapper.XXXXXX")
tmp_manifest=$(mktemp "$lib/.tern-yazi-owned.XXXXXX")
trap 'rm -f -- "$tmp_helper" "$tmp_original" "$tmp_wrapper" "$tmp_manifest"' EXIT HUP INT TERM
cp -- "$repo/launcher/target/release/tern-yazi-launch" "$tmp_helper"
chmod 700 "$tmp_helper"
printf '%s\n' "$real" > "$tmp_original"
cat > "$tmp_wrapper" <<'WRAPPER'
#!/bin/sh
# tern-yazi owned launcher wrapper v1
set -eu
base=$(CDPATH= cd -- "${0%/*}/../libexec" && pwd -P)
IFS= read -r original < "$base/tern-yazi-launch.real" || {
    printf '%s\n' 'tern-yazi: original Yazi path is missing; reinstall the launcher' >&2
    exit 1
}
if [ -z "${TERN_PANE:-}" ]; then
    exec "$original" "$@"
fi
exec "$base/tern-yazi-launch" --real "$original" -- "$@"
WRAPPER
chmod 700 "$tmp_wrapper"
{
    printf '%s\n' 'tern-yazi owned installation v1'
    hash "$tmp_wrapper"
    hash "$tmp_helper"
    hash "$tmp_original"
} > "$tmp_manifest"
# Rename completed files only; neither original binary nor unrelated wrappers are modified.
mv -f -- "$tmp_helper" "$helper"
mv -f -- "$tmp_original" "$original"
mv -f -- "$tmp_wrapper" "$wrapper"
mv -f -- "$tmp_manifest" "$manifest"
trap - EXIT HUP INT TERM
printf 'Installed %s (original: %s).\n' "$wrapper" "$real"
printf '%s\n' 'Put this prefix/bin before the original Yazi directory on PATH.' 'Tern and Yazi plugins must also be linked/enabled; see the repository installation instructions.'
