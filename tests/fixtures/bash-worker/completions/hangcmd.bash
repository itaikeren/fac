# A spec function that never returns, whatever ^C does to its commands
# (bash_worker tests).
_fac_test_hang() {
  while :; do
    sleep 0.05
  done
}
complete -F _fac_test_hang hangcmd
