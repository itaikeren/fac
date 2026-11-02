// The host globals a spec's context gets besides `console` (which the spec
// thread installs from Rust): setTimeout / clearTimeout, the one part of
// Intl the specs use, and a `localeCompare` that sorts as V8 does. Nothing
// else: no require, process, fetch or import().
// Evaluating this file as a script installs them and yields the timer drain.
//
// drain() runs every timer that is due, in due order, and returns 0 when it
// ran any, else the milliseconds until the next one, or -1 when none is left.
// The spec thread calls it while it drives a promise.
(function () {
  let nextId = 1;
  const timers = new Map();

  globalThis.setTimeout = function setTimeout(callback, delay, ...args) {
    const id = nextId;
    nextId += 1;
    if (typeof callback === "function") {
      // As browsers do: no number (NaN) is 0, and the longest wait is
      // 2^31 - 1 ms (Infinity too).
      const wait = Math.min(Math.max(0, Number(delay) || 0), 2147483647);
      timers.set(id, { at: Date.now() + wait, callback, args });
    }
    return id;
  };

  globalThis.clearTimeout = function clearTimeout(id) {
    timers.delete(id);
  };

  // cargo's crate versions generator formats download counts with
  // new Intl.NumberFormat(undefined, {notation: "compact",
  // compactDisplay: "short", maximumSignificantDigits: 3}).format(n), and
  // that is the only Intl in the specs. English short units, as Intl gives.
  class NumberFormat {
    constructor(locales, options = {}) {
      this.compact = options.notation === "compact";
      this.digits = options.maximumSignificantDigits;
    }

    format(value) {
      const n = Number(value);
      if (!this.compact || !Number.isFinite(n)) return String(n);
      const round = (x) => {
        if (this.digits) return Number(x.toPrecision(this.digits));
        return x < 100 ? Number(x.toPrecision(2)) : Math.round(x);
      };
      const units = ["", "K", "M", "B", "T"];
      let unit = 0;
      let x = Math.abs(n);
      while (unit < units.length - 1 && x >= 1000) {
        x /= 1000;
        unit += 1;
      }
      let rounded = round(x);
      if (rounded >= 1000 && unit < units.length - 1) {
        rounded = round(rounded / 1000);
        unit += 1;
      }
      return `${n < 0 ? "-" : ""}${rounded}${units[unit]}`;
    }
  }

  globalThis.Intl = { NumberFormat };

  // String.prototype.localeCompare as V8 orders under ICU's root collation,
  // approximated (QuickJS's own compares code units, so Fig's
  // `sortFilesAlphabetically` would put `My Documents/` before
  // `fixture-dir/`). It is the parser's port (`locale_compare` in
  // `parser.rs`), exact for ASCII: first by base character, with control
  // characters ignored, punctuation and symbols, then digits, then letters,
  // case and accents aside (other characters after the letters, by code
  // point, their accents stripped through NFD); then by accent; then
  // lowercase before uppercase. The locales are ignored, and of the options
  // only `sensitivity: "base"` and `"accent"`.
  const ORDER = "\t\n\v\f\r _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789";
  const strip = (text) => {
    try {
      return text.normalize("NFD").replace(/[\u0300-\u036f]/g, "");
    } catch (_) {
      return text;
    }
  };
  const weight = (c) => {
    const code = c.codePointAt(0);
    if (code < 0x80) {
      const at = ORDER.indexOf(c);
      if (at >= 0) return at + 1;
      const lower = c.toLowerCase();
      if (lower >= "a" && lower <= "z") return ORDER.length + 1 + lower.charCodeAt(0) - 97;
      return 0;
    }
    return ORDER.length + 27 + c.toLowerCase().codePointAt(0);
  };
  const keys = (text) => {
    const bases = [];
    const upper = [];
    for (const c of strip(text)) {
      const w = weight(c);
      if (w === 0) continue;
      bases.push(w);
      upper.push(c !== c.toLowerCase() ? 1 : 0);
    }
    return { bases, upper };
  };
  const compare = (a, b) => {
    for (let i = 0; i < Math.min(a.length, b.length); i += 1) {
      if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
    }
    return a.length === b.length ? 0 : a.length < b.length ? -1 : 1;
  };
  Object.defineProperty(String.prototype, "localeCompare", {
    value: function localeCompare(that, locales, options) {
      const a = String(this);
      const b = String(that);
      const x = keys(a);
      const y = keys(b);
      const bases = compare(x.bases, y.bases);
      if (bases !== 0) return bases;
      const sensitivity = options && options.sensitivity;
      if (sensitivity === "base") return 0;
      const lower = (text) => {
        try {
          return text.normalize("NFD").toLowerCase();
        } catch (_) {
          return text.toLowerCase();
        }
      };
      const la = lower(a);
      const lb = lower(b);
      if (la !== lb && strip(la) === strip(lb)) return la < lb ? -1 : 1;
      if (sensitivity === "accent") return 0;
      return compare(x.upper, y.upper);
    },
    writable: true,
    configurable: true,
  });

  return function drain() {
    const now = Date.now();
    const due = [...timers]
      .filter(([, timer]) => timer.at <= now)
      .sort(([a, x], [b, y]) => x.at - y.at || a - b);
    for (const [id, timer] of due) {
      timers.delete(id);
      try {
        timer.callback(...timer.args);
      } catch (error) {
        console.debug(`a timer threw: ${error}`);
      }
    }
    if (due.length > 0) return 0;
    let next = -1;
    for (const timer of timers.values()) {
      const wait = Math.max(1, timer.at - now);
      if (next < 0 || wait < next) next = wait;
    }
    return next;
  };
})()
