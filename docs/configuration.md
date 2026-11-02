# Configuration

fac reads `~/.config/fac/config.toml` once, when it starts. `FAC_CONFIG=<path>` names another file. A missing file gives the defaults; a bad file or a bad key gives the defaults with a warning in the log. fac never refuses to start over its config.

| Key | Default | Meaning |
|---|---|---|
| `theme` | `"default"` | Colour theme, see [themes.md](themes.md) |
| `icons` | `"plain"` | `plain` (coloured characters), `nerd` (Nerd Font glyphs) or `none` |
| `fuzzy` | `true` | Fuzzy matches below the starts-with matches |
| `sort` | `"recent"` | Order of equally good suggestions: `recent` or `alphabetical` |
| `history` | `true` | Suggest lines from your history |
| `description_popout_key` | `""` (off) | Key that opens the full description beside the popup, for example `"ctrl+k"` |
| `generator_timeout_ms` | `5000` | How long a spec generator may run, 100 to 60000 |
| `disable_for_commands` | `[]` | Commands for which fac stays out of the way |

## Files and environment

| Variable | Meaning |
|---|---|
| `FAC_CONFIG` | Config file path (default `~/.config/fac/config.toml`) |
| `FAC_DATA_DIR` | Folder for fac's data (default `~/Library/Application Support/fac`). It holds `usage.json`, which records what you picked, so recent choices rank first |
| `FAC_DISABLE` | `1` starts your shell plain, without fac |
| `FAC_LOG` | Log filter, for example `debug` (default `info`) |
| `FAC_LOG_FILE` | Log file (default `~/Library/Logs/fac/fac.log`) |
| `FAC_STATS` | `1` logs the popup's latency every 100 painted keystrokes |

`kill -USR1 <fac pid>` logs fac's model of the screen, for bug reports.
