# tools/live

Scripts that open a test session in Terminal.app, iTerm2, VS Code and Zed with a
chosen command, type into it, read its screen, screenshot it and close it,
without touching the user's own windows or settings. Phase 3 uses them for live
runs.

Every script takes the scratch live dir as an argument or as `FAC_LIVE_DIR`.
Nothing here names a personal path; generated files (the Terminal profile, the
VS Code settings, the Zed workspace) live in the live dir.

## Why the command is the terminal's own

Kiro wraps any interactive shell that lacks `PROCESS_LAUNCHED_BY_Q`, and a
login shell can pick up its hooks too. So the command under test (fac, a probe,
`cat -v`) is started by the terminal itself, through `entry.sh`, never typed
into a shell. `root-check.sh` proves it for every session.

## The scripts

| Script | What it does |
|---|---|
| `entry.sh <live> <session>` | The command every terminal runs. Writes `<live>/<session>/session.json` (tty, pid, pgid, injector pid, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION`, and the names, never the values, of any `Q_TERM`, `KERO_*`, `WARP_*`). Starts `inject.pl`, then execs the command in `<live>/<session>/cmd`, one argv per line. The exec keeps the pid. |
| `inject.pl <fifo> <pid>` | Started by `entry.sh` in the background. Reads bytes from `<live>/<session>/in.fifo` and pushes each one into its own controlling tty (`/dev/tty`, so no other tty is reachable) with `TIOCSTI` = `_IOW('t', 114, char)` = `0x80017472` (macOS SDK `sys/ttycom.h`). It ignores `SIGTTOU`, so it is never stopped for touching the tty from the background. It exits when the session's pid is gone. |
| `type.sh <live> <session> <string> [delay-ms]` | Types into a session through its injector. Escapes: `\e \t \r \n \\ \xHH`. With a delay, keys (an escape sequence, a UTF-8 character or a byte) go one at a time. |
| `keys.pl` | The escape decoder behind `type.sh` and `vscode.sh send`. |
| `root-check.sh <tty>` | Lists every process on the tty. Fails if any is Kiro's (`kiro-cli-term`, `qterm`, `zsh (kiro-cli-term)`), or if the session leader is not `entry.sh` or the command it exec'd. Terminal.app's `login -pfq <user> <entry.sh …>` is accepted as the leader only when the session's pid is its direct child. Needs `FAC_LIVE_DIR` to find `session.json`. |
| `iterm.sh open\|read\|write\|close\|winid <session>` | iTerm2. `open` uses `create window with default profile command "<entry.sh> <live> <session>"` and records the window id; `read` prints `contents of current session`; `write` uses iTerm2's own `write text … newline no`; `close` closes only that window and waits for its processes to exit (iTerm2 keeps a closed session alive for a few seconds, for Undo Close). The AppleScript window id is the CGWindowID that `screencapture -l` takes; `winid` checks it against the window list. |
| `terminal.sh profile-add\|profile-delete\|profiles\|open\|read\|close\|winid` | Terminal.app, always session `term`. `profile-add` saves the profile names, then imports the temporary `fac-test` profile from a generated `<live>/term/fac-test.terminal`. `profile-delete` deletes it with AppleScript and checks the names match. `open` opens a window with `fac-test`; `read` prints `contents` of its tab; `close` hangs up the session's own command first (Terminal asks before closing a window whose command still runs, and AppleScript cannot answer that sheet), so the window closes itself. The AppleScript window id is the CGWindowID. |
| `vscode.sh start\|send\|command\|stop\|winid\|procs\|user-procs` | VS Code. `start` writes the scratch `settings.json` and starts a separate instance (its own `--user-data-dir`) with the driver extension. `send <text> [ms]` types through the driver, one `sendText` per key with a delay. `command <id>` runs one workbench command through the driver and accepts only `workbench.action.toggleSidebarVisibility` and `workbench.action.toggleMaximizedPanel` (they resize the terminal). `stop` asks the driver to quit and waits for the instance's processes to exit. `winid` finds the window by the instance's main pid. The `--user-data-dir` is a short path: VS Code puts its main IPC socket inside it, and under the scratch live dir that path is longer than the 103 bytes a macOS socket path may hold, so the instance would exit at start. `start` makes `mktemp -d /tmp/fv.XXXXXX` holding one symlink, `d` -> `<live>/vsc`, and passes `<tmp>/d`; all data stays in `<live>/vsc`. `stop` removes the link and the dir (`rm`, then `rmdir`) once the instance's processes, found by the short path, are gone. |
| `vscode-driver/` | A minimal extension. On startup it opens a terminal with the `fac` profile, shows it, and polls `<live>/vsc/cmd.json`: `{"send": …}` calls `terminal.sendText(text, false)`; `{"command": "<id>"}` runs one of those two allowlisted ids; `{"quit": true}` runs `workbench.action.quit`. |
| `zed.sh prepare\|start` | Zed. `prepare` writes `<live>/zed-ws/.zed/settings.json` with `terminal.shell.with_arguments` = `{program: entry.sh, args: [<live>, "zed"]}`; `start` opens that folder in a new window under `env -i`. The user opens the terminal panel. The folder then shows in Zed's recent projects. |
| `screenshot.sh <window-id> <out.png> [crop x y w h]` | `screencapture -o -x -l <id>`, then an optional crop with `sips`, in image pixels (2x points on Retina). |
| `winlist.swift` | Lists on-screen windows by owner or pid, with their CGWindowIDs. Built once into `<live>/bin`. |
| `common.sh` | Sourced by the scripts: `$live`, `$entry`, `winlist`, and `osa`, which stops on a macOS permission error instead of working round it. |

## Safety rules

- Open only your own windows and close them when done. Never close, type into
  or read a window you did not open.
- Never quit Terminal.app, iTerm2, VS Code or Zed. The user has their own
  windows and instances.
- A terminal that is not running is started under `env -i` (only `HOME` and
  `PATH`), through an AppleScript `launch`, which opens no default window. An
  app that is already running keeps the environment it was started with;
  `session.json` shows whether `Q_TERM`, `KERO_*` or `WARP_*` leaked in.
- Screenshots: only `screencapture -l <your window id>`, never the whole
  screen. Commit only cropped images.
- Terminal.app: only `fac-test` is ever created, and it is deleted with
  AppleScript (`delete settings set "fac-test"`) at the end of every step that
  creates it. Never `defaults write` or edit `com.apple.Terminal`.
- VS Code: only the isolated instance, stopped through its driver. Never
  `quit app "Visual Studio Code"`, never kill. Check `vscode.sh user-procs`
  is the same before and after.
- Zed: only the scratch folder's `.zed/settings.json`.
- If an AppleScript call fails with a permission error (-1743, -1744, 1002,
  "not authorized"), stop and ask the user to grant it.
- Run `root-check.sh` on every session before using it.

## Order of use

```sh
export FAC_LIVE_DIR=<scratch>/live
printf '/bin/cat\n-v\n' > "$FAC_LIVE_DIR/<session>/cmd"   # the command under test

# iTerm2
tools/live/iterm.sh open it
tools/live/root-check.sh "$(sed -n 's/.*"tty": "\(.*\)".*/\1/p' "$FAC_LIVE_DIR/it/session.json")"
tools/live/type.sh "$FAC_LIVE_DIR" it 'hello\e[A\t'
tools/live/iterm.sh read it
tools/live/screenshot.sh "$(tools/live/iterm.sh winid it)" "$FAC_LIVE_DIR/it/shot.png"
tools/live/iterm.sh close it

# Terminal.app (session "term")
tools/live/terminal.sh profile-add
tools/live/terminal.sh open        # then root-check, type.sh … term, read, screenshot
tools/live/terminal.sh close
tools/live/terminal.sh profile-delete   # prints the names before and after

# VS Code (session "vsc")
tools/live/vscode.sh user-procs > before.txt
tools/live/vscode.sh start
tools/live/vscode.sh send 'hello' 30
tools/live/screenshot.sh "$(tools/live/vscode.sh winid)" "$FAC_LIVE_DIR/vsc/shot.png"
tools/live/vscode.sh stop
tools/live/vscode.sh user-procs | diff before.txt -

# Zed (session "zed"); the user opens the terminal panel
tools/live/zed.sh prepare
tools/live/zed.sh start
```
