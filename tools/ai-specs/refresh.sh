#!/bin/sh
# Regenerate the specs for the AI command-line tools in specs/overrides/ from
# each tool's own latest --help output, then repack the archive.
#
#   RUN THIS BEFORE EACH RELEASE:  tools/ai-specs/refresh.sh [tool ...]
#
# With no arguments all ten tools are refreshed: aider amp claude codex
# cursor-agent gemini goose kiro-cli llm opencode. Name some to refresh only
# those.
#
# Each tool's latest version is fetched into a fresh temporary folder through
# its official channel (npm @latest, uvx, the vendor's release archive or
# install script's download URL). Nothing is installed system-wide, the copies
# on this Mac are never used, and the real HOME, ~/.npm and ~/.cache are not
# touched. Only `--version`, `--help` and `<sub> --help` are run, under
# `env -i` with a scratch HOME. The temporary folder is removed at the end,
# also on failure or interrupt.
#
# A spec whose header already names the fetched version is left alone. A tool
# that cannot be fetched is reported and its spec stays as it is; generate.py
# writes each spec to a temporary file and moves it into place only when it is
# complete. Hand patches live in tools/ai-specs/patches.json (applied by
# generate.py after parsing), so they survive a refresh.
#
# FORCE=1 regenerates even when the version is unchanged (after a change to
# generate.py).
#
# Ends with tools/fig-specs/update.sh. Exits 1 if any tool failed.
set -eu

repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
here=$repo/tools/ai-specs
out=$repo/specs/overrides
date=$(date +%Y-%m-%d)

all="aider amp claude codex cursor-agent gemini goose kiro-cli llm opencode"
want=${*:-$all}
for t in $want; do
    case " $all " in
    *" $t "*) ;;
    *) echo "unknown tool: $t (known: $all)" >&2; exit 2 ;;
    esac
done

tmp=$(mktemp -d "${TMPDIR:-/tmp}/ai-specs.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' INT TERM
mkdir -p "$tmp/home" "$tmp/npm-cache" "$tmp/uv-cache" "$tmp/uv-tools" "$tmp/uv-python" "$tmp/dl"

node_dir=$(dirname "$(command -v node)")
uv_dir=$(dirname "$(command -v uvx)")
base_path="$node_dir:$uv_dir:/usr/bin:/bin"

# Run the generator for one tool: gen <tool> <label> <format> <command...>
gen() {
    tool=$1 label=$2 format=$3
    shift 3
    python3 -I "$here/generate.py" --tool "$tool" --label "$label" --format "$format" \
        --out "$out/$tool.js" --date "$date" --patches "$here/patches.json" ${FORCE:+--force} \
        --env "HOME=$tmp/home" --env "PATH=$base_path:$tmp/npm/node_modules/.bin" \
        --env "UV_CACHE_DIR=$tmp/uv-cache" --env "UV_TOOL_DIR=$tmp/uv-tools" \
        --env "UV_PYTHON_INSTALL_DIR=$tmp/uv-python" \
        -- "$@"
}

# npm <package> installs it into its own prefix under $tmp/npm-<tool>.
npm_fetch() {
    tool=$1 pkg=$2
    HOME=$tmp/home npm_config_cache=$tmp/npm-cache npm install --prefix "$tmp/npm-$tool" \
        --no-audit --no-fund --loglevel=error "$pkg@latest" >/dev/null
}

refresh() {
    case $1 in
    claude)
        npm_fetch claude @anthropic-ai/claude-code
        gen claude "Claude Code" commander "$tmp/npm-claude/node_modules/.bin/claude"
        ;;
    codex)
        npm_fetch codex @openai/codex
        gen codex "Codex CLI" clap "$tmp/npm-codex/node_modules/.bin/codex"
        ;;
    gemini)
        npm_fetch gemini @google/gemini-cli
        gen gemini "Gemini CLI" yargs "$tmp/npm-gemini/node_modules/.bin/gemini"
        ;;
    opencode)
        npm_fetch opencode opencode-ai
        gen opencode "opencode" yargs "$tmp/npm-opencode/node_modules/.bin/opencode"
        ;;
    amp)
        npm_fetch amp @sourcegraph/amp
        gen amp "Amp CLI" commander "$tmp/npm-amp/node_modules/.bin/amp"
        ;;
    cursor-agent)
        # The install script names the current package URL.
        url=$(curl -fsSL https://cursor.com/install |
            sed -n 's|^DOWNLOAD_URL="\(https://downloads.cursor.com/[^"]*\)".*|\1|p' | head -n 1)
        [ -n "$url" ] || { echo "cursor-agent: no download URL in the install script" >&2; return 1; }
        case $(uname -m) in
        arm64 | aarch64) arch=arm64 ;;
        *) arch=x64 ;;
        esac
        url=$(printf '%s' "$url" | sed "s|\${OS}|darwin|; s|\${ARCH}|$arch|")
        mkdir -p "$tmp/cursor"
        curl -fsSL "$url" | tar --strip-components=1 -xzf - -C "$tmp/cursor"
        gen cursor-agent "Cursor Agent" commander "$tmp/cursor/cursor-agent"
        ;;
    goose)
        case $(uname -m) in
        arm64 | aarch64) arch=aarch64 ;;
        *) arch=x86_64 ;;
        esac
        mkdir -p "$tmp/goose"
        curl -fsSL "https://github.com/aaif-goose/goose/releases/latest/download/goose-$arch-apple-darwin.tar.gz" |
            tar -xzf - -C "$tmp/goose"
        gen goose "goose" clap "$tmp/goose/goose"
        ;;
    kiro-cli)
        base=https://prod.download.cli.kiro.dev/stable/latest
        curl -fsSL -o "$tmp/dl/kiro.zip" "$base/kirocli-universal-darwin.zip"
        mkdir -p "$tmp/kiro"
        unzip -q "$tmp/dl/kiro.zip" -d "$tmp/kiro"
        gen kiro-cli "kiro-cli" clap "$tmp/kiro/kirocli/bin/kiro-cli"
        ;;
    aider)
        gen aider "aider" argparse uvx --python 3.12 --from aider-chat aider
        ;;
    llm)
        gen llm "llm" click uvx --python 3.12 --from llm llm
        ;;
    esac
}

failed=""
for t in $want; do
    # A plain subshell, not `if ! (...)`: errexit must stay on inside it.
    set +e
    (
        set -e
        refresh "$t"
    )
    rc=$?
    set -e
    if [ "$rc" -ne 0 ]; then
        echo "$t: could not be refreshed; its spec is unchanged" >&2
        failed="$failed $t"
    fi
done

"$here/../fig-specs/update.sh"

if [ -n "$failed" ]; then
    echo "failed:$failed" >&2
    exit 1
fi
