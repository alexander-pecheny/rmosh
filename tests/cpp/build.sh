#!/bin/sh
# Build upstream mosh, which the differential tests compare the Rust against. They skip
# silently until this has run.
set -e

root=$(cd "$(dirname "$0")/../.." && pwd)
upstream=$root/third_party/mosh

if [ ! -f "$upstream/configure.ac" ]; then
    echo "third_party/mosh is empty; run: git submodule update --init" >&2
    exit 1
fi

cd "$upstream"

[ -f configure ] || ./autogen.sh
[ -f Makefile ] || ./configure
make -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"
