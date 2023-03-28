// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::path::Path;

use crate::{
    mapping::{postprocess, preprocess, replace_regex},
    EnvironmentContext,
};
use lazy_static::lazy_static;
use regex::Regex;
use serde_json::Value;

/// Replaces `$BUILD_DIR` with the path to the current environment context's
/// build directory. If there's no build directory set in the current context,
/// the value will be skipped.
pub(crate) fn build(ctx: &EnvironmentContext, value: Value) -> Option<Value> {
    lazy_static! {
        static ref REGEX: Regex = Regex::new(r"\$(BUILD_DIR)").unwrap();
    }

    let Some(s) = preprocess(&value) else { return Some(value) };
    let build_dir = ctx.build_dir().and_then(Path::to_str);

    match (build_dir, REGEX.is_match(&s)) {
        // there's a build directory and this string wants it, so replace it
        (Some(build_dir), true) => {
            Some(postprocess(replace_regex(&s, &*REGEX, |_| Ok(build_dir.to_owned()))))
        }
        // there's no build directory but this string wants one, so skip this value
        (None, true) => None,
        // in any other case, let the original pass through
        _ => Some(value),
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests
#[cfg(test)]
mod test {
    use crate::environment::ExecutableKind;

    use super::*;

    #[test]
    fn test_mapper() {
        let ctx = EnvironmentContext::in_tree(
            ExecutableKind::Test,
            "/tmp".into(),
            Some("/tmp/build".into()),
            Default::default(),
            Default::default(),
        );
        let value = ctx.build_dir().expect("Getting build path").to_string_lossy().to_string();
        let test = Value::String("$BUILD_DIR".to_string());
        assert_eq!(build(&ctx, test), Some(Value::String(value)));
    }

    #[test]
    fn test_mapper_multiple() {
        let ctx = EnvironmentContext::in_tree(
            ExecutableKind::Test,
            "/tmp".into(),
            Some("/tmp/build".into()),
            Default::default(),
            None,
        );
        let value = ctx.build_dir().expect("Getting runtime path").to_string_lossy().to_string();
        let test = Value::String("$BUILD_DIR/$BUILD_DIR".to_string());
        assert_eq!(build(&ctx, test), Some(Value::String(format!("{}/{}", value, value))));
    }

    #[test]
    fn test_no_build_dir() {
        let ctx = EnvironmentContext::in_tree(
            ExecutableKind::Test,
            "/tmp".into(),
            None,
            Default::default(),
            Default::default(),
        );
        let test = Value::String("$BUILD_DIR".to_string());
        assert_eq!(build(&ctx, test), None);
    }

    #[test]
    fn test_mapper_returns_pass_through() {
        let ctx = EnvironmentContext::in_tree(
            ExecutableKind::Test,
            "/tmp".into(),
            Some("/tmp/build".into()),
            Default::default(),
            Default::default(),
        );
        let test = Value::String("$WHATEVER".to_string());
        assert_eq!(build(&ctx, test), Some(Value::String("$WHATEVER".to_string())));
    }
}
