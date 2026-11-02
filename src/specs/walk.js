// The spec walker, shared by the packer (`fac-pack-specs`) and the spec thread,
// so both number a spec's functions the same way. Evaluating this file as a
// script yields the `walk` function.
//
// walk(value) -> {json, fns, refs}
// walk(value, prefix, first) -> {json, fns, refs}
// walk(value, prefix, first, false) -> {json: null, fns, refs}
//
// Numbering contract:
// - The walk is depth first. Object properties are visited in the order
//   `Object.keys` gives them (insertion order, integer-like keys first), array
//   elements in index order.
// - A path is the dotted list of property names and array indices from the
//   root to a value, e.g. "subcommands.3.args.0.generators.1.postProcess".
//   Array segments are indices into the original array. The root's path is "".
// - Each function is replaced by {"$fn": n}, where n counts the functions met
//   so far, from 0. fns[n] is that function's path.
// - With `prefix` and `first`, paths start with `prefix.` and n starts at
//   `first` (fns[n - first] is the path). A version file is walked twice:
//   walk(default), then walk(versions, "versions", fns.length) for its
//   `versions` diff table; the entry's fns is the two lists joined, so ids
//   are unique across the module.
// - A property or array element whose value is undefined is dropped.
// - An object reached by two paths is copied at each one; its functions get
//   one id per path.
// - Only plain objects, arrays, strings, finite numbers, booleans, null and
//   functions are accepted. Anything else, or a cycle, throws.
//
// json is the resulting tree as JSON text, or null when the fourth argument
// is false (the spec thread needs only the functions of a spec whose tree it
// already has). refs[n - first] is [function, owner] for fns[n - first]: the
// live function and the object or array that holds it (globalThis for a root
// function), so the spec thread can call it with `this` bound as Fig does.
(function walk(value, prefix = "", first = 0, json = true) {
  const fns = [];
  const refs = [];
  const stack = new Set();
  // The keys from the root to the value being visited; a path is built from
  // them only when one is needed.
  const keys = [];
  const path = () => keys.reduce((at, key) => (at === "" ? String(key) : `${at}.${key}`), prefix);
  function visit(v, owner) {
    switch (typeof v) {
      case "function":
        fns.push(path());
        refs.push([v, owner]);
        return { $fn: first + fns.length - 1 };
      case "string":
      case "boolean":
        return v;
      case "number":
        if (!Number.isFinite(v)) throw new Error(`non-finite number at "${path()}"`);
        return v;
      case "object":
        break;
      default:
        throw new Error(`unsupported ${typeof v} at "${path()}"`);
    }
    if (v === null) return null;
    if (stack.has(v)) throw new Error(`cycle at "${path()}"`);
    stack.add(v);
    let out = null;
    if (Array.isArray(v)) {
      if (json) out = [];
      v.forEach((item, index) => {
        if (item === undefined) return;
        keys.push(index);
        const visited = visit(item, v);
        keys.pop();
        if (json) out.push(visited);
      });
    } else {
      const proto = Object.getPrototypeOf(v);
      if (proto !== Object.prototype && proto !== null) {
        throw new Error(`unsupported object at "${path()}"`);
      }
      if (json) out = {};
      for (const key of Object.keys(v)) {
        const item = v[key];
        if (item === undefined) continue;
        keys.push(key);
        const visited = visit(item, v);
        keys.pop();
        if (json) out[key] = visited;
      }
    }
    stack.delete(v);
    return out;
  }
  const tree = visit(value, globalThis);
  return { json: json ? JSON.stringify(tree) : null, fns, refs };
})
