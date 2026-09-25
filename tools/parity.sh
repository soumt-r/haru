#!/usr/bin/env bash
# Checks that Haru's parser reads every program exactly as Hana's does: same
# tree, same syntax diagnostics. Needs Go and the hana repo next to haru.
#
#   tools/parity.sh [mutants-per-program]     (default 20)
#
# The corpus is every .hr/.knd file, every ```hari/```kanade block in the docs,
# and the program strings in Hana's Go tests; mutants are damaged copies that
# reach the parser's recovery paths.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(cd "$here/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

(cd "$here/tools/astdump" && GOFLAGS=-mod=mod go build -o "$work/astdump" .)
(cd "$here" && cargo build -q --release -p haru-cli)

sources=()
for d in hana hari-docs/src kanade-docs/src loh scratch vscode-hari; do
    [ -d "$root/$d" ] && sources+=("$root/$d")
done

"$work/astdump" -mutate "${1:-20}" -out "$work/corpus" "${sources[@]}"
"$here/target/release/haru" ast-check "$work/corpus"
