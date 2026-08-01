//! Datagram framing: compression, fragmentation and reassembly.
//!
//! Everything here is on the wire and shared with C++ endpoints, so the layouts are
//! contractual.

#![forbid(unsafe_code)]

pub mod compressor;
pub mod connection;
pub mod fragment;
pub mod packet;
pub mod sender;
pub mod transport;

/// A state diff that could not be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateError;

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("could not apply a state diff")
    }
}

impl std::error::Error for StateError {}
