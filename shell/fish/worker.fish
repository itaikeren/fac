# fac's fish completion worker: a hidden `fish --no-config` that fac keeps
# warm on its own PTY and asks what fish's own completions (`complete -C`)
# would offer for a command line.
#
# fac starts it as `fish --no-config -c 'source $argv[1]' <this file>`.
# Commands come in on the pipe at fd $FAC_WORKER_IN and results go out on
# the pipe at fd $FAC_WORKER_OUT, one line each. The PTY is there only as
# a terminal for what completions run; fac discards whatever is drawn on it.
#
# fac -> worker (fields percent-encoded: %, \, ;, space, every byte below
# 0x20 and 0x7f; a list is its elements, each also with , as %2C, joined
# with ,):
#   ctx <key> <value>   path, fish_function_path, fish_complete_path
#                       (lists): the user's, so their own functions and
#                       completions load
#   q <id> <cwd> <lbuffer> <rbuffer>
# worker -> fac (fields percent-encoded, fac decodes any %XX):
#   ready <ms> <completions> <rebuilt>    at once: fish has no setup
#   c <id> <word>;<display>;<description>;<group>;<suffix>;<flags>;<iprefix>;<prefix>;<apre>;<hpre>;<hsuf>
#   d <id> <count> <elapsed-ms>
#   err <id> <code>     cwd (cd failed)
# `complete -C` gives each match as the whole token, unquoted, with a TAB
# and its description. A directory's trailing `/` becomes the suffix (flag
# S); a match with no description that names an existing file gets flag
# f. The fields fish has no answer for stay empty. fish completes the token
# at the end of <lbuffer>; <rbuffer> is not used.

# The PTY is a terminal for completions only: no echo, and fac's ^C (sent
# when a query takes too long) interrupts nothing; fac kills a worker that
# takes too long.
stty -echo -isig -ixon 2>/dev/null

# A field from fac decoded into $__fw_out, exactly: printf turns each %XX,
# made \xXX, into its byte, and `read -z` keeps every newline.
function __fw_decode --argument-names field
    set -l format (string replace -a -- % '\x' $field)
    set -g __fw_out
    printf "%s$format" '' | read -gz __fw_out
end

# A list field from fac decoded into $__fw_list.
function __fw_decode_list --argument-names field
    set -g __fw_list
    test -n "$field"; or return 0
    for element in (string split -- , $field)
        __fw_decode $element
        set -a __fw_list $__fw_out
    end
end

# ctx <key> <value>
function __fw_ctx --argument-names key value
    __fw_decode_list $value
    switch $key
        case path
            set -gx PATH $__fw_list
        case fish_function_path
            set -g fish_function_path $__fw_list
        case fish_complete_path
            set -g fish_complete_path $__fw_list
    end
end

# q <id> <cwd> <lbuffer> <rbuffer>
function __fw_query --argument-names id cwd lbuffer
    __fw_decode $cwd
    if not cd -- $__fw_out 2>/dev/null
        echo "err $id cwd"
        return
    end
    __fw_decode $lbuffer
    # Nothing a completion runs reads fac's commands.
    set -l found (complete -C -- $__fw_out </dev/null 2>/dev/null)
    set -l records
    for match in $found
        set -l parts (string split -m1 -- \t $match)
        set -l word $parts[1]
        set -l description $parts[2]
        test -n "$word"; or continue
        set -l suffix
        set -l flags
        if string match -q -- '*/' $word; and test "$word" != /
            set word (string sub -e -1 -- $word)
            set suffix /
            set flags S
        else if test -z "$description"; and test -e "$word"
            set flags f
        end
        set -l fields (string escape --style=url -- $word $word $description)
        set -a records "c $id $fields[1];$fields[2];$fields[3];;$suffix;$flags;;;;;"
    end
    set -a records "d $id "(count $records)" 0"
    printf '%s\n' $records
end

# One line from fac. Unknown commands are ignored.
function __fw_dispatch --argument-names line
    set -l w (string split ' ' -- $line)
    switch $w[1]
        case ctx
            test (count $w) -eq 3; and __fw_ctx $w[2] $w[3]
        case q
            test (count $w) -eq 5; and string match -qr '^[0-9]+$' -- $w[2]
            and __fw_query $w[2] $w[3] $w[4]
    end
end

# The loop reads the pipe through one fd opened once (fish's `read` takes
# one line at a time from it), and writes every answer to the result pipe.
function __fw_main
    echo "ready 0 - 0"
    while read -l line
        __fw_dispatch $line
    end
end

__fw_main </dev/fd/$FAC_WORKER_IN >/dev/fd/$FAC_WORKER_OUT
