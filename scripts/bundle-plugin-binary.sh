#!/bin/sh
set -eu

# Place a compatible orbit-graph executable at <plugin-root>/bin/orbit-graph.bin,
# where the plugin launcher selects it ahead of PATH. The plugin root is either
# this checkout (the default) or an installed tree printed as "Install path" by
# `orbit plugin show graph`. The binary is copied, never linked: Orbit refuses a
# plugin tree that contains a symbolic link.

usage() {
    echo "usage: $0 [--binary PATH] [PLUGIN_ROOT]" >&2
    exit 2
}

graph_binary=""
plugin_root=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary) [ "$#" -ge 2 ] && [ -n "$2" ] || usage; graph_binary=$2; shift 2 ;;
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
trap 'rm -f "$staging"' EXIT
cp "$graph_binary" "$staging"
chmod 755 "$staging"
mv -f "$staging" "$target"
trap - EXIT
echo "bundled $graph_binary as $target (extractor_version=$extractor, plugin_schema_version=$plugin_schema)"
