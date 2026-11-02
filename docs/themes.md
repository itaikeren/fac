# Themes

fac ships one theme, `default`. It derives its colours from the terminal's own background and blue (read with OSC 11 and OSC 4), so it is light or dark on its own.

## Your own theme

1. Write a starting file: `fac theme new mine` (or `fac theme new mine --light` for light colours). It creates `~/.config/fac/themes/mine.json` and never overwrites an existing file. `--stdout` prints the file instead.
2. Edit the colours.
3. Set `theme = "mine"` in `~/.config/fac/config.toml`.

Theme files use Fig's v1 format. Colours are `#rrggbb`, `#rrggbbaa` or `rgb(r,g,b)` (no spaces). An unknown or broken theme gives the default plus a warning in the log.

| Key | What it colours |
|---|---|
| `backgroundColor` | The list rows |
| `textColor` | The names in the list |
| `matchBackgroundColor` | The letters that match what you typed |
| `selection.backgroundColor` | The selected row |
| `selection.textColor` | The selected row's text |
| `selection.matchBackgroundColor` | Matched letters on the selected row (optional) |
| `description.textColor` | The description line at the bottom |
| `description.backgroundColor` | The description line's background (optional; else the card with a little of `description.borderColor`, or of the text) |
| `description.borderColor` | Only tints the description line's background when `description.backgroundColor` is absent (optional) |

Argument hints, the selected row's hints and the icons are derived from these colours and the terminal's own.

A theme others could use is welcome as a pull request; the built-in theme lives in `src/popup/palette.rs`.
