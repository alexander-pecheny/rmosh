//! One direction of a pipe as a state mosh can synchronise.

use mosh_network::sender::SyncState;
use mosh_network::StateError;

/// What one end has written so far, and how it ended. Every state is a prefix of the
/// next, so a diff is just what was appended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stream {
    pub out: Vec<u8>,
    pub err: Vec<u8>,
    /// The exit status of the command, or 0 for a client whose input ended.
    pub end: Option<i32>,
}

impl Stream {
    /// Bytes not yet known to have reached the peer, once the transport has subtracted
    /// what it acknowledged.
    pub fn backlog(&self) -> usize {
        self.out.len() + self.err.len()
    }
}

fn put(diff: &mut Vec<u8>, bytes: &[u8]) {
    diff.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    diff.extend_from_slice(bytes);
}

fn take<'a>(diff: &mut &'a [u8], len: usize) -> Result<&'a [u8], StateError> {
    if diff.len() < len {
        return Err(StateError);
    }
    let (head, rest) = diff.split_at(len);
    *diff = rest;
    Ok(head)
}

fn take_bytes<'a>(diff: &mut &'a [u8]) -> Result<&'a [u8], StateError> {
    let len = u32::from_le_bytes(take(diff, 4)?.try_into().expect("four bytes"));
    take(diff, len as usize)
}

impl SyncState for Stream {
    fn subtract(&mut self, prefix: &Self) {
        if self.out.starts_with(&prefix.out) {
            self.out.drain(..prefix.out.len());
        }
        if self.err.starts_with(&prefix.err) {
            self.err.drain(..prefix.err.len());
        }
    }

    fn diff_from(&self, existing: &Self) -> Vec<u8> {
        let (out, err) = (&self.out[existing.out.len().min(self.out.len())..], &self.err[existing.err.len().min(self.err.len())..]);
        let end = self.end.filter(|_| existing.end.is_none());
        if out.is_empty() && err.is_empty() && end.is_none() {
            return Vec::new();
        }
        let mut diff = Vec::with_capacity(out.len() + err.len() + 13);
        put(&mut diff, out);
        put(&mut diff, err);
        if let Some(code) = end {
            diff.extend_from_slice(&code.to_le_bytes());
        }
        diff
    }

    fn init_diff(&self) -> Vec<u8> {
        self.diff_from(&Stream::default())
    }

    fn apply_string(&mut self, diff: &[u8]) -> Result<(), StateError> {
        let mut diff = diff;
        let out = take_bytes(&mut diff)?;
        let err = take_bytes(&mut diff)?;
        let end = match diff.len() {
            0 => None,
            4 => Some(i32::from_le_bytes(diff.try_into().expect("four bytes"))),
            _ => return Err(StateError),
        };
        self.out.extend_from_slice(out);
        self.err.extend_from_slice(err);
        self.end = self.end.or(end);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(out: &str, err: &str, end: Option<i32>) -> Stream {
        Stream { out: out.into(), err: err.into(), end }
    }

    #[test]
    fn a_diff_carries_what_was_appended() {
        let old = stream("ab", "", None);
        let new = stream("abcd", "x", Some(3));
        let mut got = old.clone();
        got.apply_string(&new.diff_from(&old)).unwrap();
        assert_eq!(got, new);
    }

    #[test]
    fn nothing_new_is_an_empty_diff() {
        let s = stream("ab", "c", Some(0));
        assert!(s.diff_from(&s).is_empty());
    }

    #[test]
    fn subtracting_the_acknowledged_prefix_leaves_the_backlog() {
        let mut s = stream("abcd", "xy", None);
        s.subtract(&stream("ab", "x", None));
        assert_eq!(s, stream("cd", "y", None));
        assert_eq!(s.backlog(), 3);
    }

    #[test]
    fn a_truncated_diff_is_refused() {
        let diff = stream("abcd", "", None).init_diff();
        assert!(Stream::default().apply_string(&diff[..diff.len() - 1]).is_err());
        assert!(Stream::default().apply_string(&[1, 2]).is_err());
    }
}
