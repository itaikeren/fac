//! Fig's completion specs: the spec model, the archive fac embeds, and the
//! spec thread that loads them and runs their generators. The archive is
//! built by `fac-pack-specs` (see `tools/fig-specs/`).

pub mod archive;
pub mod cache;
pub mod generated;
pub mod generators;
pub mod model;
pub mod needs;
pub mod parser;
pub mod process;
pub mod runtime;
pub mod starter;
pub mod user;

#[cfg(test)]
mod generator_tests;

/// The walker that numbers a spec's functions; see the contract at the top of
/// the file. Evaluating it as a script yields the `walk` function.
pub const WALK_JS: &str = include_str!("walk.js");

/// Fig's diff-versioned spec resolution, ported; evaluating it as a script
/// yields `getVersionFromVersionedSpec`.
pub const VERSIONS_JS: &str = include_str!("versions.js");
