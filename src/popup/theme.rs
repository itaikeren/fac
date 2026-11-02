//! The popup's colours: a palette's [`Roles`] (`palette.rs`), written once
//! as SGR parameter strings for the terminal's colour depth.
//!
//! With true colour every role is a 24-bit colour. With 256 colours the
//! card is [`palette::card_index_256`] (one step away from the background's
//! index when both round to the same one, so the borderless card stays
//! visible), the band is [`palette::band_index_256`] (kept off the card's
//! index, so the bottom line stands out) and every other role is its
//! [`nearest_256`] index.

use crate::complete::engine::Kind;
use crate::popup::palette::{self, Roles};
use crate::term::caps::TermCaps;
use crate::term::ident::TermKind;

pub type Rgb = (u8, u8, u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorDepth {
    TrueColor,
    Ansi256,
}

/// The styles of a list row on one background: the card's, or the
/// selected row's. Each is the full parameter list of one SGR
/// (`CSI <style> m`), starting from a reset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowStyles {
    /// The label, the padding and the gaps.
    pub text: String,
    /// Matched letters: the label's colour on the match tint.
    pub matched: String,
    /// The argument hint.
    pub dim: String,
    /// The icons, bold, in the order of [`icon_slot`].
    pub icons: [String; 7],
}

impl RowStyles {
    pub fn icon(&self, kind: Kind) -> &str {
        &self.icons[icon_slot(kind)]
    }
}

/// Where a kind's icon colour is in [`RowStyles::icons`]: subcommand,
/// option, arg, folder, file, history, then special (shortcuts too).
pub fn icon_slot(kind: Kind) -> usize {
    match kind {
        Kind::Subcommand => 0,
        Kind::Option => 1,
        Kind::Arg => 2,
        Kind::Folder => 3,
        Kind::File => 4,
        Kind::History => 5,
        Kind::Special | Kind::Shortcut => 6,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub roles: Roles,
    pub depth: ColorDepth,
    /// A list row on the card.
    pub card: RowStyles,
    /// The selected row.
    pub selected: RowStyles,
    /// The bottom line and the popout: italic `band_text` on `band`.
    pub band: String,
}

impl Theme {
    /// `background`: the terminal's (OSC 11), which the 256-colour card
    /// is kept apart from.
    pub fn new(roles: Roles, background: Option<Rgb>, depth: ColorDepth) -> Self {
        let fg = |rgb: Rgb| color(38, rgb, depth);
        let bg = |rgb: Rgb| color(48, rgb, depth);
        let (card_bg, band_bg) = match depth {
            ColorDepth::TrueColor => (bg(roles.card), bg(roles.band)),
            ColorDepth::Ansi256 => (
                format!("48;5;{}", palette::card_index_256(&roles, background)),
                format!("48;5;{}", palette::band_index_256(&roles, background)),
            ),
        };
        let icons = roles.icons;
        let icon_colors = [
            icons.subcommand,
            icons.option,
            icons.arg,
            icons.folder,
            icons.file,
            icons.history,
            icons.special,
        ];
        let row = |text: Rgb, dim: Rgb, background: &str, matched: Rgb| RowStyles {
            text: format!("0;{};{background}", fg(text)),
            matched: format!("0;{};{}", fg(text), bg(matched)),
            dim: format!("0;{};{background}", fg(dim)),
            icons: icon_colors.map(|icon| format!("0;1;{};{background}", fg(icon))),
        };
        Self {
            card: row(roles.text, roles.dim, &card_bg, roles.matched),
            selected: row(
                roles.selected_text,
                roles.selected_dim,
                &bg(roles.selected),
                roles.selected_matched,
            ),
            band: format!("0;3;{};{band_bg}", fg(roles.band_text)),
            roles,
            depth,
        }
    }

    /// The configured palette on the terminal's background and colours.
    pub fn from_caps(theme: &palette::Theme, caps: &TermCaps, depth: ColorDepth) -> Self {
        Self::new(
            theme.roles(caps.background, &caps.ansi),
            caps.background,
            depth,
        )
    }

    /// `COLORTERM` `truecolor` or `24bit` means true colour. Without it, so
    /// does a terminal measured drawing true colour: all five measured kinds
    /// (findings.md's display rows). An unknown one gets 256 colours.
    pub fn depth_from_env(colorterm: Option<&str>, kind: TermKind) -> ColorDepth {
        match (colorterm, kind) {
            (Some("truecolor" | "24bit"), _) => ColorDepth::TrueColor,
            (
                _,
                TermKind::Apple
                | TermKind::Iterm2
                | TermKind::Vscode
                | TermKind::Zed
                | TermKind::Ghostty,
            ) => ColorDepth::TrueColor,
            (_, TermKind::Unknown) => ColorDepth::Ansi256,
        }
    }
}

/// `38;2;r;g;b` (or `48;…` with `base` 48), or `38;5;n` with 256 colours.
fn color(base: u8, (r, g, b): Rgb, depth: ColorDepth) -> String {
    match depth {
        ColorDepth::TrueColor => format!("{base};2;{r};{g};{b}"),
        ColorDepth::Ansi256 => format!("{base};5;{}", nearest_256((r, g, b))),
    }
}

/// The xterm cube's channel levels.
const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The nearest 256-colour index (16–255) by squared RGB distance.
pub fn nearest_256(rgb: Rgb) -> u8 {
    let distance = |(r, g, b): Rgb| {
        let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
        d(r, rgb.0) + d(g, rgb.1) + d(b, rgb.2)
    };
    let level = |value: u8| {
        (0..LEVELS.len())
            .min_by_key(|&i| (i32::from(LEVELS[i]) - i32::from(value)).abs())
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (level(rgb.0), level(rgb.1), level(rgb.2));
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let cube_index = 16 + 36 * ri + 6 * gi + bi;
    let average = (u32::from(rgb.0) + u32::from(rgb.1) + u32::from(rgb.2)) / 3;
    // Grey ramp: 232 + i is 8 + 10 i, for i in 0..24.
    let grey_step = (average.saturating_sub(3) / 10).min(23);
    let grey_level = u8::try_from(8 + 10 * grey_step).unwrap_or(238);
    let grey = (grey_level, grey_level, grey_level);
    let grey_index = 232 + grey_step as usize;
    let index = if distance(grey) < distance(cube) {
        grey_index
    } else {
        cube_index
    };
    u8::try_from(index).unwrap_or(16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::caps::Ansi;

    fn default_roles() -> Roles {
        palette::Theme::default().roles(None, &Ansi::default())
    }

    #[test]
    fn depth() {
        let unknown = TermKind::Unknown;
        assert_eq!(
            Theme::depth_from_env(Some("truecolor"), unknown),
            ColorDepth::TrueColor
        );
        assert_eq!(
            Theme::depth_from_env(Some("24bit"), unknown),
            ColorDepth::TrueColor
        );
        assert_eq!(
            Theme::depth_from_env(Some("yes"), unknown),
            ColorDepth::Ansi256
        );
        assert_eq!(Theme::depth_from_env(None, unknown), ColorDepth::Ansi256);
        assert_eq!(
            Theme::depth_from_env(Some(""), unknown),
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn depth_from_a_measured_kind() {
        for kind in [
            TermKind::Apple,
            TermKind::Iterm2,
            TermKind::Vscode,
            TermKind::Zed,
            TermKind::Ghostty,
        ] {
            assert_eq!(
                Theme::depth_from_env(None, kind),
                ColorDepth::TrueColor,
                "{kind:?}"
            );
            // COLORTERM only ever adds true colour.
            assert_eq!(
                Theme::depth_from_env(Some("yes"), kind),
                ColorDepth::TrueColor,
                "{kind:?}"
            );
            assert_eq!(
                Theme::depth_from_env(Some("truecolor"), kind),
                ColorDepth::TrueColor,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn nearest() {
        assert_eq!(nearest_256((0, 0, 0)), 16);
        assert_eq!(nearest_256((255, 255, 255)), 231);
        assert_eq!(nearest_256((255, 0, 0)), 196);
        assert_eq!(nearest_256((0, 0, 255)), 21);
        assert_eq!(nearest_256((128, 128, 128)), 244);
        assert_eq!(nearest_256((28, 31, 38)), 234);
    }

    #[test]
    fn true_colour_styles_are_the_roles() {
        let roles = default_roles();
        let theme = Theme::new(roles, None, ColorDepth::TrueColor);
        let (r, g, b) = roles.text;
        let (cr, cg, cb) = roles.card;
        assert_eq!(
            theme.card.text,
            format!("0;38;2;{r};{g};{b};48;2;{cr};{cg};{cb}")
        );
        let (mr, mg, mb) = roles.matched;
        assert_eq!(
            theme.card.matched,
            format!("0;38;2;{r};{g};{b};48;2;{mr};{mg};{mb}")
        );
        let (sr, sg, sb) = roles.selected;
        assert!(
            theme
                .selected
                .text
                .ends_with(&format!("48;2;{sr};{sg};{sb}"))
        );
        let (ir, ig, ib) = roles.icons.option;
        assert_eq!(
            theme.selected.icon(Kind::Option),
            format!("0;1;38;2;{ir};{ig};{ib};48;2;{sr};{sg};{sb}")
        );
        assert_eq!(
            theme.card.icon(Kind::Shortcut),
            theme.card.icon(Kind::Special)
        );
        assert!(theme.band.starts_with("0;3;38;2;"));
    }

    #[test]
    fn ansi_256_keeps_the_card_off_the_background() {
        let roles = default_roles();
        let background = Some(roles.card);
        let theme = Theme::new(roles, background, ColorDepth::Ansi256);
        let card = palette::card_index_256(&roles, background);
        assert_ne!(card, nearest_256(roles.card), "stepped away");
        assert!(theme.card.text.ends_with(&format!(";48;5;{card}")));
        assert!(theme.card.dim.ends_with(&format!(";48;5;{card}")));
        let selected = nearest_256(roles.selected);
        assert!(theme.selected.text.ends_with(&format!(";48;5;{selected}")));
        assert!(theme.card.text.starts_with("0;38;5;"));
    }
}
