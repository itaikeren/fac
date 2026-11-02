# fac's bash completion worker: a hidden bash that fac keeps warm on its
# own PTY and asks what bash's programmable completion (bash-completion's
# specs, git's own, the user's `complete` lines) would complete for a
# command line.
#
# fac starts it as `bash --noprofile --norc -c 'source <this file>'`, so no
# rc file runs (nor bash-completion's ~/.bash_completion, below). Commands
# come in on the pipe at fd $FAC_WORKER_IN and results go out on the pipe
# at fd $FAC_WORKER_OUT, one line each. The PTY is only a controlling
# terminal, for fac's ^C; fac discards whatever is drawn on it.
#
# fac -> worker (fields percent-encoded: %, \, ;, space, every byte below
# 0x20 and 0x7f; a list is its elements, each also with , as %2C, joined
# with ,):
#   ctx <key> <value>   path, env (lists): the PATH and the exported
#                       NAME=value pairs; cwd; bash_completion: the
#                       bash-completion loader to source (empty: none);
#                       complete (list): the `complete -p` lines to replay
#   init                source the loader and replay the lines; then `ready`
#   q <id> <cwd> <lbuffer> <rbuffer>
# worker -> fac (fields percent-encoded: %, ;, space, every byte below 0x20
# and 0x7f):
#   ready <load-ms> <specs> 0
#                       specs: how many `complete -p` lines are defined
#   c <id> <word>;<display>;<description>;<group>;<suffix>;<flags>;<iprefix>;<prefix>;<apre>;<hpre>;<hsuf>
#   d <id> <count> <elapsed-ms>
#   err <id> <code>     cwd (cd failed), int (interrupted)
# bash has no descriptions or groups: display is the word, the rest empty.
# Flags: Q (inserted as it is: bash quotes only `-o filenames` matches),
# f (`-o filenames`), S (the suffix is given, even empty: `-o nospace`, a
# word git ended with a space, a directory's slash). prefix: the word
# readline completes, up to the cursor; iprefix: the shell word before it
# (`--color=`), unquoted; hpre: the directory part of a path match.
#
# A query runs the spec as readline does: COMP_LINE, COMP_POINT,
# COMP_WORDS and COMP_CWORD of the current command, split at
# COMP_WORDBREAKS; actions, -G and -W through `compgen`; the -F function
# with a `compopt` shim; -C; -X, -P and -S; then -o dirnames, plusdirs,
# bashdefault and default. A spec whose function is not defined here is
# loaded: bash-completion's `_comp_load`, then the command's own completion
# (git's ships with git). The query path is forkless except for what the
# specs themselves run.

# The pipes, kept from the specs' commands where this bash can (its
# `fdflags` loadable, as Homebrew's has).
__fac_in=${FAC_WORKER_IN-}
__fac_out=${FAC_WORKER_OUT-}
unset FAC_WORKER_IN FAC_WORKER_OUT

# patsub_replacement is bash 5.2's default; the records rely on it.
shopt -s extglob patsub_replacement
HISTFILE=
# Interactive bash's default.
COMP_WORDBREAKS=$' \t\n"\'@><=;|&(:'

# The id of the query being completed, empty between queries.
__fac_qid=
# Set by fac's ^C during a query.
__fac_int=
__fac_ready=
__fac_loader=
# The `complete` lines from the shell.
__fac_lines=()
# The env names imported from the shell.
__fac_imported=()
# Commands whose spec was looked for once already.
declare -A __fac_tried=()
__fac_git_tried=
# The current query's options (`-o`, and what `compopt` sets) and matches.
declare -A __fac_o=()
__fac_m=()
__fac_s=
__fac_list=()
__fac_w=()

# The control bytes and DEL, and their escapes.
__fac_ctl=()
__fac_esc=()
for __fac_i in {1..31} 127; do
  printf -v __fac_hex '%02X' "$__fac_i"
  printf -v '__fac_ctl[__fac_i]' "\\x$__fac_hex"
  __fac_esc[__fac_i]=%$__fac_hex
done
unset __fac_i __fac_hex

# Encode each element of the array __fac_enc in place, as fields to fac.
__fac_encode() {
  __fac_enc=("${__fac_enc[@]//%/%25}")
  __fac_enc=("${__fac_enc[@]//;/%3B}")
  __fac_enc=("${__fac_enc[@]// /%20}")
  if [[ ${__fac_enc[*]} == *[$'\x01'-$'\x1f\x7f']* ]]; then
    local i
    for i in "${!__fac_ctl[@]}"; do
      __fac_enc=("${__fac_enc[@]//"${__fac_ctl[i]}"/"${__fac_esc[i]}"}")
    done
  fi
}

# Decode the field $1 into __fac_s.
__fac_decode() {
  printf -v __fac_s '%b' "${1//\%/\\x}"
}

# Decode the list field $1 into __fac_list. The empty field is the empty
# list.
__fac_decode_list() {
  __fac_list=()
  [[ -n $1 ]] || return 0
  local rest=$1 item
  while :; do
    item=${rest%%,*}
    printf -v item '%b' "${item//\%/\\x}"
    __fac_list+=("$item")
    [[ $rest == *,* ]] || break
    rest=${rest#*,}
  done
}

__fac_send() {
  printf '%s\n' "$@" >&"$__fac_out"
}

# $1 with its quotes and backslashes removed, into __fac_s.
__fac_dequote() {
  local s=$1 out= c q= i n=${#1}
  if [[ $s != *[\'\"\\]* ]]; then
    __fac_s=$s
    return
  fi
  for (( i = 0; i < n; i++ )); do
    c=${s:i:1}
    if [[ $q == "'" ]]; then
      if [[ $c == "'" ]]; then q=; else out+=$c; fi
    elif [[ $c == '\' ]]; then
      if [[ $q == '"' && ${s:i+1:1} != [\$\`\"\\] ]]; then
        out+=$c
      else
        out+=${s:i+1:1}
        (( i++ ))
      fi
    elif [[ $q == '"' ]]; then
      if [[ $c == '"' ]]; then q=; else out+=$c; fi
    elif [[ $c == [\'\"] ]]; then
      q=$c
    else
      out+=$c
    fi
  done
  __fac_s=$out
}

# Split a `complete -p` line into __fac_w as the shell would, without
# expanding anything: blanks, single quotes and backslashes are all that
# line uses. 1 for anything else.
__fac_split_spec() {
  local s=$1 word= in= chunk
  __fac_w=()
  while [[ -n $s ]]; do
    case $s in
      [$' \t']*)
        if [[ -n $in ]]; then
          __fac_w+=("$word")
          word= in=
        fi
        s=${s#[$' \t']}
        ;;
      "'"*)
        s=${s#\'}
        [[ $s == *"'"* ]] || return 1
        word+=${s%%\'*}
        s=${s#*\'}
        in=1
        ;;
      '\'*)
        word+=${s:1:1}
        s=${s:2}
        in=1
        ;;
      [\"\$\`]*) return 1 ;;
      *)
        chunk=${s%%[\'\\\"\$\`$' \t']*}
        word+=$chunk
        s=${s#"$chunk"}
        in=1
        ;;
    esac
  done
  [[ -n $in ]] && __fac_w+=("$word")
  return 0
}

# Replay the shell's `complete` lines, all of them in order (a later line
# for a command wins, as in the shell). A line is split, not evaluated,
# and must be a `complete` that defines a spec (no -p, -r).
__fac_replay() {
  local line w k letters
  for line in "${__fac_lines[@]}"; do
    [[ -n $line ]] || continue
    __fac_split_spec "$line" || continue
    (( ${#__fac_w[@]} > 1 )) && [[ ${__fac_w[0]} == complete ]] || continue
    for (( k = 1; k < ${#__fac_w[@]}; k++ )); do
      w=${__fac_w[k]}
      [[ $w == -?* && $w != -- ]] || break
      letters=${w#-}
      [[ $letters == *[pr]* ]] && continue 2
      # An option's argument is the next word.
      [[ $letters == *[oAGWFCXPS] ]] && (( k++ ))
    done
    builtin complete "${__fac_w[@]:1}" 2>/dev/null
  done
}

# The shell's environment, but not what would change this bash itself.
__fac_env() {
  local entry name
  local -A keep=()
  for entry in "${__fac_list[@]}"; do
    name=${entry%%=*}
    [[ $entry == *=* && $name == [A-Za-z_]*([A-Za-z0-9_]) ]] || continue
    case $name in
      PATH | FAC_* | BASH | BASHOPTS | SHELLOPTS | BASH_ENV | ENV | \
        BASH_XTRACEFD | BASH_COMPAT | BASH_LOADABLES_PATH | BASH_ARGV0 | \
        POSIXLY_CORRECT | IFS | PS[0-4] | PROMPT_COMMAND | HISTFILE | \
        HISTFILESIZE | HISTSIZE | SHLVL | PWD | OLDPWD | _ | COMP_* | \
        UID | EUID | PPID | TERM | LINES | COLUMNS)
        continue
        ;;
    esac
    export "$name=${entry#*=}" 2>/dev/null
    keep[$name]=1
  done
  for name in "${__fac_imported[@]}"; do
    [[ -n ${keep[$name]+set} || $name == @(HOME|LANG) ]] || unset -v "$name" 2>/dev/null
  done
  __fac_imported=("${!keep[@]}")
}

# ctx <key> <value>
__fac_ctx() {
  local key=$1
  # What completes may change with any of them.
  __fac_tried=()
  __fac_git_tried=
  case $key in
    path)
      __fac_decode_list "$2"
      local IFS=:
      PATH="${__fac_list[*]}"
      ;;
    cwd)
      __fac_decode "$2"
      builtin cd -- "$__fac_s" 2>/dev/null
      ;;
    env)
      __fac_decode_list "$2"
      __fac_env
      ;;
    bash_completion)
      # Sourced at `init`; fac restarts the worker when it changes.
      __fac_decode "$2"
      __fac_loader=$__fac_s
      ;;
    complete)
      __fac_decode_list "$2"
      __fac_lines=("${__fac_list[@]}")
      [[ -n $__fac_ready ]] && __fac_replay
      ;;
  esac
}

# The `compopt` builtin works only for a completion readline started: this
# stands in for it. Options for the current completion (no names) go to
# __fac_o; a named spec's are left alone. With no options it prints them.
compopt() {
  local -a set=() unset=()
  local named=
  while (( $# )); do
    case $1 in
      -o) set+=("$2"); shift 2 ;;
      +o) unset+=("$2"); shift 2 ;;
      -[DEI]) named=1; shift ;;
      --) shift; break ;;
      -*) shift ;;
      *) break ;;
    esac
  done
  (( $# )) && named=1
  [[ -n $named ]] && return 0
  if (( ! ${#set[@]} && ! ${#unset[@]} )); then
    local o line=compopt
    for o in bashdefault default dirnames filenames noquote nosort nospace plusdirs; do
      if [[ -n ${__fac_o[$o]+set} ]]; then line+=" -o $o"; else line+=" +o $o"; fi
    done
    printf '%s\n' "$line"
    return 0
  fi
  local o
  for o in "${set[@]}"; do __fac_o[$o]=1; done
  for o in "${unset[@]}"; do unset -v "__fac_o[$o]"; done
  return 0
}

# Split the command line $1, cursor at char $2, as readline does for a
# completion function. Sets:
#   __fac_line, __fac_point   COMP_LINE, COMP_POINT: the current command
#                             (after the last ;, |, &, (, {, ` or newline
#                             before the cursor, its leading blanks and
#                             assignments), in chars
#   __fac_words, __fac_cword  COMP_WORDS, COMP_CWORD, split at
#                             COMP_WORDBREAKS, a run of non-blank breaks
#                             one word
#   __fac_cur                 readline's word up to the cursor ($2): after
#                             the last break, or the open quote
#   __fac_ipre                the shell word before it, unquoted
__fac_scan() {
  local s=$1 n=${#1} c q= nest= cs=0 ce i
  local -i p=$2
  ce=$n
  # The current command.
  for (( i = 0; i < n; i++ )); do
    c=${s:i:1}
    if [[ $q == "'" ]]; then
      [[ $c == "'" ]] && q=
      continue
    fi
    if [[ $c == '\' ]]; then
      (( i++ ))
      continue
    fi
    if [[ $q == '"' ]]; then
      [[ $c == '"' ]] && q=
      continue
    fi
    case $c in
      [\'\"])
        q=$c
        continue
        ;;
      '$')
        case ${s:i+1:1} in
          '(') nest+=')'; (( i++ )); continue ;;
          '{') nest+='}'; (( i++ )); continue ;;
        esac
        ;;
    esac
    if [[ -n $nest ]]; then
      case $c in
        '(') nest+=')' ;;
        [\)\}]) [[ $c == "${nest: -1}" ]] && nest=${nest%?} ;;
      esac
      continue
    fi
    # `{` only as a word of its own (`{ cmd`), not in `a{b,c}`.
    [[ $c == '{' && ${s:i+1:1} != [$' \t\n'] ]] && continue
    case $c in
      [\;\|\&\(\{\`$'\n'])
        if (( i < p )); then
          cs=$(( i + 1 ))
        else
          ce=$i
          break
        fi
        ;;
    esac
  done
  # Its leading blanks and assignments (not one the cursor is in).
  local rest end
  while :; do
    while (( cs < ce )) && [[ ${s:cs:1} == [$' \t\n'] ]]; do (( cs++ )); done
    rest=${s:cs:ce-cs}
    [[ $rest =~ ^[A-Za-z_][A-Za-z0-9_]*(\[[^]]*\])?\+?= ]] || break
    __fac_word_end "$rest" 0
    end=$(( cs + __fac_end ))
    (( end < p )) || break
    cs=$end
  done
  local cl=${s:cs:ce-cs}
  local -i pt=$(( p - cs )) m=${#cl}
  (( pt < 0 )) && pt=0
  (( pt > m )) && pt=m
  __fac_line=$cl
  __fac_point=$pt

  # The words.
  local breaks=${COMP_WORDBREAKS//[\"\'$' \t\n']/}
  local -a starts=() ends=()
  i=0
  while (( i < m )); do
    while (( i < m )) && [[ ${cl:i:1} == [$' \t\n'] ]]; do (( i++ )); done
    (( i < m )) || break
    starts+=("$i")
    c=${cl:i:1}
    if [[ -n $breaks && $breaks == *"$c"* ]]; then
      while (( i < m )) && c=${cl:i:1} && [[ $breaks == *"$c"* ]]; do (( i++ )); done
    else
      __fac_word_end "$cl" "$i" "$breaks"
      i=$__fac_end
    fi
    ends+=("$i")
  done
  local -i sentinel=$(( pt - 1 )) k cw=-1
  __fac_words=()
  for (( k = 0; k < ${#starts[@]}; k++ )); do
    if (( cw < 0 && sentinel == starts[k] - 1 )); then
      cw=${#__fac_words[@]}
    elif (( cw < 0 && sentinel < starts[k] - 1 )); then
      cw=${#__fac_words[@]}
      __fac_words+=("")
    fi
    if (( cw < 0 && sentinel >= starts[k] && sentinel < ends[k] )); then
      cw=${#__fac_words[@]}
    fi
    __fac_words+=("${cl:starts[k]:ends[k]-starts[k]}")
  done
  if (( cw < 0 )); then
    cw=${#__fac_words[@]}
    __fac_words+=("")
  fi
  __fac_cword=$cw

  # Readline's word, and the shell word before it.
  local -i ts=0 ws=0 qs=0
  q=
  for (( i = 0; i < pt; i++ )); do
    c=${cl:i:1}
    if [[ $q == "'" ]]; then
      [[ $c == "'" ]] && q=
      continue
    fi
    if [[ $c == '\' ]]; then
      (( i++ ))
      continue
    fi
    if [[ $q == '"' ]]; then
      [[ $c == '"' ]] && q=
      continue
    fi
    if [[ $c == [\'\"] ]]; then
      q=$c
      qs=$(( i + 1 ))
      continue
    fi
    # A break ends readline's word before it; `@` and `$`, readline's
    # special prefixes, start the next one.
    if [[ $COMP_WORDBREAKS == *"$c"* ]]; then
      if [[ $c == [@\$] ]]; then ts=i; else ts=$(( i + 1 )); fi
    fi
    [[ $c == [$' \t\n'\<\>\;\|\&\(\)] ]] && ws=$(( i + 1 ))
  done
  [[ -n $q ]] && ts=qs
  (( ts > pt )) && ts=pt
  __fac_cur=${cl:ts:pt-ts}
  if (( ts > ws )); then
    __fac_dequote "${cl:ws:ts-ws}"
    __fac_ipre=$__fac_s
  else
    __fac_ipre=
  fi
}

# Where the word in $1 from char $2 ends: the first blank outside quotes
# and $( ), or one of the chars $3. Into __fac_end.
__fac_word_end() {
  local s=$1 brk=${3-} c q= nest=
  local -i i=$2 n=${#1}
  for (( ; i < n; i++ )); do
    c=${s:i:1}
    if [[ $q == "'" ]]; then
      [[ $c == "'" ]] && q=
      continue
    fi
    if [[ $c == '\' ]]; then
      (( i++ ))
      continue
    fi
    if [[ $q == '"' ]]; then
      [[ $c == '"' ]] && q=
      continue
    fi
    case $c in
      [\'\"])
        q=$c
        continue
        ;;
      '$')
        case ${s:i+1:1} in
          '(') nest+=')'; (( i++ )); continue ;;
          '{') nest+='}'; (( i++ )); continue ;;
        esac
        ;;
    esac
    if [[ -n $nest ]]; then
      case $c in
        '(') nest+=')' ;;
        [\)\}]) [[ $c == "${nest: -1}" ]] && nest=${nest%?} ;;
      esac
      continue
    fi
    [[ $c == [$' \t\n'] ]] && break
    [[ -n $brk && $brk == *"$c"* ]] && break
  done
  (( i > n )) && i=n
  __fac_end=$i
}

# The spec for the command $1 (its base name if it has none), into
# __fac_spec.
__fac_lookup() {
  __fac_spec=${ builtin complete -p -- "$1" 2>/dev/null; }
  if [[ -z $__fac_spec && $1 == */* ]]; then
    __fac_spec=${ builtin complete -p -- "${1##*/}" 2>/dev/null; }
  fi
}

# Whether __fac_spec is there and its function is defined.
__fac_usable() {
  [[ -n $__fac_spec ]] || return 1
  [[ $__fac_spec =~ \ -F\ ([^ ]+) ]] || return 0
  declare -F -- "${BASH_REMATCH[1]}" >/dev/null
}

# Source the completion the command $1 ships with itself, if it has one
# and nothing else defined it: git's, found from git's own exec path. The
# system git is Apple's shim, which offers to install the developer tools
# when they are not there; its completion is looked for without running
# it.
__fac_own() {
  case $1 in
    git | gitk) ;;
    *) return 1 ;;
  esac
  [[ -z $__fac_git_tried ]] || return 1
  __fac_git_tried=1
  local git exec f
  local -a files=()
  git=$(type -P git) || return 1
  if [[ $git == /usr/bin/git ]]; then
    for exec in ${DEVELOPER_DIR:+"$DEVELOPER_DIR"} /Library/Developer/CommandLineTools \
      /Applications/Xcode.app/Contents/Developer; do
      files+=("$exec/usr/share/git-core/git-completion.bash")
    done
  else
    exec=$("$git" --exec-path 2>/dev/null) || return 1
    exec=${exec%/libexec/git-core}
    files+=("$exec/share/git-core/git-completion.bash"
      "$exec/etc/bash_completion.d/git-completion.bash"
      "$exec/share/bash-completion/completions/git")
  fi
  for f in "${files[@]}"; do
    if [[ -f $f && -r $f ]]; then
      source -- "$f" >/dev/null 2>&1
      return 0
    fi
  done
  return 1
}

# The spec readline would use for the command $1, into __fac_spec: its
# own, loaded the first time it is needed (or when its function is not
# defined here), else the default (-D) spec.
__fac_resolve() {
  local name=$1 base=${1##*/}
  __fac_lookup "$name"
  if ! __fac_usable && [[ -n $base && -z ${__fac_tried["$base"]+set} ]]; then
    __fac_tried["$base"]=1
    if declare -F _comp_load >/dev/null; then
      _comp_load -- "$name" >/dev/null 2>&1
      __fac_lookup "$name"
    fi
    __fac_usable || { __fac_own "$base" && __fac_lookup "$name"; }
  fi
  [[ -n $__fac_spec ]] || __fac_spec=${ builtin complete -p -D 2>/dev/null; }
}

# Run the spec line $1 for the current words, adding to __fac_m and
# __fac_o. $2: what the function gets as the command ($1). 124: the
# function asks for the spec to be looked up again.
__fac_run() {
  __fac_split_spec "$1" || return 1
  local -a a=("${__fac_w[@]}") actions=() got=()
  local glob= words= fn= cmd= filter= pre= suf= w
  local -i k have_glob=0 have_words=0 n=${#a[@]}
  for (( k = 1; k < n; k++ )); do
    w=${a[k]}
    case $w in
      -o) __fac_o[${a[k+1]}]=1; (( k++ )) ;;
      -A) actions+=(-A "${a[k+1]}"); (( k++ )) ;;
      -G) glob=${a[k+1]}; have_glob=1; (( k++ )) ;;
      -W) words=${a[k+1]}; have_words=1; (( k++ )) ;;
      -F) fn=${a[k+1]}; (( k++ )) ;;
      -C) cmd=${a[k+1]}; (( k++ )) ;;
      -X) filter=${a[k+1]}; (( k++ )) ;;
      -P) pre=${a[k+1]}; (( k++ )) ;;
      -S) suf=${a[k+1]}; (( k++ )) ;;
      -[DEI]) ;;
      --) break ;;
      -[abcdefgjksuv]*) actions+=("$w") ;;
      *) break ;;
    esac
  done
  local -a m=()
  __fac_dequote "$__fac_cur"
  local dcur=$__fac_s
  if (( ${#actions[@]} || have_glob || have_words )); then
    local -a args=("${actions[@]}")
    (( have_glob )) && args+=(-G "$glob")
    (( have_words )) && args+=(-W "$words")
    compgen -V got "${args[@]}" -- "$dcur" 2>/dev/null
    m+=("${got[@]}")
    # readline's own file name completion makes these file names: -f, -d,
    # -A file, -A directory.
    for w in "${actions[@]}"; do
      if [[ $w == -+([a-z]) && $w == *[fd]* || $w == @(file|directory) ]]; then
        __fac_o[filenames]=1
      fi
    done
  fi
  local prev
  if (( __fac_cword > 0 )); then
    prev=${COMP_WORDS[__fac_cword - 1]}
  else
    prev=${COMP_WORDS[0]-}
  fi
  # A function this bash does not have (one from the user's rc files)
  # gives nothing, as in readline; its name is never run as a command.
  if [[ -n $fn ]] && declare -F -- "$fn" >/dev/null; then
    COMPREPLY=()
    local IFS=$' \t\n'
    "$fn" "$2" "$__fac_cur" "$prev"
    (( $? == 124 )) && return 124
    m+=("${COMPREPLY[@]}")
    COMPREPLY=()
  fi
  if [[ -n $cmd ]]; then
    local out
    out=$(
      export COMP_LINE COMP_POINT COMP_KEY COMP_TYPE
      eval "$cmd"' "$2" "$__fac_cur" "$prev"' 2>/dev/null
    )
    local IFS=$'\n'
    set -f
    got=($out)
    set +f
    m+=("${got[@]}")
  fi
  if [[ -n $filter ]]; then
    local neg= pat=$filter
    if [[ $pat == '!'* ]]; then
      neg=1
      pat=${pat#!}
    fi
    pat=${pat//&/"$dcur"}
    got=()
    for w in "${m[@]}"; do
      if [[ $w == $pat ]]; then
        [[ -n $neg ]] && got+=("$w")
      else
        [[ -z $neg ]] && got+=("$w")
      fi
    done
    m=("${got[@]}")
  fi
  [[ -n $pre ]] && m=("${m[@]/#/"$pre"}")
  [[ -n $suf ]] && m=("${m[@]/%/"$suf"}")
  __fac_m+=("${m[@]}")
  return 0
}

# What bash itself completes for the current word: the default -o
# options' fallbacks, after the spec gave nothing (dirnames before plusdirs,
# then bashdefault and default).
__fac_fallbacks() {
  local -a got=()
  __fac_dequote "$__fac_cur"
  local dcur=$__fac_s
  if (( ! ${#__fac_m[@]} )) && [[ -n ${__fac_o[dirnames]+set} ]]; then
    compgen -V got -d -- "$dcur" 2>/dev/null
    __fac_m+=("${got[@]}")
    __fac_o[filenames]=1
  fi
  if [[ -n ${__fac_o[plusdirs]+set} ]]; then
    compgen -V got -d -- "$dcur" 2>/dev/null
    __fac_m+=("${got[@]}")
  fi
  if (( ! ${#__fac_m[@]} )) && [[ -n ${__fac_o[bashdefault]+set} ]]; then
    case $dcur in
      '$'* | '~'*)
        local mark=${dcur:0:1}
        got=()
        if [[ $mark == '$' ]]; then
          compgen -V got -v -- "${dcur#?}" 2>/dev/null
        else
          compgen -V got -u -- "${dcur#?}" 2>/dev/null
        fi
        __fac_m+=("${got[@]/#/"$mark"}")
        ;;
    esac
  fi
  if (( ! ${#__fac_m[@]} )) && [[ -n ${__fac_o[default]+set} ]]; then
    compgen -V got -f -- "$dcur" 2>/dev/null
    __fac_m+=("${got[@]}")
    __fac_o[filenames]=1
  fi
}

# The command names for a word at command position: bash's own, without
# this worker's functions. A word with a slash is a path.
__fac_commands() {
  local -a got=() fns=()
  local -A skip=()
  local w
  __fac_dequote "$__fac_cur"
  if [[ $__fac_s == */* ]]; then
    compgen -V got -f -- "$__fac_s" 2>/dev/null
    __fac_m+=("${got[@]}")
    __fac_o[filenames]=1
    return
  fi
  compgen -V fns -A function -- "$__fac_s"
  for w in "${fns[@]}"; do skip[$w]=1; done
  compgen -V got -c -- "$__fac_s" 2>/dev/null
  # A command in two PATH directories is listed twice.
  for w in "${got[@]}"; do
    [[ -n ${skip["$w"]+set} ]] && continue
    skip["$w"]=1
    __fac_m+=("$w")
  done
}

# Complete the line $1$2 (cursor after $1) into __fac_m and __fac_o.
__fac_complete() {
  __fac_scan "$1$2" "${#1}"
  COMP_LINE=$__fac_line
  COMP_POINT=$__fac_point
  COMP_WORDS=("${__fac_words[@]}")
  COMP_CWORD=$__fac_cword
  COMP_TYPE=9
  COMP_KEY=9
  __fac_m=()
  __fac_o=()
  local -i tries
  if (( COMP_CWORD == 0 )); then
    __fac_spec=${ builtin complete -p -I 2>/dev/null; }
    if [[ -n $__fac_spec ]]; then
      __fac_run "$__fac_spec" _InitialWorD_
    else
      __fac_commands
    fi
  else
    local name=${COMP_WORDS[0]}
    __fac_dequote "$name"
    name=$__fac_s
    __fac_resolve "$name"
    for (( tries = 0; tries < 3; tries++ )); do
      [[ -n $__fac_spec ]] || break
      __fac_run "$__fac_spec" "${COMP_WORDS[0]}"
      (( $? == 124 )) || break
      __fac_m=()
      __fac_o=()
      __fac_lookup "$name"
      [[ -n $__fac_spec ]] || __fac_spec=${ builtin complete -p -D 2>/dev/null; }
    done
    if [[ -z $__fac_spec ]]; then
      # No spec at all: readline completes file names.
      __fac_o[default]=1
    fi
  fi
  __fac_fallbacks
}

# The records of __fac_m for query $1, into __fac_records. A match given
# twice is sent twice: fac drops the second.
__fac_records() {
  local id=$1 w hpre suffix flags dir=
  local filenames=${__fac_o[filenames]+set} nospace=${__fac_o[nospace]+set}
  local base=
  [[ -z $filenames || -n ${__fac_o[noquote]+set} ]] && base+=Q
  [[ -n $filenames ]] && base+=f
  [[ $__fac_cur == */* ]] && dir=${__fac_cur%/*}/
  local -a __fac_enc=("$__fac_ipre" "$__fac_cur")
  __fac_encode
  local shared="${__fac_enc[0]};${__fac_enc[1]};"
  if [[ -z $filenames && -z $nospace && -z $dir ]]; then
    # No match takes a suffix or a hidden prefix: all at once (a long list
    # of commands), each with the word twice (`&`, patsub_replacement).
    __fac_enc=("${__fac_m[@]%' '}")
    __fac_encode
    # Encoded, a word has no space: an empty one shows as two.
    if [[ " ${__fac_enc[*]} " != *'  '* ]]; then
      __fac_records=("${__fac_enc[@]/*/"c $id "&";"&";;;;$base;$shared;;"}")
      return
    fi
  fi
  local -a words=() hpres=() suffixes=() flagses=()
  for w in "${__fac_m[@]}"; do
    [[ -n $w ]] || continue
    flags=$base suffix=
    if [[ $w == ?*' ' ]]; then
      w=${w%' '}
      [[ -n $nospace ]] && flags+=S suffix=' '
    elif [[ -n $nospace ]]; then
      flags+=S
    fi
    hpre=
    if [[ -n $filenames ]]; then
      if [[ $w == ?*/ ]]; then
        w=${w%/}
        [[ $flags == *S* ]] || flags+=S
        suffix=/
      fi
      if [[ $w == */?* ]]; then
        hpre=${w%/*}/
        w=${w##*/}
      fi
    elif [[ -n $dir && $w == "$dir"?* ]]; then
      hpre=$dir
      w=${w#"$dir"}
    fi
    words+=("$w")
    hpres+=("$hpre")
    suffixes+=("$suffix")
    flagses+=("$flags")
  done
  __fac_enc=("${words[@]}")
  __fac_encode
  words=("${__fac_enc[@]}")
  __fac_enc=("${hpres[@]}")
  __fac_encode
  hpres=("${__fac_enc[@]}")
  __fac_enc=("${suffixes[@]}")
  __fac_encode
  suffixes=("${__fac_enc[@]}")
  local -i k
  __fac_records=()
  for (( k = 0; k < ${#words[@]}; k++ )); do
    __fac_records+=("c $id ${words[k]};${words[k]};;;${suffixes[k]};${flagses[k]};$shared;${hpres[k]};")
  done
}

# Elapsed ms since the EPOCHREALTIME $1, into __fac_s.
__fac_ms() {
  local now=$EPOCHREALTIME then=$1
  local -i us=$(( 10#${now//[.,]/} - 10#${then//[.,]/} ))
  printf -v __fac_s '%d.%d' $(( us / 1000 )) $(( us % 1000 / 100 ))
}

# q <id> <cwd> <lbuffer> <rbuffer>
__fac_query() {
  local id=$1 lbuffer rbuffer start=$EPOCHREALTIME
  __fac_decode "$2"
  if ! builtin cd -- "$__fac_s" 2>/dev/null; then
    __fac_send "err $id cwd"
    return
  fi
  __fac_decode "$3"
  lbuffer=$__fac_s
  __fac_decode "$4"
  rbuffer=$__fac_s
  __fac_int=
  __fac_qid=$id
  __fac_complete "$lbuffer" "$rbuffer"
  __fac_qid=
  if [[ -n $__fac_int ]]; then
    # ^C from fac: the query took too long.
    __fac_int=
    __fac_m=()
    __fac_send "err $id int"
    return
  fi
  __fac_records "$id"
  __fac_ms "$start"
  __fac_records+=("d $id ${#__fac_records[@]} $__fac_s")
  __fac_m=()
  __fac_send "${__fac_records[@]}"
  __fac_records=()
}

# One line from fac, split at its spaces (an empty field is kept).
# Unknown commands are ignored.
__fac_dispatch() {
  local -a words=()
  local rest=$1
  while :; do
    words+=("${rest%% *}")
    [[ $rest == *' '* ]] || break
    rest=${rest#* }
  done
  case ${words[0]} in
    ctx) (( ${#words[@]} == 3 )) && __fac_ctx "${words[1]}" "${words[2]}" ;;
    init) (( ${#words[@]} == 1 )) && __fac_init=1 ;;
    q)
      (( ${#words[@]} == 5 )) && [[ ${words[1]} == +([0-9]) ]] &&
        __fac_query "${words[1]}" "${words[2]}" "${words[3]}" "${words[4]}"
      ;;
  esac
}

# Sourced for its functions only (fac's tests).
[[ -n $__fac_in && -n $__fac_out ]] || return 0

if [[ $BASH == /* ]] && enable -f "${BASH%/*}/../lib/bash/fdflags" fdflags 2>/dev/null; then
  fdflags -s +cloexec "$__fac_in"
  fdflags -s +cloexec "$__fac_out"
  enable -d fdflags
fi

# fac sends ^C when a query takes too long. A spec function that is in a
# command gets it too, and the query ends when the function returns; one
# that does not return gets the worker killed.
trap '__fac_int=1; (( __fac_sigs++ ))' INT

__fac_init=
__fac_sigs=0
while :; do
  __fac_seen=$__fac_sigs
  if IFS= read -r -u "$__fac_in" __fac_buf; then
    __fac_dispatch "$__fac_buf"
    if [[ -n $__fac_init ]]; then
      # At the top level, so what the loader declares stays global. Its
      # ~/.bash_completion is the user's own code, like an rc file: not run.
      __fac_init=
      __fac_start=$EPOCHREALTIME
      if [[ -n $__fac_loader && -f $__fac_loader && -r $__fac_loader ]]; then
        __fac_user_file=${BASH_COMPLETION_USER_FILE-}
        BASH_COMPLETION_USER_FILE=/dev/null
        source -- "$__fac_loader" >/dev/null 2>&1
        if [[ -n $__fac_user_file ]]; then
          BASH_COMPLETION_USER_FILE=$__fac_user_file
        else
          unset -v BASH_COMPLETION_USER_FILE
        fi
        unset -v __fac_user_file
      fi
      shopt -s extglob
      __fac_ready=1
      __fac_replay
      # git's completion and its command list, loaded now, under fac's
      # start-up timeout, not in the first `git ` query's 1 s: one
      # completion of `git `, its results thrown away.
      if type -P git >/dev/null; then
        __fac_complete 'git ' ''
        __fac_m=()
        __fac_records=()
      fi
      # One spec per line.
      __fac_specs=${ builtin complete -p; }
      __fac_count=0
      if [[ -n $__fac_specs ]]; then
        __fac_specs=${__fac_specs//[!$'\n']/}
        __fac_count=$(( ${#__fac_specs} + 1 ))
      fi
      __fac_ms "$__fac_start"
      __fac_send "ready $__fac_s $__fac_count 0"
      unset -v __fac_specs __fac_count __fac_start
    fi
  elif (( __fac_sigs != __fac_seen )); then
    # ^C between queries cut the read short (its status is EOF's).
    continue
  else
    # EOF: fac is gone.
    break
  fi
done
exit 0
