# A spec that asks for no space after its matches (bash_worker tests).
_fac_test_nospace() {
  compopt -o nospace
  COMPREPLY=(--name= --other=)
}
complete -F _fac_test_nospace facnospace
