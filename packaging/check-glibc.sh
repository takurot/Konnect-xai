#!/usr/bin/env bash
# Reject GNU Linux artifacts that cannot run on Ubuntu 22.04 / glibc 2.35.
set -euo pipefail

if (( $# == 0 )); then
    echo "Usage: $0 ELF [ELF ...]" >&2
    exit 2
fi

for binary in "$@"; do
    symbols=$(LC_ALL=C "${OBJDUMP:-objdump}" -T "$binary")
    if ! versions=$(grep -Eo 'GLIBC_[0-9]+(\.[0-9]+)+' <<< "$symbols" | LC_ALL=C sort -Vu); then
        echo "$binary: no versioned glibc symbols found" >&2
        exit 1
    fi
    highest=$(tail -n 1 <<< "$versions")
    floor=GLIBC_2.35
    if grep -qw 'GLIBC_PRIVATE' <<< "$symbols" ||
        [[ "$(printf '%s\n' "$highest" "$floor" | LC_ALL=C sort -V | tail -n 1)" != "$floor" ]]; then
        echo "$binary: glibc symbols exceed the $floor compatibility floor (highest version: $highest)" >&2
        exit 1
    fi
    echo "$binary: highest glibc symbol is $highest (limit $floor)"
done
