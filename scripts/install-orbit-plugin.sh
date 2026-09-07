#!/bin/sh
set -eu

usage() {
    echo "usage: $0 [--orbit-root PATH] [--binary PATH]" >&2
    exit 2
}

orbit_root=""
graph_binary="${ORBIT_GRAPH_BIN:-}"
while [ "$#" -gt 0 ]; do
    case "$1" in
        --orbit-root) [ "$#" -ge 2 ] || usage; orbit_root=$2; shift 2 ;;
        --binary) [ "$#" -ge 2 ] || usage; graph_binary=$2; shift 2 ;;
        *) usage ;;
    esac
done

if [ -z "$graph_binary" ]; then
    graph_binary=$(command -v orbit-graph) || {
        echo "orbit-graph is not installed; pass --binary PATH" >&2
        exit 1
    }
fi
graph_binary=$(cd "$(dirname "$graph_binary")" && pwd -P)/$(basename "$graph_binary")
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
plugin_dir=$(dirname "$script_dir")/plugin

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
