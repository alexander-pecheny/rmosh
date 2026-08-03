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

# Let the suite be pointed at the Rust binaries, which is how a session with one endpoint
# of each is tested. Upstream's scripts hardcode the C++ paths.
git checkout -- src/tests/e2e-test src/tests/local.test
git apply "$root/tests/cpp/endpoint-override.patch"

[ -f configure ] || ./autogen.sh
[ -f Makefile ] || ./configure
make -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"
