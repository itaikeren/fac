#!/bin/sh
# iterm.sh open|read|close|winid|write <session> [text]
#
#   open    new iTerm2 window whose command is entry.sh (no shell); records its id
#   read    print the screen text of that window's session
#   write   type <text> with iTerm2's own `write text ... newline no`
#   close   close that window only
#   winid   print the CGWindowID for `screencapture -l`
#
# Only the window this script opened is ever addressed, by the id it recorded.
#
# If iTerm2 is not running, open starts it under env -i first, so it does not
# inherit this shell's Q_TERM, KERO_* or WARP_*. An iTerm2 that is already
# running keeps the environment it was started with; session.json shows it.
# iTerm2 keeps a closed session alive for a few seconds (Undo Close), so close
# waits for the session's processes to exit.
set -eu
. "$(cd "$(dirname "$0")" && pwd)/common.sh"

action=${1:?usage: iterm.sh open|read|close|winid|write <session>}
name=${2:?usage: iterm.sh open|read|close|winid|write <session>}
dir=$live/$name
mkdir -p "$dir"
idfile=$dir/iterm.window

win() {
  [ -s "$idfile" ] || { echo "iterm.sh: no window recorded for $name" >&2; exit 1; }
  cat "$idfile"
}

case $action in
  open)
    [ -s "$idfile" ] && { echo "iterm.sh: $name already has window $(cat "$idfile")" >&2; exit 1; }
    if ! pgrep -xq iTerm2; then
      # iTerm2 3.7.6 started by an AppleScript call opened no default window;
      # under env -i the app gets only HOME and PATH.
      env -i HOME="$HOME" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
        osascript -e 'tell application id "com.googlecode.iterm2" to launch'
      i=0
      until pgrep -xq iTerm2; do
        i=$((i + 1))
        [ $i -gt 100 ] && { echo "iterm.sh: iTerm2 did not start" >&2; exit 1; }
        sleep 0.1
      done
    fi
    rm -f "$dir/session.json"
    id=$(osa - "$entry" "$live" "$name" <<'EOF'
on run argv
  set cmd to quoted form of (item 1 of argv) & " " & quoted form of (item 2 of argv) & " " & quoted form of (item 3 of argv)
  tell application id "com.googlecode.iterm2"
    set w to (create window with default profile command cmd)
    return id of w
  end tell
end run
EOF
)
    printf '%s\n' "$id" >"$idfile"
    echo "$id"
    ;;
  read)
    osa - "$(win)" <<'EOF'
on run argv
  tell application id "com.googlecode.iterm2"
    return contents of current session of window id ((item 1 of argv) as integer)
  end tell
end run
EOF
    ;;
  write)
    text=${3:?usage: iterm.sh write <session> <text>}
    osa - "$(win)" "$text" <<'EOF'
on run argv
  tell application id "com.googlecode.iterm2"
    tell current session of window id ((item 1 of argv) as integer) to write text (item 2 of argv) newline no
  end tell
end run
EOF
    ;;
  close)
    osa - "$(win)" <<'EOF'
on run argv
  tell application id "com.googlecode.iterm2"
    set wid to (item 1 of argv) as integer
    if (exists window id wid) then close window id wid
  end tell
end run
EOF
    rm -f "$idfile"
    pid=$(sed -n 's/^ *"pid": \([0-9]*\),*$/\1/p' "$dir/session.json" 2>/dev/null)
    i=0
    while [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; do
      i=$((i + 1))
      [ $i -gt 300 ] && { echo "iterm.sh: pid $pid still running 30 s after close" >&2; exit 1; }
      sleep 0.1
    done
    ;;
  winid)
    # iTerm2's AppleScript window id is the CGWindowID; check it against the window list.
    id=$(win)
    if winlist --owner iterm | awk -v id="$id" '$1 == id { f = 1 } END { exit !f }'; then
      echo "$id"
    else
      echo "iterm.sh: window $id is not in the CG window list" >&2
      exit 1
    fi
    ;;
  *)
    echo "usage: iterm.sh open|read|close|winid|write <session>" >&2
    exit 2
    ;;
esac
