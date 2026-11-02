# Spec overrides

Fig's specs (`withfig/autocomplete`) have been frozen since 2025-05-05. A file
here replaces the vendored spec of the same name when the archive is packed,
so fixes live in this repo instead of upstream.

## Adding a fix

1. Find the spec's name: its path in the npm package's `build/` folder without
   `.js` (`git`, `aws/ec2`, `az/2.53.0`). `specs/index.txt` lists every name.
2. Copy the compiled module from the pinned package (the tarball named in
   `vendor/fig-autocomplete/VERSION`, file `package/build/<name>.js`) to
   `specs/overrides/<name>.js`, keeping the folders
   (`specs/overrides/aws/ec2.js`).
3. Edit it. It must stay an ES module whose default export is the spec. It
   may not import anything.
4. Run `./tools/fig-specs/update.sh`. The report (`specs/pack-report.txt`)
   lists the override under "Overrides".
5. Commit the override together with the regenerated `specs/fig.zst`,
   `specs/index.txt` and `specs/pack-report.txt`.

A file with a name that has no vendored spec adds a new spec.

The AI tool specs (`aider amp claude codex cursor-agent gemini goose kiro-cli llm opencode`) are generated from each tool's own `--help`: run `tools/ai-specs/refresh.sh` before each release (it ends with the pack step above). Hand patches for them live in `tools/ai-specs/patches.json`.
