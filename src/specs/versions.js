// Fig's diff-versioned specs: getVersionFromVersionedSpec and the diff merge
// it uses, ported from withfig/autocomplete-tools, helpers/src/versions.ts
// (MIT; see THIRD_PARTY_NOTICES.md). Only the merge side is ported: fac never
// computes diffs. Evaluating this file as a script yields
// getVersionFromVersionedSpec.
//
// getVersionFromVersionedSpec(base, versions, target) -> {version, spec}
//
// `versions` maps a version to the diff that turns the spec of the version
// before it into this one. Every diff up to `target` is applied to `base`, in
// version order; without a target, every diff is (the newest version). The
// spec's objects are merged by spreading, so its functions stay live.
(function () {
  // semver.compare for the versions the specs use: the numeric parts in
  // order (a missing part is 0), then a version with a prerelease tag before
  // the same version without one.
  const parse = (version) => {
    const [core, ...pre] = String(version).split("-");
    return { parts: core.split(".").map((part) => Number.parseInt(part, 10) || 0), pre: pre.join("-") };
  };
  const compareVersions = (a, b) => {
    const x = parse(a);
    const y = parse(b);
    const length = Math.max(x.parts.length, y.parts.length);
    for (let i = 0; i < length; i += 1) {
      const diff = (x.parts[i] ?? 0) - (y.parts[i] ?? 0);
      if (diff !== 0) return diff < 0 ? -1 : 1;
    }
    if (x.pre === y.pre) return 0;
    if (x.pre === "") return 1;
    if (y.pre === "") return -1;
    return x.pre < y.pre ? -1 : 1;
  };

  const toArray = (x) => {
    if (!x) {
      return [];
    }
    return Array.isArray(x) ? x : [x];
  };

  const namesMatch = (x, y) => {
    const xNames = new Set(toArray(x.name));
    return toArray(y.name).some((name) => xNames.has(name));
  };

  const mergeSimpleObject = (original, diff) => ({
    ...original,
    ...diff,
  });

  const mergeNamedArrayDiff = (current, diffs, mergeFn) => {
    const updated = [];
    const mergedIndexes = new Set();
    diffs.forEach((diff) => {
      const idx = current.findIndex((curr, i) => !mergedIndexes.has(i) && namesMatch(curr, diff));
      if (idx === -1) {
        updated.push(diff);
      } else {
        mergedIndexes.add(idx);
        if (!("remove" in diff)) {
          const merged = mergeFn(current[idx], diff);
          if (merged !== null) {
            updated.push(merged);
          }
        }
      }
    });
    current.forEach((curr, i) => {
      if (!mergedIndexes.has(i)) {
        updated.push(curr);
      }
    });
    return updated;
  };

  const mergeOrderedArrays = (current, diffs, mergeFn) =>
    diffs
      .filter((diff) => !(typeof diff === "object" && diff && "remove" in diff) || !diff.remove)
      .map((diff, idx) => mergeFn(current[idx], diff))
      .filter((merged) => merged !== null);

  const makeNamedProcessor = (propertyMapping) => ({
    merge: (current, diff) => {
      if (diff === null) {
        return current;
      }
      const merged = { ...current, ...diff };
      for (const key in propertyMapping) {
        const property = propertyMapping[key];
        if (diff[key] && property !== undefined) {
          merged[key] = property.merge(current[key], diff[key]);
        }
      }
      return merged;
    },
  });

  const argArrayProcessor = {
    merge: (c, d) => mergeOrderedArrays(toArray(c), toArray(d), mergeSimpleObject),
  };

  const optionProcessor = makeNamedProcessor({
    args: argArrayProcessor,
  });

  const subcommandProcessor = makeNamedProcessor({
    subcommands: {
      merge: (c, d) => mergeNamedArrayDiff(toArray(c), toArray(d), subcommandProcessor.merge),
    },
    options: {
      merge: (c, d) => mergeNamedArrayDiff(toArray(c), toArray(d), optionProcessor.merge),
    },
    args: argArrayProcessor,
  });

  const getBestVersionIndex = (versions, target) => {
    if (!target) return versions.length - 1;
    // find the last element minor or equal to the target
    for (let i = versions.length - 1; i >= 0; i -= 1) {
      const version = versions[i];
      if (compareVersions(version, target) <= 0) {
        return i;
      }
    }
    return versions.length - 1;
  };

  const applySpecDiff = (spec, diff) => subcommandProcessor.merge(spec, { ...diff, name: spec.name });

  return function getVersionFromVersionedSpec(base, versions, target) {
    const versionNames = Object.keys(versions).sort(compareVersions);
    const versionIndex = getBestVersionIndex(versionNames, target);
    const spec = versionNames
      .slice(0, versionIndex + 1)
      .map((name) => versions[name])
      .reduce(applySpecDiff, base);
    return { spec, version: versionNames[versionIndex] };
  };
})()
