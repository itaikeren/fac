# A spec whose matches are file names, quoted by readline (bash_worker
# tests).
_fac_test_files() {
  compopt -o filenames
  compgen -V COMPREPLY -f -- "$2"
}
complete -F _fac_test_files facfiles
