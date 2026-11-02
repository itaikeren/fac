# A spec function that waits in a command for longer than a query may
# take; ^C ends the command, and the function returns (bash_worker tests).
_fac_test_slow() {
  sleep 30
  COMPREPLY=(late)
}
complete -F _fac_test_slow slowcmd
