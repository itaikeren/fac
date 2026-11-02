// Runs Fig's own argument parser (aws/amazon-q-developer-cli v1.10.1,
// packages/autocomplete-parser) and its suggestion list
// (packages/autocomplete/src/suggestions/index.ts, getAllSuggestions) over the
// lines below, against the compiled specs of @withfig/autocomplete, and prints
// one JSON summary per line. `src/specs/parser.rs` compares fac's parser with
// that output (tests/fixtures/fig-parse/expected.json); the tests never run
// node.
//
// Re-run (all paths outside the repo, e.g. in a scratch directory):
//
//   git clone --depth 1 --branch v1.10.1 https://github.com/aws/amazon-q-developer-cli <aq>
//   (cd <aq> && pnpm install --filter "@aws/amazon-q-developer-cli-autocomplete-parser..." --ignore-scripts)
//   npm install --prefix <tools> esbuild
//   npm pack @withfig/autocomplete@2.692.3 && tar -xzf withfig-autocomplete-2.692.3.tgz -C <specs-dir>
//   node tests/fixtures/fig-parse/make-expected.mjs \
//     --aq <aq> --specs <specs-dir>/package/build --tools <tools> \
//     > tests/fixtures/fig-parse/expected.json
//
// Shell commands that specs run (aliases, generateSpec) are stubbed to fail
// with status 1 and empty output; the CLI version of a versioned spec is
// unknown, as in fac. The shell's cwd is /tmp.

import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { createRequire } from "node:module";
import path from "node:path";
import { pathToFileURL } from "node:url";

// The lines, typed as they would be at the prompt with the cursor at the end.
export const LINES = [
  "git ",
  "git checkout -b ",
  "git checkout -b -",
  "git -C dir sta",
  "git commit -m \"msg\" --am",
  "git commit --amend --am",
  "git log --format=",
  "git push --force-with-lease=ma",
  "git foo ",
  "ls -la ",
  "ls -la",
  "ls -",
  "tar -xzf ",
  "tar -xzf",
  "tar --force-local ",
  "docker run --rm -it ",
  "npm run ",
  "sudo git ch",
  "sudo -u root git ch",
  "time git ",
  "kubectl get pods -n ",
  "aws s3 ls ",
  "rm a b ",
  "rm -- -x",
  "rm -- -x ",
  "chown -H ",
  "who -q ",
  "fold file ",
  "yalc ",
  "cd a b ",
  "cat - ",
];

function args() {
  const out = {};
  const argv = process.argv.slice(2);
  for (let i = 0; i < argv.length; i += 2) {
    out[argv[i].replace(/^--/, "")] = argv[i + 1];
  }
  for (const key of ["aq", "specs", "tools"]) {
    if (!out[key]) {
      throw new Error(`missing --${key}`);
    }
  }
  return out;
}

async function main() {
  const { aq, specs, tools } = args();
  const esbuild = createRequire(path.join(path.resolve(tools), "x.js"))("esbuild");
  const packages = path.join(path.resolve(aq), "packages");
  const parserSrc = path.join(packages, "autocomplete-parser/src");
  const build = path.resolve(specs);

  const stubs = {
    "@aws/amazon-q-developer-cli-api-bindings-wrappers": `
      export const SETTINGS = new Proxy({}, { get: (_, key) => String(key) });
      export const getSetting = (_key, fallback) => fallback;
      export const isInDevMode = () => false;
      export const executeCommand = async () => ({ stdout: "", stderr: "", status: 1 });
      export const executeLoginShell = async () => "";
      export const fread = async () => { throw new Error("no local files"); };
    `,
    "@aws/amazon-q-developer-cli-api-bindings": `
      export const Settings = { set: async () => {} };
    `,
    // Specs come from the npm package's build directory instead of a CDN.
    loadHelpers: `
      import { readFileSync } from "node:fs";
      import { pathToFileURL } from "node:url";
      const BUILD = ${JSON.stringify(build)};
      const index = JSON.parse(readFileSync(BUILD + "/index.json", "utf8"));
      const completions = new Set(index.completions);
      const diffVersioned = new Set(index.diffVersionedCompletions);
      export const importFromPublicCDN = (name) => import(pathToFileURL(BUILD + "/" + name + ".js").href);
      export const publicSpecExists = async (name) => completions.has(name);
      export const isDiffVersionedSpec = async (name) => diffVersioned.has(name);
      export const importSpecFromFile = async () => { throw new Error("no local specs"); };
      export const importFromLocalhost = async () => { throw new Error("no dev server"); };
      export const getVersionFromFullFile = async () => undefined;
      export const getCachedCLIVersion = () => null;
    `,
  };

  const resolver = {
    name: "fig-parse",
    setup(b) {
      b.onResolve({ filter: /.*/ }, (a) => {
        if (stubs[a.path] !== undefined) {
          return { path: a.path, namespace: "stub" };
        }
        if (a.path === "./loadHelpers.js") {
          return { path: "loadHelpers", namespace: "stub" };
        }
        const shared = a.path.match(/^@aws\/amazon-q-developer-cli-shared\/(.*)$/);
        if (shared) {
          return { path: path.join(packages, "shared/src", `${shared[1]}.ts`) };
        }
        const local = {
          "@aws/amazon-q-developer-cli-shell-parser": "shell-parser/src/index.ts",
          "@aws/amazon-q-developer-cli-autocomplete-parser": "autocomplete-parser/src/index.ts",
          "@aws/amazon-q-developer-cli-fuzzysort": "fuzzysort/fuzzysort.js",
        }[a.path];
        if (local) {
          return { path: path.join(packages, local) };
        }
        // TypeScript sources import siblings as ".js".
        if (a.path.startsWith(".") && a.path.endsWith(".js")) {
          const ts = path.join(a.resolveDir, a.path.replace(/\.js$/, ".ts"));
          if (existsSync(ts)) {
            return { path: ts };
          }
        }
        return undefined;
      });
      b.onLoad({ filter: /.*/, namespace: "stub" }, (a) => ({
        contents: stubs[a.path],
        loader: "js",
        resolveDir: parserSrc,
      }));
    },
  };

  const entry = `
    import logger from "loglevel";
    import { parseArguments } from ${JSON.stringify(path.join(parserSrc, "index.ts"))};
    import { getCommand } from "@aws/amazon-q-developer-cli-shell-parser";
    import { getAllSuggestions } from ${JSON.stringify(path.join(packages, "autocomplete/src/suggestions/index.ts"))};
    logger.setLevel("silent");
    export async function run(line) {
      const command = getCommand(line, {}, line.length);
      const context = {
        currentWorkingDirectory: "/tmp",
        currentProcess: "zsh",
        sshPrefix: "",
        environmentVariables: {},
      };
      const result = await parseArguments(command, context);
      const suggestions = getAllSuggestions(
        result.currentArg,
        result.completionObj,
        result.passedOptions,
        result.suggestionFlags,
        [],
        result.annotations,
      );
      return { result, suggestions };
    }
  `;

  const out = await esbuild.build({
    stdin: { contents: entry, resolveDir: parserSrc, loader: "ts", sourcefile: "entry.ts" },
    bundle: true,
    write: false,
    format: "esm",
    platform: "node",
    logLevel: "silent",
    nodePaths: [path.join(packages, "autocomplete-parser/node_modules")],
    banner: { js: "globalThis.window = { location: { protocol: 'http:' }, fig: {} };" },
    plugins: [resolver],
  });
  const dir = mkdtempSync(path.join(tmpdir(), "fig-parse-"));
  const file = path.join(dir, "bundle.mjs");
  writeFileSync(file, out.outputFiles[0].text);
  let run;
  try {
    ({ run } = await import(pathToFileURL(file).href));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }

  const names = (name) => (Array.isArray(name) ? name : name === undefined ? [] : [name]);
  const cases = [];
  for (const line of LINES) {
    try {
      const { result, suggestions } = await run(line);
      cases.push({
        line,
        arg: result.currentArg ? (result.currentArg.name ?? "") : null,
        search: result.searchTerm,
        suggestions: suggestions.map((s) => {
          const item = { type: s.type ?? null, names: names(s.name) };
          if (s.priority !== undefined) item.priority = s.priority;
          if (s.insertValue !== undefined) item.insertValue = s.insertValue;
          if (typeof s.getQueryTerm === "function") item.query = s.getQueryTerm(result.searchTerm);
          if (s.hidden) item.hidden = true;
          return item;
        }),
      });
    } catch (err) {
      cases.push({ line, error: String(err?.name ?? err) });
    }
  }
  process.stdout.write(`${JSON.stringify({ parser: "aws/amazon-q-developer-cli v1.10.1", specs: "@withfig/autocomplete 2.692.3", cases }, null, 1)}\n`);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}
