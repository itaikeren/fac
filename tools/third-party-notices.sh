#!/bin/sh
# Regenerates THIRD_PARTY_NOTICES.md (needs `cargo install --locked cargo-about --features cli`); run from the repo root.
{ cat tools/third-party-notices/header.md; cargo about generate -c tools/third-party-notices/about.toml tools/third-party-notices/about.hbs; } > THIRD_PARTY_NOTICES.md
