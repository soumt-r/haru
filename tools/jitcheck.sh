#!/usr/bin/env bash
# Runs every program of a corpus (as tools/runcheck.sh finds them) with
# `haru run`, once interpreted (HARU_JIT=0) and once with the JIT
# (HARU_JIT=1), and compares what they print and their exit status.
#
#   tools/jitcheck.sh <haru binary> <corpus dir> [jobs]
#
# Functions are compiled as soon as they run unless HARU_JIT_HOT says
# after how many calls and loop turns (the default of `haru run` is 1000).
# Mismatches are listed in <corpus>/jitcheck.txt.
set -uo pipefail
export HARU_JIT_HOT="${HARU_JIT_HOT:-0}"
haru="$(realpath "$1")" dir="$2" jobs="${3:-8}"

check() {
    local f="$1" haru="$2"
    local a b
    a="$(cd "$(dirname "$f")" && HARU_JIT=0 timeout 10 "$haru" run "$(basename "$f")" </dev/null 2>&1; echo "[exit $?]")"
    b="$(cd "$(dirname "$f")" && HARU_JIT=1 timeout 10 "$haru" run "$(basename "$f")" </dev/null 2>&1; echo "[exit $?]")"
    if [[ "$a" == *"[exit 124]" || "$b" == *"[exit 124]" ]]; then
        echo "TIMEOUT $f"
    elif [[ "$a" == "$b" ]]; then
        echo "PASS $f"
    else
        echo "FAIL $f"
    fi
}
export -f check

ls "$dir"/*.hr "$dir"/*.knd "$dir"/*/main.hr "$dir"/*/main.knd 2>/dev/null |
    xargs -P "$jobs" -I{} bash -c 'check "$@"' _ {} "$haru" > "$dir/jitcheck.txt"

pass=$(grep -c '^PASS' "$dir/jitcheck.txt")
fail=$(grep -c '^FAIL' "$dir/jitcheck.txt")
timeouts=$(grep -c '^TIMEOUT' "$dir/jitcheck.txt")
echo "same: $pass   different: $fail   timed out: $timeouts"
[ "$fail" -eq 0 ]
