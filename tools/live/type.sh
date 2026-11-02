#!/bin/sh
# type.sh <live-dir> <session> <string> [delay-ms]
#
# Types <string> into the session through its injector (inject.pl reads
# <live-dir>/<session>/in.fifo). Escapes: \e \t \r \n \\ \xHH. With a delay,
# keys (an escape sequence, a UTF-8 character or a byte) go one at a time
# with that many milliseconds between them. Fails at once if no injector is
# reading.
set -eu

live=${1:?usage: type.sh <live-dir> <session> <string> [delay-ms]}
name=${2:?usage: type.sh <live-dir> <session> <string> [delay-ms]}
text=${3?usage: type.sh <live-dir> <session> <string> [delay-ms]}
here=$(cd "$(dirname "$0")" && pwd)

exec perl "$here/keys.pl" fifo "$live/$name/in.fifo" "${4:-0}" "$text"
