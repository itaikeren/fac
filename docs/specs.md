# Specs

A spec describes a command: its subcommands, options and arguments, and where live suggestions come from. fac's specs are Fig's open-source completion specs (`withfig/autocomplete`, MIT), plus fac's own for newer tools (Claude Code, Codex, Gemini CLI and others).

## Your own specs

`fac spec new mytool` writes a starter spec, `~/.config/fac/specs/mytool.ts`, in Fig's format: a subcommand, two options and an argument whose suggestions come from a command, each with a comment.

- `fig.d.ts` and `tsconfig.json`, written next to it, give your editor (VS Code, Zed and others) completion and type errors on `Fig.Spec`, with no npm install.
- Fig's generator helpers are built in: `import { filepaths, keyValue } from "@fig/autocomplete-generators"`. Nothing else can be imported.
- It is live at your next prompt. Every save is picked up at the next prompt, without restarting fac.
- `fac spec check mytool` loads the spec as fac would and prints what is in it, or exactly what is wrong (the file, line and column of a syntax error, or the field). `fac spec check` alone checks every spec, and `fac spec list` lists them, each with the built-in spec it replaces.

A spec for a command fac already has a spec for replaces fac's. A broken one falls back to fac's own spec (or to the shell's own completions), with a warning in the log that names the file and what is wrong.

**JSON specs** (`fac spec new mytool --json`) are for specs that need no code: the same fields in plain JSON, with a generator as a `script` command plus `splitOn`, or a `template` such as `"filepaths"`. A key fac does not know is an error, so a typo does not go unnoticed.

**JavaScript specs** (`fac spec new mytool --js`) are ES modules whose default export is the spec, as Fig's compiled specs are, with the same built-in import.

When a command has more than one file, `.ts` comes first, then `.js`, then `.json`; `fac spec check` warns about the ones not used.

## fac's built-in specs

Fig's specs have been frozen upstream since 2025-05-05, so fac vendors a pinned copy: `@withfig/autocomplete` 2.692.3 (`vendor/fig-autocomplete/VERSION`), packed into `specs/fig.zst`. fac's fixes and new specs live in `specs/overrides/<name>.js`; a file there replaces the vendored spec of the same name or adds a new one (see [specs/overrides/README.md](../specs/overrides/README.md)). `tools/fig-specs/update.sh` downloads the pinned package, checks its integrity and repacks the archive, the index and the pack report.

## Contributing a spec

A spec others could use belongs in fac: put it in `specs/overrides/<name>.js` (a new command, or a fix to one of Fig's specs), run `./tools/fig-specs/update.sh`, and open a pull request with the override and the regenerated archive. Overrides are JavaScript today and cannot import anything, so a TypeScript spec goes in as its JavaScript, with any helper it uses copied in.
