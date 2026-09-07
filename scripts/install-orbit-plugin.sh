#!/bin/sh
set -eu

usage() {
    echo "usage: $0 [--orbit-root PATH] [--binary PATH]" >&2
    exit 2
}

orbit_root=""
graph_binary="${ORBIT_GRAPH_BIN:-}"
orbit_root_set=0
binary_set=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --orbit-root) [ "$#" -ge 2 ] || usage; orbit_root=$2; orbit_root_set=1; shift 2 ;;
        --binary) [ "$#" -ge 2 ] || usage; graph_binary=$2; binary_set=1; shift 2 ;;
        *) usage ;;
    esac
done

if [ "$orbit_root_set" -eq 1 ]; then
    [ -n "$orbit_root" ] || usage
    [ -d "$orbit_root" ] || {
        echo "Orbit root is not a directory: $orbit_root" >&2
        exit 2
    }
    orbit_root=$(cd "$orbit_root" && pwd -P)
fi

if [ "$binary_set" -eq 1 ] && [ -z "$graph_binary" ]; then
    usage
fi

if [ -z "$graph_binary" ]; then
    graph_binary=$(command -v orbit-graph) || {
        echo "orbit-graph is not installed; pass --binary PATH" >&2
        exit 1
    }
fi
[ -x "$graph_binary" ] || {
    echo "orbit-graph binary is not executable: $graph_binary" >&2
    exit 2
}
command -v orbit >/dev/null 2>&1 || {
    echo "orbit is not installed" >&2
    exit 1
}
graph_binary=$(cd "$(dirname "$graph_binary")" && pwd -P)/$(basename "$graph_binary")
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
plugin_dir=$(dirname "$script_dir")/plugin
for manifest in \
    "$plugin_dir/orbit-graph-recommend.orbit-tool.yaml" \
    "$plugin_dir/orbit-graph-status.orbit-tool.yaml" \
    "$plugin_dir/orbit-graph-maintain.orbit-tool.yaml"
do
    [ -r "$manifest" ] || {
        echo "plugin manifest is not readable: $manifest" >&2
        exit 1
    }
done

add_tool() {
    manifest=$1
    if [ -n "$orbit_root" ]; then
        orbit tool add "$graph_binary" --manifest "$manifest" --root "$orbit_root"
    else
        orbit tool add "$graph_binary" --manifest "$manifest"
    fi
}

add_tool "$plugin_dir/orbit-graph-recommend.orbit-tool.yaml"
add_tool "$plugin_dir/orbit-graph-status.orbit-tool.yaml"
add_tool "$plugin_dir/orbit-graph-maintain.orbit-tool.yaml"

echo "Installed orbit.graph.recommend, orbit.graph.status, and orbit.graph.maintain"
