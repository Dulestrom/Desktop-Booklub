#!/bin/bash
# Captures a screen from a dev-tools build. It only reads and writes the folders it is given.
# usage: tools/shot.sh <data-dir> <cache-dir> <light|dark> <command> <out.png> [WxH]
# commands: home all finished progress category:<name> filtered nomatch inspector
#           invalid-isbn newbook import settings about sorted:priority sorted:category:list
export XDG_DATA_HOME="$1" XDG_CACHE_HOME="$2" ADW_DEBUG_COLOR_SCHEME="prefer-$3" BOOKLUB_SHOT="$4=$5"
[ -n "$6" ] && export BOOKLUB_SIZE="$6"
timeout 25 "$(dirname "$0")/../target/debug/booklub" 2>&1 | grep -E 'captured|panicked|nothing'
