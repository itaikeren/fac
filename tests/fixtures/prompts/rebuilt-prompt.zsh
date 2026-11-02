# A prompt built in a precmd hook that keeps itself last, so fac's 133;A/B
# wrap is overwritten at every prompt and the prompt has no marks.
# Stands for powerlevel10k (v1.20.0): `_p9k_precmd` sets PROMPT at every
# prompt (internal/p10k.zsh:7063) and moves itself back to the end of
# precmd_functions (:7072-7074). zsh runs the hooks from a copy of the list
# taken at the start of each round, so no order fac picks can come after it.
_fixture_rebuild() {
  PS1=$'top line\n%% '
  if [[ $precmd_functions[-1] != _fixture_rebuild ]]; then
    precmd_functions=(${precmd_functions:#_fixture_rebuild} _fixture_rebuild)
  fi
}
precmd_functions+=(_fixture_rebuild)
