#!/usr/bin/env bash
# Runs every program of a corpus (astdump's files, or modcorpus's folders
# with a main.hr/main.knd each) with `hana run` and `haru run` and
# compares what they print (stdout, stderr) and their exit status.
#
#   tools/runcheck.sh <hana binary> <haru binary> <corpus dir> [jobs]
#
# Programs Haru cannot run yet (exit 3) and programs that crash Hana (a Go
# panic) or run past 10 seconds are counted, not compared. Each run gets
# empty input. Mismatches are listed in <corpus>/runcheck.txt.
set -uo pipefail
hana="$1" haru="$2" dir="$3" jobs="${4:-8}"

check() {
    local f="$1" hana="$2" haru="$3"
    local a b
    a="$(cd "$(dirname "$f")" && timeout 10 "$hana" run "$(basename "$f")" </dev/null 2>&1; echo "[exit $?]")"
    b="$(cd "$(dirname "$f")" && timeout 10 "$haru" run "$(basename "$f")" </dev/null 2>&1; echo "[exit $?]")"
    if [[ "$a" == *"[exit 124]" || "$b" == *"[exit 124]" ]]; then
        echo "TIMEOUT $f"
    elif [[ "$a" == panic:* ]]; then
        echo "HANA-PANIC $f"
    elif [[ "$b" == *"[exit 3]" ]]; then
        echo "SKIP $f ${b%%$'\n'*}"
    elif [[ "$a" == "$b" ]]; then
        echo "PASS $f"
    else
        echo "FAIL $f"
    fi
}
export -f check

ls "$dir"/*.hr "$dir"/*.knd "$dir"/*/main.hr "$dir"/*/main.knd 2>/dev/null |
    xargs -P "$jobs" -I{} bash -c 'check "$@"' _ {} "$hana" "$haru" > "$dir/runcheck.txt"

pass=$(grep -c '^PASS' "$dir/runcheck.txt")
fail=$(grep -c '^FAIL' "$dir/runcheck.txt")
skip=$(grep -c '^SKIP' "$dir/runcheck.txt")
panics=$(grep -c '^HANA-PANIC' "$dir/runcheck.txt")
timeouts=$(grep -c '^TIMEOUT' "$dir/runcheck.txt")
echo "same output: $pass   different: $fail   not supported yet: $skip   hana crashed: $panics   timed out: $timeouts"
echo "unsupported, by reason:"
grep '^SKIP' "$dir/runcheck.txt" | sed 's/.*not supported yet: //' | sort | uniq -c | sort -rn
[ "$fail" -eq 0 ]
