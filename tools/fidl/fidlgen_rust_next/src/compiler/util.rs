// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::ir::Id;

pub trait IdExt {
    fn split(&self) -> Split<'_>;

    fn camel(&self) -> String {
        let mut result = String::new();
        for piece in self.split() {
            let mut chars = piece.chars();
            result.push(chars.next().unwrap().to_ascii_uppercase());
            result.extend(chars.map(|c| c.to_ascii_lowercase()));
        }
        result
    }

    fn snake(&self) -> String {
        let mut result = String::new();
        for piece in self.split() {
            if !result.is_empty() {
                result.push('_');
            }
            result.extend(piece.chars().map(|c| c.to_ascii_lowercase()));
        }
        result
    }

    fn screaming_snake(&self) -> String {
        let mut result = String::new();
        for piece in self.split() {
            if !result.is_empty() {
                result.push('_');
            }
            result.extend(piece.chars().map(|c| c.to_ascii_uppercase()));
        }
        result
    }
}

impl IdExt for Id {
    fn split(&self) -> Split<'_> {
        Split { str: self.non_canonical() }
    }
}

pub struct Split<'a> {
    str: &'a str,
}

impl<'a> Iterator for Split<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let mut char_indices = self.str.char_indices().skip_while(|(_, c)| *c == '_').peekable();

        let (start, mut prev) = char_indices.next()?;
        let mut end = self.str.len();

        while let Some((index, current)) = char_indices.next() {
            if current == '_' {
                end = index;
                break;
            }

            let prev_lower = prev.is_ascii_lowercase();
            let prev_digit = prev.is_ascii_digit();
            let current_upper = current.is_ascii_uppercase();
            let next_lower = char_indices.peek().is_some_and(|(_, c)| c.is_ascii_lowercase());

            let is_first_uppercase = (prev_lower || prev_digit) && current_upper;
            let is_last_uppercase = current_upper && next_lower;
            if is_first_uppercase || is_last_uppercase {
                end = index;
                break;
            }

            prev = current;
        }

        let result = &self.str[start..end];
        self.str = &self.str[end..];
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::IdExt as _;
    use crate::ir::Id;

    const TEST_CASES: &[&str] = &[
        "foo_bar",
        "foo__bar",
        "FooBar",
        "fooBar",
        "FOOBar",
        "__foo_bar",
        "foo123bar",
        "foO123bar",
        "foo_123bar",
        "FOO123Bar",
        "FOO123bar",
    ];

    #[test]
    fn split() {
        const EXPECTEDS: [&[&str]; TEST_CASES.len()] = [
            &["foo", "bar"],
            &["foo", "bar"],
            &["Foo", "Bar"],
            &["foo", "Bar"],
            &["FOO", "Bar"],
            &["foo", "bar"],
            &["foo123bar"],
            &["fo", "O123bar"],
            &["foo", "123bar"],
            &["FOO123", "Bar"],
            &["FOO123bar"],
        ];

        for (case, expected) in TEST_CASES.iter().zip(EXPECTEDS.iter()) {
            assert_eq!(
                &Id::from_str(case).split().collect::<Vec<_>>(),
                expected,
                "{case} did not split correctly",
            );
        }
    }

    #[test]
    fn snake() {
        const EXPECTEDS: [&str; TEST_CASES.len()] = [
            "foo_bar",
            "foo_bar",
            "foo_bar",
            "foo_bar",
            "foo_bar",
            "foo_bar",
            "foo123bar",
            "fo_o123bar",
            "foo_123bar",
            "foo123_bar",
            "foo123bar",
        ];

        for (case, expected) in TEST_CASES.iter().zip(EXPECTEDS.iter()) {
            assert_eq!(
                &Id::from_str(case).snake(),
                expected,
                "{case} was not transformed to snake case correctly",
            );
        }
    }

    #[test]
    fn camel() {
        const EXPECTEDS: [&str; TEST_CASES.len()] = [
            "FooBar",
            "FooBar",
            "FooBar",
            "FooBar",
            "FooBar",
            "FooBar",
            "Foo123bar",
            "FoO123bar",
            "Foo123bar",
            "Foo123Bar",
            "Foo123bar",
        ];

        for (case, expected) in TEST_CASES.iter().zip(EXPECTEDS.iter()) {
            assert_eq!(
                &Id::from_str(case).camel(),
                expected,
                "{case} was not transformed to camel case correctly",
            );
        }
    }

    #[test]
    fn screaming_snake() {
        const EXPECTEDS: [&str; TEST_CASES.len()] = [
            "FOO_BAR",
            "FOO_BAR",
            "FOO_BAR",
            "FOO_BAR",
            "FOO_BAR",
            "FOO_BAR",
            "FOO123BAR",
            "FO_O123BAR",
            "FOO_123BAR",
            "FOO123_BAR",
            "FOO123BAR",
        ];

        for (case, expected) in TEST_CASES.iter().zip(EXPECTEDS.iter()) {
            assert_eq!(
                &Id::from_str(case).screaming_snake(),
                expected,
                "{case} was not transformed to screaming snake case correctly",
            );
        }
    }
}
