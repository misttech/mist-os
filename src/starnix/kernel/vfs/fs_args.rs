// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::vfs::{FsStr, FsString};
use starnix_uapi::{errno, errors::Errno};
use std::collections::HashMap;

/// generic_parse_mount_options parses a comma-separated list of options of the
/// form "key" or "key=value", where neither key nor value contain commas, and
/// returns it as a map. If `data` contains duplicate keys, then the last value
/// wins. For example:
///
/// data = "key0=value0,key1,key2=value2,key0=value3" -> map{"key0":"value3","key1":"","key2":"value2"}
///
/// generic_parse_mount_options is not appropriate if values may contain commas.
pub fn generic_parse_mount_options<'a>(data: &FsStr) -> HashMap<FsString, FsString> {
    let mut result = HashMap::default();
    if data.is_empty() {
        return result;
    }
    for entry in data.split(|c| *c == b',') {
        let mut kv = entry.splitn(2, |c| *c == b'=');
        let key = kv.next().expect("all mount options have at least a key");
        let value = kv.next().unwrap_or_default();
        result.insert(key.into(), value.into());
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
    std::str::from_utf8(data.as_ref())
        .map_err(|e| errno!(EINVAL, e))?
        .parse::<F>()
        .map_err(|e| errno!(EINVAL, format!("{:?}", e)))
}

#[cfg(test)]
mod tests {

    use super::{generic_parse_mount_options, parse};
    use std::collections::HashMap;

    #[::fuchsia::test]
    fn empty_data() {
        assert!(generic_parse_mount_options(Default::default()).is_empty());
    }

    #[::fuchsia::test]
    fn parse_options() {
        let data = "key0=value0,key1,key2=value2,key0=value3";
        let parsed_data = generic_parse_mount_options(data.into());
        assert_eq!(
            parsed_data,
            HashMap::from([
                ("key1".into(), Default::default()),
                ("key2".into(), "value2".into()),
                ("key0".into(), "value3".into())
            ])
        );
    }

    #[::fuchsia::test]
    fn parse_data() {
        assert_eq!(parse::<usize>("42".into()), Ok(42));
    }
}
