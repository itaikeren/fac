# A prompt updated while a line is typed, with no key pressed.
# Stands for powerlevel10k's git status (v1.20.0 with gitstatusd v1.5.4,
# VCS_MAX_SYNC_LATENCY_SECONDS reached): the answer comes through a `zle -F`
# handler that ends in `zle reset-prompt`, and zsh draws the prompt again
# (up to its start, the prompt, ED, the line). Other async prompts reprint a
# line themselves: up, the line, back down, no ED.
# A line written to the FIFO next to the fixture cwd fires it: `ed` for
# reset-prompt, anything else for the reprint.
setopt prompt_subst
_fixture_top='top line'
PS1=$'${_fixture_top}\n%% '
/usr/bin/mkfifo "${PWD:h}/async.fifo"
exec {_fixture_fd}<>"${PWD:h}/async.fifo"
_fixture_async() {
  local line
  IFS= read -r -u $1 line || return 0
  _fixture_top="top line ($line)"
  if [[ $line == ed ]]; then
    zle reset-prompt
  else
    # Up to the first line, reprint it, then back to the cursor's column.
    print -rn -- $'\e[A\r'"$_fixture_top"$'\e[K\e[B\e['"$(( 3 + CURSOR ))"'G'
  fi
}
zle -N _fixture_async
zle -F -w $_fixture_fd _fixture_async
