#!/bin/zsh -f
# probe.zsh <out.json>
#
# Measures how the terminal this runs in answers the queries fac sends and how
# it draws the bytes fac writes. Run it as the terminal's own command:
#
#   /bin/zsh -f probe.zsh <out.json>
#
# Stages are announced by writing their name to a file called "stage" next to
# <out.json>, so a driver can read the screen at the right moment:
#
#   wait-go       waits up to 8 s for the byte g
#   page-N        the screen filled up and is about to be cleared; waits up
#                 to 15 s for n (small panels only)
#   queries-done  every query ran; waits up to 15 s for n
#   display-done  the display rows are drawn; waits up to 15 s for n
#   bracket       "TYPE NOW": every input byte for 3 s is recorded
#   card-ready    the reflow card is drawn; waits for q, or 120 s
#                 (PROBE_CARD_SECS overrides, for a run that needs a person)
#   done          the JSON is written
#
# Replies are told apart by a fence: after each query, DA1 is sent, and a
# reply that arrives before the fence's reply belongs to the query. Every
# query row is followed by a CPR, whose column shows any junk the terminal
# printed.
zmodload zsh/zselect zsh/datetime zsh/system zsh/mathfunc || exit 1
export LC_ALL=C # read and print bytes, not characters

out=${1:?usage: probe.zsh <out.json>}
stage_file=${out:h}/stage
typeset -F 6 T0=$EPOCHREALTIME
saved_stty=$(stty -g </dev/tty)
json_done=0

typeset -a RB RT                   # input bytes and their arrival ms
typeset -a M_HEX M_T M_KIND M_TERM M_FIN M_LEAD
typeset -a Q_JSON D_JSON           # one JSON object per query / display row
typeset -a BRACKET_BYTES
typeset -g ROWS=24 COLS=80 USED=0 PAGE=0 QT0=0 CPR_ROW=0 CPR_COL=0

# --- output helpers ----------------------------------------------------------

stage() { print -rn -- "$1" >"$stage_file.tmp" && mv "$stage_file.tmp" "$stage_file"; }

# hexof <string>: the bytes of the string as lowercase hex, in REPLY.
hexof() {
  local s=$1 c h= i
  for ((i = 1; i <= ${#s}; i++)); do
    c=${s[i]}
    printf -v c '%02x' $((#c))
    h+=$c
  done
  REPLY=$h
}

# jstr <string>: the string as a JSON string, in REPLY.
jstr() {
  local s=${1//\\/\\\\}
  s=${s//\"/\\\"}
  REPLY="\"$s\""
}

# ms_since <epoch-seconds>: milliseconds from then to now, one decimal, in REPLY.
ms_since() { printf -v REPLY '%.1f' $(((EPOCHREALTIME - $1) * 1000)); }

# --- reading -----------------------------------------------------------------

# collect <fence-final-byte> <seconds> [keep]: read input into RB and RT until a
# reply that ends the exchange arrives (DA1: final byte 99 after `CSI ?`; CPR:
# final byte 82) or the time is up. With `keep` the arrays are not emptied
# first and no fence ends the wait.
collect() {
  local fin=$1 limit=$2 keep=${3:-} end now cs buf c i k
  [[ -z $keep ]] && RB=() RT=()
  end=$((EPOCHREALTIME + limit))
  while :; do
    now=$EPOCHREALTIME
    ((now >= end)) && return 1
    cs=$((int((end - now) * 100) + 1))
    zselect -r -t $cs 0 || continue
    sysread -s 4096 buf || return 1
    ms_since $QT0
    for ((i = 1; i <= ${#buf}; i++)); do
      c=${buf[i]}
      RB+=($((#c)))
      RT+=($REPLY)
    done
    if [[ -z $keep ]]; then
      parse
      for ((k = 1; k <= ${#M_HEX}; k++)); do
        [[ ${M_KIND[k]} == csi ]] || continue
        if ((fin == 82)); then
          ((M_FIN[k] == 82)) && return 0
        else
          ((M_FIN[k] == fin && M_LEAD[k] == 63)) && return 0
        fi
      done
    fi
  done
}

# parse: split RB into messages. M_KIND is csi, osc, dcs, apc, esc, text or
# partial; M_TERM is st or bel for a string reply; M_T is the arrival of its
# last byte.
parse() {
  M_HEX=() M_T=() M_KIND=() M_TERM=() M_FIN=() M_LEAD=()
  local n=${#RB} i=1 j b nb kind term fin lead h k
  while ((i <= n)); do
    b=${RB[i]} kind= term= fin=0 lead=0
    if ((b == 27)); then
      if ((i == n)); then
        kind=partial j=$i
      else
        nb=${RB[i + 1]}
        case $nb in
          91) # CSI
            j=$((i + 2))
            lead=${RB[j]:-0}
            while ((j <= n && !(RB[j] >= 64 && RB[j] <= 126))); do ((j++)); done
            if ((j > n)); then
              kind=partial j=$n
            else
              kind=csi fin=${RB[j]}
            fi
            ;;
          93 | 80 | 95 | 94 | 88) # OSC DCS APC PM SOS
            j=$((i + 2))
            while ((j <= n)); do
              if ((nb == 93 && RB[j] == 7)); then
                term=bel
                break
              fi
              if ((RB[j] == 27 && RB[j + 1] == 92)); then
                term=st
                j=$((j + 1))
                break
              fi
              ((j++))
            done
            if ((j > n)); then
              kind=partial j=$n term=
            else
              case $nb in
                93) kind=osc ;;
                80) kind=dcs ;;
                *) kind=apc ;;
              esac
            fi
            ;;
          *)
            kind=esc j=$((i + 1))
            ;;
        esac
      fi
    else
      kind=text j=$i
      while ((j + 1 <= n && RB[j + 1] != 27)); do ((j++)); done
    fi
    h=
    for ((k = i; k <= j; k++)); do
      printf -v nb '%02x' ${RB[k]}
      h+=$nb
    done
    M_HEX+=($h) M_T+=(${RT[j]}) M_KIND+=($kind) M_TERM+=($term) M_FIN+=($fin) M_LEAD+=($lead)
    i=$((j + 1))
  done
}

# drain: read and drop anything waiting; its hex is left in DRAINED.
drain() {
  local buf
  DRAINED=
  while zselect -r -t 3 0; do
    sysread -s 4096 buf || break
    hexof $buf
    DRAINED+=$REPLY
  done
}

# hold <seconds>: wait for the byte n, or the time to pass.
hold() {
  local end=$((EPOCHREALTIME + $1)) buf now
  while :; do
    now=$EPOCHREALTIME
    ((now >= end)) && return
    zselect -r -t $((int((end - now) * 100) + 1)) 0 || continue
    sysread -s 4096 buf || return
    [[ $buf == *n* ]] && return
  done
}

# --- terminal ----------------------------------------------------------------

cleanup() {
  printf '\e[?2004l\e[0m\e[?25h' >/dev/tty
  stty "$saved_stty" </dev/tty 2>/dev/null
  ((json_done)) || write_json
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP

# cpr: ask where the cursor is; sets CPR_ROW and CPR_COL (0 if unanswered).
cpr() {
  CPR_ROW=0 CPR_COL=0
  QT0=$EPOCHREALTIME
  printf '\e[6n'
  collect 82 0.5 || return 1
  local k
  for ((k = 1; k <= ${#M_HEX}; k++)); do
    if [[ ${M_KIND[k]} == csi && ${M_FIN[k]} -eq 82 ]]; then
      local body=${M_HEX[k]}
      # ESC [ r ; c R: decode the digits
      local txt= j c
      for ((j = 5; j <= ${#body} - 2; j += 2)); do
        printf -v c "\\x${body[j,j + 1]}"
        txt+=$c
      done
      CPR_ROW=${txt%%;*}
      CPR_COL=${txt##*;}
      return 0
    fi
  done
  return 1
}

# nl: end a row; start a new page (after a hold) when the screen is nearly full.
nl() {
  printf '\r\n'
  ((USED++))
  if ((USED >= ROWS - 3)); then
    ((PAGE++))
    stage page-$PAGE
    hold 15
    printf '\e[H\e[2J'
    USED=0
    [[ -n ${1:-} ]] && $1
  fi
}

# --- queries -----------------------------------------------------------------

typeset -i QN=0

# query <name> <bytes> <fence 1|0>: send the query, then a DA1 fence if asked,
# and record every reply with its time. A CPR then shows the junk it printed.
query() {
  local name=$1 bytes=$2 fence=$3 label sent_hex stray k role fence_i=0 answered=0
  local -a rep
  QN+=1
  printf -v label 'Q%02d %s: ' $QN $name
  drain
  stray=$DRAINED
  printf '%s' "$label"
  hexof $bytes
  sent_hex=$REPLY
  QT0=$EPOCHREALTIME
  printf '%s' "$bytes"
  ((fence)) && printf '\e[c'
  collect 99 1.0
  collect 0 0.05 keep # a late reply, after the fence
  parse
  # the first DA1 reply (CSI ? ... c) is the fence
  for ((k = 1; k <= ${#M_HEX}; k++)); do
    if [[ ${M_KIND[k]} == csi && ${M_FIN[k]} -eq 99 && ${M_LEAD[k]} -eq 63 ]]; then
      fence_i=$k
      break
    fi
  done
  for ((k = 1; k <= ${#M_HEX}; k++)); do
    if ((fence_i == 0 || k < fence_i)); then
      role=reply
    elif ((k == fence_i)); then
      role=fence
      ((fence)) || role=reply
    else
      role=late
    fi
    [[ $role == reply && ${M_KIND[k]} != text && ${M_KIND[k]} != partial ]] && ((answered++))
    jstr "${M_TERM[k]}"
    rep+=("{\"hex\": \"${M_HEX[k]}\", \"ms\": ${M_T[k]}, \"kind\": \"${M_KIND[k]}\", \"term\": $REPLY, \"role\": \"$role\"}")
  done
  # a fence-less DA1 query is answered by the DA1 reply itself
  ((fence == 0 && fence_i > 0 && answered == 0)) && answered=1
  local fence_ms=null
  ((fence_i > 0)) && fence_ms=${M_T[fence_i]}
  local expect=$((${#label} + 1))
  cpr
  local junk=$((CPR_COL - expect))
  ((CPR_COL == 0)) && junk=null
  jstr "$name"
  Q_JSON+=("{\"id\": $QN, \"name\": $REPLY, \"sent_hex\": \"$sent_hex\", \"fenced\": $((fence ? 1 : 0)), \"answered\": $((answered > 0 ? 1 : 0)), \"fence_ms\": $fence_ms, \"stray_before_hex\": \"$stray\", \"cursor_col_after\": $CPR_COL, \"cursor_col_expected\": $expect, \"junk_cols\": $junk, \"messages\": [${(j:, :)rep}]}")
  nl
}

# --- display rows ------------------------------------------------------------

LABEL_W=10
ruler() {
  printf '%*s' $LABEL_W ''
  local i
  for ((i = 0; i < COLS - LABEL_W; i++)); do printf '%d' $((i % 10)); done
  printf '\r\n'
  ((USED++))
}

# drow <name> <expected-extra-cols>: after the row's bytes, a CPR records where
# the cursor stands; the expected column is the label plus the visible width.
typeset -i DN=0
dstart() { printf '%-*s' $LABEL_W "$1"; DNAME=$1; }
dend() { # dend <expected-cursor-col-or-0> [note]
  local expect=$1 note=${2:-}
  cpr
  DN+=1
  jstr "$DNAME"
  local nm=$REPLY
  jstr "$note"
  D_JSON+=("{\"name\": $nm, \"cursor_row\": $CPR_ROW, \"cursor_col\": $CPR_COL, \"expected_col\": $expect, \"note\": $REPLY}")
  nl ruler
}

# --- JSON --------------------------------------------------------------------

write_json() {
  json_done=1
  local names n pat v k
  local -a env_vals env_names hit exported quoted
  for n in TERM COLORTERM TERM_PROGRAM TERM_PROGRAM_VERSION LC_TERMINAL LC_TERMINAL_VERSION ZED_TERM; do
    if ((${+parameters[$n]})); then
      jstr "${(P)n}"
      env_vals+=("\"$n\": $REPLY")
    else
      env_vals+=("\"$n\": null")
    fi
  done
  exported=(${(k)parameters[(R)*export*]})
  for pat in 'VSCODE_*' 'ITERM_*' 'KERO_*' 'WARP_*' 'Q_TERM*'; do
    hit=(${(o)${(M)exported:#${~pat}}})
    quoted=()
    for n in $hit; do quoted+=("\"$n\""); done
    env_names+=("\"$pat\": [${(j:, :)quoted}]")
  done
  local bracket=${(j::)BRACKET_BYTES}
  {
    printf '{\n'
    printf '  "tool": "probe.zsh",\n'
    printf '  "rows": %s,\n  "cols": %s,\n' $ROWS $COLS
    printf '  "env": {%s},\n' "${(j:, :)env_vals}"
    printf '  "env_names": {%s},\n' "${(j:, :)env_names}"
    printf '  "queries": [\n    %s\n  ],\n' "${(pj:,\n    :)Q_JSON}"
    printf '  "display": [\n    %s\n  ],\n' "${(pj:,\n    :)D_JSON}"
    printf '  "bracketed": {"bytes": "%s", "wrapped": %s}\n' "$BRACKET_HEX" "$BRACKET_WRAPPED"
    printf '}\n'
  } >"$out.tmp" && mv "$out.tmp" "$out"
}
BRACKET_HEX= BRACKET_WRAPPED=false

# --- run ---------------------------------------------------------------------

stty raw -echo </dev/tty
exec </dev/tty >/dev/tty

stage wait-go
end=$((EPOCHREALTIME + 8))
while ((EPOCHREALTIME < end)); do
  zselect -r -t 20 0 || continue
  sysread -s 4096 buf || break
  [[ $buf == *g* ]] && break
done

sz=$(stty size)
ROWS=${sz% *} COLS=${sz#* }
printf '\e[H\e[2J'

# 1. queries
query DA1 $'\e[c' 0
query DA2 $'\e[>c' 1
query XTVERSION $'\e[>q' 1
query DECRQM-2026 $'\e[?2026$p' 1
query DECRQM-2027 $'\e[?2027$p' 1
query DECRQM-2004 $'\e[?2004$p' 1
query DECRQM-1049 $'\e[?1049$p' 1
query OSC10-ST $'\e]10;?\e\\' 1
query OSC10-BEL $'\e]10;?\a' 1
query OSC11-ST $'\e]11;?\e\\' 1
query OSC11-BEL $'\e]11;?\a' 1
for i in 2 3 4 5 6; do
  query OSC4-$i $'\e]4;'$i$';?\e\\' 1
done
query OSC4-combined $'\e]4;2;?;3;?;4;?;5;?;6;?\e\\' 1
query CPR $'\e[6n' 1
query KITTY $'\e[?u' 1
# fac's PROBE bytes (src/term/caps.rs), one write, no fence of its own
query FAC-PROBE $'\e[?2026$p\e]11;?\e\\\e]4;2;?\e\\\e]4;3;?\e\\\e]4;4;?\e\\\e]4;5;?\e\\\e]4;6;?\e\\\e[6n\e[c' 0
printf -- '-- queries done --'
stage queries-done
hold 15
printf '\e[H\e[2J'
USED=0

# 2. display rows
ruler
dstart OSC133A;   printf '[\e]133;A\a]';           dend 13
dstart OSC133B;   printf '[\e]133;B\a]';           dend 13
dstart OSC133C;   printf '[\e]133;C\a]';           dend 13
dstart OSC133D;   printf '[\e]133;D;0\a]';         dend 13
dstart OSC133P;   printf '[\e]133;P;k=s\a]';       dend 13
dstart OSC7;      printf '[\e]7;file://localhost/tmp\a]'; dend 13
dstart OSC7790;   printf '[\e]7790;hi\a]';         dend 13
dstart italic;    printf '\e[3mitalic\e[0m';       dend 17 'italic text, look at the screenshot'
# Step 6: which form of italic Zed draws upright (fac's description line is
# italic in the other terminals and upright in Zed). Each row says italic
# text in the screenshot when that form is drawn italic.
dstart italic-fg; printf '\e[3;38;2;170;180;190mitalic fg\e[0m'; dend 20 'italic with a truecolor foreground: slanted letters?'
dstart italic-bg; printf '\e[3;48;2;20;28;36mitalic bg\e[0m'; dend 20 'italic with a truecolor background: slanted letters?'
dstart fac-band;  printf '\e[0;3;38;2;170;180;190;48;2;20;28;36m fac band line \e[0m'; dend 26 "fac's exact band SGR (0;3;fg;bg in one sequence): slanted letters?"
dstart italic-delta; printf '\e[0;38;2;170;180;190;48;2;20;28;36m plain \e[3m italic by delta \e[0m'; dend 35 "plain text, then 3 alone after a 0;fg;bg style (as fac's delta paints): is the second part slanted?"
dstart bg256;     printf '\e[48;5;24m  256  \e[0m'; dend 18 '256-colour background'
dstart truecolor; printf '\e[48;2;0;95;135m  rgb  \e[0m'; dend 18 'truecolor background'
dstart BCE;       printf '\e[44m\e[5X\e[0m\e[5C<- 5 cells blue if ECH erases with the background'; dend 0 'BCE under ECH'
# DECSC/DECRC mid-row: abQYZ if the cursor came back after ab
dstart DECSC-mid; printf 'ab\e7XYZ\e8Q';            dend 14 'abQYZ expected if the cursor came back after ab'
# at a pending wrap: print to the last column, DECSC, X, DECRC, Y
dstart DECSC-wrap
for ((i = LABEL_W; i < COLS; i++)); do printf '.'; done
printf '\e7X\e8Y'
dend 0 'cursor (r,cols) = DECRC restored the pending wrap lost; (r+1,2) = pending wrap kept'
# wide characters: width = column after the trailing bar - 13
wide() { # wide <name> <string>
  dstart "$1"
  printf '|%s|' "$2"
  cpr
  DN+=1
  jstr "$DNAME"
  local nm=$REPLY
  jstr "$2"
  D_JSON+=("{\"name\": $nm, \"text\": $REPLY, \"cursor_row\": $CPR_ROW, \"cursor_col\": $CPR_COL, \"width\": $((CPR_COL - 13)), \"note\": \"width = cursor_col - 13\"}")
  nl ruler
}
wide cjk '漢字'
wide flag '🇮🇱'
wide zwj '👨‍👩‍👧'
wide vs16 '❤️'
wide ambig '±§'
# scroll with a background: bottom row, background set, newline, reset
printf '\e[%d;1H' $ROWS
printf '%-*s' $LABEL_W scrollbg
printf '\e[44m\n\e[0m\rbelow:'
printf '\e[H'
stage display-done
hold 15
printf '\e[H\e[2J'

# 3. bracketed text
printf '\e[?2004h'
printf 'TYPE NOW'
stage bracket
RB=() RT=()
QT0=$EPOCHREALTIME
collect 0 3.0 keep
for b in $RB; do printf -v h '%02x' $b; BRACKET_BYTES+=($h); done
BRACKET_HEX=${(j::)BRACKET_BYTES}
[[ $BRACKET_HEX == *1b5b3230307e* ]] && BRACKET_WRAPPED=true
printf '\e[?2004l'

# 4. reflow card
printf '\e[H\e[2J'
line=
for ((i = 0; i < COLS; i++)); do line+=.; done
printf '\e[1;1H%s' $line
for ((r = 2; r <= 7; r++)); do
  printf '\e[%d;20H\e[48;5;24m%-30s\e[0m' $r " card row $((r - 1))"
done
printf '\e[9;1HREADY'
stage card-ready
end=$((EPOCHREALTIME + ${PROBE_CARD_SECS:-120}))
while ((EPOCHREALTIME < end)); do
  zselect -r -t 50 0 || continue
  sysread -s 4096 buf || break
  [[ $buf == *q* ]] && break
done
write_json
stage done
