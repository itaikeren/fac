# A transient prompt: when a line is done, its prompt is drawn again smaller,
# and the next prompt is the full one.
# Stands for powerlevel10k's TRANSIENT_PROMPT=always (v1.20.0): a
# zle-line-finish hook sets a short PROMPT and runs `zle reset-prompt`
# (zsh goes up to the prompt's start, draws it, ED); a precmd hook puts the
# full prompt back.
_fixture_full=$'top line\n%% '
PS1=$_fixture_full
_fixture_transient() {
  PS1='> '
  zle reset-prompt
}
_fixture_restore() {
  PS1=$_fixture_full
}
autoload -Uz add-zle-hook-widget
add-zle-hook-widget line-finish _fixture_transient
precmd_functions+=(_fixture_restore)
