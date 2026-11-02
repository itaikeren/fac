# For tests that send a bash hook's context themselves (tests/bash_worker.rs
# seeds the completion worker with it): __fact_encode and __fact_context,
# which encode and send a 7791 as fac.bash does, with the session token the
# real hook took (__fac_token, set before the login files run). Sourced from
# the test's ~/.bash_profile; the real hook loads after it.

__fact_seq=0

# The control bytes and DEL, and their escapes.
__fact_ctl=()
__fact_esc=()
for __fact_i in {1..31} 127; do
  printf -v __fact_hex '%02X' "$__fact_i"
  printf -v '__fact_ctl[__fact_i]' "\\x$__fact_hex"
  __fact_esc[__fact_i]=%$__fact_hex
done
unset __fact_i __fact_hex

# $1 encoded as a 779x field into __fact_s: %, ;, control bytes, DEL.
__fact_encode() {
  local s=${1//%/%25} i
  s=${s//;/%3B}
  if [[ $s == *[$'\x01'-$'\x1f\x7f']* ]]; then
    for i in "${!__fact_ctl[@]}"; do
      s=${s//"${__fact_ctl[i]}"/${__fact_esc[i]}}
    done
  fi
  __fact_s=$s
}

# 7791 for key $1 with the encoded value $2.
__fact_context() {
  printf '\e]7791;1:%s;%d;%s;%s\a' "${__fac_token-}" $((++__fact_seq)) "$1" "$2"
}
