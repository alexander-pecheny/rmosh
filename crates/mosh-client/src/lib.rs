//! The mosh client: renders the screen and predicts what the server will echo.

#![forbid(unsafe_code)]

pub mod overlay;
pub mod prediction;
pub mod session;

pub use session::{Event, Handle, Options, Session};
