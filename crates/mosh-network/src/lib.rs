//! Datagram framing: compression, fragmentation and reassembly.
//!
//! Everything here is on the wire and shared with C++ endpoints, so the layouts are
//! contractual.

#![forbid(unsafe_code)]

pub mod compressor;
pub mod connection;
pub mod fragment;
pub mod packet;
