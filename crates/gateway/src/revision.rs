//! Rule revisions for the contract's `match_revision` and `play_revision`: hashes of what decides
//! a rule's answers, so lineup resyncs a route when its rule's config or the code that interprets
//! it changes.

use std::fmt::Debug;

/// Bump when matching or template semantics change (same config, different identities or refs).
pub const MATCHER_VERSION: u32 = 1;
/// Bump when play URLs change for the same config and ref.
pub const PLAY_VERSION: u32 = 1;

/// FNV-1a 64-bit: stable across builds and platforms (unlike `DefaultHasher`).
pub fn short_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// `match_revision`: the matcher version, the kind and the version of hints its code produces,
/// and the rule's match config (patterns, vars, templates) and configured hints.
pub fn match_revision(kind: &str, hints_version: u32, config: &impl Debug) -> String {
    short_hash(&format!("m{MATCHER_VERSION}|{kind}|h{hints_version}|{config:?}"))
}

/// `play_revision`: the play version, the kind, what the play URL is made of, and the match
/// revision (play templates read the variables the patterns capture).
pub fn play_revision(kind: &str, play: &str, match_revision: &str) -> String {
    short_hash(&format!("p{PLAY_VERSION}|{kind}|{play}|{match_revision}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_and_sensitive() {
        assert_eq!(short_hash(""), "cbf29ce484222325");
        let a = match_revision("relay", 0, &("x", 1));
        assert_eq!(a, match_revision("relay", 0, &("x", 1)));
        assert_ne!(a, match_revision("relay", 0, &("x", 2)));
        assert_ne!(a, match_revision("relay", 1, &("x", 1)));
        assert_ne!(play_revision("relay", "p", &a), play_revision("relay", "q", &a));
    }
}
