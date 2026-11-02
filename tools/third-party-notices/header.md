# Third-party notices

fac includes or is built with the following third-party software. The first sections cover code and data that fac ports or embeds; the last section, generated from the dependency tree, covers every Rust crate compiled into the release binary.

## withfig/autocomplete

Fig's completion specs, `@withfig/autocomplete` 2.692.3, packed into `specs/fig.zst` (see `vendor/fig-autocomplete/VERSION`). MIT License; the full text is in `vendor/fig-autocomplete/LICENSE`:

```
MIT License

Copyright (c) 2021 Hercules Labs Inc. (Fig)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## withfig/autocomplete-tools

`src/specs/versions.js` is a port of `getVersionFromVersionedSpec` and the spec diff merge from `helpers/src/versions.ts` in https://github.com/withfig/autocomplete-tools (`@fig/autocomplete-helpers`).

`src/specs/autocomplete-generators.js`, the `@fig/autocomplete-generators` module a user spec can import, is a port to plain JavaScript of `@fig/autocomplete-generators` 2.4.0 (`generators/index.ts` and `generators/src/{filepaths,keyvalue,resolve,ai}.ts` in the same repository).

`src/specs/fig.d.ts`, which `fac spec new` writes next to a TypeScript spec for the editor, holds `@withfig/autocomplete-types` 1.31.0 (`types/index.d.ts` in the same repository) unchanged, followed by the types of `@fig/autocomplete-generators` written from `generators/src/*.ts`.

All three are under the MIT License:

```
MIT License

Copyright (c) 2021 Hercules Labs Inc. (Fig)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## aws/amazon-q-developer-cli

These fac files port Fig or Amazon Q logic, with the file and line references in their comments:

- `src/specs/parser.rs`: Fig's argument parser, `packages/autocomplete-parser/src/parseArguments.ts`, and the parse-dependent part of `getAllSuggestions` in `packages/autocomplete/src/suggestions/index.ts`, from https://github.com/aws/amazon-q-developer-cli at tag v1.10.1.
- `src/specs/needs.rs`: which parts of a spec a command line needs (from `parseArguments.ts`).
- `src/specs/generated.rs`: when generators run and what they add to the list (`triggerGenerators` in `packages/autocomplete/src/state/generators.ts`, and `suggestions/index.ts`).
- `src/specs/generators.rs`, and the generator and version-detection parts of `src/specs/runtime.rs`, `src/specs/process.rs` and `src/specs/exec.js`: Fig's generator logic from `packages/autocomplete/src/generators/` (script, custom and template generators, `helpers.ts` and the generator cache in `cache.ts`), `triggerGenerator` in `state/generators.ts`, `executeCommandTimeout` and `cleanOutput` in `packages/api-bindings-wrappers/src/executeCommand.ts`, `getCWDForFilesAndFolders` in `packages/shared/src/utils.ts`, and the CLI version detection in `packages/autocomplete-parser/src/loadHelpers.ts` and `tryResolveSpecToSubcommand.ts`.
- `src/complete/engine.rs`: how spec items are matched, labelled, ranked and inserted (`suggestions/index.ts` and `helpers.ts`, `components/Suggestion.tsx`, `state/insertion.ts`, `state/index.ts`, and `updatePriorities` in `sorting.ts`).
- `src/complete/usage.rs`: the recency index (`sorting.ts`).
- `src/popup/layout.rs` and `src/popup/state.rs`: the description line (`Description.tsx`) and the popup's reaction to keystrokes (`state/insertion.ts`).

The sources are https://github.com/aws/amazon-q-developer-cli (tag v1.10.1, for the parser) and https://github.com/aws/amazon-q-developer-cli-autocomplete (tag v1.19.7, for the rest).

They are licensed MIT OR Apache-2.0 and used here under the MIT License:

```
MIT License

Copyright (c) 2024 Amazon.com, Inc. or its affiliates.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## oxc

The JavaScript and TypeScript parser, semantic analysis, transformer and code generator (the `oxc_*` crates, https://github.com/oxc-project/oxc), which fac uses to turn a user's TypeScript spec into JavaScript. MIT License; the crates carry no licence file, so the licence is given in the generated section below, and the crates' authors are Boshen and the Oxc contributors.

## nucleo-matcher

The fuzzy matcher, `nucleo-matcher` 0.3.1 (https://github.com/helix-editor/nucleo), is licensed MPL-2.0. fac uses it unmodified, as a Rust dependency. Its source is available at https://github.com/helix-editor/nucleo and at https://crates.io/crates/nucleo-matcher/0.3.1. The full licence text is in the generated section below.

## rquickjs

Rust bindings to QuickJS, https://github.com/DelSkayn/rquickjs. MIT License:

```
MIT License

Copyright (c) 2020 Mees Delzenne
Copyright (c) 2025 Rquickjs Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## QuickJS-NG

The JavaScript engine bundled by rquickjs, https://github.com/quickjs-ng/quickjs. MIT License:

```
The MIT License (MIT)

Copyright (c) 2017-2026 Fabrice Bellard
Copyright (c) 2017-2024 Charlie Gordon
Copyright (c) 2023-2026 Ben Noordhuis
Copyright (c) 2023-2026 Saúl Ibarra Corretgé

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

## Zstandard (zstd)

The Zstandard compression library, https://github.com/facebook/zstd, built through the `zstd` crates (https://github.com/gyscos/zstd-rs, BSD 3-Clause, Copyright (c) 2026, Alexandre Bury). BSD License:

```
BSD License

For Zstandard software

Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.

Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:

 * Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

 * Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

 * Neither the name Facebook, nor Meta, nor the names of its contributors may
   be used to endorse or promote products derived from this software without
   specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR
ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON
ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

