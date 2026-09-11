//! The mosh launcher, as a library: option handling, the server command line, and the
//! handshake the far end announces. The binary is one caller; an embedder that runs the
//! client in-process is another.

#![forbid(unsafe_code)]

pub mod handshake;
pub mod launch;
pub mod opts;
