#!/bin/sh
# Assert the safety boundary from ADR 0003: mosh-sys is the only crate that may contain
# unsafe, and every other crate must say so with #![forbid(unsafe_code)].
set -eu

cd "$(dirname "$0")/.."
status=0

for manifest in crates/*/Cargo.toml; do
    crate_dir=$(dirname "$manifest")
    crate=$(basename "$crate_dir")

    if [ "$crate" = "mosh-sys" ]; then
        continue
    fi

    root="$crate_dir/src/lib.rs"
    [ -f "$root" ] || root="$crate_dir/src/main.rs"
    if [ ! -f "$root" ]; then
        printf 'no crate root found for %s\n' "$crate" >&2
        status=1
        continue
    fi

    if ! grep -q '^#!\[forbid(unsafe_code)\]' "$root"; then
        printf '%s: missing #![forbid(unsafe_code)] in %s\n' "$crate" "$root" >&2
        status=1
    fi

    if grep -rn '\bunsafe\b' "$crate_dir/src" >/dev/null 2>&1; then
        printf '%s: contains unsafe outside mosh-sys:\n' "$crate" >&2
        grep -rn '\bunsafe\b' "$crate_dir/src" >&2
        status=1
    fi
done

if [ $status -eq 0 ]; then
    blocks=$(grep -rc 'unsafe' crates/mosh-sys/src/*.rs | awk -F: '{n+=$2} END{print n}')
    printf 'safety boundary holds; mosh-sys mentions unsafe %s times\n' "$blocks"
fi

exit $status
