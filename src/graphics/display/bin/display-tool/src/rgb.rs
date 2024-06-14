// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{fmt, str};

#[derive(Debug, PartialEq)]
pub enum ParseRgbError {
    UnexpectedCharacter,
    IncorrectSize(usize),
}

impl fmt::Display for ParseRgbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseRgbError::UnexpectedCharacter => write!(f, "Unexpected character"),
            ParseRgbError::IncorrectSize(size) => write!(f, "Incorrect size {}", size),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Rgb888 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl str::FromStr for Rgb888 {
    type Err = ParseRgbError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if !s.chars().all(|x| x.is_ascii_hexdigit()) {
            return Err(ParseRgbError::UnexpectedCharacter);
        }
        if s.len() != 6 {
            return Err(ParseRgbError::IncorrectSize(s.len()));
        }
        Ok(Rgb888 {
            r: u8::from_str_radix(&s[0..2], 16).unwrap(),
            g: u8::from_str_radix(&s[2..4], 16).unwrap(),
            b: u8::from_str_radix(&s[4..6], 16).unwrap(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[fuchsia::test]
    fn rgb_from_str_invalid() {
        assert_eq!(Rgb888::from_str("zz00ff"), Err(ParseRgbError::UnexpectedCharacter));
        assert_eq!(Rgb888::from_str("0000vv"), Err(ParseRgbError::UnexpectedCharacter));
        assert_eq!(Rgb888::from_str("0x010101"), Err(ParseRgbError::UnexpectedCharacter));

        assert_eq!(Rgb888::from_str(""), Err(ParseRgbError::IncorrectSize(0)));
        assert_eq!(Rgb888::from_str("10101"), Err(ParseRgbError::IncorrectSize(5)));
        assert_eq!(Rgb888::from_str("1010111"), Err(ParseRgbError::IncorrectSize(7)));
    }

    #[fuchsia::test]
    fn rgb_from_str_valid() {
        assert_eq!(Rgb888::from_str("ef0000"), Ok(Rgb888 { r: 0xef, g: 0x00, b: 0x00 }));
        assert_eq!(Rgb888::from_str("00ab00"), Ok(Rgb888 { r: 0x00, g: 0xab, b: 0x00 }));
        assert_eq!(Rgb888::from_str("0000cd"), Ok(Rgb888 { r: 0x00, g: 0x00, b: 0xcd }));
        assert_eq!(Rgb888::from_str("012345"), Ok(Rgb888 { r: 0x01, g: 0x23, b: 0x45 }));
        assert_eq!(Rgb888::from_str("000000"), Ok(Rgb888 { r: 0x00, g: 0x00, b: 0x00 }));
        assert_eq!(Rgb888::from_str("ffffff"), Ok(Rgb888 { r: 0xff, g: 0xff, b: 0xff }));
    }
}
