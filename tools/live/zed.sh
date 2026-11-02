#!/bin/sh
# zed.sh prepare|start
#
#   prepare  write <live>/zed-ws/.zed/settings.json so the terminal panel runs
#            entry.sh <live> zed instead of a shell (Zed 1.25 shape:
#            terminal.shell.with_arguments.{program,args})
#   start    open that folder in a new Zed window, under env -i
#
# Only the scratch folder's .zed/settings.json is written; Zed's own settings
# are never touched. The user opens the terminal panel; the folder then shows
# in Zed's recent projects.
set -eu
. "$(cd "$(dirname "$0")" && pwd)/common.sh"

action=${1:?usage: zed.sh prepare|start}
ws=$live/zed-ws

case $action in
  prepare)
    mkdir -p "$ws/.zed"
    cat >"$ws/.zed/settings.json" <<EOF
{
  "terminal": {
    "shell": {
      "with_arguments": {
        "program": "$entry",
        "args": ["$live", "zed"]
      }
    }
  }
}
EOF
    perl -MJSON::PP -e 'local $/; open my $f, "<", $ARGV[0] or die; JSON::PP->new->decode(<$f>); print "settings.json: valid JSON\n"' "$ws/.zed/settings.json"
    ;;
  start)
    [ -f "$ws/.zed/settings.json" ] || { echo "zed.sh: run prepare first" >&2; exit 1; }
    cli=$(command -v zed || echo /Applications/Zed.app/Contents/MacOS/cli)
    env -i HOME="$HOME" PATH=/usr/bin:/bin:/usr/sbin:/sbin "$cli" -n "$ws"
    ;;
  *)
    echo "usage: zed.sh prepare|start" >&2
    exit 2
    ;;
esac
