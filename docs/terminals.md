# Terminals

fac draws its popup inside the terminal, so it works the same in each of these. Tested on macOS 26.3:

| Terminal | Version | Notes |
|---|---|---|
| Terminal.app | 2.15 | No synchronized output. fac writes each cell of the popup once per keystroke, so it does not flicker. |
| iTerm2 | 3.7.6 | Synchronized output. |
| VS Code | 1.104.3 | No synchronized output in this version; drawn as in Terminal.app. |
| Zed | 1.25.2 | With Zed's default font the popup's description line is not italic: Zed draws no italic there. |
| Ghostty | libghostty 1.3.2-dev | Synchronized output. |

fac steps aside in Warp, which has its own autocomplete.

## Starting fac by hand

`fac init` is the easy way. To set a terminal up yourself instead, make `fac` its startup command, with the full path (`/opt/homebrew/bin/fac` on Apple Silicon Macs, `/usr/local/bin/fac` on Intel Macs, `~/.cargo/bin/fac` from cargo):

- **Terminal.app:** Settings, General, "Shells open with": Command (complete path), and enter the path.
- **iTerm2:** Settings, Profiles, General, Command: Custom Shell, and enter the path.
- **Ghostty:** `command = /opt/homebrew/bin/fac` in `~/.config/ghostty/config`.
- **VS Code:** in `settings.json`: `"terminal.integrated.profiles.osx": { "fac": { "path": "/opt/homebrew/bin/fac" } }` and `"terminal.integrated.defaultProfile.osx": "fac"`.
- **Zed:** `"terminal": { "shell": { "program": "/opt/homebrew/bin/fac" } }` in `settings.json`.

## Keep your terminal's shell integration

When fac is the terminal's startup command, the terminal does not see a shell, so it does not set up its own shell integration. fac sends the prompt marks itself (OSC 133). To get the rest of a terminal's integration, source the terminal's own script from your shell's startup file, as the terminal's docs describe. The examples below are for zsh's `~/.zshrc`; the terminals' docs give the bash and fish versions.

- **VS Code:** with fac as the profile's command, VS Code does not inject its integration (no `VSCODE_*` variables reach the shell in 1.104.3). Add the line from [VS Code's docs](https://code.visualstudio.com/docs/terminal/shell-integration) (Manual installation) to `~/.zshrc`:

      [[ "$TERM_PROGRAM" == "vscode" ]] && . "$(code --locate-shell-integration-path zsh)"

  VS Code's docs list what it brings (command decorations, command navigation, sticky scroll, run recent command) and suggest also setting `terminal.integrated.shellIntegration.enabled` to `false` with a manual install. In fac's tests the script works behind fac; the features themselves were not checked in VS Code. VS Code's own terminal suggestions are off by default in 1.104. Turned on, they may show beside fac's popup (not tested), so keep `"terminal.integrated.suggest.enabled": false`.
- **iTerm2:** the install by hand from [iTerm2's docs](https://iterm2.com/documentation-shell-integration.html):

      curl -L https://iterm2.com/shell_integration/zsh -o ~/.iterm2_shell_integration.zsh

  and, as the docs say, at the end of `~/.zshrc`:

      source ~/.iterm2_shell_integration.zsh

  In fac's tests the script works behind fac: its reports reach the terminal, each prompt has one prompt mark, and the popup is placed and cleared as without it. Not tested in iTerm2 itself.
- **Ghostty:** Ghostty sets up its integration only when the command's name is a shell's, which `fac` is not. Source its zsh script from `~/.zshrc` ([Ghostty's docs](https://ghostty.org/docs/features/shell-integration) give the path):

      if [ -n "${GHOSTTY_RESOURCES_DIR}" ]; then
        builtin source "${GHOSTTY_RESOURCES_DIR}/shell-integration/zsh/ghostty-integration"
      fi

  Not tested: Ghostty's script was not run behind fac.
- **powerlevel10k:** powerlevel10k draws its own prompt over fac's prompt marks. To keep your terminal's prompt features (jump to prompt, VS Code's gutter marks), add `typeset -g POWERLEVEL9K_TERM_SHELL_INTEGRATION=true` to `~/.p10k.zsh`.

## Shells

fac works with zsh, bash 5.3+ and fish 4.3+, whichever `$SHELL` names. With another shell, or an older bash or fish (such as macOS's own `/bin/bash` 3.2), fac starts it plain.

- **zsh:** fac starts zsh as a login shell and loads its hook with a ZDOTDIR shim; your own startup files run as usual.
- **bash:** fac reads `/etc/profile` and the first of `~/.bash_profile`, `~/.bash_login` and `~/.profile`, then its hook. The shell's own completions come from bash-completion, when your startup files load it, without descriptions.
- **fish:** fish reads its usual config, then fac's hook. fish's own completions come with their descriptions.

bash and fish have no hook that runs on every key, so fac types a private key after yours, which the shell answers with the line. fac keeps that key out of programs, bash's history search, your own key widgets and the shell's own questions.
