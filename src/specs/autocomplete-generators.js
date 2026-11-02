// `@fig/autocomplete-generators`, the module a user spec imports Fig's
// generator helpers from: `import { filepaths } from
// "@fig/autocomplete-generators"`. A port to plain JavaScript of
// `generators/index.ts` and `generators/src/{filepaths,keyvalue,resolve}.ts`
// of @fig/autocomplete-generators 2.4.0, from
// https://github.com/withfig/autocomplete-tools (MIT, see
// THIRD_PARTY_NOTICES.md). The behaviour is Fig's; only the types are gone.
// `ai` asked Fig's servers, which are gone: it gives no suggestions.

// generators/src/resolve.ts

const ensureTrailingSlash = (str) => (str.endsWith("/") ? str : `${str}/`);

const replaceTilde = (path, homeDir) => {
  if (path.startsWith("~") && (path.length === 1 || path.charAt(1) === "/")) {
    return path.replace("~", homeDir);
  }
  return path;
};

const replaceVariables = (path, environmentVariables) => {
  // Replace simple $VAR variables
  const resolvedSimpleVariables = path.replace(/\$([A-Za-z0-9_]+)/g, (key) => {
    const envKey = key.slice(1);
    return environmentVariables[envKey] ?? key;
  });

  // Replace complex ${VAR} variables
  const resolvedComplexVariables = resolvedSimpleVariables.replace(
    /\$\{([A-Za-z0-9_]+)(?::-([^}]+))?\}/g,
    (match, envKey, defaultValue) => environmentVariables[envKey] ?? defaultValue ?? match
  );

  return resolvedComplexVariables;
};

const shellExpand = (path, context) => {
  const { environmentVariables } = context;
  return replaceVariables(
    replaceTilde(path, environmentVariables?.HOME ?? "~"),
    environmentVariables
  );
};

// generators/src/filepaths.ts

function sortFilesAlphabetically(array, skip = []) {
  const skipLower = skip.map((str) => str.toLowerCase());
  const results = array.filter((x) => !skipLower.includes(x.toLowerCase()));

  // Put all files beginning with . after all those that don't, sort alphabetically within each.
  return [
    ...results.filter((x) => !x.startsWith(".")).sort((a, b) => a.localeCompare(b)),
    ...results.filter((x) => x.startsWith(".")).sort((a, b) => a.localeCompare(b)),
    "../",
  ];
}

const getCurrentInsertedDirectory = (cwd, searchTerm, context) => {
  if (cwd === null) return "/";

  const resolvedPath = shellExpand(searchTerm, context);

  const dirname = resolvedPath.slice(0, resolvedPath.lastIndexOf("/") + 1);

  if (dirname === "") {
    return ensureTrailingSlash(cwd);
  }

  return dirname.startsWith("/") ? dirname : `${ensureTrailingSlash(cwd)}${dirname}`;
};

function filepathsFn(options = {}) {
  const {
    extensions = [],
    equals = [],
    matches,
    filterFolders = false,
    editFileSuggestions,
    editFolderSuggestions,
    rootDirectory,
    showFolders = "always",
  } = options;
  const extensionsSet = new Set(extensions);
  const equalsSet = new Set(equals);

  // If no filter is provided the suggestions are not filtered.
  const shouldFilterSuggestions = () => extensions.length > 0 || equals.length > 0 || matches;

  const filterSuggestions = (suggestions = []) => {
    if (!shouldFilterSuggestions()) return suggestions;

    return suggestions.filter(({ name = "", type }) => {
      if (!filterFolders && type === "folder") return true;

      if (equalsSet.has(name)) return true;
      if (matches && !!name.match(matches)) return true;
      // handle extensions
      const [, ...suggestionExtensions] = name.split(".");
      if (suggestionExtensions.length >= 1) {
        let i = suggestionExtensions.length - 1;
        let stackedExtensions = suggestionExtensions[i];
        do {
          if (extensionsSet.has(stackedExtensions)) {
            return true;
          }
          i -= 1;
          // `i` may become -1, which is not a valid index, but the check at
          // the top of the loop does not run then.
          stackedExtensions = [suggestionExtensions[i], stackedExtensions].join(".");
        } while (i >= 0);
      }
      return false;
    });
  };

  const postProcessSuggestions = (suggestions = []) => {
    if (!editFileSuggestions && !editFolderSuggestions) return suggestions;

    return suggestions.map((suggestion) => ({
      ...suggestion,
      ...((suggestion.type === "file" ? editFileSuggestions : editFolderSuggestions) || {}),
    }));
  };

  return {
    trigger: (oldToken, newToken) => {
      const oldLastSlashIndex = oldToken.lastIndexOf("/");
      const newLastSlashIndex = newToken.lastIndexOf("/");
      // If the final path segment has changed, trigger new suggestions
      if (oldLastSlashIndex !== newLastSlashIndex) {
        return true;
      }
      // No slashes: no new suggestions on each keystroke.
      if (oldLastSlashIndex === -1 && newLastSlashIndex === -1) {
        return false;
      }
      // There is a slash: trigger if anything before the final one changed.
      return oldToken.slice(0, oldLastSlashIndex) !== newToken.slice(0, newLastSlashIndex);
    },
    getQueryTerm: (token) => token.slice(token.lastIndexOf("/") + 1),

    custom: async (_, executeCommand, generatorContext) => {
      const { isDangerous, currentWorkingDirectory, searchTerm } = generatorContext;
      const currentInsertedDirectory =
        getCurrentInsertedDirectory(
          rootDirectory ?? currentWorkingDirectory,
          searchTerm,
          generatorContext
        ) ?? "/";

      try {
        const data = await executeCommand({
          command: "ls",
          args: ["-1ApL"],
          cwd: currentInsertedDirectory,
        });
        const sortedFiles = sortFilesAlphabetically(data.stdout.split("\n"), [".DS_Store"]);

        const generatorOutputArray = [];
        for (const name of sortedFiles) {
          if (name) {
            const templateType = name.endsWith("/") ? "folders" : "filepaths";
            if (
              (templateType === "filepaths" && showFolders !== "only") ||
              (templateType === "folders" && showFolders !== "never")
            ) {
              generatorOutputArray.push({
                type: templateType === "filepaths" ? "file" : "folder",
                name,
                insertValue: name,
                isDangerous,
                context: { templateType },
              });
            }
          }
        }

        return postProcessSuggestions(filterSuggestions(generatorOutputArray));
      } catch (err) {
        return [];
      }
    },
  };
}

export const folders = Object.assign(
  () => filepathsFn({ showFolders: "only" }),
  Object.freeze(filepathsFn({ showFolders: "only" }))
);
export const filepaths = Object.assign(filepathsFn, Object.freeze(filepathsFn()));

// generators/src/keyvalue.ts

/** Cache of suggestions, by the string[], Suggestion[] or function given. */
const suggestionCache = new Map();

function appendToInsertValue(append, suggestions) {
  if (append.length === 0) {
    return suggestions;
  }
  return suggestions.map((item) =>
    item.insertValue ? item : { ...item, insertValue: item.name + append }
  );
}

async function kvSuggestionsToFigSuggestions(suggestions, append, init) {
  if (typeof suggestions === "function") {
    const out = await suggestions(...init);
    return appendToInsertValue(append, out);
  }
  if (typeof suggestions[0] === "string") {
    const out = suggestions.map((name) => ({ name }));
    return appendToInsertValue(append, out);
  }
  return appendToInsertValue(append, suggestions);
}

async function getSuggestions(suggestions, append, useSuggestionCache, init) {
  if (useSuggestionCache || Array.isArray(suggestions)) {
    let value = suggestionCache.get(suggestions);
    if (value === undefined) {
      value = await kvSuggestionsToFigSuggestions(suggestions, append, init);
      suggestionCache.set(suggestions, value);
    }
    return value;
  }
  return kvSuggestionsToFigSuggestions(suggestions, append, init);
}

function shouldUseCache(isKey, cache) {
  if (typeof cache === "string") {
    return (isKey && cache === "keys") || (!isKey && cache === "values");
  }
  return cache;
}

/** Get the final index of any of the strings */
function lastIndexOf(haystack, ...needles) {
  return Math.max(...needles.map((needle) => haystack.lastIndexOf(needle)));
}

function removeRepeatSuggestions(alreadyUsed, suggestions) {
  const seen = new Set(alreadyUsed);
  return suggestions.filter((suggestion) => {
    if (typeof suggestion.name === "string") {
      return !seen.has(suggestion.name);
    }
    return !suggestion.name?.some((name) => seen.has(name));
  });
}

/** Suggestions for `val,val,...` arguments. */
export function valueList({
  delimiter = ",",
  values = [],
  cache = false,
  insertDelimiter = false,
  allowRepeatedValues = false,
}) {
  return {
    trigger: (newToken, oldToken) =>
      newToken.lastIndexOf(delimiter) !== oldToken.lastIndexOf(delimiter),

    getQueryTerm: (token) => token.slice(token.lastIndexOf(delimiter) + delimiter.length),

    custom: async (...init) => {
      const out = await getSuggestions(values, insertDelimiter ? delimiter : "", cache, init);
      if (allowRepeatedValues) {
        return out;
      }
      const [tokens] = init;
      const valuesInList = tokens[tokens.length - 1]?.split(delimiter);
      return removeRepeatSuggestions(valuesInList, out);
    },
  };
}

/** Suggestions for `key=value` arguments. */
export function keyValue({
  separator = "=",
  keys = [],
  values = [],
  cache = false,
  insertSeparator = true,
}) {
  return {
    trigger: (newToken, oldToken) => newToken.indexOf(separator) !== oldToken.indexOf(separator),
    getQueryTerm: (token) => token.slice(token.indexOf(separator) + 1),
    custom: async (...init) => {
      const [tokens] = init;
      const finalToken = tokens[tokens.length - 1];
      const isKey = !finalToken.includes(separator);
      const suggestions = isKey ? keys : values;
      const useCache = shouldUseCache(isKey, cache);
      const append = isKey ? (insertSeparator ? separator : "") : "";
      return getSuggestions(suggestions, append, useCache, init);
    },
  };
}

/** Suggestions for `k=v,k=v,...` arguments. */
export function keyValueList({
  separator = "=",
  delimiter = ",",
  keys = [],
  values = [],
  cache = false,
  insertSeparator = true,
  insertDelimiter = false,
  allowRepeatedKeys = false,
  allowRepeatedValues = true,
}) {
  return {
    trigger: (newToken, oldToken) => {
      const newTokenIdx = lastIndexOf(newToken, separator, delimiter);
      const oldTokenIdx = lastIndexOf(oldToken, separator, delimiter);
      return newTokenIdx !== oldTokenIdx;
    },
    getQueryTerm: (token) => {
      const index = lastIndexOf(token, separator, delimiter);
      return token.slice(index + 1);
    },
    custom: async (...init) => {
      const [tokens] = init;

      const finalToken = tokens[tokens.length - 1];
      const index = lastIndexOf(finalToken, separator, delimiter);
      const isKey = index === -1 || finalToken.slice(index, index + separator.length) !== separator;

      const suggestions = isKey ? keys : values;
      const useCache = shouldUseCache(isKey, cache);
      const append = isKey ? (insertSeparator ? separator : "") : insertDelimiter ? delimiter : "";
      const out = await getSuggestions(suggestions, append, useCache, init);

      if (isKey) {
        if (allowRepeatedKeys) {
          return out;
        }
        const existingKeys = finalToken
          .split(delimiter)
          .map((chunk) => chunk.slice(0, chunk.indexOf(separator)));
        return removeRepeatSuggestions(existingKeys, out);
      }

      if (allowRepeatedValues) {
        return out;
      }
      const existingValues = finalToken
        .split(delimiter)
        .map((chunk) => chunk.slice(chunk.indexOf(separator) + separator.length));
      return removeRepeatSuggestions(existingValues, out);
    },
  };
}

// generators/src/ai.ts: Fig's AI service is gone, so the generator it makes
// gives no suggestions.

export function ai() {
  return {
    scriptTimeout: 15000,
    custom: async () => [],
  };
}
