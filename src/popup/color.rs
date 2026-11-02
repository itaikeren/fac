//! sRGB ↔ OKLab ↔ OKLCH and colour mixing, ported exactly from the popup
//! mockup the palettes were chosen from
//! (`docs/plans/phase-2-prompts/popup-mockup.html`: `lin`, `gam`, `toOklch`,
//! `labToRgb`, `oklch`, `mix`). Colours are 8-bit sRGB, rounded as the
//! mockup's `rgbToHex` rounds, so a chain of mixes gives the mockup's values.

use std::f64::consts::PI;

use crate::popup::theme::Rgb;

/// A colour in OKLCH: lightness 0–1, chroma, hue in degrees (0–360).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oklch {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

/// sRGB channel (0–255) to linear light (0–1).
fn lin(v: u8) -> f64 {
    let v = f64::from(v) / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light (0–1) to an sRGB channel (0–255, unrounded).
fn gam(v: f64) -> f64 {
    let v = if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    v * 255.0
}

/// JavaScript's `Math.round` of a value clamped to 0–255.
fn to_channel(v: f64) -> u8 {
    let rounded = (v.clamp(0.0, 255.0) + 0.5).floor();
    // In 0..=255 after the clamp; the cast cannot truncate.
    rounded as u8
}

pub fn to_oklch((r, g, b): Rgb) -> Oklch {
    let (r, g, b) = (lin(r), lin(g), lin(b));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    let lightness = 0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s;
    let a = 1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s;
    let b = 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s;
    // In the mockup's order of operations, so the floats match.
    let mut h = b.atan2(a) * 180.0 / PI;
    if h < 0.0 {
        h += 360.0;
    }
    Oklch {
        l: lightness,
        c: (a * a + b * b).sqrt(),
        h,
    }
}

/// OKLab to linear sRGB, unclamped.
fn lab_to_linear(l: f64, a: f64, b: f64) -> [f64; 3] {
    let lc = (l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let mc = (l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let sc = (l - 0.089_484_177_5 * a - 1.291_485_548_0 * b).powi(3);
    [
        4.076_741_662_1 * lc - 3.307_711_591_3 * mc + 0.230_969_929_2 * sc,
        -1.268_438_004_6 * lc + 2.609_757_401_1 * mc - 0.341_319_396_5 * sc,
        -0.004_196_086_3 * lc - 0.703_418_614_7 * mc + 1.707_614_701_0 * sc,
    ]
}

fn linear_to_rgb([r, g, b]: [f64; 3]) -> Rgb {
    let channel = |v: f64| to_channel(gam(v.clamp(0.0, 1.0)));
    (channel(r), channel(g), channel(b))
}

/// The mockup's `oklch(L, C, h)`: lightness is clamped to 0–1, and while the
/// colour is outside sRGB (beyond 0.0005 either side) the chroma shrinks by
/// a tenth, up to 30 times; then the grey of that lightness.
pub fn oklch(l: f64, c: f64, h: f64) -> Rgb {
    let l = l.clamp(0.0, 1.0);
    let (sin, cos) = (h * PI / 180.0).sin_cos();
    let mut c = c;
    for _ in 0..30 {
        let linear = lab_to_linear(l, c * cos, c * sin);
        if linear.iter().all(|&v| (-0.0005..=1.0005).contains(&v)) || c < 0.001 {
            return linear_to_rgb(linear);
        }
        c *= 0.9;
    }
    linear_to_rgb(lab_to_linear(l, 0.0, 0.0))
}

/// The mockup's `mix(a, b, t)`: `a + (b - a) t` per gamma-encoded channel,
/// rounded. `t` 0 is `a`, 1 is `b`.
pub fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let channel = |x: u8, y: u8| {
        let (x, y) = (f64::from(x), f64::from(y));
        to_channel(x + (y - x) * t)
    };
    (channel(a.0, b.0), channel(a.1, b.1), channel(a.2, b.2))
}

/// `#rrggbb`.
pub fn hex((r, g, b): Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// WCAG 2's relative luminance (0–1). Not in the mockup:
/// `tests/fixtures/palettes/make-expected.mjs` has the same formula.
fn luminance((r, g, b): Rgb) -> f64 {
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// WCAG 2's contrast ratio of two colours, 1 to 21, in either order.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mockup's `#rrggbb` values.
    fn rgb(text: &str) -> Rgb {
        let channel = |at: usize| {
            text.get(at..at + 2)
                .and_then(|digits| u8::from_str_radix(digits, 16).ok())
                .unwrap_or_else(|| panic!("bad hex {text}"))
        };
        assert!(text.len() == 7 && text.starts_with('#'), "bad hex {text}");
        (channel(1), channel(3), channel(5))
    }

    fn close(a: f64, b: f64, what: &str) {
        assert!((a - b).abs() < 1e-9, "{what}: {a} vs {b}");
    }

    // Every expected value below was printed by the mockup's own functions
    // under node 25.

    #[test]
    fn to_oklch_matches_the_mockup() {
        for (text, l, c, h) in [
            (
                "#1e1f23",
                0.239_914_592_966,
                0.007_752_973_067_03,
                274.616_499_247,
            ),
            (
                "#fbfbf9",
                0.987_532_337_053,
                0.002_635_263_714_89,
                106.447_531_180,
            ),
            (
                "#002b36",
                0.267_336_614_814,
                0.048_612_102_614_2,
                219.816_883_738,
            ),
            (
                "#61afef",
                0.730_413_649_845,
                0.121_280_251_450,
                245.297_155_383,
            ),
            (
                "#ff0000",
                0.627_955_360_615,
                0.257_683_307_736,
                29.233_885_192_3,
            ),
        ] {
            let o = to_oklch(rgb(text));
            close(o.l, l, text);
            close(o.c, c, text);
            assert!((o.h - h).abs() < 1e-7, "{text}: {} vs {h}", o.h);
        }
        let black = to_oklch((0, 0, 0));
        close(black.l, 0.0, "black");
        close(black.c, 0.0, "black");
        let white = to_oklch((255, 255, 255));
        close(white.l, 0.999_999_993_474, "white");
        assert!(white.c < 1e-6);
    }

    #[test]
    fn round_trips() {
        for text in [
            "#1e1f23", "#fbfbf9", "#002b36", "#61afef", "#ff0000", "#000000", "#ffffff",
        ] {
            let o = to_oklch(rgb(text));
            assert_eq!(hex(oklch(o.l, o.c, o.h)), text);
        }
        // Every grey and a spread of colours come back exactly.
        for v in 0..=255u8 {
            let o = to_oklch((v, v, v));
            assert_eq!(oklch(o.l, o.c, o.h), (v, v, v), "grey {v}");
        }
        for r in (0..=255u8).step_by(17) {
            for g in (0..=255u8).step_by(51) {
                for b in (0..=255u8).step_by(85) {
                    let o = to_oklch((r, g, b));
                    assert_eq!(oklch(o.l, o.c, o.h), (r, g, b));
                }
            }
        }
    }

    #[test]
    fn oklch_matches_the_mockup() {
        for ((l, c, h), text) in [
            ((0.6, 0.13, 262.0), "#547ecd"),
            ((0.55, 0.15, 262.0), "#3e6dc8"),
            // Lightness is clamped.
            ((1.2, 0.0, 0.0), "#ffffff"),
            ((-0.1, 0.1, 0.0), "#010000"),
            ((0.62, 0.0005, 10.0), "#868686"),
        ] {
            assert_eq!(hex(oklch(l, c, h)), text, "oklch({l}, {c}, {h})");
        }
    }

    #[test]
    fn gamut_reduction_matches_the_mockup() {
        // Out of sRGB: the chroma shrinks 7, 17, 8 and 10 times.
        for ((l, c, h), text) in [
            ((0.7, 0.4, 150.0), "#08be58"),
            ((0.9, 0.3, 30.0), "#fed2cb"),
            ((0.5, 0.37, 140.0), "#237703"),
            ((0.2, 0.3, 300.0), "#1f013d"),
        ] {
            assert_eq!(hex(oklch(l, c, h)), text, "oklch({l}, {c}, {h})");
        }
    }

    #[test]
    fn mix_matches_the_mockup() {
        for (a, b, t, text) in [
            ("#61afef", "#282a2e", 0.68, "#3a556c"),
            ("#000000", "#ffffff", 0.5, "#808080"),
            ("#e5c07b", "#292b2f", 0.72, "#5e5544"),
            ("#ffffff", "#3a556c", 0.35, "#bac4cc"),
            ("#123456", "#abcdef", 0.0, "#123456"),
        ] {
            assert_eq!(hex(mix(rgb(a), rgb(b), t)), text, "mix({a}, {b}, {t})");
        }
        assert_eq!(mix((10, 20, 30), (200, 100, 0), 1.0), (200, 100, 0));
    }

    #[test]
    fn contrast_is_wcag() {
        let close = |a: f64, b: f64| (a - b).abs() < 0.005;
        assert!(close(contrast((0, 0, 0), (255, 255, 255)), 21.0));
        assert!(close(contrast((255, 255, 255), (0, 0, 0)), 21.0));
        assert!(close(contrast(rgb("#61afef"), rgb("#61afef")), 1.0));
        // #767676 is the classic 4.54:1 grey on white.
        assert!(close(contrast(rgb("#767676"), (255, 255, 255)), 4.54));
    }
}
