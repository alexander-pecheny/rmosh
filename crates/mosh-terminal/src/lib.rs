//! Terminal emulation: escape-sequence parsing, screen state, and frame generation.

#![forbid(unsafe_code)]

pub mod dispatcher;
pub mod display;
pub mod emulator;
pub mod framebuffer;
pub mod parser;
