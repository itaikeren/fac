# Known limits

The cases fac does not handle yet. Most are rare.

## Everywhere

- `exec zsh`, `exec bash`, `exec fish` and `omz reload` drop fac's hooks. A shell started inside fac's shell (`bash` in bash, say) gets no popup; it comes back when you exit it.
- With `fac init` set up, a bash or fish started inside fac's shell starts as a login shell, so it reads your login files too.
- Slow completions (for example git branches) arrive a moment after the popup opens.
- Very large output (`cat` of hundreds of MB) is somewhat slower than a plain shell: it passes through a second pseudo-terminal.
- Once a command has printed more than 512 KiB, fac stops modelling its output and keeps only the last 256 KiB. Colours and modes (scroll regions, origin and insert modes) set in the skipped part are not modelled; the cursor is corrected at the next prompt. Smaller output is modelled in full.
- Commands started from zsh and fish inherit fac's command pipe (fd 63); bash closes it for its commands.
- The popup sits under the word you are typing. With the shell in the C locale, or with invalid bytes or flag emoji in the word, it can sit a few columns off.
- fac reads the terminal's colours once, when it starts. After switching the terminal between a light and a dark theme, open a new window for a popup that matches.
- A `HISTFILE` that is a named pipe would freeze fac.
- macOS only.

## Resizing

- In Terminal.app and iTerm2, narrowing the window while the popup is open can leave pieces of the popup above the prompt. The terminal rewraps the popup's rows and moves the cursor off its line, and zsh's redraw clears only from the cursor down. Widening, VS Code, and narrowing with the popup closed are clean. Ctrl-L clears what is left.
- When the window's width changes, zsh's own redraw clears the old popup. Where zsh does not clear (a terminal without the `ed` capability, `setopt SINGLE_LINE_ZLE`, or a window under 3 rows), pieces of it stay until output scrolls them away or Ctrl-L.

## Terminal integration

- With iTerm2's shell integration script sourced, the terminal receives each command's start and exit marks (OSC 133;C and 133;D), and the first prompt's end mark (133;B), twice: once from the script and once from fac. fac logs no warning; the terminal's side was not tested.

## bash and fish

- bash: after Ctrl-R (bash's history search) or Ctrl-X Ctrl-E (edit the line in your editor), the popup stays closed until the next prompt.
- bash: two Ctrl-W (or other kill keys) more than about 150 ms apart are kept as two kills, so Ctrl-Y gives back only the last one (Alt-Y then gives the other). In the same way, Alt-. pressed again more than about 150 ms later inserts the same word again instead of the one before it.
- bash: after a Tab that bash completes itself, fac sees the new line only with your next key.
- bash and fish: after an Enter typed while a command still runs, the popup stays off until the shell has no typed-ahead line left, usually the prompt after that line runs. fish keeps typed-ahead lines in a buffer of its own, so with two or more typed ahead the popup comes back once fish has run them all. At such a prompt, bash waits for fac to count its unread input (well under a millisecond). After typing ahead with Backspace, Ctrl-U or other control keys, or more than about 1000 characters, the popup can stay off for one more line.
- bash: if the shell's output does not reach the terminal (after `exec >/dev/null`, say), the prompt after a line typed ahead waits until Ctrl-C, which also drops that line.
- bash: control keys typed ahead into a full-screen program just as it exits, and not read by it (Ctrl-V, Ctrl-R, Ctrl-O and the like), are handled by the terminal's line settings at the next prompt, not by readline.
- bash and zsh: `ps -E` shows the session token in the shell's starting environment, to processes of your own user only.
- bash and fish: `set` lists the hook's variables, the session token among them (zsh hides it). The token only tells fac which messages come from the hook; it is new each session.
- fish: Alt-E (edit the line in your editor), or a key bound to a function of your own (an fzf widget, say), closes the popup until the next prompt.
- bash and fish: if the shell is too busy to answer fac for about 60 ms, the popup stops for the rest of that line and comes back at the next prompt.
