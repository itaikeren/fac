# An early prompt printed while .zshrc still runs, before fac's hooks are in
# (they go in at the first prompt), then erased and drawn again.
# Stands for powerlevel10k's instant prompt (v1.20.0, INSTANT_PROMPT verbose
# and quiet): drawn from a saved cursor (DECSC), the cursor put back (DECRC)
# and hidden; at the first prompt the early one is erased (ED) and the real
# prompt drawn where it was.
print -rn -- $'\e7early %% \e[?25l\e8'
_fixture_instant_done() {
  print -rn -- $'\e[J\e[?25h'
  precmd_functions=(${precmd_functions:#_fixture_instant_done})
}
precmd_functions+=(_fixture_instant_done)
