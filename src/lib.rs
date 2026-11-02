//! fac (fast auto complete) is a Fig-style autocomplete for the macOS terminal.
//! It wraps the user's shell in a pseudo-terminal and draws a completion popup
//! inside the terminal with ANSI sequences, so it works the same in every
//! terminal emulator.

pub mod complete;
pub mod config;
pub mod guards;
pub mod init;
pub mod logging;
pub mod popup;
pub mod proxy;
pub mod pty;
pub mod shell;
pub mod specs;
pub mod term;
pub mod wake;
