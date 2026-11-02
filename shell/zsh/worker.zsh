# fac's compsys worker: a hidden `zsh -f -i` that fac keeps warm on its own
# PTY and asks what zsh's own completion system would complete for a
# command line.
#
# fac starts it as `zsh -f -i -c 'source <this file>'`. Commands come in on
# the pipe at fd $FAC_WORKER_IN and results go out on the pipe at fd
# $FAC_WORKER_OUT, one line each. The PTY is only there so ZLE can run;
# fac discards whatever is drawn on it.
#
# fac -> worker (fields percent-encoded: %, \, ;, space, every byte below
# 0x20 and 0x7f; a list is its elements, each also with , as %2C, joined
# with ,):
#   ctx <key> <value>   path, fpath, options, functions (lists); zstyles,
#                       aliases
#   init <dump>         compinit -C on <dump> if it exists, else a full
#                       compinit that writes it (also when the dump defined
#                       no completions); then `ready`
#   q <id> <cwd> <lbuffer> <rbuffer>
# `ctx aliases` and `ctx functions` that come before `init` are held until
# compinit is done: a stub for the user's `compinit` (a function in any
# shell that ran it) would stand in for compinit itself.
# worker -> fac (fields percent-encoded: %, ;, space, every byte below 0x20
# and 0x7f):
#   ready <compinit-ms> <completions> <rebuilt>
#                       completions: how many commands compinit defined;
#                       rebuilt: 1 if the dump gave none and was rebuilt
#   c <id> <word>;<display>;<description>;<group>;<suffix>;<flags>;<iprefix>;<prefix>;<apre>;<hpre>;<hsuf>
#   d <id> <count> <elapsed-ms>
#   err <id> <code>     cwd (cd failed), int (interrupted), init (not ready)
# Flags: Q (-Q, verbatim), f (-f, file), S (-S was given, even empty).
# apre, hpre, hsuf: compadd's -P, -p and -s, empty when not given.
#
# The query path is forkless except for what completers themselves run.

zmodload zsh/datetime zsh/zutil 2>/dev/null

# The PTY is for ZLE only: no echo, and ^S/^Q reach no one.
stty -echo -ixon 2>/dev/null
unsetopt flow_control beep
PS1= PS2= RPS1= HISTFILE= SAVEHIST=0

# The completion options fac mirrors from the user's shell.
typeset -ga _fac_allowed=(always_to_end auto_list auto_param_slash
  auto_remove_slash case_glob complete_aliases complete_in_word
  extended_glob glob_complete glob_dots list_types mark_dirs
  menu_complete nomatch)
# The names given a stub function for the user's functions.
typeset -ga _fac_stubs
# zsh's special functions: a stub of one would be run (as a hook or a trap).
typeset -ga _fac_special=(chpwd periodic precmd preexec zshaddhistory zshexit
  zsh_directory_name command_not_found_handler)
# The id of the query being completed, empty between queries.
typeset -g _fac_qid=
# The records of the query being completed.
typeset -ga _fac_records
typeset -ga _fac_reply
typeset -g _fac_out
typeset -gi _fac_ready=0
# The `ctx` lines held until `init` is done.
typeset -ga _fac_held
typeset -g _fac_buf

# Percent-encode each argument as a field to fac, into $_fac_reply.
_fac_encode() {
  _fac_reply=("${(@)argv//\%/%25}")
  _fac_reply=("${(@)_fac_reply//;/%3B}")
  _fac_reply=("${(@)_fac_reply// /%20}")
  _fac_reply=("${(@)_fac_reply//$'\n'/%0A}")
  _fac_reply=("${(@)_fac_reply//$'\r'/%0D}")
  _fac_reply=("${(@)_fac_reply//$'\t'/%09}")
  _fac_reply=("${(@)_fac_reply//$'\e'/%1B}")
  _fac_reply=("${(@)_fac_reply//$'\a'/%07}")
  [[ ${(j::)_fac_reply} == *[[:cntrl:]]* ]] || return 0
  # A rarer control byte is left: encode char by char.
  local -i i
  for (( i = 1; i <= $#_fac_reply; i++ )); do
    if [[ $_fac_reply[i] == *[[:cntrl:]]* ]]; then
      _fac_encode_controls "$_fac_reply[i]"
      _fac_reply[i]=$_fac_out
    fi
  done
}

# $1 with every byte below 0x20 and 0x7f as %XX, into $_fac_out.
_fac_encode_controls() {
  local s=$1 c hex
  local -i i n
  _fac_out=
  for (( i = 1; i <= $#s; i++ )); do
    c=$s[i]
    n=$(( #c ))
    if (( n < 0x20 || n == 0x7f )); then
      printf -v hex '%%%02X' $n
      _fac_out+=$hex
    else
      _fac_out+=$c
    fi
  done
}

# Decode each argument (a field from fac), into $_fac_reply.
_fac_decode() {
  local word
  _fac_reply=()
  for word; do
    printf -v word '%b' "${word//\%/\\x}"
    _fac_reply+=("$word")
  done
}

# Decode the list field $1 into $_fac_reply. The empty field is the empty list.
_fac_decode_list() {
  if [[ -z $1 ]]; then
    _fac_reply=()
  else
    _fac_decode "${(@s:,:)1}"
  fi
}

# The styles the capture needs, over the user's: one display per match,
# `word  -- description`, instead of matches lined up in columns with the
# descriptions on rows of their own.
_fac_styles() {
  zstyle ':completion:*' list-grouped false
  zstyle ':completion:*' list-separator --
}

_fac_send() {
  print -r -u $FAC_WORKER_OUT -- "$@"
}

# ctx <key> <value>
_fac_ctx() {
  local key=$1 o
  case $key in
    (path)
      _fac_decode_list "$2"
      path=("$_fac_reply[@]")
      ;;
    (fpath)
      _fac_decode_list "$2"
      fpath=("$_fac_reply[@]")
      ;;
    (options)
      _fac_decode_list "$2"
      for o in $_fac_allowed; do
        # The shell reports option names the way $options spells them.
        if (( $_fac_reply[(Ie)${o//_/}] )); then
          setopt $o
        else
          unsetopt $o
        fi
      done
      ;;
    (zstyles)
      _fac_decode "$2"
      zstyle -d
      eval "$_fac_reply[1]" 2>/dev/null
      _fac_styles
      ;;
    (aliases)
      _fac_decode "$2"
      unalias -m '*' 2>/dev/null
      eval "$_fac_reply[1]" 2>/dev/null
      ;;
    (functions)
      # A stub `name() { : }` for each of the user's functions that is
      # nothing here yet, so `_command_names` offers it. Set through
      # $functions: no eval or fork per name.
      _fac_decode_list "$2"
      (( $#_fac_stubs )) && unfunction -- $_fac_stubs 2>/dev/null
      _fac_stubs=()
      for o in $_fac_reply; do
        (( ${+functions[$o]} || ${+builtins[$o]} || ${+aliases[$o]} )) && continue
        (( $reswords[(Ie)$o] || $_fac_special[(Ie)$o] )) && continue
        [[ $o == TRAP* ]] && continue
        functions[$o]=:
        _fac_stubs+=("$o")
      done
      ;;
  esac
}

# init <dump>
_fac_init() {
  _fac_decode "$1"
  local dump=$_fac_reply[1] ms line
  local -i rebuilt=0
  local -F start=$EPOCHREALTIME
  autoload -Uz compinit
  if [[ -s $dump ]]; then
    compinit -C -d "$dump"
    # A dump that defines nothing (cut short, or not a dump) is rebuilt.
    if (( ! $#_comps )); then
      rm -f -- "$dump"
      compinit -i -d "$dump"
      rebuilt=1
    fi
  else
    # -i: insecure directories are skipped instead of asked about.
    compinit -i -d "$dump"
  fi
  printf -v ms '%.1f' $(( (EPOCHREALTIME - start) * 1000 ))
  _fac_ready=1
  for line in "$_fac_held[@]"; do
    _fac_dispatch "$line"
  done
  _fac_held=()
  _fac_send "ready $ms $#_comps $rebuilt"
}

# Record the matches compsys adds, then add them as usual. Modelled on
# fzf-tab's -ftb-compadd and Valodim/zsh-capture-completion.
compadd() {
  local -A apre hpre hsuf dscrs _oad _mesg asuf
  local -a isfile verbatim _opts __ expl
  zparseopts -a _opts P:=apre p:=hpre d:=dscrs X+:=expl O:=_oad A:=_oad D:=_oad f=isfile \
    i: S:=asuf s:=hsuf I: x:=_mesg r: R: W: F: M+: E: q e Q=verbatim n U C \
    J:=__ V:=__ a=__ l=__ k=__ o::=__ 1=__ 2=__
  # Only collecting matches (-O, -A, -D), or nothing to capture for.
  if (( $#_oad )) || [[ -z $_fac_qid ]]; then
    builtin compadd "$@"
    return
  fi
  local -a __hits __dscr
  if (( $#dscrs == 1 )); then
    __dscr=("${(@P)${(v)dscrs}}")
  fi
  builtin compadd -A __hits -D __dscr "$@"
  local ret=$?
  if (( ! $#__hits )); then
    return $ret
  fi
  local group=${expl[2]:-$curtag} flags= suffix= word display rest description
  (( $#verbatim )) && flags+=Q
  (( $#isfile )) && flags+=f
  if (( ${+asuf[-S]} )); then
    flags+=S
    suffix=$asuf[-S]
  fi
  # The fields every match of this call shares, encoded once.
  _fac_encode "$group" "$suffix" "$flags" "$IPREFIX" "$PREFIX" \
    "$apre[-P]" "$hpre[-p]" "$hsuf[-s]"
  local shared=${(j:;:)_fac_reply}
  local -i i
  for (( i = 1; i <= $#__hits; i++ )); do
    word=$__hits[i]
    [[ -n $word ]] || continue
    display=${__dscr[i]//$'\n'}
    [[ -n $display ]] || display=$word
    # `_describe` displays look like `-l  -- long listing`.
    description=
    rest=${display#"$word"}
    if [[ $rest == *' -- '* ]]; then
      description=${rest#*' -- '}
    fi
    _fac_encode "$word" "$display" "$description"
    _fac_records+=("c $_fac_qid ${(j:;:)_fac_reply};$shared")
  done
  builtin compadd "$@"
}

_fac_capture_fn() {
  unset 'compstate[vared]'
  _main_complete
  compstate[insert]=''
  compstate[list]=''
}

# q <id> <cwd> <lbuffer> <rbuffer>
_fac_query() {
  local id=$1 ms
  if (( ! _fac_ready )); then
    _fac_send "err $id init"
    return
  fi
  _fac_decode "$2" "$3" "$4"
  if ! cd -q -- "$_fac_reply[1]" 2>/dev/null; then
    _fac_send "err $id cwd"
    return
  fi
  local -F start=$EPOCHREALTIME
  _fac_records=()
  _fac_qid=$id
  LBUFFER=$_fac_reply[2]
  RBUFFER=$_fac_reply[3]
  zle _fac_capture
  _fac_qid=
  BUFFER=
  printf -v ms '%.1f' $(( (EPOCHREALTIME - start) * 1000 ))
  _fac_records+=("d $id $#_fac_records $ms")
  print -rl -u $FAC_WORKER_OUT -- "$_fac_records[@]"
  _fac_records=()
}

# One line from fac. Unknown commands are ignored.
_fac_dispatch() {
  local -a words=("${(@s: :)1}")
  case $words[1] in
    (ctx)
      (( $#words == 3 )) || return 0
      if (( ! _fac_ready )) && [[ $words[2] == (aliases|functions) ]]; then
        _fac_held+=("$1")
      else
        _fac_ctx "$words[2]" "$words[3]"
      fi
      ;;
    (init) (( $#words == 2 )) && _fac_init "$words[2]" ;;
    (q) (( $#words == 5 )) && [[ $words[2] == <-> ]] && _fac_query "$words[2]" "$words[3]" "$words[4]" "$words[5]" ;;
  esac
}

# The `zle -F -w` handler for fd $FAC_WORKER_IN.
_fac_on_input() {
  local line
  if ! IFS= read -r -u $FAC_WORKER_IN line; then
    # EOF: fac is gone. A handler left registered would spin on it, and
    # neither `exit` nor send-break ends vared from a handler.
    zle -F $FAC_WORKER_IN
    kill -HUP $$
    return 0
  fi
  _fac_dispatch "$line"
  while IFS= read -t 0 -r -u $FAC_WORKER_IN line; do
    _fac_dispatch "$line"
  done
  return 0
}

_fac_styles

# fac sends ^C when a query takes too long. It aborts the query; between
# queries it is ignored, since at the top level of `-c` it would end the
# worker.
TRAPINT() {
  [[ -n $_fac_qid ]] && return $(( 128 + $1 ))
  return 0
}

# Until `init`, commands are read without ZLE.
while (( ! _fac_ready )) && IFS= read -r -u $FAC_WORKER_IN _fac_buf; do
  _fac_dispatch "$_fac_buf"
done
(( _fac_ready )) || exit 0

zle -N _fac_on_input
zle -C _fac_capture complete-word _fac_capture_fn
zle -F -w $FAC_WORKER_IN _fac_on_input
while :; do
  _fac_buf=
  vared -p '' _fac_buf
  if [[ -n $_fac_qid ]]; then
    # ^C from fac ended a query that took too long.
    _fac_send "err $_fac_qid int"
    _fac_qid=
    _fac_records=()
  fi
done
