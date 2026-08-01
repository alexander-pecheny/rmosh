//! The two states a session keeps in step: the server's screen and the client's
//! keystrokes.
//!
//! Both expose the same shape -- diff against a previous state, apply a diff, compare --
//! which is what lets the transport carry either without knowing which it has.

#![forbid(unsafe_code)]

pub mod complete;
pub mod user;

pub use complete::Complete;
pub use user::{ApplyError, UserEvent, UserStream};
