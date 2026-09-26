#!/bin/sh
set -eu

usage() {
    echo "usage: $0 [--orbit-root PATH] [--binary PATH]" >&2
    exit 2
}

orbit_root=""
graph_binary=""
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

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
plugin_dir=$(dirname "$script_dir")/plugin
if [ -e "$plugin_dir/bin/orbit-graph.bin" ]; then
    graph_binary=$plugin_dir/bin/orbit-graph.bin
elif [ "$binary_set" -eq 0 ] && [ -n "${ORBIT_GRAPH_BIN:-}" ]; then
    graph_binary=$ORBIT_GRAPH_BIN
elif [ -z "$graph_binary" ]; then
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
# Legacy sidecars also need the current plugin contract; refuse a stale PATH
# binary at install time instead of registering it for every future request.
probe=$(printf '%s\n' '{"tool":"orbit.graph.version","input":{}}' |
    ORBIT_TOOL_NAME=orbit.graph.version "$graph_binary" 2>/dev/null) || {
        echo "incompatible orbit-graph binary: $graph_binary (expected v2 envelope, plugin_schema_version=1, extractor_version=12)" >&2
        exit 1
    }
case "$probe" in
    *'"ok":true'*'"extractor_version":12'*'"plugin_schema_version":1'*) ;;
    *) echo "incompatible orbit-graph binary: $graph_binary (expected v2 envelope, plugin_schema_version=1, extractor_version=12)" >&2; exit 1 ;;
esac
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
