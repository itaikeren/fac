# fac's fish hook, sourced by fish's --init-command, so after conf.d and
# config.fish.
#
# fish has no hook that runs on every edit: after a key, fac types a
# private trigger key (U+10FFFD), bound here in every mode to a function
# that reports the command line. It tells fac, over private OSC sequences
# that fac strips from the output:
#   7790  the command line (buffer report), from the trigger's binding
#   7791  context: the trigger keys, `defer`, `armed`, PATH, cwd, the
#         exported environment, the history file, fish_function_path,
#         fish_complete_path, fish_escape_delay_ms
#   7792  each executed command
# fish sends OSC 133 and OSC 7 itself. Edits from fac come on the pipe at
# fd $__fac_cmd_fd, read when fac types the apply trigger (U+10FFFC).
#
# Message format: ESC ] 779N ; 1:<token> ; <seq> ; <field> ... BEL, the
# token the session's (fac drops any message without it). Fields are
# percent-encoded (fac decodes any %XX); a list is its elements encoded,
# joined with `,`. See src/shell/protocol.rs.

# No trace (fish_trace) of this file, which reads the session token, nor
# of the functions below that send it: each sets it empty for itself.
set -l fish_trace

# fish cannot mark an fd close-on-exec, so the command pipe reaches the
# shell's children. Startup is done: Kiro's pre block (in config.fish) has
# seen PROCESS_LAUNCHED_BY_Q, and the shell's children must not inherit it.
set -g __fac_cmd_fd $FAC_CMD_FD
set -e PROCESS_LAUNCHED_BY_Q
set -e FAC_CMD_FD
# FAC_SHELL keeps `fac init`'s conf.d/fac.fish from starting fac again in
# this shell (src/init/mod.rs, "No loops"): kept, but no longer exported,
# so children do not inherit it.
set -q FAC_SHELL; and set -gu FAC_SHELL $FAC_SHELL

# Only under fac, and once.
set -q FAC_ACTIVE[1]; or return 0
set -q __fac_loaded; and return 0
set -g __fac_loaded 1

# The session token: the first line fac wrote on the command pipe, before
# fish started (src/shell/protocol.rs, "The session token"). Not from the
# environment: config.fish has run by now, and its programs would have
# inherited it. The pipe is non-blocking, so `read` takes what is there. A
# `ta` fac wrote while fish started (an Enter typed meanwhile) is kept for
# the first prompt's look.
set -g __fac_token
set -g __fac_init_marks
if string match -qr -- '^[0-9]+$' "$__fac_cmd_fd"
    read -z -l init </dev/fd/$__fac_cmd_fd 2>/dev/null
    for line in (string split -n \n -- $init)
        if string match -qr -- '^token [0-9a-f]{32}$' $line
            set __fac_token (string sub -s 7 -- $line)
        else
            set -a __fac_init_marks $line
        end
    end
end
# Every message's first field: the version and the token (with no token,
# the bare version, which fac drops all the same).
set -g __fac_v 1
test -n "$__fac_token"; and set __fac_v 1:$__fac_token

# One counter for every 779x message.
set -g __fac_seq 0
# The seq of the latest 7790 report; an apply for any other seq is stale.
set -g __fac_report_seq 0
# Whether fac asked for a look at the input (`ta`) since the last look
# (src/shell/trigger.rs, "Typeahead").
set -g __fac_marked 0
# 1 when the key bindings have vi's insert mode: `v` outside it.
set -g __fac_vi 0
# The `bind` listing `defer` was built from.
set -g __fac_binds
# The modes the trigger is bound in.
set -g __fac_modes

# 7791 for key $argv[1] with the encoded value $argv[2], unless that value
# was sent last.
function __fac_context --argument-names key value
    set -l fish_trace
    set -l sent __fac_sent_$key
    if set -q $sent; and test "$$sent" = "$value"
        return 0
    end
    set -g $sent $value
    set -g __fac_seq (math $__fac_seq + 1)
    printf '\e]7791;%s;%d;%s;%s\a' "$__fac_v" $__fac_seq $key $value
end

# The arguments as one list field.
function __fac_list
    string join , -- (string escape --style=url -- $argv)
end

# The flags of a report: `v` outside vi's insert mode, `i` while fish's own
# pager (completions, history) is open.
function __fac_flags
    set -l flags ''
    if test $__fac_vi = 1; and test "$fish_bind_mode" != insert
        set flags v
    end
    if commandline --paging-mode
        set flags "$flags"i
    end
    echo $flags
end

# 7790: start, the cursor in chars, the flags, no prebuffer (fish edits a
# multi-line command as one buffer), the buffer. `commandline -b` ends with
# one newline of its own, kept by `collect -N` (a trailing newline of the
# buffer is kept with it) and cut from the encoded text.
function __fac_report
    set -l fish_trace
    set -g __fac_seq (math $__fac_seq + 1)
    set -g __fac_report_seq $__fac_seq
    set -l buffer (string escape --style=url -- (commandline -b | string collect -N))
    printf '\e]7790;%s;%d;start;%d;%s;;%s\a' "$__fac_v" $__fac_seq (commandline -C) \
        (__fac_flags) \
        (string replace -r -- '%0A$' '' $buffer)
end

# A field from fac (`%`, `\`, space and controls as %XX, UTF-8 raw) decoded
# into the global $__fac_out, exactly: printf turns each %XX, made \xXX,
# into its byte, and `read -z` keeps every newline.
function __fac_decode --argument-names field
    set -l format (string replace -a -- % '\x' $field)
    set -g __fac_out
    printf "%s$format" '' | read -gz __fac_out
end

# The apply trigger's binding: every apply waiting on the pipe, `apply
# <seq> <L> <R> <LINE>`. The one for the latest report, whose line is the
# line now, replaces it; the others are stale. The pipe is non-blocking,
# so with none waiting `read` returns at once; fac writes an apply whole
# before it types this key. Then the report, applied or not.
function __fac_apply
    set -l data
    read -z -l data </dev/fd/$__fac_cmd_fd 2>/dev/null
    for line in (string split -n \n -- $data)
        __fac_take_mark $line; and continue
        set -l w (string split ' ' -- $line)
        test (count $w) -eq 5; and test "$w[1]" = apply; and test "$w[2]" = "$__fac_report_seq"
        or continue
        __fac_decode $w[5]
        set -l now (commandline -b | string collect -N)
        test "$now" = "$__fac_out"\n; or continue
        __fac_decode $w[3]
        set -l left $__fac_out
        __fac_decode $w[4]
        commandline -r -- "$left$__fac_out"
        commandline -C (string length -- "$left")
    end
    __fac_report
end

# `ta` from the command pipe (look at the input at the next prompt) into
# $__fac_marked; false for any other line.
function __fac_take_mark --argument-names line
    test "$line" = ta; or return 1
    set -g __fac_marked 1
    return 0
end
for line in $__fac_init_marks
    __fac_take_mark $line
end
set -e __fac_init_marks

# The look at the input for typeahead (src/shell/trigger.rs,
# "Typeahead"): the lines waiting on the command pipe are read (stale
# applies go), and after a `ta`, fac is asked to look (`look`); fish's line
# editor reads nothing before its fish_prompt event. fac counts the bytes
# of the terminal's input no one has read, from its end, and answers on
# the pipe (`looked <seq> <mark> <bytes>`). The pipe is non-blocking, so
# fish polls it, a few times at once, then with a 5 ms sleep between, for
# about a second at most; nothing is read meanwhile, so the count is this
# moment's. `unread` sends fac's numbers back, which is what settles
# anything: an answer that comes later is never sent back.
function __fac_queue
    set -l fish_trace
    set -l data
    read -z -l data </dev/fd/$__fac_cmd_fd 2>/dev/null
    for line in (string split -n \n -- $data)
        __fac_take_mark $line
    end
    test $__fac_marked = 1; or return 0
    set -g __fac_marked 0
    set -g __fac_seq (math $__fac_seq + 1)
    set -l seq $__fac_seq
    printf '\e]7791;%s;%d;look;\a' "$__fac_v" $seq
    set -l tries 0
    set -l answered
    while test $tries -lt 160
        set tries (math $tries + 1)
        read -z -l data </dev/fd/$__fac_cmd_fd 2>/dev/null
        # Every line of the read, the ones after the answer too (a `ta`).
        for line in (string split -n \n -- $data)
            if not set -q answered[1]
                and string match -qr -- "^looked $seq [0-9]{1,20} [0-9]{1,20}\$" $line
                set answered 1
                set -l w (string split ' ' -- $line)
                set -g __fac_seq (math $__fac_seq + 1)
                printf '\e]7791;%s;%d;unread;%s,%s\a' "$__fac_v" $__fac_seq $w[3] $w[4]
                # Something was unread: look again at the next prompt,
                # whether or not fac's `ta` for it comes in time.
                test "$w[4]" = 0; or set -g __fac_marked 1
                continue
            end
            __fac_take_mark $line
        end
        set -q answered[1]; and return 0
        test $tries -gt 20; and command /bin/sleep 0.005
    end
end

# Bind the trigger and the apply trigger in each mode not bound yet.
function __fac_bind
    for mode in $argv
        contains -- $mode $__fac_modes; and continue
        bind -M $mode \U10FFFD __fac_report
        bind -M $mode \U10FFFC __fac_apply
        set -g -a __fac_modes $mode
    end
end

# The legacy bytes of one fish key name (`ctrl-x`, `alt-e`, `f1`, `a`,
# `\e[A`); nothing for a key fac cannot match.
function __fac_key_bytes --argument-names key
    set -l mods
    while string match -qr -- '^(ctrl|alt|shift|super)-.' $key
        set -l parts (string split -m1 -- - $key)
        set -a mods $parts[1]
        set key $parts[2]
    end
    contains -- super $mods; and return 1
    set -l base
    switch $key
        case space
            set base ' '
        case comma
            set base ,
        case minus
            set base -
        case enter
            set base \r
        case tab
            set base \t
        case backspace
            set base \x7f
        case escape
            set base \e
        case delete
            set base \e\[3~
        case insert
            set base \e\[2~
        case up
            set base \e\[A
        case down
            set base \e\[B
        case right
            set base \e\[C
        case left
            set base \e\[D
        case home
            set base \e\[H
        case end
            set base \e\[F
        case pageup
            set base \e\[5~
        case pagedown
            set base \e\[6~
        case f1
            set base \eOP
        case f2
            set base \eOQ
        case f3
            set base \eOR
        case f4
            set base \eOS
        case f5
            set base \e\[15~
        case f6
            set base \e\[17~
        case f7
            set base \e\[18~
        case f8
            set base \e\[19~
        case f9
            set base \e\[20~
        case f10
            set base \e\[21~
        case f11
            set base \e\[23~
        case f12
            set base \e\[24~
        case '*'
            # One character, or a raw sequence (`\e[A`, as bind prints it).
            test -n "$key"; or return 1
            set base $key
    end
    if contains -- shift $mods
        if string match -qr -- '^[a-z]$' $base
            set base (string upper -- $base)
        else if test "$base" = \t
            set base \e\[Z
        else
            return 1
        end
    end
    if contains -- ctrl $mods
        set -l c (string lower -- $base)
        set -l ascii a b c d e f g h i j k l m n o p q r s t u v w x y z \[ \\ \] ^ _
        set -l at (contains -i -- $c $ascii)
        if test -n "$at"
            set base (printf \\x(printf %02x $at))
        else if test "$c" = /
            set base \x1f
        else
            return 1
        end
    end
    contains -- alt $mods; and set base \e$base
    printf %s $base
end

# The builtins and keywords a binding's script may start with and still
# read no key and run no program.
set -g __fac_safe set commandline test if for while status and or not begin \
    string math count echo printf true false
# fish's own functions that open a program on the terminal (an editor, a
# pager, man) or print over the prompt: `n`.
set -g __fac_programs edit_command_buffer __fish_man_page man \
    __fish_preview_current_file __fish_list_current_token \
    __fish_whatis_current_token
# What accepts the line (fish's `execute`, also from `commandline -f`, and
# its own functions that call it): `a`, so that no trigger follows a key
# bound to one, the user's own included (kitty's ctrl-m is such a key), and
# typed ahead it is an Enter.
set -g __fac_accepts execute __fish_toggle_comment_commandline
# Bindings after which the shell waits for a key of its own (a count, an
# operator, a jump target): `p`.
set -g __fac_pending forward-jump backward-jump forward-jump-till \
    backward-jump-till get-key fish_vi_arg_digit fish_vi_start_operator
# The modes a binding can enter that wait for one key.
set -g __fac_pending_modes f F t T operator replace_one

# Whether $argv[1] is one of fish's own functions (from its data dir).
function __fac_own_function --argument-names name
    set -l file (functions --details -- $name 2>/dev/null)
    string match -q -- 'embedded:*' $file; and return 0
    set -q __fish_data_dir[1]; and string match -q -- "$__fish_data_dir/*" $file
end

# The class of a binding's commands: a (one that accepts the line), n (a
# program, or a widget of the user's: a command that is not fish's own), p
# (pending), or nothing (safe).
function __fac_class --argument-names new_mode
    set -e argv[1]
    set -l class
    if contains -- $new_mode $__fac_pending_modes
        set class p
    end
    for command in $argv
        # `execute` anywhere in it, as an input function (`commandline -f
        # repaint execute`).
        if string match -qr -- '(^|[\s;])execute([\s;]|$)' $command
            echo a
            return
        end
        set -l word (string split -m1 -f1 ' ' -- (string trim -- $command))
        set word (string replace -r -- ';$' '' $word[1])
        # An empty command (a mode switch alone).
        if test -z "$word"
            continue
        else if contains -- $word $__fac_pending
            set class p
        else if contains -- $word $__fac_accepts
            echo a
            return
        else if contains -- $word $__fac_programs
            echo n
            return
        else if contains -- $word $__fac_input_functions $__fac_safe
            continue
        else if not __fac_own_function $word
            echo n
            return
        end
    end
    echo $class
end

# `defer` from the live bindings: each key after which no trigger may go
# at once, with its class (see "The defer list" in protocol.rs). vi's
# modes other than insert are vi command mode (upper case).
function __fac_defer
    set -g __fac_binds $argv
    set -g __fac_input_functions (bind --function-names)
    set -g __fac_vi 0
    bind -M insert '' >/dev/null 2>&1; and set -g __fac_vi 1
    # Per mode group (0: every mode, 1: vi command mode): the keys and
    # their classes, the strongest kept.
    set -l keys0
    set -l classes0
    set -l keys1
    set -l classes1
    for line in $argv
        string match -q -- 'bind *' $line; or continue
        set -l t
        echo $line | read -lat t
        set -e t[1]
        set -l mode default
        set -l new_mode
        while string match -q -- '-*' $t[1]
            switch $t[1]
                case -M --mode
                    set mode $t[2]
                    set -e t[1]
                case -m --sets-mode
                    set new_mode $t[2]
                    set -e t[1]
            end
            set -e t[1]
        end
        set -l key $t[1]
        set -e t[1]
        # The generic binding, and fac's own.
        test -n "$key"; or continue
        contains -- __fac_report $t; and continue
        contains -- __fac_apply $t; and continue
        set -l class (__fac_class "$new_mode" $t)
        # A key sequence: every proper prefix is a prefix key, so the whole
        # needs an element even when safe; its report waits for no key.
        set -l parts (string split -- , $key)
        test -z "$class"; and test (count $parts) -gt 1; and set class i
        test -n "$class"; or continue
        set -l bytes
        for part in $parts
            set -l b (__fac_key_bytes $part)
            or begin
                set bytes
                break
            end
            set -a bytes $b
        end
        test -n "$bytes"; or continue
        set bytes (string join '' -- $bytes)
        # The strongest class of a key wins: a, n, then p, then i.
        if test $__fac_vi = 1; and test $mode != insert
            set -l at (contains -i -- $bytes $keys1)
            if test -z "$at"
                set -a keys1 $bytes
                set -a classes1 $class
            else if test (__fac_rank $class) -gt (__fac_rank $classes1[$at])
                set classes1[$at] $class
            end
        else
            set -l at (contains -i -- $bytes $keys0)
            if test -z "$at"
                set -a keys0 $bytes
                set -a classes0 $class
            else if test (__fac_rank $class) -gt (__fac_rank $classes0[$at])
                set classes0[$at] $class
            end
        end
    end
    set -l elements
    set -l i 0
    for key in $keys0
        set i (math $i + 1)
        set -a elements $classes0[$i]$key
    end
    # vi command mode's: upper case.
    set i 0
    for key in $keys1
        set i (math $i + 1)
        set -a elements (string upper -- $classes1[$i])$key
    end
    __fac_context defer "$(__fac_list $elements)"
end

function __fac_rank --argument-names class
    switch $class
        case a
            echo 4
        case n
            echo 3
        case p
            echo 2
        case '*'
            echo 1
    end
end

# The history file: $__fish_user_data_dir/<session>_history; none when
# fish_history is empty (fish saves no history then).
function __fac_histfile
    set -l session fish
    if set -q fish_history
        test -n "$fish_history"; or return 0
        test "$fish_history" = default; or set session $fish_history
    end
    printf '%s/%s_history' $__fish_user_data_dir $session
end

# Every context key, at each prompt; only what changed is sent.
function __fac_prompt --on-event fish_prompt
    # The bindings are complete by the first prompt (fish sets its key
    # bindings up after the init command); a mode the user added is bound
    # too, and `defer` follows the bindings.
    __fac_bind (bind --list-modes)
    set -l binds (bind)
    test "$binds" = "$__fac_binds"; or __fac_defer $binds
    __fac_queue
    __fac_context armed $fish_pid
    __fac_context path "$(__fac_list $PATH)"
    __fac_context cwd "$(string escape --style=url -- $PWD)"
    __fac_context histfile "$(string escape --style=url -- "$(__fac_histfile)")"
    __fac_context fish_function_path "$(__fac_list $fish_function_path)"
    __fac_context fish_complete_path "$(__fac_list $fish_complete_path)"
    set -l delay 30
    set -q fish_escape_delay_ms[1]; and set delay $fish_escape_delay_ms[1]
    __fac_context escape_delay $delay
    # The exported environment, as NAME=value, for the commands spec
    # generators run. A path variable (fish exports it joined with `:`).
    set -l env
    for name in (set --names --export)
        if set -q --path $name
            set -a env "$name="(string join : -- $$name)
        else
            set -a env "$name="(string join ' ' -- $$name)
        end
    end
    __fac_context env "$(__fac_list $env)"
    # `armed` is sent at every prompt, changed or not.
    set -e __fac_sent_armed
end

# The cwd as soon as it changes (a binding may cd with no prompt after
# it): fac takes no OSC 7 as its own cwd, since any program can print one.
function __fac_cwd --on-variable PWD
    __fac_context cwd "$(string escape --style=url -- $PWD)"
end

# The line is accepted: the report with `f`, then the command (7792).
function __fac_preexec --on-event fish_preexec
    set -l fish_trace
    set -l line (string escape --style=url -- $argv[1])
    set -g __fac_seq (math $__fac_seq + 1)
    printf '\e]7790;%s;%d;start;%d;f;;%s\a' "$__fac_v" $__fac_seq \
        (string length -- $argv[1]) $line
    set -g __fac_seq (math $__fac_seq + 1)
    printf '\e]7792;%s;%d;%s\a' "$__fac_v" $__fac_seq $line
end

__fac_bind default insert visual replace replace_one operator f F t T
__fac_context trigger (string escape --style=url -- \U10FFFD)
__fac_context apply_trigger (string escape --style=url -- \U10FFFC)
