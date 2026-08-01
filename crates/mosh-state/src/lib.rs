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

/// Both states plug into the transport through the same trait, which is what lets one
/// sender implementation carry either direction.
mod sync_impls {
    use mosh_network::sender::SyncState;
    use mosh_network::StateError;

    use crate::{Complete, UserStream};

    impl SyncState for UserStream {
        fn subtract(&mut self, prefix: &Self) {
            UserStream::subtract(self, prefix);
        }
        fn diff_from(&self, existing: &Self) -> Vec<u8> {
            UserStream::diff_from(self, existing)
        }
        fn init_diff(&self) -> Vec<u8> {
            UserStream::init_diff(self)
        }
        fn apply_string(&mut self, diff: &[u8]) -> Result<(), StateError> {
            UserStream::apply_string(self, diff).map_err(|_| StateError)
        }
    }

    impl SyncState for Complete {
        /// The screen has no prefix to drop; it is always a whole state.
        fn subtract(&mut self, _prefix: &Self) {}
        fn diff_from(&self, existing: &Self) -> Vec<u8> {
            Complete::diff_from(self, existing)
        }
        fn init_diff(&self) -> Vec<u8> {
            Complete::init_diff(self)
        }
        fn apply_string(&mut self, diff: &[u8]) -> Result<(), StateError> {
            Complete::apply_string(self, diff).map_err(|_| StateError)
        }
        fn compare(&self, other: &Self) -> bool {
            Complete::compare(self, other)
        }
    }
}
