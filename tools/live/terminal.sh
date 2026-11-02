#!/bin/sh
# terminal.sh profile-add|profile-delete|profiles|open|read|close|winid [term]
#
#   profile-add [--dark]  save the profile names, then import the temporary fac-test
#                   profile from a generated <live>/term/fac-test.terminal
#   profile-delete  delete fac-test with AppleScript and check the names match
#   profiles        print every profile name plus the default and startup ones
#   open            open a window with fac-test (it runs entry.sh <live> term)
#   read            print the text of that window's tab
#   close           close that window only
#   winid           print the CGWindowID for `screencapture -l`
#
# The session is always "term": the profile's command is fixed. Never edits
# com.apple.Terminal; only fac-test is created and it is deleted by AppleScript.
#
# fac-test.terminal (Terminal 2.15, build 466):
#   CommandString      "<entry.sh> <live> term"
#   RunCommandAsShell  true: Terminal execs the command itself, as the shell.
#                      With false it runs it inside the login shell, and Kiro's
#                      ~/.zprofile hook then wraps it (root-check.sh fails).
#                      As the shell, the words are split on spaces and quotes
#                      are kept literally, so neither path may hold a space.
#   shellExitAction    0: close the window when the command exits.
set -eu
. "$(cd "$(dirname "$0")" && pwd)/common.sh"

usage="usage: terminal.sh profile-add|profile-delete|profiles|open|read|close|winid [term]"
action=${1:?$usage}
dark=0
if [ "${2:-}" = --dark ]; then dark=1; shift; fi
[ "${2:-term}" = term ] || { echo "terminal.sh: the session is always term" >&2; exit 2; }
name=term
dir=$live/$name
mkdir -p "$dir"
idfile=$dir/terminal.window
profile=$dir/fac-test.terminal

# launch: start Terminal if it is not running, under env -i so it inherits no
# Q_TERM, KERO_* or WARP_*. An AppleScript launch opens no default window.
launch() {
  pgrep -xq Terminal && return 0
  env -i HOME="$HOME" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
    osascript -e 'tell application "Terminal" to launch'
  i=0
  until pgrep -xq Terminal; do
    i=$((i + 1))
    [ $i -gt 100 ] && { echo "terminal.sh: Terminal did not start" >&2; exit 1; }
    sleep 0.1
  done
}

profiles() {
  osa <<'EOF'
tell application "Terminal"
  set out to ""
  repeat with s in (every settings set)
    set out to out & "profile: " & (name of s) & linefeed
  end repeat
  set out to out & "default: " & (name of default settings) & linefeed
  set out to out & "startup: " & (name of startup settings)
  return out
end tell
EOF
}

# window_ids: ids of Terminal's visible windows. A closed window stays in
# Terminal's list for a while, invisible, so it is left out.
window_ids() {
  osa <<'EOF'
tell application "Terminal"
  set out to ""
  repeat with w in (every window whose visible is true)
    set out to out & (id of w) & linefeed
  end repeat
  return out
end tell
EOF
}

# open_profile_file: open fac-test.terminal and print the id of the window it opens.
open_profile_file() {
  before=$(window_ids)
  env -i HOME="$HOME" PATH=/usr/bin:/bin:/usr/sbin:/sbin open -a Terminal "$profile"
  i=0
  while [ $i -lt 50 ]; do
    new=$(window_ids | grep -vxF -e "${before:-none}" -e '' || true)
    [ -n "$new" ] && { printf '%s\n' "$new" | head -n 1; return 0; }
    sleep 0.1
    i=$((i + 1))
  done
  return 1
}

# visible <id>: whether that window is still open (visible).
visible() {
  window_ids | grep -qx "$1"
}

# wait_gone <id>: wait up to 5 s for the window to close; fail if it does not.
wait_gone() {
  i=0
  while visible "$1"; do
    i=$((i + 1))
    [ $i -gt 50 ] && return 1
    sleep 0.1
  done
  return 0
}

win() {
  [ -s "$idfile" ] || { echo "terminal.sh: no window recorded" >&2; exit 1; }
  cat "$idfile"
}

case $action in
  profiles)
    profiles
    ;;
  profile-add)
    for p in "$entry" "$live"; do
      case $p in
        *[[:space:]]*) echo "terminal.sh: $p holds a space; Terminal cannot run it as the shell" >&2; exit 1 ;;
      esac
    done
    launch
    if profiles | grep -qx 'profile: fac-test'; then
      echo "terminal.sh: fac-test already exists" >&2
      exit 1
    fi
    profiles >"$dir/profiles.before"
    rm -f "$profile"
    plutil -create xml1 "$profile"
    plutil -insert name -string fac-test "$profile"
    plutil -insert type -string 'Window Settings' "$profile"
    plutil -insert ProfileCurrentVersion -float 2.07 "$profile"
    plutil -insert CommandString -string "$entry $live $name" "$profile"
    plutil -insert RunCommandAsShell -bool true "$profile"
    plutil -insert shellExitAction -integer 0 "$profile"
    # --dark: only the generated file's background, text and bold text colours (NSColor
    # archives made with JXA); without it the profile keeps Terminal's defaults.
    if [ $dark -eq 1 ]; then
      for kv in "BackgroundColor 0.10 0.10 0.12" "TextColor 0.90 0.90 0.90" "TextBoldColor 1.00 1.00 1.00"; do
        set -- $kv
        data=$(osascript -l JavaScript -e "
          ObjC.import('AppKit');
          var c = \$.NSColor.colorWithCalibratedRedGreenBlueAlpha($2, $3, $4, 1);
          var d = \$.NSKeyedArchiver.archivedDataWithRootObjectRequiringSecureCodingError(c, false, null);
          ObjC.unwrap(d.base64EncodedStringWithOptions(0));")
        plutil -insert "$1" -data "$data" "$profile"
      done
    fi
    # Opening the file imports the profile and also opens one window with it.
    # With the cmd file moved aside entry.sh exits at once, so that window
    # closes itself (shellExitAction 0).
    rm -f "$dir/cmd.saved" "$dir/session.json"
    [ -f "$dir/cmd" ] && mv "$dir/cmd" "$dir/cmd.saved"
    id=$(open_profile_file) || id=
    [ -f "$dir/cmd.saved" ] && mv "$dir/cmd.saved" "$dir/cmd"
    if [ -z "$id" ]; then
      echo "import window: none"
    elif wait_gone "$id"; then
      echo "import window: $id, closed itself"
    else
      echo "terminal.sh: import window $id is still open; closing it" >&2
      osa - "$id" <<'EOF'
on run argv
  tell application "Terminal" to close window id ((item 1 of argv) as integer)
end run
EOF
    fi
    profiles
    ;;
  profile-delete)
    launch
    osa <<'EOF'
tell application "Terminal"
  if (exists settings set "fac-test") then delete settings set "fac-test"
end tell
EOF
    profiles >"$dir/profiles.after"
    echo "before:"; cat "$dir/profiles.before"
    echo "after:"; cat "$dir/profiles.after"
    if cmp -s "$dir/profiles.before" "$dir/profiles.after"; then
      echo "profiles: identical"
    else
      echo "terminal.sh: profile names differ" >&2
      exit 1
    fi
    ;;
  open)
    [ -s "$idfile" ] && { echo "terminal.sh: window $(cat "$idfile") already open" >&2; exit 1; }
    [ -f "$dir/cmd" ] || { echo "terminal.sh: write $dir/cmd first" >&2; exit 1; }
    [ -f "$profile" ] || { echo "terminal.sh: run profile-add first" >&2; exit 1; }
    launch
    # Opening the file again opens a window with the existing fac-test; it
    # does not import a second copy (checked against the profile names).
    rm -f "$dir/session.json"
    id=$(open_profile_file) || { echo "terminal.sh: no window opened" >&2; exit 1; }
    printf '%s\n' "$id" >"$idfile"
    echo "$id"
    ;;
  read)
    osa - "$(win)" <<'EOF'
on run argv
  tell application "Terminal"
    return contents of selected tab of window id ((item 1 of argv) as integer)
  end tell
end run
EOF
    ;;
  close)
    id=$(win)
    # Terminal asks before closing a window whose command still runs, and
    # AppleScript cannot answer that sheet. So the session's own command is
    # hung up first (only when the window's tty is the one session.json
    # names); the window then closes itself (shellExitAction 0).
    if visible "$id"; then
      wtty=$(osa - "$id" <<'EOF'
on run argv
  tell application "Terminal" to return tty of selected tab of window id ((item 1 of argv) as integer)
end run
EOF
)
      sess_tty=$(sed -n 's/^ *"tty": "\(.*\)",*$/\1/p' "$dir/session.json" 2>/dev/null)
      spid=$(sed -n 's/^ *"pid": \([0-9]*\),*$/\1/p' "$dir/session.json" 2>/dev/null)
      ptty=$(ps -o tty= -p "${spid:-0}" 2>/dev/null | tr -d ' ')
      if [ -n "$spid" ] && [ -n "$wtty" ] && [ "$wtty" = "$sess_tty" ] && [ "/dev/$ptty" = "$sess_tty" ]; then
        kill -HUP "$spid"
      fi
    fi
    if ! wait_gone "$id"; then
      osa - "$id" <<'EOF'
on run argv
  tell application "Terminal" to close window id ((item 1 of argv) as integer)
end run
EOF
      wait_gone "$id" || { echo "terminal.sh: window $id did not close" >&2; exit 1; }
    fi
    rm -f "$idfile"
    ;;
  winid)
    id=$(win)
    if winlist --owner terminal | awk -v id="$id" '$1 == id { f = 1 } END { exit !f }'; then
      echo "$id"
    else
      echo "terminal.sh: window $id is not in the CG window list" >&2
      exit 1
    fi
    ;;
  *)
    echo "$usage" >&2
    exit 2
    ;;
esac
