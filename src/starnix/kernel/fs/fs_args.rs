// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::FsStr,
    types::{errno, Errno},
};
use std::collections::HashMap;

/// generic_parse_mount_options parses a comma-separated list of options of the
/// form "key" or "key=value", where neither key nor value contain commas, and
/// returns it as a map. If `data` contains duplicate keys, then the last value
/// wins. For example:
///
/// data = "key0=value0,key1,key2=value2,key0=value3" -> map{"key0":"value3","key1":""],"key2":"value2"}
///
/// generic_parse_mount_options is not appropriate if values may contain commas.
pub fn generic_parse_mount_options(data: &FsStr) -> HashMap<&FsStr, &FsStr> {
    let mut result: HashMap<&FsStr, &FsStr> = Default::default();
    if data.is_empty() {
        return result;
    }
    for entry in data.split(|c| *c == b',') {
        match entry.splitn(2, |c| *c == b'=').collect::<Vec<_>>().as_slice() {
            [k] => {
                result.insert(k, b"");
            }
            [k, v] => {
                result.insert(k, v);
            }
            _ => unreachable!(),
        }
    }
    result
}

/// Parses `data` slice into another type.
///
/// This relies on str::parse so expects `data` to be utf8.
pub fn parse<F: std::str::FromStr>(data: &FsStr) -> Result<F, Errno>
where
    <F as std::str::FromStr>::Err: std::fmt::Debug,
{
    std::str::from_utf8(data)
        .map_err(|e| errno!(EINVAL, e))?
        .parse::<F>()
        .map_err(|e| errno!(EINVAL, format!("{:?}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::fuchsia::test]
    fn empty_data() {
        assert!(generic_parse_mount_options(b"").is_empty());
    }

    #[::fuchsia::test]
    fn parse_options() {
        let data = b"key0=value0,key1,key2=value2,key0=value3";
        let parsed_data = generic_parse_mount_options(data);
        assert_eq!(
            parsed_data,
            HashMap::from([
                (b"key1" as &FsStr, b"" as &FsStr),
                (b"key2", b"value2"),
                (b"key0", b"value3")
            ])
        );
    }

    #[::fuchsia::test]
    fn parse_data() {
        assert_eq!(parse::<usize>(b"42"), Ok(42));
    }
}
