#!/bin/sh
# entry.sh <live-dir> <session>
#
# The command every test terminal runs, in place of a shell. It:
#   1. writes <live-dir>/<session>/session.json: its tty, pid, pgid,
#      TERM_PROGRAM, TERM_PROGRAM_VERSION, and the names (never the values)
#      of any Q_TERM, KERO_* or WARP_* variables it was started with;
#   2. starts inject.pl in the background on that tty, reading in.fifo;
#   3. execs the command in <live-dir>/<session>/cmd, one argv per line.
#
# The exec keeps this pid, so session.json's pid is also the command's.
set -u

live=${1:?usage: entry.sh <live-dir> <session>}
name=${2:?usage: entry.sh <live-dir> <session>}
here=$(cd "$(dirname "$0")" && pwd)
dir=$live/$name
mkdir -p "$dir"

# json_str: print $1 as a JSON string (these values never hold control bytes).
json_str() {
  printf '"%s"' "$(printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g')"
}

# names: a JSON array of the variable names matching the pattern, sorted.
names() {
  printf '[%s]' "$(env | sed -n "s/^\($1\)=.*/\1/p" | sort -u |
    awk '{ printf "%s\"%s\"", (NR > 1 ? ", " : ""), $0 }')"
}

t=$(tty) || t=none
pgid=$(ps -o pgid= -p $$ | tr -d ' ')

# Read the command before starting anything, so a missing cmd leaves nothing behind.
if [ ! -s "$dir/cmd" ]; then
  echo "entry.sh: $dir/cmd is missing or empty" >&2
  exit 1
fi
set --
while IFS= read -r arg || [ -n "$arg" ]; do
  set -- "$@" "$arg"
done <"$dir/cmd"

rm -f "$dir/in.fifo"
mkfifo "$dir/in.fifo"
perl "$here/inject.pl" "$dir/in.fifo" $$ </dev/null >>"$dir/inject.log" 2>&1 &
injector=$!

{
  printf '{\n'
  printf '  "tty": %s,\n' "$(json_str "$t")"
  printf '  "pid": %s,\n' $$
  printf '  "pgid": %s,\n' "$pgid"
  printf '  "injector_pid": %s,\n' "$injector"
  printf '  "TERM_PROGRAM": %s,\n' "$(json_str "${TERM_PROGRAM:-}")"
  printf '  "TERM_PROGRAM_VERSION": %s,\n' "$(json_str "${TERM_PROGRAM_VERSION:-}")"
  printf '  "Q_TERM": %s,\n' "$(names 'Q_TERM')"
  printf '  "KERO": %s,\n' "$(names 'KERO_[A-Za-z0-9_]*')"
  printf '  "WARP": %s\n' "$(names 'WARP_[A-Za-z0-9_]*')"
  printf '}\n'
} >"$dir/session.json.tmp"
mv "$dir/session.json.tmp" "$dir/session.json"

exec "$@"
