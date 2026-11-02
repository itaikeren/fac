#!/bin/sh
# Runs target/release/fac with bash or fish in this terminal, under a
# scratch HOME (with its XDG dirs) in target/try-shell/, so your own config
# and history are never read or written. The scratch HOME has a small git
# repo with three branches and a package.json with scripts, and the shell
# starts in it. It is removed when fac exits.
#   tools/try-shell.sh bash   Homebrew's bash 5.3+ (/opt/homebrew/bin/bash),
#                             with bash-completion from target/test-shells/
#   tools/try-shell.sh fish   fish from target/test-shells/
# Build fac first (`cargo build --release`) and, for fish and
# bash-completion, run tools/test-shells.sh.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
fac="$root/target/release/fac"
shells="$root/target/test-shells"

case ${1-} in
  bash)
    shell=${FAC_TRY_BASH:-/opt/homebrew/bin/bash}
    ;;
  fish)
    shell="$shells/fish/base/usr/local/bin/fish"
    ;;
  *)
    echo "usage: $0 <bash|fish>" >&2
    exit 2
    ;;
esac
if [ ! -x "$fac" ]; then
  echo "no $fac: run cargo build --release" >&2
  exit 1
fi
if [ ! -x "$shell" ]; then
  echo "no $shell: run tools/test-shells.sh (fish) or install bash 5.3+" >&2
  exit 1
fi

mkdir -p "$root/target/try-shell"
home=$(mktemp -d "$root/target/try-shell/$1.XXXXXX")
trap 'rm -rf "$home"' EXIT
trap 'exit 129' HUP
trap 'exit 143' TERM

mkdir -p "$home/.config" "$home/.local/share" "$home/.cache" \
  "$home/.local/share/fish/generated_completions" \
  "$home/.cache/fish/generated_completions"
repo="$home/repo"
mkdir -p "$repo/docs" "$repo/src"
(
  # git as the scratch HOME's: none of your git config or templates.
  export HOME="$home" XDG_CONFIG_HOME="$home/.config" GIT_CONFIG_NOSYSTEM=1
  unset GIT_CONFIG_GLOBAL GIT_TEMPLATE_DIR
  cd "$repo"
  git init -q -b main
  git -c user.name=fac -c user.email=fac@example.com commit -q --allow-empty -m init
  git branch feature-one
  git branch fix/two
  printf '{"name":"try","scripts":{"build":"echo build","test":"echo test","lint":"echo lint"}}\n' >package.json
)
case $1 in
  bash)
    {
      if [ -r "$shells/bash-completion/bash_completion" ]; then
        printf "source '%s'\n" "$shells/bash-completion/bash_completion"
      fi
      printf '%s\n' "PS1='\\w\\n\\\$ '" 'HISTFILE=$HOME/.bash_history' "cd ~/repo"
    } >"$home/.bash_profile"
    ;;
  fish)
    mkdir -p "$home/.config/fish"
    printf '%s\n' 'set -g fish_greeting' 'cd ~/repo' >"$home/.config/fish/config.fish"
    ;;
esac

echo "fac with $1; scratch HOME: $home (removed when you exit)"
# The terminal's own variables, as it set them (fac tells terminals apart
# by them).
set -- HOME="$home" SHELL="$shell" \
  PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin \
  TERM="${TERM:-xterm-256color}" LANG="${LANG:-en_US.UTF-8}" \
  XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
  XDG_CACHE_HOME="$home/.cache" FAC_LOG_FILE="$home/fac.log"
for name in TERM_PROGRAM TERM_PROGRAM_VERSION COLORTERM; do
  eval "value=\${$name-}"
  [ -n "$value" ] && set -- "$@" "$name=$value"
done
status=0
/usr/bin/env -i "$@" "$fac" || status=$?
exit "$status"
