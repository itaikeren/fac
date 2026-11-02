# common.sh: sourced by the tools in tools/live. Sets $here and $live.
#
# The live dir comes from FAC_LIVE_DIR; nothing here names a personal path.

here=$(cd "$(dirname "$0")" && pwd)
live=${FAC_LIVE_DIR:?set FAC_LIVE_DIR to the scratch live dir}
mkdir -p "$live"
entry=$here/entry.sh

# winlist: build tools/live/winlist.swift once into $live/bin.
winlist() {
  if [ ! -x "$live/bin/winlist" ] || [ "$here/winlist.swift" -nt "$live/bin/winlist" ]; then
    mkdir -p "$live/bin"
    swiftc -O -o "$live/bin/winlist" "$here/winlist.swift" || return 1
  fi
  "$live/bin/winlist" "$@"
}

# osa: run AppleScript; stop on a permission error instead of working round it.
osa() {
  if ! res=$(osascript "$@" 2>&1); then
    case $res in
      *"(-1743)"* | *"(-1744)"* | *"(1002)"* | *"(-1002)"* | *"ot authorized"*)
        echo "PERMISSION: $res" >&2
        echo "STOP: macOS denied Automation or Screen Recording; grant it, do not work round it." >&2
        exit 3
        ;;
    esac
    echo "$res" >&2
    return 1
  fi
  [ -n "$res" ] && printf '%s\n' "$res"
  return 0
}
