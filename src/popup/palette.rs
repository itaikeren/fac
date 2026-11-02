//! The popup's colour roles, built from the terminal's background (OSC 11)
//! and ANSI colours (OSC 4).
//!
//! The one built-in theme, `default`, follows the formulas of the
//! mockup the user chose (`docs/plans/phase-2-prompts/popup-mockup.html`:
//! `graphite` and `graphiteAccent(bg, 'tint')`), with the colour maths of
//! [`crate::popup::color`]. `tests/fixtures/palettes/` holds the values those
//! functions print under node, and the tests compare.
//!
//! A theme name that is not built in names a Fig theme file,
//! `~/.config/fac/themes/<name>.json` (Fig's v1 format). [`create_theme_file`]
//! writes the built-in theme in that format, to start a theme from.
//!
//! `theme.rs` writes these roles as the popup's SGR styles.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use serde_json::{Value, json};

use crate::popup::color::{contrast, hex, mix, oklch, to_oklch};
use crate::popup::theme::{Rgb, nearest_256};
use crate::term::caps::Ansi;

/// One colour per kind of row icon.
///
/// The mockup defines the subcommand, option, folder and history colours
/// (`sub`, `opt`, `dir`, `hist`). The other three:
/// - `arg`: the terminal's blue, or the derived blue (`oklch(0.6, 0.13, 262)`
///   on dark, `oklch(0.55, 0.15, 262)` on light) when it did not report one,
///   made readable on the card by [`readable_blue`].
/// - `file`: the theme's `dim`.
/// - `special`: the folder colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconColors {
    pub subcommand: Rgb,
    pub option: Rgb,
    pub arg: Rgb,
    pub folder: Rgb,
    pub file: Rgb,
    pub history: Rgb,
    pub special: Rgb,
}

/// Every colour the popup paints with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Roles {
    /// The list rows' background.
    pub card: Rgb,
    /// The description line's background.
    pub band: Rgb,
    pub text: Rgb,
    /// Argument hints.
    pub dim: Rgb,
    /// The description line's text.
    pub band_text: Rgb,
    /// The selected row's background, text and argument hints.
    pub selected: Rgb,
    pub selected_text: Rgb,
    pub selected_dim: Rgb,
    /// The background of matched letters, on a plain and on the selected row.
    pub matched: Rgb,
    pub selected_matched: Rgb,
    pub icons: IconColors,
    /// Whether the theme is a dark one (OKLCH lightness of the background,
    /// or of the card for a Fig theme file, below 0.6).
    pub dark: bool,
}

/// The built-in theme names.
pub const NAMES: &[&str] = &["default"];

/// The background the default theme is built on when the terminal did not
/// report one: the mockup's default dark background. Phase 1 assumes dark
/// then too.
pub const DEFAULT_BACKGROUND: Rgb = (0x1e, 0x1f, 0x23);

/// The background `print_theme` uses for a light theme.
const LIGHT_BACKGROUND: Rgb = (0xfb, 0xfb, 0xf9);

/// A theme name, resolved: the built-in theme or a Fig theme file, read.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Theme {
    /// `default`, derived from the terminal's own colours.
    #[default]
    Default,
    Fig(FigTheme),
}

/// Resolve a theme name: the built-in name, or a Fig theme file in
/// `~/.config/fac/themes/`. Anything else gives the default plus a warning.
/// Warnings name the theme and the problem, never what the file holds.
pub fn resolve(name: &str) -> (Theme, Option<String>) {
    resolve_in(name, THEMES_DIR.as_deref())
}

/// `~/.config/fac/themes`, from `HOME` read once.
static THEMES_DIR: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".config/fac/themes"))
});

fn resolve_in(name: &str, dir: Option<&Path>) -> (Theme, Option<String>) {
    if name == NAMES[0] {
        return (Theme::Default, None);
    }
    let fallback = |problem: String| {
        let warning = format!("theme {name:?}: {problem}; using {}", NAMES[0]);
        (Theme::Default, Some(warning))
    };
    if !valid_name(name) {
        return fallback(
            "not a valid name (only letters, digits, '.', '_' and '-' are allowed)".to_owned(),
        );
    }
    let Some(dir) = dir else {
        return fallback("not built in, and HOME is not set to find theme files".to_owned());
    };
    let path = dir.join(format!("{name}.json"));
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return fallback(format!(
                "not built in, and {} does not exist",
                path.display()
            ));
        }
        Err(err) => return fallback(format!("cannot read {}: {}", path.display(), err.kind())),
    };
    match FigTheme::parse(&bytes) {
        Ok(theme) => (Theme::Fig(theme), None),
        Err(problem) => fallback(format!("{}: {problem}", path.display())),
    }
}

/// `[A-Za-z0-9._-]+`, so a name can never leave the themes directory.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

impl Theme {
    /// The roles on this background (`None`: [`DEFAULT_BACKGROUND`]) with
    /// these terminal colours.
    pub fn roles(&self, background: Option<Rgb>, ansi: &Ansi) -> Roles {
        let bg = background.unwrap_or(DEFAULT_BACKGROUND);
        match self {
            Self::Default => graphite_tint(bg, ansi.blue).roles(ansi.blue),
            Self::Fig(theme) => theme.roles(bg, ansi),
        }
    }
}

/// A Fig v1 theme file that holds the built-in theme's colours on a dark
/// background (`light`: on a light one), for a user to start their own theme
/// from. Every key fac reads is in it but `description.borderColor`, which
/// fac reads only to derive the description band when a file has no
/// `description.backgroundColor` (this one has).
///
/// `dim` and the selected row's argument hints are not keys: fac derives
/// them from the others when it reads a theme file.
pub fn print_theme(light: bool) -> String {
    let background = if light {
        LIGHT_BACKGROUND
    } else {
        DEFAULT_BACKGROUND
    };
    let roles = Theme::Default.roles(Some(background), &Ansi::default());
    let theme = json!({
        "author": { "name": "fac" },
        "version": "1.0",
        "theme": {
            "backgroundColor": hex(roles.card),
            "textColor": hex(roles.text),
            "matchBackgroundColor": hex(roles.matched),
            "selection": {
                "textColor": hex(roles.selected_text),
                "backgroundColor": hex(roles.selected),
                "matchBackgroundColor": hex(roles.selected_matched),
            },
            "description": {
                "textColor": hex(roles.band_text),
                "backgroundColor": hex(roles.band),
            },
        },
    });
    // Serialising a `Value` of strings cannot fail.
    serde_json::to_string_pretty(&theme).unwrap_or_default()
}

/// Write [`print_theme`] to `~/.config/fac/themes/<name>.json`, creating the
/// folders, and return the path. An existing file is never overwritten.
pub fn create_theme_file(name: &str, light: bool) -> Result<PathBuf, String> {
    let dir = THEMES_DIR
        .as_deref()
        .ok_or_else(|| "HOME is not set, so there is nowhere to write the theme".to_owned())?;
    create_theme_file_in(dir, name, light)
}

fn create_theme_file_in(dir: &Path, name: &str, light: bool) -> Result<PathBuf, String> {
    if !valid_name(name) {
        return Err(format!(
            "{name:?} is not a valid theme name (only letters, digits, '.', '_' and '-' are allowed)"
        ));
    }
    if name == NAMES[0] {
        return Err(format!("{name:?} is the built-in theme; pick another name"));
    }
    let path = dir.join(format!("{name}.json"));
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("cannot create {}: {}", dir.display(), err.kind()))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::AlreadyExists => {
                format!("{} already exists; not overwriting it", path.display())
            }
            kind => format!("cannot write {}: {kind}", path.display()),
        })?;
    std::io::Write::write_all(&mut file, format!("{}\n", print_theme(light)).as_bytes())
        .map_err(|err| format!("cannot write {}: {}", path.display(), err.kind()))?;
    Ok(path)
}

/// The derived blue, for when the terminal did not report its own.
fn derived_blue(dark: bool) -> Rgb {
    if dark {
        oklch(0.6, 0.13, 262.0)
    } else {
        oklch(0.55, 0.15, 262.0)
    }
}

/// The contrast the arg icon keeps against the card: WCAG's 4.5:1 for text.
pub const READABLE_CONTRAST: f64 = 4.5;

/// The terminal's blue (or the derived one) as the built-in theme paints it
/// on `card`: unchanged, bit for bit, when it reaches [`READABLE_CONTRAST`]
/// against the card. Otherwise it keeps its OKLCH hue and takes the nearest
/// lightness that does: the floor on a dark card (raised), the ceiling on a
/// light one (lowered). [`oklch`] caps the chroma so the colour stays in
/// sRGB. Terminal.app's navy `#0000b3` reaches 4.5:1 on the default dark
/// card (`#282a2e`) from L 0.658 (it starts at 0.347). On a card where no
/// lightness reaches the bar (a mid-grey one), the end of the range: white
/// or black.
pub fn readable_blue(blue: Rgb, card: Rgb, dark: bool) -> Rgb {
    if contrast(blue, card) >= READABLE_CONTRAST {
        return blue;
    }
    let o = to_oklch(blue);
    let reaches = |l: f64| contrast(oklch(l, o.c, o.h), card) >= READABLE_CONTRAST;
    // `short` never reaches the bar; `enough` does, or is the range's end.
    let (mut short, mut enough) = (o.l, if dark { 1.0 } else { 0.0 });
    for _ in 0..40 {
        let mid = (short + enough) / 2.0;
        if reaches(mid) {
            enough = mid;
        } else {
            short = mid;
        }
    }
    oklch(enough, o.c, o.h)
}

/// A palette as the mockup's functions return it.
struct Mock {
    card: Rgb,
    band: Rgb,
    text: Rgb,
    dim: Rgb,
    sel: Rgb,
    sel_text: Rgb,
    sel_dim: Rgb,
    matched: Rgb,
    sel_match: Rgb,
    ico: MockIcons,
    dark: bool,
}

#[derive(Clone, Copy)]
struct MockIcons {
    sub: Rgb,
    opt: Rgb,
    dir: Rgb,
    hist: Rgb,
}

impl Mock {
    /// `blue`: the terminal's blue, for the arg icon ([`readable_blue`]).
    fn roles(&self, blue: Option<Rgb>) -> Roles {
        Roles {
            card: self.card,
            band: self.band,
            text: self.text,
            dim: self.dim,
            // The mockup's `bandText`: `dim`.
            band_text: self.dim,
            selected: self.sel,
            selected_text: self.sel_text,
            selected_dim: self.sel_dim,
            matched: self.matched,
            selected_matched: self.sel_match,
            icons: icons(
                self.ico,
                readable_blue(
                    blue.unwrap_or_else(|| derived_blue(self.dark)),
                    self.card,
                    self.dark,
                ),
                self.dim,
            ),
            dark: self.dark,
        }
    }
}

/// The icon rule of [`IconColors`].
fn icons(ico: MockIcons, arg: Rgb, dim: Rgb) -> IconColors {
    IconColors {
        subcommand: ico.sub,
        option: ico.opt,
        arg,
        folder: ico.dir,
        file: dim,
        history: ico.hist,
        special: ico.dir,
    }
}

/// The mockup's `base`: lightness, hue (260 for a grey), the tint's chroma
/// (at most 0.03) and darkness.
struct Base {
    l: f64,
    h: f64,
    t: f64,
    dark: bool,
}

fn base(bg: Rgb) -> Base {
    let o = to_oklch(bg);
    Base {
        l: o.l,
        h: if o.c < 0.015 { 260.0 } else { o.h },
        t: o.c.min(0.03),
        dark: o.l < 0.6,
    }
}

/// The mockup's `graphite`, the neutral base that [`graphite_tint`] tints.
fn graphite(bg: Rgb) -> Mock {
    let Base { l, h, t, dark } = base(bg);
    if dark {
        Mock {
            card: oklch(l + 0.045, t, h),
            band: oklch(l + 0.022, t, h),
            text: oklch(0.86, 0.008, h),
            dim: oklch(0.6, 0.01, h),
            sel: oklch(l + 0.17, t, h),
            sel_text: oklch(0.98, 0.004, h),
            sel_dim: oklch(0.78, 0.01, h),
            matched: oklch((l + 0.13).min(0.42), 0.07, 70.0),
            sel_match: oklch((l + 0.3).min(0.6), 0.09, 70.0),
            ico: MockIcons {
                sub: oklch(0.7, 0.06, 300.0),
                opt: oklch(0.72, 0.06, 160.0),
                dir: oklch(0.76, 0.07, 80.0),
                hist: oklch(0.68, 0.04, 220.0),
            },
            dark,
        }
    } else {
        Mock {
            card: oklch(l - 0.035, t, h),
            band: oklch(l - 0.06, t, h),
            text: oklch(0.28, 0.008, h),
            dim: oklch(0.54, 0.01, h),
            sel: oklch(l - 0.15, t, h),
            sel_text: oklch(0.18, 0.006, h),
            sel_dim: oklch(0.38, 0.01, h),
            matched: oklch(0.9, 0.08, 85.0),
            sel_match: oklch(0.8, 0.1, 80.0),
            ico: MockIcons {
                sub: oklch(0.5, 0.08, 300.0),
                opt: oklch(0.52, 0.07, 160.0),
                dir: oklch(0.56, 0.08, 70.0),
                hist: oklch(0.5, 0.05, 220.0),
            },
            dark,
        }
    }
}

/// The mockup's `graphiteAccent(bg, 'tint')`, the built-in theme: graphite with the
/// selected row and its matches tinted with the terminal's blue, or the
/// derived blue when it did not report one, made readable on the card by
/// [`readable_blue`].
fn graphite_tint(bg: Rgb, blue: Option<Rgb>) -> Mock {
    let mut p = graphite(bg);
    let dark = p.dark;
    let accent = readable_blue(blue.unwrap_or_else(|| derived_blue(dark)), p.card, dark);
    p.sel = mix(accent, p.card, if dark { 0.68 } else { 0.78 });
    p.sel_text = if dark {
        oklch(0.97, 0.005, 260.0)
    } else {
        oklch(0.2, 0.01, 260.0)
    };
    p.sel_dim = mix(p.sel_text, p.sel, 0.35);
    p.sel_match = mix(accent, p.card, if dark { 0.45 } else { 0.55 });
    p
}

/// A colour from a Fig theme file, with its alpha (0–1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FigColor {
    rgb: Rgb,
    alpha: f64,
}

impl FigColor {
    /// Premultiplied over `under`.
    fn over(self, under: Rgb) -> Rgb {
        if self.alpha >= 1.0 {
            self.rgb
        } else {
            mix(under, self.rgb, self.alpha)
        }
    }

    /// As Fig's `mapColor` reads them: `#rrggbb`, `#rrggbbaa` and
    /// `rgb(r,g,b)` without spaces.
    fn parse(text: &str) -> Option<Self> {
        if let Some(digits) = text.strip_prefix('#') {
            if !matches!(digits.len(), 6 | 8) || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let byte = |at: usize| u8::from_str_radix(digits.get(at..at + 2)?, 16).ok();
            let alpha = match digits.len() {
                8 => f64::from(byte(6)?) / 255.0,
                _ => 1.0,
            };
            return Some(Self {
                rgb: (byte(0)?, byte(2)?, byte(4)?),
                alpha,
            });
        }
        let inner = text.strip_prefix("rgb(")?.strip_suffix(')')?;
        let mut parts = inner.split(',').map(|part| {
            if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse::<u8>().ok()
        });
        let rgb = (parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(Self { rgb, alpha: 1.0 })
    }
}

/// A Fig v1 theme file (`"version": "1.0"`, colours under `theme`).
#[derive(Debug, Clone, PartialEq)]
pub struct FigTheme {
    background: FigColor,
    text: FigColor,
    matched: FigColor,
    selected: FigColor,
    selected_text: FigColor,
    selected_matched: FigColor,
    description_text: FigColor,
    description_background: Option<FigColor>,
    description_border: Option<FigColor>,
}

/// Fig's default `selection.matchBackgroundColor`.
const FIG_SELECTED_MATCH: FigColor = FigColor {
    rgb: (106, 142, 218),
    alpha: 1.0,
};

impl FigTheme {
    /// The problem, when the file is not a usable v1 theme, names a key at
    /// most, never a value.
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let json: Value = serde_json::from_slice(bytes).map_err(|_| "not valid JSON".to_owned())?;
        if json.get("version").and_then(Value::as_str) != Some("1.0") {
            return Err(
                "not a Fig v1 theme (no \"version\": \"1.0\"); v2 themes are not supported"
                    .to_owned(),
            );
        }
        let theme = json
            .get("theme")
            .filter(|theme| theme.is_object())
            .ok_or_else(|| "`theme` is missing".to_owned())?;
        let optional = |path: &str| -> Result<Option<FigColor>, String> {
            let Some(value) = path.split('.').try_fold(theme, |value, key| value.get(key)) else {
                return Ok(None);
            };
            value
                .as_str()
                .and_then(FigColor::parse)
                .map(Some)
                .ok_or_else(|| format!("`{path}` is not a colour"))
        };
        let required = |path: &str| optional(path)?.ok_or_else(|| format!("`{path}` is missing"));
        Ok(Self {
            background: required("backgroundColor")?,
            text: required("textColor")?,
            matched: required("matchBackgroundColor")?,
            selected: required("selection.backgroundColor")?,
            selected_text: required("selection.textColor")?,
            selected_matched: optional("selection.matchBackgroundColor")?
                .unwrap_or(FIG_SELECTED_MATCH),
            description_text: required("description.textColor")?,
            description_background: optional("description.backgroundColor")?,
            description_border: optional("description.borderColor")?,
        })
    }

    /// Fig's keys mapped to roles; the rest derived:
    /// - `dim` is text mixed halfway into the card (Fig drew args at 50%
    ///   opacity);
    /// - `band` is `description.backgroundColor` when the file has it, else
    ///   the card with 8% of the description border colour (or of the text);
    /// - `selected_dim` is the selected text with 30% of the selected row;
    /// - the icons are the built-in theme's, light or dark by the card, the
    ///   arg icon made readable on this card ([`readable_blue`]).
    ///
    /// Colours with alpha are premultiplied over the card, and the card over
    /// the terminal's background.
    fn roles(&self, bg: Rgb, ansi: &Ansi) -> Roles {
        let card = self.background.over(bg);
        let text = self.text.over(card);
        let selected = self.selected.over(card);
        let selected_text = self.selected_text.over(card);
        let dim = mix(text, card, 0.5);
        let band = self.description_background.map_or_else(
            || {
                let border = self
                    .description_border
                    .map_or(text, |color| color.over(card));
                mix(card, border, 0.08)
            },
            |color| color.over(card),
        );
        let dark = to_oklch(card).l < 0.6;
        Roles {
            card,
            band,
            text,
            dim,
            band_text: self.description_text.over(card),
            selected,
            selected_text,
            selected_dim: mix(selected_text, selected, 0.3),
            matched: self.matched.over(card),
            selected_matched: self.selected_matched.over(card),
            // The icon colours of `graphite_tint` on the card: a theme file
            // has no icon keys.
            icons: icons(
                graphite_tint(card, ansi.blue).ico,
                readable_blue(ansi.blue.unwrap_or_else(|| derived_blue(dark)), card, dark),
                dim,
            ),
            dark,
        }
    }
}

/// The card's 256-colour index. When it is the background's own index, a
/// borderless card would vanish, so it steps one index lighter on a dark
/// palette (darker on a light one), staying within the grey ramp or the
/// colour cube; at the ramp's or the cube's end it steps the other way.
pub fn card_index_256(roles: &Roles, background: Option<Rgb>) -> u8 {
    let card = nearest_256(roles.card);
    if card != nearest_256(background.unwrap_or(DEFAULT_BACKGROUND)) {
        return card;
    }
    step_256(card, roles.dark)
        .or_else(|| step_256(card, !roles.dark))
        .unwrap_or(card)
}

/// The band's (the bottom line's) 256-colour index. When it is the card's
/// own index ([`card_index_256`]), the bottom line would not stand out from
/// the list, so it steps one index away from the card the same way.
pub fn band_index_256(roles: &Roles, background: Option<Rgb>) -> u8 {
    let card = card_index_256(roles, background);
    let band = nearest_256(roles.band);
    if band != card {
        return band;
    }
    step_256(band, roles.dark)
        .or_else(|| step_256(band, !roles.dark))
        .unwrap_or(band)
}

/// One step lighter or darker from a 256-colour index (16–255), if there is
/// one in its family.
fn step_256(index: u8, lighter: bool) -> Option<u8> {
    if index >= 232 {
        // The grey ramp, dark to light.
        return if lighter {
            (index < 255).then(|| index + 1)
        } else {
            (index > 232).then(|| index - 1)
        };
    }
    // The 6×6×6 cube: every channel's level moves one step where it can.
    let cube = index.checked_sub(16)?;
    let levels = [cube / 36, cube / 6 % 6, cube % 6];
    let stepped = levels.map(|level| {
        if lighter {
            (level + 1).min(5)
        } else {
            level.saturating_sub(1)
        }
    });
    (stepped != levels).then(|| 16 + 36 * stepped[0] + 6 * stepped[1] + stepped[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::complete::test_util::TempDir;
    /// A `#rrggbb` colour, read as a theme file's are.
    fn rgb(text: &str) -> Rgb {
        FigColor::parse(text)
            .filter(|color| text.len() == 7 && color.alpha >= 1.0)
            .map(|color| color.rgb)
            .unwrap_or_else(|| panic!("bad colour {text}"))
    }

    /// One Dark's colours, the mockup's `ansiFor` dark set.
    fn one_dark() -> Ansi {
        Ansi {
            green: Some(rgb("#98c379")),
            yellow: Some(rgb("#e5c07b")),
            blue: Some(rgb("#61afef")),
            magenta: Some(rgb("#c678dd")),
            cyan: Some(rgb("#56b6c2")),
        }
    }

    fn color(value: &Value, key: &str) -> Rgb {
        rgb(value[key]
            .as_str()
            .unwrap_or_else(|| panic!("{key} is not a string")))
    }

    fn expected_roles(value: &Value) -> Roles {
        let icons = &value["icons"];
        Roles {
            card: color(value, "card"),
            band: color(value, "band"),
            text: color(value, "text"),
            dim: color(value, "dim"),
            band_text: color(value, "band_text"),
            selected: color(value, "selected"),
            selected_text: color(value, "selected_text"),
            selected_dim: color(value, "selected_dim"),
            matched: color(value, "matched"),
            selected_matched: color(value, "selected_matched"),
            icons: IconColors {
                subcommand: color(icons, "subcommand"),
                option: color(icons, "option"),
                arg: color(icons, "arg"),
                folder: color(icons, "folder"),
                file: color(icons, "file"),
                history: color(icons, "history"),
                special: color(icons, "special"),
            },
            dark: value["dark"].as_bool().unwrap_or_else(|| panic!("dark")),
        }
    }

    /// Every colour of the roles, named.
    fn colors(roles: &Roles) -> Vec<(&'static str, Rgb)> {
        let icons = &roles.icons;
        vec![
            ("card", roles.card),
            ("band", roles.band),
            ("text", roles.text),
            ("dim", roles.dim),
            ("band_text", roles.band_text),
            ("selected", roles.selected),
            ("selected_text", roles.selected_text),
            ("selected_dim", roles.selected_dim),
            ("matched", roles.matched),
            ("selected_matched", roles.selected_matched),
            ("icons.subcommand", icons.subcommand),
            ("icons.option", icons.option),
            ("icons.arg", icons.arg),
            ("icons.folder", icons.folder),
            ("icons.file", icons.file),
            ("icons.history", icons.history),
            ("icons.special", icons.special),
        ]
    }

    #[test]
    fn every_palette_matches_the_golden_table() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/palettes/expected.json"
        );
        let text = std::fs::read_to_string(path).unwrap_or_else(|err| panic!("{path}: {err}"));
        let table: Value = serde_json::from_str(&text).unwrap_or_else(|err| panic!("{err}"));
        let cases = table["cases"].as_array().unwrap_or_else(|| panic!("cases"));
        // The theme on four backgrounds, plus without a blue.
        assert_eq!(cases.len(), 4 * (NAMES.len() + 1));
        let mut seen = std::collections::BTreeSet::new();
        for case in cases {
            let name = case["theme"].as_str().unwrap_or_else(|| panic!("theme"));
            let bg = color(case, "background");
            let ansi_value = &case["ansi"];
            let slot = |key: &str| ansi_value[key].as_str().map(rgb);
            let ansi = Ansi {
                green: slot("green"),
                yellow: slot("yellow"),
                blue: slot("blue"),
                magenta: slot("magenta"),
                cyan: slot("cyan"),
            };
            let (theme, warning) = resolve(name);
            let roles = theme.roles(Some(bg), &ansi);
            assert_eq!(warning, None, "{name}");
            let expected = expected_roles(&case["roles"]);
            assert_eq!(roles.dark, expected.dark, "{name} on {bg:?}");
            for ((role, got), (_, want)) in colors(&roles).into_iter().zip(colors(&expected)) {
                let off = |a: u8, b: u8| a.abs_diff(b);
                assert!(
                    off(got.0, want.0) <= 1 && off(got.1, want.1) <= 1 && off(got.2, want.2) <= 1,
                    "{name} on {bg:?} (blue {:?}): {role} is {got:?}, the mockup has {want:?}",
                    ansi.blue
                );
            }
            seen.insert(name.to_owned());
        }
        let names: std::collections::BTreeSet<String> =
            NAMES.iter().map(|name| (*name).to_owned()).collect();
        assert_eq!(seen, names);
    }

    #[test]
    fn the_built_in_theme_is_the_default() {
        assert_eq!(NAMES, ["default"]);
        assert_eq!(NAMES[0], crate::config::Config::default().theme);
        assert_eq!(resolve(NAMES[0]), (Theme::Default, None));
    }

    #[test]
    fn unknown_background_is_the_mockup_default() {
        for name in NAMES {
            for ansi in [one_dark(), Ansi::default()] {
                assert_eq!(
                    resolve(name).0.roles(None, &ansi),
                    resolve(name).0.roles(Some(DEFAULT_BACKGROUND), &ansi),
                    "{name}"
                );
            }
        }
        assert!(resolve("default").0.roles(None, &Ansi::default()).dark);
    }

    #[test]
    fn default_follows_the_terminal_blue() {
        let bg = Some(DEFAULT_BACKGROUND);
        let mut ansi = one_dark();
        let one = resolve("default").0.roles(bg, &ansi);
        ansi.blue = Some((0, 0, 255));
        let other = resolve("default").0.roles(bg, &ansi);
        assert_ne!(one.selected, other.selected);
        assert_ne!(one.selected_matched, other.selected_matched);
        assert_eq!(one.card, other.card);
        assert_eq!(one.icons.arg, rgb("#61afef"));
        // Pure blue is too dark on the dark card: lifted.
        assert_eq!(
            other.icons.arg,
            readable_blue((0, 0, 255), other.card, true)
        );
        assert_ne!(other.icons.arg, (0, 0, 255));
        // Without a blue: the derived one, which is lifted on the default
        // dark card too (3.59:1) and lowered a little on the light one
        // (4.32:1).
        ansi.blue = None;
        let derived = resolve("default").0.roles(bg, &ansi);
        let dark_blue = oklch(0.6, 0.13, 262.0);
        assert!(contrast(dark_blue, derived.card) < READABLE_CONTRAST);
        assert_eq!(
            derived.icons.arg,
            readable_blue(dark_blue, derived.card, true)
        );
        let light = resolve("default").0.roles(Some(LIGHT_BACKGROUND), &ansi);
        let light_blue = oklch(0.55, 0.15, 262.0);
        assert!(contrast(light_blue, light.card) < READABLE_CONTRAST);
        assert_eq!(
            light.icons.arg,
            readable_blue(light_blue, light.card, false)
        );
    }

    /// Terminal.app's blue as fac reads it: OSC 4 colour 4 answered
    /// `rgb:0000/0000/b333` in its default profile
    /// (`docs/plans/phase-3-live/probe/findings.md`, `apple.json`).
    const APPLE_BLUE: Rgb = (0x00, 0x00, 0xb3);

    /// Degrees between two hues.
    fn hue_apart(a: Rgb, b: Rgb) -> f64 {
        let apart = (to_oklch(a).h - to_oklch(b).h).abs();
        apart.min(360.0 - apart)
    }

    #[test]
    fn terminal_app_blue_is_lifted_on_the_dark_card() {
        let card = Theme::Default.roles(None, &Ansi::default()).card;
        assert_eq!(card, rgb("#282a2e"), "fac's default dark card");
        assert!(contrast(APPLE_BLUE, card) < 1.2);
        let lifted = readable_blue(APPLE_BLUE, card, true);
        // As make-expected.mjs's readableBlue gives it under node.
        assert_eq!(lifted, rgb("#598cfc"));
        assert!(contrast(lifted, card) >= READABLE_CONTRAST, "{lifted:?}");
        assert!(hue_apart(lifted, APPLE_BLUE) <= 2.0, "{lifted:?}");
        // The floor: the lightness that just reaches the bar.
        let l = to_oklch(lifted).l;
        assert!((0.64..0.67).contains(&l), "L {l}");
        assert!(contrast(lifted, card) < READABLE_CONTRAST + 0.1);
        // One Dark's blue already reaches it: unchanged.
        assert_eq!(readable_blue(rgb("#61afef"), card, true), rgb("#61afef"));
        // In the roles: the arg icon and the selected row's tint.
        let ansi = Ansi {
            blue: Some(APPLE_BLUE),
            ..one_dark()
        };
        let roles = Theme::Default.roles(None, &ansi);
        assert_eq!(roles.icons.arg, lifted);
        assert_eq!(roles.selected, mix(lifted, card, 0.68));
        assert_eq!(roles.selected_matched, mix(lifted, card, 0.45));
        let one = Theme::Default.roles(None, &one_dark());
        assert_eq!(one.icons.arg, rgb("#61afef"));
        assert_eq!(one.selected, mix(rgb("#61afef"), card, 0.68));
    }

    #[test]
    fn a_pale_blue_is_lowered_on_the_light_card() {
        let card = Theme::Default
            .roles(Some(LIGHT_BACKGROUND), &Ansi::default())
            .card;
        assert_eq!(card, rgb("#eeeff1"), "fac's default light card");
        // One Dark's blue on a light background.
        let pale = rgb("#61afef");
        assert!(contrast(pale, card) < 2.1);
        let lowered = readable_blue(pale, card, false);
        assert_eq!(lowered, rgb("#1e71ad"));
        assert!(contrast(lowered, card) >= READABLE_CONTRAST, "{lowered:?}");
        assert!(hue_apart(lowered, pale) <= 2.0, "{lowered:?}");
        // The ceiling: the lightness that just reaches the bar.
        assert!(to_oklch(lowered).l < to_oklch(pale).l);
        assert!(contrast(lowered, card) < READABLE_CONTRAST + 0.1);
        // Terminal.app's navy already reaches it on a light card: unchanged.
        assert_eq!(readable_blue(APPLE_BLUE, card, false), APPLE_BLUE);
        let ansi = Ansi {
            blue: Some(pale),
            ..one_dark()
        };
        let roles = Theme::Default.roles(Some(LIGHT_BACKGROUND), &ansi);
        assert_eq!(roles.icons.arg, lowered);
        assert_eq!(roles.selected, mix(lowered, card, 0.78));
    }

    #[test]
    fn a_blue_that_cannot_reach_the_bar_takes_the_end_of_the_range() {
        // A mid-grey card: no lightness of navy reaches 4.5:1 above it.
        let card = (0x8a, 0x8a, 0x8a);
        assert!(contrast((255, 255, 255), card) < READABLE_CONTRAST);
        assert_eq!(readable_blue(APPLE_BLUE, card, true), (255, 255, 255));
    }

    fn themes_dir(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new("themes");
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).unwrap_or_else(|err| panic!("{err}"));
        }
        dir
    }

    fn sample_fixture() -> String {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/palettes/themes/sample.json"
        );
        std::fs::read_to_string(path).unwrap_or_else(|err| panic!("{path}: {err}"))
    }

    fn fig_theme_on(dir: &TempDir, name: &str, background: Rgb, ansi: &Ansi) -> Roles {
        let (theme, warning) = resolve_in(name, Some(dir.path()));
        assert_eq!(warning, None);
        assert!(matches!(theme, Theme::Fig(_)), "{theme:?}");
        theme.roles(Some(background), ansi)
    }

    fn fig_theme(dir: &TempDir, name: &str) -> Roles {
        fig_theme_on(dir, name, DEFAULT_BACKGROUND, &one_dark())
    }

    #[test]
    fn fig_theme_file_maps_its_keys() {
        // A theme file in withfig/themes' format.
        let dir = themes_dir(&[("mine.json", &sample_fixture())]);
        let roles = fig_theme(&dir, "mine");
        // Expected values printed by the mockup's mix and fig under node.
        assert_eq!(
            roles,
            Roles {
                card: rgb("#343746"),
                band: rgb("#333645"),
                text: rgb("#f8f8f2"),
                dim: rgb("#96989c"),
                band_text: rgb("#bd93f9"),
                selected: rgb("#4f5370"),
                selected_text: rgb("#f8f8f2"),
                selected_dim: rgb("#c5c7cb"),
                matched: rgb("#5d537c"),
                selected_matched: rgb("#8170ae"),
                icons: IconColors {
                    subcommand: rgb("#a496bf"),
                    option: rgb("#84b198"),
                    arg: rgb("#61afef"),
                    folder: rgb("#c9ad7e"),
                    file: rgb("#96989c"),
                    history: rgb("#7d9faa"),
                    special: rgb("#c9ad7e"),
                },
                dark: true,
            }
        );
    }

    #[test]
    fn fig_theme_alpha_rgb_and_optional_keys() {
        let theme = r##"{
            "version": "1.0",
            "theme": {
                "backgroundColor": "#10203080",
                "textColor": "#ff000080",
                "matchBackgroundColor": "rgb(1,2,3)",
                "selection": { "textColor": "#ffffff", "backgroundColor": "rgb(0,0,255)" },
                "description": { "textColor": "rgb(9,9,9)" }
            }
        }"##;
        let dir = themes_dir(&[("alpha.json", theme)]);
        let roles = fig_theme(&dir, "alpha");
        // The card over the terminal background, the text over the card.
        assert_eq!(roles.card, rgb("#17202a"));
        assert_eq!(roles.text, mix(rgb("#17202a"), (255, 0, 0), 128.0 / 255.0));
        assert_eq!(roles.matched, (1, 2, 3));
        assert_eq!(roles.selected, (0, 0, 255));
        // Missing: Fig's default selected match, and the band from the text.
        assert_eq!(roles.selected_matched, (106, 142, 218));
        assert_eq!(roles.band, mix(roles.card, roles.text, 0.08));
        assert_eq!(roles.band_text, (9, 9, 9));
        // On a light card, the built-in theme's light icons.
        let light = theme.replace("#10203080", "#fefefe");
        let dir = themes_dir(&[("light.json", &light)]);
        let roles = fig_theme(&dir, "light");
        assert!(!roles.dark);
        assert_eq!(roles.icons.subcommand, rgb("#6a588a"));
        // #ff000080 over #fefefe.
        assert_eq!(roles.text, mix((254, 254, 254), (255, 0, 0), 128.0 / 255.0));
    }

    #[test]
    fn fig_colors() {
        let parse = FigColor::parse;
        assert_eq!(parse("#ff8000").map(|c| c.rgb), Some((255, 128, 0)));
        assert_eq!(
            parse("#FF8000ff").map(|c| (c.rgb, c.alpha)),
            Some(((255, 128, 0), 1.0))
        );
        assert_eq!(parse("rgb(1,22,255)").map(|c| c.rgb), Some((1, 22, 255)));
        for bad in [
            "rgb(1, 2, 3)",
            "rgb(1,2,256)",
            "rgb(1,2)",
            "rgb(1,2,3,4)",
            "rgb(+1,2,3)",
            "rgba(1,2,3,1)",
            "#fff",
            "#ff80001",
            "#gg8000",
            "#+f8000",
            "red",
            "",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    fn warning_for(dir: Option<&Path>, name: &str) -> String {
        let (theme, warning) = resolve_in(name, dir);
        assert_eq!(theme, Theme::Default, "{name}");
        let warning = warning.unwrap_or_else(|| panic!("{name}: no warning"));
        assert!(warning.contains(&format!("{name:?}")), "{warning}");
        assert!(warning.ends_with("; using default"), "{warning}");
        warning
    }

    #[test]
    fn bad_theme_files_give_the_default_and_a_warning() {
        let secret = "secret-value";
        let dir = themes_dir(&[
            ("v2.json", r#"{ "theme": { "textColor": "secret-value" } }"#),
            ("v3.json", r#"{ "version": "2.0", "secret-value": 1 }"#),
            ("broken.json", "{ \"version\": \"1.0\", secret-value"),
            (
                "missing.json",
                r#"{ "version": "1.0", "theme": { "textColor": "rgb(1,1,1)" } }"#,
            ),
            (
                "badcolor.json",
                r##"{ "version": "1.0", "theme": {
                    "backgroundColor": "#000000", "textColor": "secret-value",
                    "matchBackgroundColor": "#000000",
                    "selection": { "textColor": "#000000", "backgroundColor": "#000000" },
                    "description": { "textColor": "#000000" } } }"##,
            ),
            ("notheme.json", r#"{ "version": "1.0" }"#),
        ]);
        std::fs::create_dir(dir.path().join("dir.json")).unwrap_or_else(|err| panic!("{err}"));
        for (name, problem) in [
            ("v2", "v2 themes are not supported"),
            ("v3", "v2 themes are not supported"),
            ("broken", "not valid JSON"),
            ("missing", "`backgroundColor` is missing"),
            ("badcolor", "`textColor` is not a colour"),
            ("notheme", "`theme` is missing"),
            ("dir", "cannot read"),
            ("nowhere", "does not exist"),
        ] {
            let warning = warning_for(Some(dir.path()), name);
            assert!(warning.contains(problem), "{name}: {warning}");
            assert!(!warning.contains(secret), "{name}: {warning}");
        }
        let warning = warning_for(None, "nowhere");
        assert!(warning.contains("HOME"), "{warning}");
    }

    #[test]
    fn bad_names_never_touch_the_file_system() {
        // A file that a traversal would reach.
        let dir = themes_dir(&[]);
        let inner = dir.path().join("inner");
        std::fs::create_dir(&inner).unwrap_or_else(|err| panic!("{err}"));
        std::fs::write(dir.path().join("x.json"), sample_fixture())
            .unwrap_or_else(|err| panic!("{err}"));
        for name in ["../x", "a/b", "", "x y", "é", "x\u{1b}[31m", "/etc/passwd"] {
            let warning = warning_for(Some(&inner), name);
            assert!(warning.contains("not a valid name"), "{name:?}: {warning}");
            assert!(!warning.contains('\u{1b}'), "{warning}");
        }
        assert!(valid_name("my.theme_2-x"));
        assert!(matches!(resolve_in("x", Some(dir.path())).0, Theme::Fig(_)));
    }

    #[test]
    fn unknown_name_gives_the_default_and_a_warning() {
        let (theme, warning) = resolve("no-such-theme-for-fac-tests");
        assert_eq!(
            theme.roles(None, &one_dark()),
            resolve(NAMES[0]).0.roles(None, &one_dark())
        );
        let warning = warning.unwrap_or_else(|| panic!("no warning"));
        assert!(
            warning.contains("\"no-such-theme-for-fac-tests\""),
            "{warning}"
        );
        assert!(warning.ends_with("; using default"), "{warning}");
    }

    #[test]
    fn theme_new_writes_a_file_that_reads_back() {
        let home = themes_dir(&[]);
        let dir = home.path().join("a/b/themes");
        let path = create_theme_file_in(&dir, "mine", true).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(path, dir.join("mine.json"));
        let (theme, warning) = resolve_in("mine", Some(&dir));
        assert_eq!(warning, None);
        let ansi = Ansi::default();
        let read = theme.roles(Some(LIGHT_BACKGROUND), &ansi);
        let built_in = Theme::Default.roles(Some(LIGHT_BACKGROUND), &ansi);
        assert_eq!(read.card, built_in.card);
        assert_eq!(read.selected, built_in.selected);
        assert!(!read.dark);
    }

    #[test]
    fn theme_new_never_overwrites_and_rejects_bad_names() {
        let home = themes_dir(&[]);
        let dir = home.path();
        std::fs::write(dir.join("mine.json"), "keep").unwrap_or_else(|err| panic!("{err}"));
        let err = create_theme_file_in(dir, "mine", false).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.join("mine.json")).unwrap_or_default(),
            "keep"
        );
        for name in ["../x", "a/b", "", "x y", "default"] {
            assert!(create_theme_file_in(dir, name, false).is_err(), "{name:?}");
        }
        assert!(!dir.join("x.json").exists());
    }

    #[test]
    fn printed_theme_reads_back_as_the_built_in_theme() {
        for (light, background) in [(false, DEFAULT_BACKGROUND), (true, LIGHT_BACKGROUND)] {
            let text = print_theme(light);
            let dir = themes_dir(&[("printed.json", &text)]);
            let ansi = Ansi::default();
            let read = fig_theme_on(&dir, "printed", background, &ansi);
            let built_in = Theme::Default.roles(Some(background), &ansi);
            let close = |a: Rgb, b: Rgb| {
                a.0.abs_diff(b.0) <= 1 && a.1.abs_diff(b.1) <= 1 && a.2.abs_diff(b.2) <= 1
            };
            for ((role, got), (_, want)) in colors(&read).into_iter().zip(colors(&built_in)) {
                // Derived from the other colours when a theme file is read,
                // and `file` is `dim`.
                if matches!(role, "dim" | "selected_dim" | "icons.file") {
                    continue;
                }
                assert!(
                    close(got, want),
                    "light={light}: {role} is {got:?}, not {want:?}"
                );
            }
            assert_eq!(read.dark, built_in.dark);
        }
        assert_ne!(print_theme(false), print_theme(true));
    }

    fn with_card(card: Rgb, dark: bool) -> Roles {
        let mut roles = resolve(NAMES[0]).0.roles(None, &Ansi::default());
        roles.card = card;
        roles.dark = dark;
        roles
    }

    #[test]
    fn card_256_steps_off_the_background() {
        // Both round to grey 234: one step lighter on dark, darker on light.
        let bg = Some((28, 28, 28));
        assert_eq!(nearest_256((28, 28, 28)), 234);
        assert_eq!(nearest_256((30, 29, 29)), 234);
        assert_eq!(card_index_256(&with_card((30, 29, 29), true), bg), 235);
        assert_eq!(card_index_256(&with_card((30, 29, 29), false), bg), 233);
        // Different indices are kept.
        assert_eq!(
            card_index_256(&with_card((58, 58, 58), true), bg),
            nearest_256((58, 58, 58))
        );
        // The ends of the grey ramp step the other way.
        assert_eq!(
            card_index_256(&with_card((8, 8, 8), false), Some((8, 8, 8))),
            233
        );
        assert_eq!(
            card_index_256(&with_card((238, 238, 238), true), Some((238, 238, 238))),
            254
        );
        // In the cube: every level one step.
        let navy = (0, 0, 135);
        assert_eq!(nearest_256(navy), 18);
        assert_eq!(
            card_index_256(&with_card(navy, true), Some(navy)),
            16 + 36 + 6 + 3
        );
        assert_eq!(card_index_256(&with_card(navy, false), Some(navy)), 17);
        let white = (255, 255, 255);
        assert_eq!(card_index_256(&with_card(white, true), Some(white)), 188);
        // The built-in theme `default`: its card on the default background.
        let roles = resolve(NAMES[0]).0.roles(None, &Ansi::default());
        let card = card_index_256(&roles, None);
        assert_ne!(card, nearest_256(DEFAULT_BACKGROUND));
    }

    #[test]
    fn band_256_steps_off_the_card() {
        let bg = Some((28, 28, 28));
        // The card is stepped to 235; a band that rounds to it steps on,
        // lighter on dark, darker on light.
        let mut roles = with_card((30, 29, 29), true);
        roles.band = (38, 38, 38);
        assert_eq!(nearest_256(roles.band), 235);
        assert_eq!(card_index_256(&roles, bg), 235);
        assert_eq!(band_index_256(&roles, bg), 236);
        let mut roles = with_card((30, 29, 29), false);
        roles.band = (18, 18, 18);
        assert_eq!(card_index_256(&roles, bg), 233);
        assert_eq!(nearest_256(roles.band), 233);
        assert_eq!(band_index_256(&roles, bg), 232);
        // At the ramp's end it steps the other way.
        let mut roles = with_card((8, 8, 8), false);
        roles.band = (8, 8, 8);
        assert_eq!(card_index_256(&roles, Some((60, 60, 60))), 232);
        assert_eq!(band_index_256(&roles, Some((60, 60, 60))), 233);
        // Apart already: kept.
        let mut roles = with_card((58, 58, 58), true);
        roles.band = (30, 30, 30);
        assert_eq!(band_index_256(&roles, bg), nearest_256((30, 30, 30)));
        // The real default on the harness's background (#1e1e1e): band and
        // card rounded to the same grey before.
        let background = Some((0x1e, 0x1e, 0x1e));
        let roles = resolve(NAMES[0]).0.roles(background, &Ansi::default());
        let card = card_index_256(&roles, background);
        assert_eq!(nearest_256(roles.band), card);
        assert_ne!(band_index_256(&roles, background), card);
    }
}
