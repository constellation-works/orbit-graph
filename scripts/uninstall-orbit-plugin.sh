#!/bin/sh
set -eu

case "$#" in
    0) ;;
    2) [ "$1" = "--orbit-root" ] || {
        echo "usage: $0 [--orbit-root PATH]" >&2
        exit 2
    } ;;
    *)
        echo "usage: $0 [--orbit-root PATH]" >&2
        exit 2
        ;;
esac

orbit_root=""
if [ "$#" -eq 2 ]; then
    orbit_root=$2
fi

remove_tool() {
    name=$1
    if [ -n "$orbit_root" ]; then
        orbit tool remove "$name" --root "$orbit_root"
    else
        orbit tool remove "$name"
    fi
}

remove_tool orbit.graph.recommend
remove_tool orbit.graph.status
remove_tool orbit.graph.maintain

echo "Removed orbit-graph external tools; repository .orbit-graph indexes were retained"
