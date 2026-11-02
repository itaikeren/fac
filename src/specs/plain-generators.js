// Run on a user spec's default export before it is walked. Fig's helpers
// `filepaths` and `folders` (from `@fig/autocomplete-generators`) are
// functions that also carry a generator's fields, so a spec can use them
// called (`filepaths({ extensions: ["ts"] })`) or as they are
// (`generators: filepaths`), and Fig reads the fields either way. The walker
// would see a function, so a generator that is a function with fields of its
// own becomes a plain object of those fields. Evaluating this file as a
// script yields the function; it returns the spec.
(function plainGenerators(spec) {
  const seen = new Set();
  const plain = (generator) =>
    typeof generator === "function" && Object.keys(generator).length > 0
      ? { ...generator }
      : generator;
  function visit(value) {
    if (value === null || typeof value !== "object" || seen.has(value)) return;
    seen.add(value);
    if (Array.isArray(value)) {
      value.forEach(visit);
      return;
    }
    for (const key of Object.keys(value)) {
      if (key === "generators") {
        const generators = value[key];
        const made = Array.isArray(generators) ? generators.map(plain) : plain(generators);
        // A frozen object keeps what it has.
        if (made !== generators && !Object.isFrozen(value)) value[key] = made;
      }
      visit(value[key]);
    }
  }
  visit(spec);
  return spec;
})
