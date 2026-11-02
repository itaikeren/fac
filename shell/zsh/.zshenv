# fac's ZDOTDIR shim. fac starts zsh with ZDOTDIR pointing here, so zsh reads
# this file first. It puts the user's ZDOTDIR back, so zsh then reads the
# user's own .zprofile, .zshrc and .zlogin, sources the user's .zshenv, and
# loads fac.zsh in interactive shells. Same pattern as Ghostty's integration.

# The session token, which only fac's hook may send (src/shell/protocol.rs,
# "The session token"): out of the environment before anything else runs,
# so no program this shell starts inherits it, kept unexported and hidden
# from `set` and `typeset` listings. In a function with xtrace off, so a
# trace never prints it.
() {
  'builtin' 'setopt' 'localoptions' 'noxtrace'
  'builtin' 'typeset' -gH _fac_token="${FAC_TOKEN-}"
  'builtin' 'typeset' -g +x _fac_token
  'builtin' 'unset' 'FAC_TOKEN'
}

if [[ -n ${FAC_USER_ZDOTDIR-} ]]; then
  'builtin' 'export' ZDOTDIR="$FAC_USER_ZDOTDIR"
else
  'builtin' 'unset' 'ZDOTDIR'
fi
'builtin' 'unset' 'FAC_USER_ZDOTDIR'

{
  'builtin' 'typeset' _fac_file="${ZDOTDIR:-$HOME}/.zshenv"
  [[ ! -r $_fac_file ]] || 'builtin' 'source' -- "$_fac_file"
} always {
  if [[ -o interactive ]]; then
    # %x is this file; :A resolves symlinks, :h takes the directory.
    _fac_file="${${(%):-%x}:A:h}/fac.zsh"
    [[ ! -r $_fac_file ]] || 'builtin' 'source' -- "$_fac_file"
  fi
  'builtin' 'unset' '_fac_file'
}
