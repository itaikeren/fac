# fac's bash hook, bash's --init-file.
#
# fac starts bash as `bash --init-file <this file> -i`, not as a login
# shell: a login bash ignores --init-file. So this reads the login files
# the way a login bash does (but `shopt login_shell` stays off). It must
# parse under bash 3.2, though fac never starts one older than 5.3.
#
# It tells fac, over private OSC sequences that fac strips from the output
# (the format is in src/shell/protocol.rs), each with the session token
# from FAC_TOKEN, without which fac drops it:
#   7790  the command line, from the binding of fac's trigger key, which
#         fac types after the user's keys (bash has no hook on every edit)
#   7791  shell context: the trigger keys, the keys after which no trigger
#         may follow at once (`defer`), PATH, cwd, the exported environment,
#         the history file, bash-completion's loader and `complete -p`
#   7792  each executed command
# It marks the prompt with OSC 133 (A from PROMPT_COMMAND, B at the end of
# PS1, C from PS0, D from PROMPT_COMMAND), reports the cwd with OSC 7, and
# takes buffer edits from fac over the pipe at fd $FAC_CMD_FD.
#
# bash redraws the prompt's last line after every `bind -x`, so a mark in
# PS1 is sent again with each report: 133;A comes from PROMPT_COMMAND
# instead, once per prompt, and fac takes only the first B of a prompt.
#
# Everything on the per-key path is forkless: no $(...), no external
# commands. `${ ...; }` (bash 5.3) runs in this shell, with no fork.

# The session token (src/shell/protocol.rs, "The session token"): out of
# the environment before the login files run, so no program this shell
# starts inherits it; a plain global, never exported. __fac_v is every
# message's first field: the version and the token (with no token, the
# bare version, which fac drops all the same).
# With xtrace off (bash started with it, from SHELLOPTS), so no trace
# prints it.
case $- in
  *x*) __fac_x=1 && set +x ;;
  *) __fac_x=0 ;;
esac
__fac_token=${FAC_TOKEN-}
unset FAC_TOKEN
__fac_v=1${__fac_token:+:$__fac_token}
((__fac_x)) && set -x
unset __fac_x

if [ -r /etc/profile ]; then
  . /etc/profile
fi
if [ -r ~/.bash_profile ]; then
  . ~/.bash_profile
elif [ -r ~/.bash_login ]; then
  . ~/.bash_login
elif [ -r ~/.profile ]; then
  . ~/.profile
fi

# The rest of this file runs with the options bash starts with, whatever
# the login files set (errexit would end the shell at the first command
# here that fails, nounset at an unset name, xtrace would print every
# command); they are put back at the end. Each function the shell calls
# (PROMPT_COMMAND's, PS0's and PS2's, the bindings) does the same with
# `local -`.
__fac_set=$-
__fac_pipefail=0
shopt -qo pipefail && __fac_pipefail=1
set +eux
set +o pipefail

# The command pipe at fd $FAC_CMD_FD must not reach the shell's children.
# bash cannot mark an fd close-on-exec by itself; its `fdflags` loadable
# builtin can, where this bash's own install has one (Homebrew's does).
case ${FAC_CMD_FD-} in
  '' | *[!0-9]*) ;;
  *)
    case ${BASH-} in
      /*)
        if enable -f "${BASH%/*}/../lib/bash/fdflags" fdflags 2>/dev/null; then
          fdflags -s +cloexec "$FAC_CMD_FD"
          enable -d fdflags
        fi
        ;;
    esac
    ;;
esac

# One counter for every 779x message, and the latest report's seq (what an
# apply must name).
__fac_seq=0
__fac_report_seq=0
# Whether fac asked for a look at the input (`ta`) since the last look,
# and the latest apply read (src/shell/trigger.rs, "Typeahead").
__fac_marked=0
__fac_cmd=
# The line of the latest report, the lines of the command before it (PS2
# lines), and the report's context: `start`, or `cont` on a PS2 line.
__fac_line=
__fac_prebuf=
__fac_ctx=start
# Set after the first prompt: no 133;D before it.
__fac_prompted=0
# `pre` once this prompt's __fac_precmd ran, `drawn` once __fac_prompt did,
# `cmd` once a command was read (PS0).
__fac_phase=
# HISTCONTROL had ignorespace (or ignoreboth): fac took it out, so that
# `history 1` names the command, and takes such commands out itself.
__fac_ignorespace=0
# What the defer list was built from, and whether a command may have bound
# keys since.
__fac_keys_seen=
__fac_keys_dirty=1
__fac_s=
__fac_a=()
__fac_kb=()
__fac_kt=()
__fac_ke=()

__fac_trigger='\e[9999~'
__fac_apply_trigger='\e[9998~'
__fac_mark_b='\[\e]133;B\a\]'
__fac_ps0='${ __fac_preexec; }'
__fac_ps2='${ __fac_ps2; }'

# The control bytes and DEL, and their escapes.
__fac_ctl=()
__fac_esc=()
for __fac_i in {1..31} 127; do
  printf -v __fac_hex '%02X' "$__fac_i"
  printf -v '__fac_ctl[__fac_i]' "\\x$__fac_hex"
  __fac_esc[__fac_i]=%$__fac_hex
done
# The printable ASCII characters, by code.
__fac_chr=()
for ((__fac_i = 32; __fac_i < 127; __fac_i++)); do
  printf -v __fac_hex '%02X' "$__fac_i"
  printf -v '__fac_chr[__fac_i]' "\\x$__fac_hex"
done
unset __fac_i __fac_hex

# $1 encoded as a 779x field into __fac_s: %, ;, control bytes, DEL.
__fac_encode() {
  local s=${1//%/%25} i
  s=${s//;/%3B}
  if [[ $s == *[$'\x01'-$'\x1f\x7f']* ]]; then
    for i in "${!__fac_ctl[@]}"; do
      s=${s//"${__fac_ctl[i]}"/${__fac_esc[i]}}
    done
  fi
  __fac_s=$s
}

# The elements of __fac_a as one list field into __fac_s: each encoded,
# with , as %2C, joined with ,.
__fac_encode_list() {
  local i
  __fac_a=("${__fac_a[@]//%/%25}")
  __fac_a=("${__fac_a[@]//;/%3B}")
  __fac_a=("${__fac_a[@]//,/%2C}")
  if [[ "${__fac_a[*]}" == *[$'\x01'-$'\x1f\x7f']* ]]; then
    for i in "${!__fac_ctl[@]}"; do
      __fac_a=("${__fac_a[@]//"${__fac_ctl[i]}"/${__fac_esc[i]}}")
    done
  fi
  local IFS=,
  __fac_s="${__fac_a[*]}"
}

# $1 split on $2 into __fac_a (no globbing; empty fields kept, a trailing
# separator gives none).
__fac_split() {
  local - IFS=$2
  set -f
  __fac_a=($1)
}

# 7791 for key $1 with the encoded value $2, unless that value was sent
# last.
declare -gA __fac_sent=()
__fac_send_context() {
  [[ -n ${__fac_sent[$1]+x} && ${__fac_sent[$1]} == "$2" ]] && return 0
  __fac_sent[$1]=$2
  printf '\e]7791;%s;%d;%s;%s\a' "$__fac_v" $((++__fac_seq)) "$1" "$2"
}

# The trigger's binding. 7790: CONTEXT, CURSOR (READLINE_POINT is in
# characters in bash 5.3), flags (`v` from the vi-command keymap's
# binding), PREBUFFER, BUFFER.
__fac_report() {
  local -
  set +euxo pipefail
  local pre=
  __fac_line=$READLINE_LINE
  if [[ $__fac_ctx == cont ]]; then
    __fac_encode "$__fac_prebuf"
    pre=$__fac_s
  fi
  __fac_encode "$READLINE_LINE"
  __fac_report_seq=$((++__fac_seq))
  printf '\e]7790;%s;%d;%s;%d;%s;%s;%s\a' "$__fac_v" "$__fac_report_seq" "$__fac_ctx" \
    "$READLINE_POINT" "${1-}" "$pre" "$__fac_s"
  # A tool that set PROMPT_COMMAND anew dropped fac's prompt hooks.
  [[ ${PROMPT_COMMAND[0]-} == __fac_precmd && ${PROMPT_COMMAND[-1]-} == __fac_prompt ]] ||
    __fac_fix_prompt_command
  return 0
}

# `apply <seq> <L> <R> <LINE>`, every field percent-encoded; empty fields
# kept. Applied only while <seq> is the latest report and <LINE> the line.
__fac_apply_line() {
  local rest=$1 l r line
  local -a w=()
  while :; do
    w+=("${rest%% *}")
    [[ $rest == *' '* ]] || break
    rest=${rest#* }
  done
  ((${#w[@]} == 5)) || return 0
  [[ ${w[0]} == apply && ${w[1]} == "$__fac_report_seq" ]] || return 0
  printf -v l '%b' "${w[2]//\%/\\x}"
  printf -v r '%b' "${w[3]//\%/\\x}"
  printf -v line '%b' "${w[4]//\%/\\x}"
  [[ $line == "$READLINE_LINE" ]] || return 0
  READLINE_LINE=$l$r
  READLINE_POINT=${#l}
}

# A line from fac's command pipe: `ta` (look at the input at the next
# prompt) into __fac_marked, an apply into __fac_cmd (the latest one wins).
# A `looked` no look waits for is stale.
__fac_take() {
  case $1 in
    ta) __fac_marked=1 ;;
    'apply '*) __fac_cmd=$1 ;;
  esac
  return 0
}

# Every line waiting on the command pipe, read with no wait, into
# __fac_take.
__fac_drain() {
  local line
  [[ -n $__fac_cmd_fd ]] || return 0
  while read -t 0 -u "$__fac_cmd_fd" && IFS= read -r -u "$__fac_cmd_fd" line; do
    __fac_take "$line"
  done
}

# The apply trigger's binding: every line waiting (fac writes the apply
# before the trigger), the last apply applied if it is not stale, then the
# report. $1: the report's flags.
__fac_apply() {
  local -
  set +euxo pipefail
  local line
  __fac_cmd=
  if [[ -n $__fac_cmd_fd ]] && IFS= read -r -t 0.05 -u "$__fac_cmd_fd" line; then
    __fac_take "$line"
    __fac_drain
    [[ -n $__fac_cmd ]] && __fac_apply_line "$__fac_cmd"
  fi
  __fac_report "${1-}"
}

# PS0, as the line is accepted: 133;C and 7792 with the command, as
# `history 1` has it (HISTCONTROL as bash-preexec adjusts it, so that the
# command is there).
__fac_preexec() {
  local -
  set +euxo pipefail
  local h=
  __fac_phase=cmd
  __fac_ctx=start
  __fac_prebuf=
  printf '\e]133;C\a'
  if [[ -o history ]]; then
    h=${ HISTTIMEFORMAT= builtin history 1; }
    h=${h#*[0-9][* ] }
    if ((__fac_ignorespace)) && [[ $h == ' '* ]]; then
      # What ignorespace would have left out of the history.
      builtin history -d -1
    fi
    __fac_encode "$h"
    printf '\e]7792;%s;%d;%s\a' "$__fac_v" $((++__fac_seq)) "$__fac_s"
  fi
  # A command that may bind keys: the defer list is built again at the
  # next prompt.
  case $h in
    *bind* | *inputrc* | *source* | .[[:space:]]* | *[[:space:]\;\&\|\(].[[:space:]]*)
      __fac_keys_dirty=1
      ;;
  esac
  return 0
}

# PS2, before a continuation line: the line before joins the prebuffer, and
# the trigger is armed for the new line. Written straight to the terminal
# (the expansion is empty), so a redraw of PS2 does not send it again.
__fac_ps2() {
  local -
  set +euxo pipefail
  __fac_prebuf+=$__fac_line$'\n'
  __fac_line=
  __fac_ctx=cont
  __fac_editing &&
    printf '\e]7791;%s;%d;armed;%d\a' "$__fac_v" $((++__fac_seq)) "$$" >&2
  return 0
}

# Whether readline edits the line (`set +o emacs`, `set +o vi` and
# --noediting turn it off): with no line editor, a trigger would go into
# the terminal's cooked input, so the trigger is not armed.
__fac_editing() {
  [[ -o emacs || -o vi ]]
}

# fac's two hooks first and last in PROMPT_COMMAND (array form), the
# user's in between, as they are.
__fac_fix_prompt_command() {
  local -a keep=()
  local e
  for e in "${PROMPT_COMMAND[@]}"; do
    [[ -z $e || $e == __fac_precmd || $e == __fac_prompt ]] || keep+=("$e")
  done
  PROMPT_COMMAND=(__fac_precmd "${keep[@]}" __fac_prompt)
}

# First in PROMPT_COMMAND: 133;D with the last command's status, which it
# keeps for the hooks after it (and `$_`); not under the user's errexit, where
# a failed status (`! true`) would end the shell.
__fac_precmd() {
  local ret=$? last=$_ opts=$-
  local -
  set +euxo pipefail
  __fac_mark_d "$ret"
  [[ $opts == *e* ]] && ret=0
  : "$last"
  return "$ret"
}

# 133;D with status $1, once per prompt, and a new line's state.
__fac_mark_d() {
  [[ $__fac_phase == pre ]] && return 0
  __fac_phase=pre
  ((__fac_prompted)) && printf '\e]133;D;%d\a' "$1"
  __fac_prompted=1
  __fac_ctx=start
  __fac_prebuf=
  __fac_line=
}

# The prompt strings: 133;B at the end of PS1, fac's PS0 and PS2 parts,
# each put back where another tool (bash-preexec, starship, atuin) set the
# string anew, theirs kept. PS0 and PS2 need promptvars.
__fac_fix_prompts() {
  local p=${PS1-} t n=0
  p=${p//"$__fac_mark_b"/}
  # A trailing lone backslash would escape the marker's.
  t=$p
  while [[ $t == *'\' ]]; do
    t=${t%'\'}
    n=$((n + 1))
  done
  ((n % 2)) && p+='\'
  PS1=$p$__fac_mark_b
  if shopt -q promptvars; then
    [[ ${PS0-} == *"$__fac_ps0"* ]] || PS0=${PS0-}$__fac_ps0
    [[ ${PS2-} == *"$__fac_ps2"* ]] || PS2=$__fac_ps2${PS2-}
  else
    PS0=${PS0//"$__fac_ps0"/}
    PS2=${PS2//"$__fac_ps2"/}
  fi
}

# HISTCONTROL as bash-preexec adjusts it: no ignorespace (ignoreboth is
# ignoredups), remembered so __fac_preexec leaves such commands out.
__fac_fix_histcontrol() {
  local h=${HISTCONTROL-}
  [[ $h == *ignorespace* || $h == *ignoreboth* ]] || return 0
  __fac_ignorespace=1
  h=${h//ignorespace/}
  [[ $h == *ignoreboth* ]] && h=ignoredups:${h//ignoreboth/}
  HISTCONTROL=$h
}

# OSC 7 with $PWD; %, ;, space and controls percent-encoded.
__fac_osc7() {
  local p=${PWD//%/%25} i
  p=${p//;/%3B}
  p=${p// /%20}
  if [[ $p == *[$'\x01'-$'\x1f\x7f']* ]]; then
    for i in "${!__fac_ctl[@]}"; do
      p=${p//"${__fac_ctl[i]}"/${__fac_esc[i]}}
    done
  fi
  printf '\e]7;file://%s%s\a' "${HOSTNAME-}" "$p"
}

# Every context key, sent when it changed.
__fac_context() {
  local name ext=0 text
  local -a env=()
  __fac_split "${PATH-}" :
  __fac_encode_list
  __fac_send_context path "$__fac_s"
  __fac_encode "$PWD"
  __fac_send_context cwd "$__fac_s"
  __fac_encode "${HISTFILE-}"
  __fac_send_context histfile "$__fac_s"
  # The exported environment, as NAME=value, for the commands spec
  # generators run.
  __fac_split "${ compgen -e; }" $'\n'
  for name in "${__fac_a[@]}"; do
    [[ $name == __fac* ]] || env+=("$name=${!name}")
  done
  __fac_a=("${env[@]}")
  __fac_encode_list
  __fac_send_context env "$__fac_s"
  # bash-completion's loader: its source file, from extdebug's `declare
  # -F` (`<name> <line> <file>`), for the completion worker.
  shopt -q extdebug && ext=1
  shopt -s extdebug
  text=${ declare -F _comp_load 2>/dev/null; }
  ((ext)) || shopt -u extdebug
  if [[ -n $text ]]; then
    text=${text#* }
    text=${text#* }
    __fac_encode "$text"
    __fac_send_context bash_completion "$__fac_s"
  fi
  # The completion specs, which bash-completion loads as they are used:
  # the whole list, sent at the first prompt even when empty (the worker
  # starts on it) and whenever it changed.
  __fac_split "${ complete -p 2>/dev/null; }" $'\n'
  __fac_encode_list
  __fac_send_context complete "$__fac_s"
}

# The look at the input for typeahead (src/shell/trigger.rs, "Typeahead"):
# the lines waiting on the command pipe are read (stale applies go), and
# after a `ta`, fac is asked to look (`look`), here, before the line editor
# reads anything. fac counts the bytes of the terminal's input no one has
# read, from its end, and answers on the pipe (`looked <seq> <mark>
# <bytes>`); nothing is read meanwhile, so the count is this moment's.
# `unread` sends fac's numbers back, which is what settles anything. To
# count, fac takes the terminal out of cooked mode for an instant, so the
# wait has no time limit of its own: readline must not start while fac
# does that. It ends with the answer, or at the pipe's end (fac exited, or
# stopped reading the hook's messages and closed it), or at the user's
# TMOUT; fac changes no modes once readline has the terminal (out of
# cooked mode, VMIN 1), so a late count is harmless.
__fac_queue() {
  local seq line mark bytes
  __fac_cmd=
  __fac_drain
  ((__fac_marked)) || return 0
  __fac_marked=0
  [[ -n $__fac_cmd_fd ]] || return 0
  seq=$((++__fac_seq))
  printf '\e]7791;%s;%d;look;\a' "$__fac_v" "$seq"
  while IFS= read -r -u "$__fac_cmd_fd" line; do
    case $line in
      "looked $seq "*)
        line=${line#"looked $seq "}
        mark=${line%% *}
        bytes=${line#* }
        [[ -n $mark && -n $bytes && $mark$bytes != *[!0-9]* ]] || return 0
        printf '\e]7791;%s;%d;unread;%s,%s\a' "$__fac_v" $((++__fac_seq)) \
          "$mark" "$bytes"
        # Something was unread: look again at the next prompt, whether or
        # not fac's `ta` for it comes in time.
        [[ $bytes == 0 ]] || __fac_marked=1
        return 0
        ;;
      *) __fac_take "$line" ;;
    esac
  done
  return 0
}

# Last in PROMPT_COMMAND: fac's parts of the prompt strings, the context,
# the defer list, the look for typeahead, `armed` (with a line editor),
# then 133;A, as the prompt is drawn. Its status is the last command's, as
# __fac_precmd's.
__fac_prompt() {
  local ret=$? last=$_ opts=$-
  local -
  set +euxo pipefail
  # A PROMPT_COMMAND set anew this prompt dropped __fac_precmd.
  __fac_mark_d "$ret"
  __fac_phase=drawn
  [[ ${PROMPT_COMMAND[0]-} == __fac_precmd && ${PROMPT_COMMAND[-1]-} == __fac_prompt ]] ||
    __fac_fix_prompt_command
  __fac_fix_prompts
  __fac_fix_histcontrol
  __fac_osc7
  __fac_keys
  __fac_context
  __fac_queue
  __fac_editing && printf '\e]7791;%s;%d;armed;%d\a' "$__fac_v" $((++__fac_seq)) "$$"
  printf '\e]133;A\a'
  [[ $opts == *e* ]] && ret=0
  : "$last"
  return "$ret"
}

# ---- The defer list ----------------------------------------------------
#
# Built from the live keymaps (`bind -p`, `bind -s`, `bind -X`) whenever
# they change: emacs's keys in emacs mode; in vi mode vi-insert's (all
# modes, lower-case class) and vi-command's (upper-case: vi command mode
# only). Each element is a class letter and the key's bytes; the classes
# are in protocol.rs ("The defer list").

# The class of each readline function that defers its key: `p`, `i`, `n`,
# `m`, or `a` for one that accepts the line (left out for Enter, C-j and
# C-o, fac's accept keys already). The completion keys
# are `p`: a trigger between two Tabs, however far apart, would stop the
# second from listing (or cycling), so the line is reported with the next
# key's trigger.
declare -gA __fac_cls=()
for __fac_i in digit-argument universal-argument vi-arg-digit quoted-insert \
  character-search character-search-backward vi-char-search vi-change-char \
  vi-set-mark vi-goto-mark vi-delete-to vi-change-to vi-yank-to prefix-meta \
  complete menu-complete menu-complete-backward old-menu-complete \
  complete-filename complete-username complete-hostname complete-variable \
  complete-command complete-into-braces dynamic-complete-history \
  glob-complete-word bash-vi-complete; do
  __fac_cls[$__fac_i]=p
done
for __fac_i in kill-line \
  backward-kill-line unix-line-discard kill-whole-line kill-word \
  backward-kill-word unix-word-rubout unix-filename-rubout kill-region \
  copy-region-as-kill copy-backward-word copy-forward-word shell-kill-word \
  shell-backward-kill-word vi-unix-word-rubout yank yank-pop yank-nth-arg \
  yank-last-arg insert-last-argument; do
  __fac_cls[$__fac_i]=i
done
for __fac_i in edit-and-execute-command vi-edit-and-execute-command \
  reverse-search-history forward-search-history \
  non-incremental-reverse-search-history \
  non-incremental-forward-search-history \
  non-incremental-reverse-search-history-again \
  non-incremental-forward-search-history-again vi-search vi-replace \
  call-last-kbd-macro execute-named-command emacs-editing-mode \
  vi-editing-mode; do
  __fac_cls[$__fac_i]=n
done
for __fac_i in accept-line operate-and-get-next insert-comment \
  vi-insert-comment; do
  __fac_cls[$__fac_i]=a
done
# vi-insert's ESC: the keys after it are vi command mode's (`m`).
__fac_cls[vi-movement-mode]=m
unset __fac_i

# Readline key sequence $1 (as `bind` prints it, without its quotes) into
# __fac_kb (its bytes, as hex pairs) and __fac_kt (the text of each byte's
# escape). 1 for a NUL or a byte past ASCII.
__fac_keybytes() {
  local s=$1 c n d
  __fac_kb=()
  __fac_kt=()
  while [[ -n $s ]]; do
    c=${s:0:1}
    s=${s:1}
    if [[ $c != '\' ]]; then
      printf -v n '%d' "'$c"
      ((n > 0 && n < 128)) || return 1
      printf -v n '%02X' "$n"
      __fac_kb+=("$n")
      __fac_kt+=("$c")
      continue
    fi
    d='\'${s:0:1}
    c=${s:0:1}
    s=${s:1}
    case $c in
      C)
        if [[ ${s:0:1} != - ]]; then
          __fac_kb+=(43)
          __fac_kt+=("$d")
          continue
        fi
        c=${s:1:1}
        d+=-$c
        s=${s:2}
        if [[ $c == '\' ]]; then
          c=${s:0:1}
          d+=$c
          s=${s:1}
        fi
        if [[ $c == '?' ]]; then
          n=7F
        else
          printf -v n '%d' "'$c"
          ((n > 0 && n < 128)) || return 1
          n=$((n & 31))
          ((n > 0)) || return 1
          printf -v n '%02X' "$n"
        fi
        ;;
      M)
        if [[ ${s:0:1} == - ]]; then
          s=${s:1}
          n=1B
          d='\e'
        else
          n=4D
        fi
        ;;
      e) n=1B ;;
      a) n=07 ;;
      b) n=08 ;;
      d) n=7F ;;
      f) n=0C ;;
      n) n=0A ;;
      r) n=0D ;;
      t) n=09 ;;
      v) n=0B ;;
      [0-7])
        n=$c
        while [[ ${#n} -lt 3 && ${s:0:1} == [0-7] ]]; do
          n+=${s:0:1}
          s=${s:1}
        done
        d='\'$n
        n=$((8#$n))
        ((n > 0 && n < 128)) || return 1
        printf -v n '%02X' "$n"
        ;;
      x)
        n=
        while [[ ${#n} -lt 2 && ${s:0:1} == [0-9a-fA-F] ]]; do
          n+=${s:0:1}
          s=${s:1}
        done
        d='\x'$n
        [[ -n $n ]] || return 1
        n=$((16#$n))
        ((n > 0 && n < 128)) || return 1
        printf -v n '%02X' "$n"
        ;;
      '')
        n=5C
        d='\'
        ;;
      *)
        printf -v n '%d' "'$c"
        ((n > 0 && n < 128)) || return 1
        printf -v n '%02X' "$n"
        ;;
    esac
    __fac_kb+=("$n")
    __fac_kt+=("$d")
  done
  ((${#__fac_kb[@]} > 0))
}

# Where fac's key splitter ends each key of __fac_kb (as in
# src/term/keys.rs): ESC with a CSI, an SS3 or one character (also after a
# second ESC), else one byte. Into __fac_ke, as offsets past each key.
__fac_keyends() {
  local i=0 n=${#__fac_kb[@]} j
  __fac_ke=()
  while ((i < n)); do
    if [[ ${__fac_kb[i]} == 1B ]] && ((i + 1 < n)); then
      j=$((i + 1))
      if [[ ${__fac_kb[j]} == 1B ]] && ((j + 1 < n)) && [[ ${__fac_kb[j + 1]} == 5B || ${__fac_kb[j + 1]} == 4F ]]; then
        j=$((j + 1))
      fi
      case ${__fac_kb[j]} in
        5B)
          j=$((j + 1))
          while ((j < n)) && ! ((16#${__fac_kb[j]} >= 0x40 && 16#${__fac_kb[j]} <= 0x7e)); do
            j=$((j + 1))
          done
          i=$((j + 1))
          ;;
        4F) i=$((j + 2)) ;;
        *) i=$((j + 1)) ;;
      esac
    else
      i=$((i + 1))
    fi
    ((i > n)) && i=$n
    __fac_ke+=("$i")
  done
}

# The first $2 bytes of __fac_kb as a defer element of class $1, encoded
# as a list element, into __fac_s.
__fac_element() {
  local i h out=$1
  for ((i = 0; i < $2; i++)); do
    h=${__fac_kb[i]}
    case $h in
      0? | 1? | 7F | 25 | 3B | 2C) out+=%$h ;;
      *) out+=${__fac_chr[16#$h]} ;;
    esac
  done
  __fac_s=$out
}

# Whether readline key sequence $1 is surely one key to fac's splitter
# without working out its bytes: a character, an escape (`\t`, `\C-x`, an
# octal byte), ESC with one of those, or ESC with a CSI or SS3.
__fac_single() {
  local s=$1 alt=0 body
  if [[ $s == '\e'?* && $s != '\e\e'* ]]; then
    s=${s:2}
    alt=1
  fi
  case $s in
    ? | '\'? | '\C-'? | '\C-\\' | '\'[0-7][0-7][0-7]) return 0 ;;
    O?) ((alt)) ;;
    '['*)
      ((alt)) || return 1
      body=${s:1}
      [[ -n $body && ${body: -1} == [@-~] && ${body%?} != *[!0-9\;:\<=\>?]* ]]
      ;;
    *) return 1 ;;
  esac
}

# The elements of keymap $1 into __fac_defer (with their order in
# __fac_order), upper-cased when $2 is 1; `bind -p`, `-s` and `-X` are $3,
# $4 and $5. Our own bindings, if missing from $5, are bound again.
__fac_keymap() {
  local map=$1 up=$2 line spec fn class i k t pre own_esc macro
  local -A own=() esc_after=() prefixes=()
  local -a lines=()
  # The keys bound to functions that accept the line, as bind prints them:
  # a macro with one of them in it accepts the line too.
  local -a accepts=('\C-j' '\C-m' '\C-o')
  # bind -p: `"<keys>": <function>`; bind -s: `"<keys>": "<macro>"`;
  # bind -X: `"<keys>" "<command>"`.
  __fac_split "$3" $'\n'
  for line in "${__fac_a[@]}"; do
    [[ $line == '"'* ]] && lines+=("p${line}")
  done
  __fac_split "$4" $'\n'
  for line in "${__fac_a[@]}"; do
    [[ $line == '"'* ]] && lines+=("s${line}")
  done
  # bind -X prints a command's newlines as they are: a line that does not
  # start a binding goes on the one before.
  __fac_split "$5" $'\n'
  local -a xs=()
  for line in "${__fac_a[@]}"; do
    if [[ $line == '"'* || ${#xs[@]} == 0 ]]; then
      xs+=("$line")
    else
      xs[-1]+=$'\n'$line
    fi
  done
  local mine=0
  for line in "${xs[@]}"; do
    [[ $line == '"'* ]] || continue
    case $line in
      *'" "__fac_report'* | *'" "__fac_apply'*) mine=$((mine + 1)) ;;
      *'__fac_widget_done $?'*) lines+=("x${line}") ;;
      *)
        lines+=("x${line}")
        __fac_wrap "$map" "$line"
        ;;
    esac
  done
  if ((mine < 2)); then
    __fac_bind "$map"
  fi
  for line in "${lines[@]}"; do
    t=${line:0:1}
    line=${line:2}
    case $t in
      p)
        spec=${line%'": '*}
        fn=${line##*'": '}
        ;;
      s)
        spec=${line%%'": "'*}
        macro=${line#*'": "'}
        fn=
        ;;
      x)
        spec=${line%%'" "'*}
        fn=
        ;;
    esac
    # Most keys are one key bound to a function that defers nothing: only
    # their own binding counts (for a prefix key's).
    if [[ $t == p ]]; then
      own[$spec]=1
      class=${__fac_cls[$fn]-}
      if [[ -z $class ]]; then
        case $spec in
          ? | '\'? | '\C-'? | '\C-\\' | '\e'[!\\] | '\e\C-'? | '\e\'?) continue ;;
        esac
        __fac_single "$spec" && continue
      fi
    fi
    __fac_keybytes "$spec" || continue
    __fac_keyends
    # The texts of the sequence's starts: what own and esc_after are keyed
    # by.
    pre=
    for ((i = 0; i < ${#__fac_kt[@]}; i++)); do
      [[ ${__fac_kb[i]} == 1B ]] && ((i > 0)) && esc_after[$pre]=1
      pre+=${__fac_kt[i]}
    done
    [[ $t == p ]] && own[$pre]=1
    # Every proper start of the sequence at a key's end is a prefix key;
    # so is a first ESC (fac's splitter joins ESC to the key after it, but
    # typed alone it is a key of its own).
    for k in "${__fac_ke[@]}"; do
      ((k < ${#__fac_kb[@]})) || continue
      pre=
      for ((i = 0; i < k; i++)); do pre+=${__fac_kt[i]}; done
      __fac_element p "$k"
      prefixes[$pre]=$__fac_s
    done
    if [[ ${__fac_kb[0]} == 1B ]] && ((${#__fac_kb[@]} > 1)); then
      __fac_element p 1
      prefixes[${__fac_kt[0]}]=$__fac_s
    fi
    case $t in
      p)
        class=${__fac_cls[$fn]-}
        if [[ $class == a ]]; then
          accepts+=("$spec")
          __fac_element '' "${#__fac_kb[@]}"
          case $__fac_s in
            %0D | %0A | %0F) class= ;;
          esac
        fi
        ;;
      s)
        class=n
        for k in "${accepts[@]}"; do
          [[ $macro == *"$k"* ]] && class=a
        done
        ;;
      *) class=n ;;
    esac
    [[ -n $class ]] || continue
    __fac_element "$class" "${#__fac_kb[@]}"
    __fac_add "$up" "$__fac_s"
  done
  # A prefix key with a binding of its own is safe to trigger after:
  # readline runs that binding and reads the trigger afresh, unless a
  # sequence goes on from it with ESC (the trigger's first byte). ESC with
  # no binding of its own (emacs, vi-command) is `w`: readline waits for
  # the key after it however long the pause (keyseq-timeout is only for a
  # prefix that has one), so fac judges that key together with it.
  for pre in "${!prefixes[@]}"; do
    [[ -n ${own[$pre]-} && -z ${esc_after[$pre]-} ]] && continue
    if [[ $pre == '\e' && -z ${own[$pre]-} ]]; then
      __fac_add "$up" w%1B
      continue
    fi
    __fac_add "$up" "${prefixes[$pre]}"
  done
  # vi command mode: 0 goes on a count (it is beginning-of-line only
  # without one).
  if ((up)); then
    __fac_add 1 P0
  fi
}

# A user's `bind -x` widget (fzf's, say) reads the terminal itself, so its
# key is `n`: no trigger until the shell reports. Its binding, `bind -X`
# line $2 in keymap $1, is bound again with __fac_widget_done after the
# command, so that the widget reports as it ends. bind -X prints the
# command with `\` and `"` escaped (and a control character as `\C-x`);
# bind -x takes it back as it is. A command that would not come back the
# same (another escape, a quote first, which bind -x strips, or a `#`
# that may start a comment) is left alone: its key stays `n` until the
# next prompt.
__fac_wrap() {
  local head=${2%%'" "'*} cmd=${2#*'" "'} flag= sep='; '
  cmd=${cmd%'"'}
  cmd=${cmd//'\\'/$'\1'}
  cmd=${cmd//'\"'/'"'}
  [[ $cmd == *'\'* || $cmd == [\"\']* || $cmd == *'#'* || -z $cmd ]] && return 0
  cmd=${cmd//$'\1'/'\'}
  while [[ $cmd == *[[:space:]] ]]; do
    cmd=${cmd%?}
  done
  case $cmd in
    *'&' | *';') sep=' ' ;;
    *'|') return 0 ;;
  esac
  [[ $1 == vi-command ]] && flag=' v'
  bind -m "$1" -x "$head\": $cmd${sep}__fac_widget_done \$?$flag"
}

# The end of a user's widget: the line as it left it is reported, and its
# status kept (not under errexit, as __fac_precmd's).
__fac_widget_done() {
  local ret=$1 opts=$-
  local -
  set +euxo pipefail
  # A widget may cd, with no prompt after it: fac takes no OSC 7 as its
  # own cwd.
  __fac_encode "$PWD"
  __fac_send_context cwd "$__fac_s"
  __fac_report "${2-}"
  [[ $opts == *e* ]] && ret=0
  return "$ret"
}

# Add element $2 to __fac_defer (upper-cased class when $1 is 1), once.
__fac_add() {
  local e=$2 c
  if (($1)); then
    c=${2:0:1}
    e=${c^^}${2:1}
  fi
  [[ -n ${__fac_defer[$e]-} ]] && return 0
  __fac_defer[$e]=1
  __fac_order+=("$e")
}

# fac's trigger and apply trigger in keymap $1 (vi-command's report `v`).
__fac_bind() {
  local flag=
  [[ $1 == vi-command ]] && flag=' v'
  bind -m "$1" -x "\"$__fac_trigger\": __fac_report$flag"
  bind -m "$1" -x "\"$__fac_apply_trigger\": __fac_apply$flag"
}

# The defer list and the escape delay, sent when the keymaps changed:
# when the editing mode, the macros or the `bind -x` keys did (cheap to
# list at every prompt), or after a command that may have bound keys
# (`bind -p`, a few ms, is listed only then).
__fac_keys() {
  local mode seen p s x p2 s2 x2 delay
  if [[ -o vi ]]; then
    mode=vi
    s=${ bind -m vi-insert -s 2>/dev/null; }
    x=${ bind -m vi-insert -X 2>/dev/null; }
    s2=${ bind -m vi-command -s 2>/dev/null; }
    x2=${ bind -m vi-command -X 2>/dev/null; }
  elif [[ -o emacs ]]; then
    mode=emacs
    s=${ bind -m emacs -s 2>/dev/null; }
    x=${ bind -m emacs -X 2>/dev/null; }
  else
    return 0
  fi
  seen=$mode$'\1'$s$'\1'$x$'\1'$s2$'\1'$x2
  [[ $seen == "$__fac_keys_seen" ]] && ((!__fac_keys_dirty)) && return 0
  __fac_keys_seen=$seen
  __fac_keys_dirty=0
  if [[ $mode == vi ]]; then
    p=${ bind -m vi-insert -p 2>/dev/null; }
    p2=${ bind -m vi-command -p 2>/dev/null; }
  else
    p=${ bind -m emacs -p 2>/dev/null; }
  fi
  declare -gA __fac_defer=()
  __fac_order=()
  if [[ $mode == vi ]]; then
    __fac_keymap vi-insert 0 "$p" "$s" "$x"
    __fac_keymap vi-command 1 "$p2" "$s2" "$x2"
  else
    __fac_keymap emacs 0 "$p" "$s" "$x"
  fi
  # Ctrl-C, the terminal's interrupt key: bash drops the line and runs
  # PROMPT_COMMAND with the terminal echoing, where a trigger after it
  # would show (`n`; armed again at the next prompt).
  __fac_add 0 n%03
  local IFS=,
  __fac_send_context defer "${__fac_order[*]}"
  # readline's keyseq-timeout: how long it waits after a lone ESC.
  delay=${ bind -v 2>/dev/null; }
  delay=${delay#*'set keyseq-timeout '}
  delay=${delay%%$'\n'*}
  [[ -n $delay && $delay != *[!0-9]* ]] && __fac_send_context escape_delay "$delay"
}

__fac_install() {
  local map
  for map in emacs vi-insert vi-command; do
    __fac_bind "$map"
  done
  __fac_fix_prompt_command
  __fac_fix_prompts
  printf -v __fac_s '%b' "$__fac_trigger"
  __fac_encode "$__fac_s"
  __fac_send_context trigger "$__fac_s"
  printf -v __fac_s '%b' "$__fac_apply_trigger"
  __fac_encode "$__fac_s"
  __fac_send_context apply_trigger "$__fac_s"
}

# Startup is done: Kiro's pre block (in the login files) has seen
# PROCESS_LAUNCHED_BY_Q, and the shell's children must not inherit it.
__fac_cmd_fd=${FAC_CMD_FD-}
unset PROCESS_LAUNCHED_BY_Q FAC_CMD_FD
# FAC_SHELL keeps `fac init`'s block from starting fac again in this shell
# (src/init/mod.rs, "No loops"): kept, so sourcing a startup file again
# still skips it, but no longer exported, so children do not inherit it.
[[ -n ${FAC_SHELL+x} ]] && export -n FAC_SHELL
if [[ -n ${FAC_ACTIVE-} && $- == *i* ]]; then
  __fac_install
fi

# The login files' options, back.
((__fac_pipefail)) && set -o pipefail
case $__fac_set in *u*) set -u ;; esac
case $__fac_set in *e*) set -e ;; esac
case $__fac_set in
  *x*)
    unset __fac_set __fac_pipefail
    set -x
    ;;
  *) unset __fac_set __fac_pipefail ;;
esac
