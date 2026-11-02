//! The completion popup, drawn into the terminal with escape sequences below
//! the cursor and erased by repainting fac's screen model. Everything here
//! produces bytes; the proxy writes them. `state` decides when the popup is
//! shown and what the keys do.

pub mod color;
pub mod layout;
pub mod palette;
pub mod render;
pub mod state;
pub mod text;
pub mod theme;
