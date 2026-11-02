// Prints the golden palette table that `src/popup/palette.rs` is tested
// against, computed by the mockup's own colour functions.
//
// Run from the repository root:
//
//   node tests/fixtures/palettes/make-expected.mjs > tests/fixtures/palettes/expected.json
//
// It reads docs/plans/phase-2-prompts/popup-mockup.html, takes the part of its
// <script> from "colour maths" up to the PALETTES list (the OKLCH maths,
// graphite, ansiFor and graphiteAccent), evaluates it, and builds the theme
// on four backgrounds. The terminal colours passed to
// each palette are the mockup's ansiFor values for that background; they are
// printed next to the roles so the Rust test passes the same ones.
//
// The roles the mockup does not have are added here with the rules written
// in palette.rs:
// - icons.arg: the terminal's blue, or the derived blue when there is none,
//   through readableBlue;
// - icons.file: the theme's dim;
// - icons.special: the folder colour.
//
// The default rows with `"ansi": { "blue": null, … }` are the no-blue
// fallback: the accent is oklch(0.6, 0.13, 262) on dark and
// oklch(0.55, 0.15, 262) on light.
//
// readableBlue is palette.rs's `readable_blue`, which the mockup does not
// have: a blue (the terminal's or the derived one) that does not reach a WCAG
// contrast of 4.5:1 against the card keeps its OKLCH hue and takes the
// nearest lightness that does, raised on a dark card and lowered on a light
// one. A blue that reaches it is used as it is. It is applied to the accent
// (the selected row's tint) and to the arg icon. The printed `ansi` stays the
// terminal's own colours.
//
// Needs only node; nothing is installed.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..');
const html = readFileSync(join(root, 'docs/plans/phase-2-prompts/popup-mockup.html'), 'utf8');
const start = html.indexOf('// ---- colour maths');
const end = html.indexOf('var PALETTES = [');
if (start < 0 || end < 0 || end < start) {
  throw new Error('the mockup script has changed shape');
}

// `ansiFor` is a function declaration, so it can be swapped after the fact;
// graphiteAccent looks it up when it runs.
const m = new Function(
  html.slice(start, end) +
    `
    var realAnsiFor = ansiFor, ansiOverride = null;
    ansiFor = function (bg, dark) { return ansiOverride ? ansiOverride(bg, dark) : realAnsiFor(bg, dark); };
    return {
      oklch: oklch, toOklch: toOklch, hexToRgb: hexToRgb, lin: lin, graphite: graphite,
      graphiteAccent: graphiteAccent, ansiFor: realAnsiFor,
      setAnsi: function (f) { ansiOverride = f; }
    };`,
)();

const BACKGROUNDS = ['#1e1f23', '#0b0c0e', '#002b36', '#fbfbf9'];

function derivedBlue(dark) {
  return dark ? m.oklch(0.6, 0.13, 262) : m.oklch(0.55, 0.15, 262);
}

const READABLE_CONTRAST = 4.5;

function luminance(hex) {
  const [r, g, b] = m.hexToRgb(hex).map(m.lin);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a, b) {
  const x = luminance(a), y = luminance(b);
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

function readableBlue(blue, card, dark) {
  if (contrast(blue, card) >= READABLE_CONTRAST) return blue;
  const o = m.toOklch(m.hexToRgb(blue));
  const reaches = (l) => contrast(m.oklch(l, o.C, o.h), card) >= READABLE_CONTRAST;
  // `short` never reaches the bar; `enough` does, or is the range's end.
  let short = o.L, enough = dark ? 1 : 0;
  for (let i = 0; i < 40; i++) {
    const mid = (short + enough) / 2;
    if (reaches(mid)) enough = mid; else short = mid;
  }
  return m.oklch(enough, o.C, o.h);
}

// The accent graphiteAccent tints with: `blue` (null: the derived blue)
// through readableBlue on the theme's card.
function useBlue(blue) {
  m.setAnsi(function (b, d) {
    const accent = readableBlue(blue === null ? derivedBlue(d) : blue, m.graphite(b).card, d);
    return Object.assign({}, m.ansiFor(b, d), { blue: accent });
  });
}

function roles(p, arg) {
  return {
    card: p.card,
    band: p.band,
    text: p.text,
    dim: p.dim,
    band_text: p.bandText,
    selected: p.sel,
    selected_text: p.selText,
    selected_dim: p.selDim,
    matched: p.match,
    selected_matched: p.selMatch,
    icons: {
      subcommand: p.ico.sub,
      option: p.ico.opt,
      arg: arg,
      folder: p.ico.dir,
      file: p.dim,
      history: p.ico.hist,
      special: p.ico.dir,
    },
    dark: p.dark,
  };
}

const cases = [];
for (const bg of BACKGROUNDS) {
  const dark = m.graphite(bg).dark;
  const ansi = m.ansiFor(bg, dark);
  useBlue(ansi.blue);
  const tinted = m.graphiteAccent(bg, 'tint');
  m.setAnsi(null);
  cases.push({
    theme: 'default',
    background: bg,
    ansi,
    roles: roles(tinted, readableBlue(ansi.blue, tinted.card, tinted.dark)),
  });
  // default when the terminal does not report its blue.
  const noBlue = Object.assign({}, ansi, { blue: null });
  useBlue(null);
  const fallback = m.graphiteAccent(bg, 'tint');
  m.setAnsi(null);
  cases.push({
    theme: 'default',
    background: bg,
    ansi: noBlue,
    roles: roles(fallback, readableBlue(derivedBlue(fallback.dark), fallback.card, fallback.dark)),
  });
}

process.stdout.write(JSON.stringify({ backgrounds: BACKGROUNDS, cases }, null, 1) + '\n');
