// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::vfs::{FsStr, FsString};
use starnix_uapi::{errno, errors::Errno};
use std::collections::HashMap;

/// Parses a comma-separated list of options of the form `key` or `key=value` or `key="value"`.
/// Commas and equals-signs are only permitted in the `key="value"` case. In the case of
/// `key=value1,key=value2` collisions, the last value wins. Returns a hashmap of key/value pairs,
/// or `EINVAL` in the case of malformed input. Note that no escape character sequence is supported,
/// so values may not contain the `"` character.
///
/// # Examples
///
/// `key0=value0,key1,key2=value2,key0=value3` -> `map{"key0":"value3","key1":"","key2":"value2"}`
///
/// `key0=value0,key1="quoted,with=punc:tua-tion."` ->
/// `map{"key0":"value0","key1":"quoted,with=punc:tua-tion."}`
///
/// `key0="mis"quoted,key2=unquoted` -> `EINVAL`
pub fn generic_parse_mount_options(data: &FsStr) -> Result<HashMap<FsString, FsString>, Errno> {
    parse_mount_options::parse_mount_options(data).map_err(|_| errno!(EINVAL))
}

/// Parses `data` slice into another type.
///
/// This relies on str::parse so expects `data` to be utf8.
pub fn parse<F: std::str::FromStr>(data: &FsStr) -> Result<F, Errno>
where
    <F as std::str::FromStr>::Err: std::fmt::Debug,
{
    std::str::from_utf8(data.as_ref())
        .map_err(|e| errno!(EINVAL, e))?
        .parse::<F>()
        .map_err(|e| errno!(EINVAL, format!("{:?}", e)))
}

mod parse_mount_options {
    use crate::vfs::{FsStr, FsString};
    use nom::{
        branch::alt,
        bytes::complete::{is_not, tag},
        multi::separated_list,
        sequence::{delimited, separated_pair},
        IResult,
    };
    use starnix_uapi::errors::{errno, error, Errno};
    use std::collections::HashMap;

    fn unquoted(input: &[u8]) -> IResult<&[u8], &[u8]> {
        is_not(",=")(input)
    }

    fn quoted(input: &[u8]) -> IResult<&[u8], &[u8]> {
        delimited(tag(b"\""), is_not("\""), tag(b"\""))(input)
    }

    fn value(input: &[u8]) -> IResult<&[u8], &[u8]> {
        alt((quoted, unquoted))(input)
    }

    fn key_value(input: &[u8]) -> IResult<&[u8], (&[u8], &[u8])> {
        separated_pair(unquoted, tag(b"="), value)(input)
    }

    fn key_only(input: &[u8]) -> IResult<&[u8], (&[u8], &[u8])> {
        let (input, key) = unquoted(input)?;
        Ok((input, (key, b"")))
    }

    fn option(input: &[u8]) -> IResult<&[u8], (&[u8], &[u8])> {
        alt((key_value, key_only))(input)
    }

    pub(super) fn parse_mount_options(input: &FsStr) -> Result<HashMap<FsString, FsString>, Errno> {
        let (input, options) =
            separated_list(tag(b","), option)(input.into()).map_err(|_| errno!(EINVAL))?;

        // `[...],last_key="mis"quoted` not allowed.
        if input.len() > 0 {
            return error!(EINVAL);
        }

        // Insert in-order so that last `key=value` containing `key` "wins".
        let mut options_map: HashMap<FsString, FsString> = HashMap::with_capacity(options.len());
        for (key, value) in options.into_iter() {
            options_map.insert(key.into(), value.into());
        }

        Ok(options_map)
    }
}

#[cfg(test)]
mod tests {
    use super::{generic_parse_mount_options, parse};
    use maplit::hashmap;

    #[::fuchsia::test]
    fn empty_data() {
        assert!(generic_parse_mount_options(Default::default()).unwrap().is_empty());
    }

    #[::fuchsia::test]
    fn parse_options_last_value_wins() {
        // Repeat key `key0`.
        let data = b"key0=value0,key1,key2=value2,key0=value3";
        let parsed_data = generic_parse_mount_options(data.into())
            .expect("mount options parse:  key0=value0,key1,key2=value2,key0=value3");
        assert_eq!(
            parsed_data,
            hashmap! {
                b"key1".into() => b"".into(),
                b"key2".into() => b"value2".into(),
                // Last `key0` value in list "wins":
                b"key0".into() => b"value3".into(),
            }
        );
    }

    #[::fuchsia::test]
    fn parse_options_quoted() {
        let data = b"key0=unqouted,key1=\"quoted,with=punc:tua-tion.\"";
        let parsed_data = generic_parse_mount_options(data.into())
            .expect("mount options parse:  key0=value0,key1,key2=value2,key0=value3");
        assert_eq!(
            parsed_data,
            hashmap! {
                b"key0".into() => b"unqouted".into(),
                b"key1".into() => b"quoted,with=punc:tua-tion.".into(),
            }
        );
    }

    #[::fuchsia::test]
    fn parse_options_misquoted() {
        let data = b"key0=\"mis\"quoted,key1=\"quoted\"";
        let parse_result = generic_parse_mount_options(data.into());
        assert!(
            parse_result.is_err(),
            "expected parse failure:  key0=\"mis\"quoted,key1=\"quoted\""
        );
    }

    #[::fuchsia::test]
    fn parse_options_misquoted_tail() {
        let data = b"key0=\"quoted\",key1=\"mis\"quoted";
        let parse_result = generic_parse_mount_options(data.into());
        assert!(
            parse_result.is_err(),
            "expected parse failure:  key0=\"quoted\",key1=\"mis\"quoted"
        );
    }

    #[::fuchsia::test]
    fn parse_data() {
        assert_eq!(parse::<usize>("42".into()), Ok(42));
    }
}
