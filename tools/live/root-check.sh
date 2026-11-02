#!/bin/sh
# root-check.sh [--stdin] <tty>
#
# Lists every process on <tty> and fails (exit 1) if anything from Kiro is
# there, or if the session leader is not entry.sh or the command entry.sh
# exec'd. entry.sh keeps its pid through the exec, so that command is the
# process whose pid session.json records; set FAC_LIVE_DIR so it is found.
#
# When that command is /usr/bin/env, env execs in turn, so the leader is
# the command env runs: the first argument after any `-u NAME` pairs and
# NAME=value words. Any other env option is not accepted.
#
# --stdin reads the process list (ps -o pid=,ppid=,pgid=,stat=,command=)
# from stdin instead of from ps, so the rules can be tried on a made-up list.
#
# Terminal.app starts every command through `login -pfq <user> <command>`,
# which stays as the session leader. That leader is accepted only when its
# command is exactly login running entry.sh and the session's pid is its
# direct child, so no shell can sit between them.
set -u

from_stdin=0
if [ "${1:-}" = --stdin ]; then
  from_stdin=1
  shift
fi
t=${1:?usage: root-check.sh [--stdin] <tty>}
t=${t#/dev/}

if [ $from_stdin -eq 1 ]; then
  list=$(cat)
else
  list=$(ps -o pid=,ppid=,pgid=,stat=,command= -t "$t" 2>/dev/null)
fi
printf '%5s %5s %5s %-5s %s\n' PID PPID PGID STAT COMMAND
printf '%s\n' "$list" | awk '{ printf "%5s %5s %5s %-5s ", $1, $2, $3, $4; $1=$2=$3=$4=""; sub(/^ +/, ""); print }'

if [ -z "$list" ]; then
  echo "root-check: FAIL: no process on $t" >&2
  exit 1
fi

fail=0
if printf '%s\n' "$list" | grep -Eq 'kiro-cli-term|qterm'; then
  echo "root-check: FAIL: a Kiro process is on $t" >&2
  fail=1
fi

# The session leader carries "s" in STAT.
leader=$(printf '%s\n' "$list" | awk '$4 ~ /s/ { print $1; exit }')
if [ -z "$leader" ]; then
  echo "root-check: FAIL: no session leader on $t" >&2
  exit 1
fi
# field <pid> <n>: field n of pid's line in the list; 5 is the command.
field() {
  printf '%s\n' "$list" | awk -v p="$1" -v n="$2" '$1 == p {
    if (n < 5) { print $n; exit }
    sub(/^ *[^ ]+ +[^ ]+ +[^ ]+ +[^ ]+ +/, ""); print; exit }'
}
lcmd=$(field "$leader" 5)

# env_target <cmd file>: the command the /usr/bin/env on its first line
# runs, one argv per line after it; nothing for any other env option.
env_target() {
  {
    IFS= read -r _
    skip=0
    while IFS= read -r arg || [ -n "$arg" ]; do
      if [ $skip -eq 1 ]; then
        skip=0
        continue
      fi
      case $arg in
        -u) skip=1 ;;
        -*) return ;;
        *=*) ;;
        *)
          printf '%s\n' "$arg"
          return
          ;;
      esac
    done
  } <"$1"
}

# The session that entry.sh recorded for this tty, if any.
spid= first= target=
if [ -n "${FAC_LIVE_DIR:-}" ]; then
  for f in "$FAC_LIVE_DIR"/*/session.json; do
    [ -f "$f" ] || continue
    grep -q "\"tty\": \"/dev/$t\"" "$f" || continue
    p=$(sed -n 's/^ *"pid": \([0-9]*\),*$/\1/p' "$f")
    # A stale session.json may name a reused tty; its pid must be on it now.
    printf '%s\n' "$list" | awk -v p="$p" '$1 == p { f = 1 } END { exit !f }' || continue
    spid=$p
    cmdf=$(dirname "$f")/cmd
    first=$(head -n 1 "$cmdf" 2>/dev/null)
    target=
    [ "$first" = /usr/bin/env ] && target=$(env_target "$cmdf")
  done
fi

ok=0
# entry.sh itself, before its exec: run directly, or by sh.
set -- $lcmd
case ${1:-} in
  */entry.sh) ok=1 ;;
  sh | /bin/sh) case ${2:-} in */entry.sh) ok=1 ;; esac ;;
esac
if [ $ok -eq 0 ] && [ -n "$spid" ] && [ -n "$first" ]; then
  if [ "$spid" = "$leader" ]; then
    # The command entry.sh exec'd.
    case $lcmd in
      "$first" | "$first "*) ok=1 ;;
    esac
    # Or, for /usr/bin/env, the command env exec'd.
    if [ $ok -eq 0 ] && [ -n "$target" ]; then
      case $lcmd in
        "$target" | "$target "*) ok=1 ;;
      esac
    fi
  else
    case $lcmd in
      login\ -pf*\ */entry.sh\ * | /usr/bin/login\ -pf*\ */entry.sh\ *)
        if [ "$(field "$spid" 2)" = "$leader" ]; then
          ok=1
          echo "root-check: leader $leader is Terminal's login; session $spid is its child"
        fi
        ;;
    esac
  fi
fi
if [ $ok -eq 0 ]; then
  echo "root-check: FAIL: session leader $leader is not entry.sh or its command: $lcmd" >&2
  fail=1
fi

[ $fail -eq 0 ] && echo "root-check: PASS ($t, leader $leader)"
exit $fail
