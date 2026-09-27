#!/bin/sh
set -eu

# Place a compatible orbit-graph executable at <plugin-root>/bin/orbit-graph.bin,
# where the plugin launcher selects it ahead of PATH, and bind the plugin to it:
# its SHA-256 is recorded as `--backend-sha256` in the tree's plugin.yaml
# `spec.backend.args`, so the launcher runs nothing else. That changes the
# manifest digest Orbit's consent is bound to, so Orbit treats the tree as a
# new manifest until the operator installs it again (`orbit plugin add <tree>
# --force`); that re-approval is the explicit step (STD-05 R21).
#
# --unbound copies the executable without binding it and without touching
# plugin.yaml. The launcher then runs it only under the manifest's
# `--allow-unbound-backend` override, which it reports on every call.
#
# The plugin root is either this checkout (the default) or a plugin tree such
# as the one printed as "Install path" by `orbit plugin show graph`. The binary
# is copied, never linked: Orbit refuses a plugin tree that contains a
# symbolic link.

usage() {
    echo "usage: $0 [--binary PATH] [--unbound] [PLUGIN_ROOT]" >&2
    exit 2
}

graph_binary=""
plugin_root=""
bind=1
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary) [ "$#" -ge 2 ] && [ -n "$2" ] || usage; graph_binary=$2; shift 2 ;;
        --unbound) bind=0; shift ;;
        -*) usage ;;
        *) [ -z "$plugin_root" ] || usage; plugin_root=$1; shift ;;
    esac
done

if [ -z "$plugin_root" ]; then
    plugin_root=$(dirname "$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)")
fi
[ -d "$plugin_root" ] || { echo "plugin root is not a directory: $plugin_root" >&2; exit 2; }
plugin_root=$(CDPATH= cd -- "$plugin_root" && pwd -P)
launcher="$plugin_root/bin/orbit-graph"
[ -f "$launcher" ] || { echo "no plugin launcher at $launcher" >&2; exit 2; }
manifest="$plugin_root/plugin.yaml"
[ -f "$manifest" ] || { echo "no plugin manifest at $manifest" >&2; exit 2; }
# The one `spec.backend.args` line the binding lives on.
args_pattern='^    args: \[.*\]$'
[ "$(grep -c "$args_pattern" "$manifest" || true)" -eq 1 ] || {
    echo "cannot find exactly one spec.backend.args line in $manifest" >&2
    exit 1
}
if [ "$bind" -eq 0 ] && ! grep -q '^    args: \[--allow-unbound-backend\]$' "$manifest"; then
    echo "$manifest binds a --backend-sha256 or lacks --allow-unbound-backend, so the launcher would refuse an unbound executable; bundle without --unbound" >&2
    exit 1
fi

if [ -z "$graph_binary" ]; then
    graph_binary=$(command -v orbit-graph) || {
        echo "orbit-graph is not on PATH; pass --binary PATH" >&2
        exit 1
    }
fi
[ -f "$graph_binary" ] && [ -x "$graph_binary" ] || {
    echo "orbit-graph binary is not an executable file: $graph_binary" >&2
    exit 2
}

# The launcher of this plugin version pins the contract; probe the candidate
# against exactly those values before it can be selected.
extractor=$(sed -n 's/.*"extractor_version":\([0-9][0-9]*\).*/\1/p' "$launcher" | head -n 1)
plugin_schema=$(sed -n 's/.*"plugin_schema_version":\([0-9][0-9]*\).*/\1/p' "$launcher" | head -n 1)
[ -n "$extractor" ] && [ -n "$plugin_schema" ] || {
    echo "cannot read the version pins from $launcher" >&2
    exit 1
}
probe=$(printf '%s\n' '{"tool":"graph.version","input":{}}' |
    ORBIT_TOOL_NAME=graph.version "$graph_binary" 2>/dev/null) || probe=""
case "$probe" in
    *'"ok":true'*) ;;
    *) probe="" ;;
esac
case "$probe" in
    *"\"extractor_version\":$extractor"[!0-9]*) ;;
    *) probe="" ;;
esac
case "$probe" in
    *"\"plugin_schema_version\":$plugin_schema"[!0-9]*) ;;
    *) probe="" ;;
esac
[ -n "$probe" ] || {
    echo "incompatible orbit-graph binary: $graph_binary (this plugin needs the v2 envelope, plugin_schema_version=$plugin_schema, extractor_version=$extractor)" >&2
    exit 1
}

target="$plugin_root/bin/orbit-graph.bin"
staging="$target.tmp.$$"
manifest_staging="$manifest.tmp.$$"
trap 'rm -f "$staging" "$manifest_staging"' EXIT
cp "$graph_binary" "$staging"
chmod 755 "$staging"
if [ "$bind" -eq 1 ]; then
    # Hash the copy that is installed, not the source, which could change.
    if command -v sha256sum >/dev/null 2>&1; then
        digest=$(sha256sum < "$staging" | cut -d ' ' -f 1)
    elif command -v shasum >/dev/null 2>&1; then
        digest=$(shasum -a 256 < "$staging" | cut -d ' ' -f 1)
    else
        echo "neither sha256sum nor shasum is available to record the executable's SHA-256" >&2
        exit 1
    fi
    sed "s/$args_pattern/    args: [--backend-sha256, $digest]/" "$manifest" > "$manifest_staging"
fi
# The executable goes first: until the manifest names its digest, the
# launcher refuses it (or, under the override, reports it) rather than
# running something unrecorded as bound.
mv -f "$staging" "$target"
if [ "$bind" -eq 1 ]; then
    mv -f "$manifest_staging" "$manifest"
fi
trap - EXIT
echo "bundled $graph_binary as $target (extractor_version=$extractor, plugin_schema_version=$plugin_schema)"
if [ "$bind" -eq 1 ]; then
    echo "recorded --backend-sha256 $digest in $manifest; its manifest digest changed, so re-approve the tree with: orbit plugin add $plugin_root --force" >&2
else
    echo "bundled unbound: the launcher runs it under --allow-unbound-backend and reports backend_override on every call" >&2
fi
