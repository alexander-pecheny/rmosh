//! Terminal emulation: escape-sequence parsing, screen state, and frame generation.

#![forbid(unsafe_code)]

pub mod dispatcher;
pub mod framebuffer;
pub mod parser;
