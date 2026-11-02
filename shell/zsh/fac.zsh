# fac's zsh integration, sourced by fac's ZDOTDIR shim (.zshenv) in
# interactive shells.
#
# It tells fac, over private OSC sequences that fac strips from the output:
#   7790  the command line, on every edit (buffer report)
#   7791  shell context: PATH, fpath, history file, aliases, functions,
#         options, cwd, the exported environment
#   7792  each executed command
#   7793  diagnostics
# It wraps the prompt in OSC 133 markers, reports the cwd with OSC 7, and
# takes buffer edits from fac over the pipe at fd $FAC_CMD_FD.
#
# Message format: ESC ] 779N ; 1:<token> ; <seq> ; <field> ... BEL. The
# token is the session's, from FAC_TOKEN (taken by fac's .zshenv shim): fac
# drops any message without it (with none, the field is the bare version, as
# before the token). Fields are percent-encoded: %, ;, every
# byte below 0x20 and 0x7f. UTF-8 passes raw. A list is its elements
# encoded, each also with , as %2C, joined with ,.
#
# Everything on the per-keystroke path is forkless: no $(...), no external
# commands. Everything that writes is gated on FAC_ACTIVE naming this tty,
# so a FAC_ACTIVE leaked into another terminal's shell stays silent.

(( ${+_fac_loaded} )) && return 0
typeset -g _fac_loaded=1

# The session token, unexported and hidden from listings; the shim took it
# already, unless this file was sourced without it. $_fac_v is every
# message's first field: the version and the token. Every function that
# prints it runs with xtrace off, so a trace never shows it.
() {
  setopt localoptions noxtrace
  if (( ! ${+_fac_token} )); then
    typeset -gH _fac_token=${FAC_TOKEN-}
    typeset -g +x _fac_token
  fi
  unset FAC_TOKEN
  typeset -gH _fac_v=1${_fac_token:+:$_fac_token}
  typeset -g +x _fac_v
}

# One counter for every 779x message.
typeset -gi _fac_seq=0
# The seq of the latest 7790 report; an apply for any other seq is stale.
typeset -gi _fac_report_seq=0
# Set by zle-isearch-update, cleared by zle-isearch-exit and at every new
# line ($KEYMAP stays `main` during an incremental search, and Ctrl-C ends
# a search without zle-isearch-exit).
typeset -gi _fac_isearch=0
# Set after the first prompt: no 133;D before it.
typeset -gi _fac_prompted=0
# Context key -> the encoded value last sent.
typeset -gA _fac_sent
typeset -ga _fac_reply
typeset -g _fac_out
typeset -g _fac_mark_a=$'%{\e]133;A\a%}'
typeset -g _fac_mark_b=$'%{\e]133;B\a%}'
typeset -g _fac_mark_p=$'%{\e]133;P;k=s\a%}'

# FAC_ACTIVE is <fac pid>:<tty of this shell>.
_fac_on() {
  [[ -n $FAC_ACTIVE && ${FAC_ACTIVE#*:} == $TTY ]]
}

# Percent-encode each argument as a 779x field, into $_fac_reply.
_fac_encode() {
  _fac_reply=("${(@)argv//\%/%25}")
  _fac_reply=("${(@)_fac_reply//;/%3B}")
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

# The arguments encoded as one list field, into $_fac_out.
_fac_encode_list() {
  _fac_encode "$@"
  _fac_out=${(j:,:)"${(@)_fac_reply//,/%2C}"}
}

# The last line scanned by _fac_undrawn: its text, how many characters it
# has, the COMBINING_CHARS flag with the text as `<flag>:<text>`, the indices
# found with their encoding, and whether the last character takes a joined
# zero-width one. A line that starts with the text is scanned from its end.
# (Plain globals: a `local` costs more than a keystroke can spare.)
typeset -g _fac_u_key _fac_u_out _fac_u_join
typeset -gi _fac_u_len=0
typeset -ga _fac_u_idx
typeset -g _fac_s_c _fac_s_rest _fac_s_pre _fac_s_run _fac_s_joined
typeset -gi _fac_s_i _fac_s_off _fac_s_found

# $1: LBUFFER, $2: 1 when COMBINING_CHARS is set, else 0. Into $_fac_out: the
# 0-based indices of the characters ZLE does not draw as themselves, runs as
# `a-b`, joined with `,`. ZLE (zle_refresh.c, zrefresh) draws a character as
# itself when it is printable and has a width, as zsh counts them (this
# zsh's own tables, not libc's); with COMBINING_CHARS a zero-width character
# after a graphic base is drawn too, joined to the base. Every other
# character is drawn as `^X` or `<XXXX>`, and fac builds that text itself.
_fac_undrawn() {
  if [[ "$2:$1" == "$_fac_u_key"* ]]; then
    # Typing at the end of the line: only the new characters are looked at.
    _fac_s_rest=${1:$_fac_u_len}
    [[ -z $_fac_s_rest ]] && { _fac_out=$_fac_u_out; return 0 }
    _fac_s_off=$_fac_u_len _fac_s_joined=${_fac_u_join:#0} _fac_s_found=0
    _fac_u_key="$2:$1" _fac_u_len=$#1
  else
    _fac_s_off=0 _fac_s_joined= _fac_s_found=1 _fac_s_rest=$1
    _fac_u_idx=()
    _fac_u_key="$2:$1" _fac_u_len=$#1
  fi
  if (( $#_fac_s_rest <= 8 )); then
    # A few characters (typing): one by one.
    for _fac_s_c in ${(s::)_fac_s_rest}; do
      if [[ $_fac_s_c == [[:graph:]] && ${(m)#_fac_s_c} -gt 0 ]]; then
        # Drawn as itself, and a base for a joined zero-width character.
        _fac_s_joined=1
      elif [[ $_fac_s_c == [[:print:]] && ${(m)#_fac_s_c} -gt 0 ]]; then
        # Drawn as itself (a space), and not a base.
        _fac_s_joined=
      elif [[ $_fac_s_c == [[:print:]] && $2 == 1 && -n $_fac_s_joined ]]; then
        # Zero width: joined to the base before it.
      elif [[ $_fac_s_c == [[:ascii:]] ]]; then
        # A control character in the ASCII text is not reported.
        _fac_s_joined=
      else
        _fac_s_joined=
        _fac_u_idx+=($_fac_s_off)
        _fac_s_found=1
      fi
      _fac_s_off+=1
    done
  else
    setopt localoptions extendedglob
    # Only the runs of non-ASCII characters are looked at, one by one; a
    # printable ASCII character is drawn as itself.
    while [[ $_fac_s_rest == (#b)([[:ascii:]]#)([^[:ascii:]]##)(*) ]]; do
      _fac_s_pre=$match[1] _fac_s_run=$match[2] _fac_s_rest=$match[3]
      (( _fac_s_off += $#_fac_s_pre ))
      # The base before the run, for COMBINING_CHARS (a run that continues
      # the cached line keeps the cached state).
      [[ -n $_fac_s_pre ]] && _fac_s_joined=${${(M)_fac_s_pre[-1]:#[[:graph:]]}:+1}
      _fac_s_i=0
      for _fac_s_c in ${(s::)_fac_s_run}; do
        (( _fac_s_i++ ))
        if [[ $_fac_s_c == [[:graph:]] && ${(m)#_fac_s_c} -gt 0 ]]; then
          _fac_s_joined=1
        elif [[ $_fac_s_c == [[:print:]] && ${(m)#_fac_s_c} -gt 0 ]]; then
          _fac_s_joined=
        elif [[ $_fac_s_c == [[:print:]] && $2 == 1 && -n $_fac_s_joined ]]; then
          :
        else
          _fac_s_joined=
          _fac_u_idx+=($(( _fac_s_off + _fac_s_i - 1 )))
          _fac_s_found=1
        fi
      done
      (( _fac_s_off += $#_fac_s_run ))
    done
    [[ -n $_fac_s_rest ]] && _fac_s_joined=${${(M)_fac_s_rest[-1]:#[[:graph:]]}:+1}
  fi
  _fac_u_join=${_fac_s_joined:-0}
  if (( _fac_s_found )); then
    _fac_encode_runs
    _fac_u_out=$_fac_out
  else
    _fac_out=$_fac_u_out
  fi
}

# $_fac_u_idx as `a-b` runs joined with `,`, into $_fac_out.
_fac_encode_runs() {
  local -i i from=-1 last=-2
  _fac_out=
  for i in $_fac_u_idx; do
    if (( i == last + 1 )); then
      last=$i
      continue
    fi
    (( from >= 0 )) && _fac_run $from $last
    from=$i last=$i
  done
  (( from >= 0 )) && _fac_run $from $last
}

# Append the run $1..$2 to $_fac_out.
_fac_run() {
  _fac_out+="${_fac_out:+,}$1"
  (( $2 > $1 )) && _fac_out+="-$2"
}

# 7790: CONTEXT, CURSOR, flags, PREBUFFER, BUFFER, and the characters ZLE
# does not draw as themselves, only when there are some. $1: extra flags.
_fac_report() {
  setopt localoptions noxtrace
  _fac_on || return 0
  local comb=${${options[combiningchars]:#off}:+1}
  emulate -L zsh
  local flags=$1 undrawn
  (( REGION_ACTIVE )) && flags+=r
  (( _fac_isearch )) && flags+=i
  [[ $KEYMAP == vicmd ]] && flags+=v
  _fac_encode "$CONTEXT" "$PREBUFFER" "$BUFFER"
  if [[ $BUFFER == *[^[:ascii:]]* ]]; then
    # (LBUFFER is cut from BUFFER at each use.)
    local lbuffer=$LBUFFER
    _fac_undrawn "$lbuffer" ${comb:-0}
    undrawn=$_fac_out
    [[ -n $undrawn ]] && undrawn=";$undrawn"
  fi
  _fac_report_seq=$(( ++_fac_seq ))
  print -rn -- $'\e]7790;'"$_fac_v;$_fac_seq;$_fac_reply[1];$CURSOR;$flags;$_fac_reply[2];$_fac_reply[3]$undrawn"$'\a'
}

# 7793 with the code $1.
_fac_diag() {
  setopt localoptions noxtrace
  _fac_on || return 0
  print -rn -- $'\e]7793;'"$_fac_v;$(( ++_fac_seq ));$1"$'\a'
}

_fac_line_init() {
  _fac_isearch=0
  _fac_u_key= _fac_u_len=0 _fac_u_out= _fac_u_join=0
  _fac_u_idx=()
  _fac_report
}

_fac_line_pre_redraw() {
  (( PENDING || KEYS_QUEUED_COUNT )) || _fac_report
}

# `f`: the line is done (Enter, Ctrl-C), so fac can hide the popup at once.
_fac_line_finish() {
  _fac_isearch=0
  _fac_report f
}

_fac_isearch_update() {
  _fac_isearch=1
}

_fac_isearch_exit() {
  _fac_isearch=0
}

# 7791 for key $1 with the encoded value $2, unless that value was sent last.
_fac_send_context() {
  setopt localoptions noxtrace
  (( ${+_fac_sent[$1]} )) && [[ $_fac_sent[$1] == $2 ]] && return 0
  _fac_sent[$1]=$2
  print -rn -- $'\e]7791;'"$_fac_v;$(( ++_fac_seq ));$1;$2"$'\a'
}

# Every context key. $@: the options that are on (read by the caller before
# `emulate`, which changes them locally).
_fac_context() {
  emulate -L zsh
  local -a on=("$@") lines
  local name
  _fac_encode_list "$path[@]"
  _fac_send_context path "$_fac_out"
  _fac_encode "${HISTFILE-}"
  _fac_send_context histfile "$_fac_reply[1]"
  _fac_encode_list "$fpath[@]"
  _fac_send_context fpath "$_fac_out"
  _fac_encode "${ZSH_COMPDUMP-}"
  _fac_send_context compdump "$_fac_reply[1]"
  # `alias -L` lines, built without forking.
  for name in ${(ok)aliases}; do
    if [[ $name == -* ]]; then
      lines+=("alias -- ${(q)name}=${(q)aliases[$name]}")
    else
      lines+=("alias ${(q)name}=${(q)aliases[$name]}")
    fi
  done
  _fac_encode "${(pj:\n:)lines}"
  _fac_send_context aliases "$_fac_reply[1]"
  # The names of the user's functions, for command position (the worker is
  # `zsh -f`): not `_` ones (completion functions, fac's own), nor names
  # with `/` or `:`.
  _fac_encode_list ${${(ok)functions:#_*}:#*[/:]*}
  _fac_send_context functions "$_fac_out"
  _fac_encode_list "$on[@]"
  _fac_send_context options "$_fac_out"
  _fac_encode "$PWD"
  _fac_send_context cwd "$_fac_reply[1]"
  # The exported environment, as NAME=value, for the commands spec
  # generators run (`export` changes it, so it is checked at every prompt).
  lines=()
  for name in ${(ok)parameters[(R)*export*]}; do
    [[ $name == _fac* ]] || lines+=("$name=${(P)name}")
  done
  _fac_encode_list "$lines[@]"
  _fac_send_context env "$_fac_out"
  if (( ! ${+_fac_sent[zstyles]} )); then
    # The one fork, at the first prompt only.
    _fac_encode "$(zstyle -L)"
    _fac_send_context zstyles "$_fac_reply[1]"
  fi
}

# OSC 7 with $PWD; %, ;, space and controls percent-encoded.
_fac_osc7() {
  _fac_on || return 0
  emulate -L zsh
  local p=${PWD//\%/%25}
  p=${p//;/%3B}
  p=${p// /%20}
  if [[ $p == *[[:cntrl:]]* ]]; then
    _fac_encode_controls "$p"
    p=$_fac_out
  fi
  print -rn -- $'\e]7;file://'"$HOST$p"$'\a'
}

# chpwd: OSC 7 for the terminal, and the cwd for fac, which takes no OSC 7
# as its own (any program can print one); a widget may cd with no prompt
# after it. Not from a subshell (zsh runs chpwd there too): its cd leaves
# this shell where it was, and `$(cd …)` would capture the message.
_fac_chpwd() {
  _fac_osc7
  _fac_on || return 0
  emulate -L zsh
  (( ZSH_SUBSHELL )) && return 0
  _fac_encode "$PWD"
  _fac_send_context cwd "$_fac_reply[1]"
}

# Strip fac's markers from the prompts, wherever they are.
_fac_unwrap() {
  PS1=${PS1//"$_fac_mark_a"/}
  PS1=${PS1//"$_fac_mark_b"/}
  PS2=${PS2//"$_fac_mark_p"/}
}

# First in precmd_functions: 133;D with the last command's status.
_fac_precmd_first() {
  local -i ret=$?
  _fac_on || return 0
  emulate -L zsh
  if [[ $precmd_functions[1] != _fac_precmd_first ]]; then
    precmd_functions=(_fac_precmd_first ${precmd_functions:#_fac_precmd_first})
  fi
  (( $precmd_functions[(Ie)_fac_precmd_last] )) || precmd_functions+=(_fac_precmd_last)
  (( _fac_prompted )) && print -rn -- $'\e]133;D;'"$ret"$'\a'
  _fac_prompted=1
}

# Late in precmd_functions: wrap the prompt in 133;A/B (PS2 in 133;P),
# report the cwd and the context. The wrap is textual and idempotent, so it
# stays exactly-once whatever another hook (Kiro) did to PS1 since.
_fac_precmd_last() {
  _fac_on || return 0
  local -a on=(${(ok)options[(R)on]})
  emulate -L zsh
  _fac_unwrap
  # A lone trailing % would swallow the `%{` of the B marker.
  [[ $PS1 == *[^%]% || $PS1 == % ]] && PS1+=%
  PS1=$_fac_mark_a$PS1$_fac_mark_b
  PS2=$_fac_mark_p$PS2
  _fac_osc7
  _fac_context "$on[@]"
}

_fac_preexec() {
  setopt localoptions noxtrace
  _fac_on || return 0
  emulate -L zsh
  _fac_unwrap
  print -rn -- $'\e]133;C\a'
  _fac_encode "$1"
  print -rn -- $'\e]7792;'"$_fac_v;$(( ++_fac_seq ));$_fac_reply[1]"$'\a'
}

# One line from fac: `apply <seq> <LBUFFER> <RBUFFER>`, both percent-encoded
# (%, \, space, bytes below 0x20 and 0x7f). Unknown commands are ignored.
_fac_handle() {
  local -a words=("${(@s: :)1}")
  [[ $words[1] == apply && $#words == 4 && $words[2] == <-> ]] || return 0
  [[ $words[2] == $_fac_report_seq ]] || return 0
  local left right
  printf -v left '%b' "${words[3]//\%/\\x}"
  printf -v right '%b' "${words[4]//\%/\\x}"
  zle split-undo
  LBUFFER=$left
  RBUFFER=$right
  (( ${+widgets[autosuggest-clear]} )) && zle autosuggest-clear
  zle -R
  # zle-line-pre-redraw does not run for programmatic edits.
  _fac_report
}

# The `zle -F -w` handler for fd $FAC_CMD_FD.
_fac_cmd() {
  emulate -L zsh
  local line
  if ! IFS= read -r -u $FAC_CMD_FD line; then
    # EOF or an error: a handler left registered would spin on it.
    zle -F $FAC_CMD_FD
    exec {FAC_CMD_FD}<&-
    _fac_diag cmd_fd_closed
    return 0
  fi
  _fac_handle "$line"
  while IFS= read -t 0 -r -u $FAC_CMD_FD line; do
    _fac_handle "$line"
  done
  return 0
}

_fac_install() {
  emulate -L zsh
  precmd_functions=(_fac_precmd_first ${precmd_functions:#_fac_precmd_(first|last)} _fac_precmd_last)
  preexec_functions+=(_fac_preexec)
  chpwd_functions+=(_fac_chpwd)
  autoload -Uz add-zle-hook-widget
  local hook
  for hook in line-init line-pre-redraw line-finish isearch-update isearch-exit; do
    add-zle-hook-widget $hook _fac_${hook//-/_} || _fac_diag hook_failed
  done
  if [[ $FAC_CMD_FD == <-> ]] && { : <&$FAC_CMD_FD } 2>/dev/null; then
    zle -N _fac_cmd
    zle -F -w $FAC_CMD_FD _fac_cmd
  else
    _fac_diag cmd_fd_missing
  fi
}

# Runs at the first prompt, so after .zshrc: hooks added by then are kept,
# and fac's go around them.
_fac_init() {
  precmd_functions=(${precmd_functions:#_fac_init})
  unfunction _fac_init
  # fac sets this so Kiro's pre blocks in .zprofile and .zshrc do not exec
  # kiro-cli-term on top of fac. They have run by now; programs started from
  # this shell (tmux, VS Code, another terminal) must not inherit it, or
  # Kiro would be off in them too.
  unset PROCESS_LAUNCHED_BY_Q
  # FAC_SHELL keeps `fac init`'s block in .zshrc from starting fac again in
  # this shell (src/init/mod.rs, "No loops"). Kept, so a `source ~/.zshrc`
  # still skips it, but no longer exported: a nested shell or a terminal
  # app started from here must not inherit it.
  (( ${+FAC_SHELL} )) && typeset -g +x FAC_SHELL
  _fac_on || return 0
  _fac_install
  _fac_precmd_first
  _fac_precmd_last
}

precmd_functions+=(_fac_init)
