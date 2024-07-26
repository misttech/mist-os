// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use std::collections::VecDeque;

mod lookup;
mod nfd;
use unicode_gen;

/// Filters a valid sequence of unicode characters, casefolding.
pub struct CaseFoldIterator<I: Iterator<Item = char>> {
    /// The not-yet-normalized input sequence.
    input: I,
    buf: VecDeque<char>,
}

impl<I: Iterator<Item = char>> Iterator for CaseFoldIterator<I> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        if let Some(ch) = self.buf.pop_front() {
            return Some(ch);
        }
        self.input.next().map(|ch| {
            if let Some(mapping) = crate::lookup::casefold(ch) {
                let mut chars = mapping.chars();
                let first = chars.next().unwrap();
                self.buf.extend(chars);
                first
            } else {
                ch
            }
        })
    }
}

/// Wraps an Iterator<Item=char> so that it case folds.
pub fn casefold<I: Iterator<Item = char>>(input: I) -> CaseFoldIterator<I> {
    CaseFoldIterator { input, buf: VecDeque::new() }
}

/// A comparison function that:
///  * Applies casefolding.
///  * Removes default ignorable characters.
///  * Applies nfd normalization.
/// That function will early-out at the first non-matching character.
pub fn casefold_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let a_it = nfd::nfd(casefold(a.chars()).filter(|x| !lookup::default_ignorable(*x)));
    let b_it = nfd::nfd(casefold(b.chars()).filter(|x| !lookup::default_ignorable(*x)));
    a_it.cmp(b_it)
}

// A simple wrapper around String that provides casefolding comparison.
#[derive(Clone, Eq)]
struct CasefoldString(String);
impl CasefoldString {
    pub fn new(s: String) -> Self {
        Self(s)
    }
}
impl std::fmt::Debug for CasefoldString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "CasefoldString(\"{}\")", self.0)
    }
}
impl std::fmt::Display for CasefoldString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.0)
    }
}
impl std::ops::Deref for CasefoldString {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}
impl std::cmp::PartialEq for CasefoldString {
    fn eq(&self, rhs: &Self) -> bool {
        casefold_cmp(&self.0, &rhs.0) == std::cmp::Ordering::Equal
    }
}
impl std::cmp::Ord for CasefoldString {
    fn cmp(&self, rhs: &Self) -> std::cmp::Ordering {
        casefold_cmp(&self.0, &rhs.0)
    }
}
impl std::cmp::PartialOrd for CasefoldString {
    fn partial_cmp(&self, rhs: &Self) -> Option<std::cmp::Ordering> {
        Some(casefold_cmp(&self.0, &rhs.0))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_casefold() {
        assert_eq!(casefold("Hello There".chars()).collect::<String>(), "hello there");
        assert_eq!(casefold("HELLO There".chars()).collect::<String>(), "hello there");
    }

    #[test]
    fn test_casefold_cmp() {
        assert_eq!(casefold_cmp("Hello", "hello"), std::cmp::Ordering::Equal);
        assert_eq!(casefold_cmp("Hello There", "hello"), std::cmp::Ordering::Greater);
        assert_eq!(casefold_cmp("hello there", "hello"), std::cmp::Ordering::Greater);
        assert_eq!(casefold_cmp("hello\u{00AD}", "hello"), std::cmp::Ordering::Equal);
        assert_eq!(casefold_cmp("\u{03AA}", "\u{0399}\u{0308}"), std::cmp::Ordering::Equal);

        // Gracefully handle the degenerate case where we start with modifiers
        assert_eq!(
            casefold_cmp("\u{308}\u{05ae}Hello", "\u{05ae}\u{308}hello"),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn test_casefoldstring() {
        let a = CasefoldString::new("Hello There".to_owned());
        let b = CasefoldString::new("hello there".to_owned());
        let c = CasefoldString::new("hello".to_owned());
        let d = CasefoldString::new("\u{03AA}".to_owned());
        let e = CasefoldString::new("\u{0399}\u{0308}".to_owned());
        // Check some comparisons.
        assert_eq!(a, a);
        assert_eq!(a, b);
        assert_eq!(d, e);
        assert!(a > c);
        assert!(b > c);
        assert_eq!(a.0, format!("{}", a));
        // Debug::fmt should show type to avoid confusion.
        assert_eq!("CasefoldString(\"Hello There\")", format!("{:?}", a));
        // Displays the same as String.
        assert_eq!(format!("{}", "Hello There".to_owned()), format!("{}", a));
    }
}
