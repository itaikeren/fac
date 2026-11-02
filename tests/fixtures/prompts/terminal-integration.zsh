# What a terminal's own shell integration does to the prompt, written from
# what VS Code's script (1.104.3) and iTerm2's (shell integration 15) were
# seen to do behind fac; neither is copied:
# - a precmd hook that puts the previous command's status in a D mark and
#   wraps PS1 in its own A and B marks (OSC 633 here; iTerm2 uses the same
#   133 marks as fac), once per command run (an empty line leaves PS1 as it
#   is), and a preexec hook that sets PS1 back and writes C;
# - a plain `zle-line-init` widget of its own, defined before fac's hooks go
#   in, as oh-my-zsh's key bindings do.
# The hooks are added here, in .zshrc, so at the first prompt they run after
# fac's `_fac_init` and, from the second on, between fac's two precmd hooks.
typeset -g _ti_prior
typeset -gi _ti_after_command=1
_ti_precmd() {
  print -rn -- $'\e]633;D;'"$?"$'\a'
  (( _ti_after_command )) || return 0
  _ti_after_command=0
  _ti_prior=$PS1
  PS1=$'%{\e]633;A\a%}'$PS1$'%{\e]633;B\a%}'
}
_ti_preexec() {
  PS1=$_ti_prior
  _ti_after_command=1
  print -rn -- $'\e]633;C\a'
}
_ti_line_init() {
  print -rn -- $'\e]633;P;LineInit\a'
}
precmd_functions+=(_ti_precmd)
preexec_functions+=(_ti_preexec)
zle -N zle-line-init _ti_line_init
