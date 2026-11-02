#!/bin/sh
# vscode.sh start|send|command|stop|winid|procs|user-procs [args]
#
#   start                 write the scratch settings, then start a separate VS Code
#                         instance (its own --user-data-dir) with the driver extension
#   send <text> [ms]      type <text> (escapes as in keys.pl) through the driver,
#                         one sendText per key with [ms] between them
#   command <id>          run one workbench command through the driver; only
#                         workbench.action.toggleSidebarVisibility and
#                         workbench.action.toggleMaximizedPanel are accepted
#   stop                  ask the driver to quit, then wait for the instance to exit
#   winid                 print the CGWindowID of that instance's window
#   procs                 pid and command of every process of that instance
#   user-procs            pids of every other VS Code process (the user's own)
#
# Never quits VS Code by name and never kills: the user's own instance is untouched.
#
# The user-data-dir is a short path. VS Code puts its main IPC socket inside the
# user-data-dir, and under the scratch live dir that path is longer than the
# 103 bytes a macOS socket path may hold, so the instance exits at start. start
# therefore makes `mktemp -d /tmp/fv.XXXXXX` (unpredictable and ours, so
# nothing can be planted there) holding one symlink, d -> <live>/vsc, and
# passes <tmp>/d. All data stays in <live>/vsc. The tmp path is recorded in
# <live>/vsc-short; stop removes the link and the dir once the instance's
# processes, found by that short path, are gone.
set -eu
. "$(cd "$(dirname "$0")" && pwd)/common.sh"

action=${1:?usage: vscode.sh start|send|command|stop|winid|procs|user-procs}
dir=$live/vsc
short_file=$live/vsc-short
code_cli="/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"
app="/Applications/Visual Studio Code.app/"

# udd: the short --user-data-dir of the current test instance, if any.
udd() {
  [ -s "$short_file" ] && printf '%s/d\n' "$(cat "$short_file")"
  return 0
}

# The test instance's processes: commands from the app bundle (so never this
# awk, whose own arguments hold the same strings) with that --user-data-dir.
procs() {
  u=$(udd)
  [ -n "$u" ] || return 0
  ps -axo pid=,command= | awk -v app="$app" -v a="--user-data-dir=$u" -v b="--user-data-dir $u" '
    { cmd = $0; sub(/^ *[0-9]+ /, "", cmd); line = cmd " " }
    index(cmd, app) == 1 && (index(line, a " ") || index(line, b " ")) { print }'
}

# Every other process from the app bundle: the user's own instance.
user_procs() {
  u=$(udd)
  ps -axo pid=,command= | awk -v app="$app" -v u="$u" -v a="--user-data-dir=$u" -v b="--user-data-dir $u" '
    { cmd = $0; sub(/^ *[0-9]+ /, "", cmd); line = cmd " " }
    index(cmd, app) == 1 && (u == "" || (!index(line, a " ") && !index(line, b " "))) { print $1 }'
}

case $action in
  start)
    if [ -s "$short_file" ]; then
      echo "vscode.sh: a test instance is recorded in $short_file; stop it first" >&2
      exit 1
    fi
    mkdir -p "$dir/User" "$live/vsc-ext" "$live/vsc-ws"
    rm -f "$dir/cmd.json" "$dir/driver.json"
    cat >"$dir/User/settings.json" <<EOF
{
  "terminal.integrated.profiles.osx": {
    "fac": { "path": "$entry", "args": ["$live", "vsc"] }
  },
  "terminal.integrated.defaultProfile.osx": "fac",
  "terminal.integrated.suggest.enabled": false,
  "security.workspace.trust.enabled": false,
  "update.mode": "none",
  "extensions.autoUpdate": false,
  "extensions.autoCheckUpdates": false,
  "telemetry.telemetryLevel": "off",
  "workbench.startupEditor": "none",
  "workbench.secondarySideBar.defaultVisibility": "hidden",
  "chat.disableAIFeatures": true,
  "chat.commandCenter.enabled": false
}
EOF
    tmp=$(mktemp -d /tmp/fv.XXXXXX)
    ln -s "$dir" "$tmp/d"
    printf '%s\n' "$tmp" >"$short_file"
    # VS Code names it after the first 4 characters of its version: 1.10-main.sock.
    ver=$(sed -n 's/^  "version": "\(.*\)",$/\1/p' "${app}Contents/Resources/app/package.json")
    sock="$tmp/d/$(printf '%.4s' "$ver")-main.sock"
    echo "vscode.sh: user-data-dir $tmp/d; main socket $sock (${#sock} bytes, limit 103)"
    [ ${#sock} -le 103 ] || { echo "vscode.sh: socket path too long" >&2; exit 1; }
    env -i HOME="$HOME" PATH=/usr/bin:/bin:/usr/sbin:/sbin "$code_cli" \
      --user-data-dir "$tmp/d" --extensions-dir "$live/vsc-ext" \
      -n "$live/vsc-ws" --extensionDevelopmentPath "$here/vscode-driver"
    i=0
    while [ ! -s "$dir/driver.json" ]; do
      i=$((i + 1))
      [ $i -gt 600 ] && { echo "vscode.sh: driver not ready after 60 s" >&2; exit 1; }
      sleep 0.1
    done
    cat "$dir/driver.json"
    ;;
  send)
    text=${2:?usage: vscode.sh send <text> [delay-ms]}
    perl "$here/keys.pl" json "${3:-0}" "$text" >"$dir/cmd.json.tmp"
    mv "$dir/cmd.json.tmp" "$dir/cmd.json"
    i=0
    while [ -e "$dir/cmd.json" ]; do
      i=$((i + 1))
      [ $i -gt 100 ] && { echo "vscode.sh: driver did not take the command" >&2; exit 1; }
      sleep 0.1
    done
    ;;
  command)
    id=${2:?usage: vscode.sh command <id>}
    case $id in
      workbench.action.toggleSidebarVisibility | workbench.action.toggleMaximizedPanel) ;;
      *) echo "vscode.sh: command $id is not allowlisted" >&2; exit 2 ;;
    esac
    printf '{"command": "%s"}\n' "$id" >"$dir/cmd.json.tmp"
    mv "$dir/cmd.json.tmp" "$dir/cmd.json"
    i=0
    while [ -e "$dir/cmd.json" ]; do
      i=$((i + 1))
      [ $i -gt 100 ] && { echo "vscode.sh: driver did not take the command" >&2; exit 1; }
      sleep 0.1
    done
    ;;
  stop)
    printf '{"quit": true}\n' >"$dir/cmd.json.tmp"
    mv "$dir/cmd.json.tmp" "$dir/cmd.json"
    i=0
    while [ -n "$(procs)" ]; do
      i=$((i + 1))
      if [ $i -gt 300 ]; then
        echo "vscode.sh: the instance is still running after 30 s:" >&2
        procs >&2
        exit 1
      fi
      sleep 0.1
    done
    echo "vscode.sh: the test instance has exited"
    tmp=$(cat "$short_file")
    rm "$tmp/d"
    rmdir "$tmp"
    rm "$short_file"
    echo "vscode.sh: removed $tmp"
    ;;
  winid)
    # The main process is the bundle's Electron itself, not a helper and not
    # the short-lived CLI (Electron running cli.js).
    main=$(procs | awk -v bin="${app}Contents/MacOS/Electron " '{ cmd = $0; sub(/^ *[0-9]+ /, "", cmd) }
      index(cmd, bin) == 1 && !index(cmd, "/cli.js ") { print $1; exit }')
    [ -n "$main" ] || { echo "vscode.sh: no main process" >&2; exit 1; }
    winlist --pid "$main" | awk '$3 == 0 { print $1; exit }'
    ;;
  procs)
    procs
    ;;
  user-procs)
    user_procs
    ;;
  *)
    echo "usage: vscode.sh start|send|command|stop|winid|procs|user-procs" >&2
    exit 2
    ;;
esac
