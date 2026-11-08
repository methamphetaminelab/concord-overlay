#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 SRC_DIR" >&2
    exit 2
fi
src=$1
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

if [[ ! -f "$src/src/discord/mod.rs" ]]; then
    echo "error: $src does not look like a concord source tree" >&2
    exit 1
fi

args=(-p1 --forward --no-backup-if-mismatch --reject-file=- -d "$src" -i "$here/concord-hook.patch")
patch --dry-run "${args[@]}"
cp "$here/voice_export.rs" "$src/src/discord/voice_export.rs"
patch "${args[@]}"
