// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mapping::{postprocess, preprocess, replace_regex as replace};
use crate::EnvironmentContext;
use anyhow::anyhow;
use lazy_static::lazy_static;
use regex::Regex;
use serde_json::Value;

// Succeeds if every environment variable ("$ENV") exists -- including
// zero env variables. If there are any mentioned that _don't_ exist, then
// this fails.
fn check(ctx: &EnvironmentContext, value: &String, regex: &Regex) -> bool {
    // First verify all environment variables exist.
    // If one does not exist, return none.
    for caps in regex.captures_iter(&value) {
        // Skip the first one since that'll be the whole string.
        for cap in caps.iter().skip(1) {
            if let Some(c) = cap {
                let var = c.as_str();
                if let Err(_) = ctx.env_var(var) {
                    return false;
                }
            }
        }
    }
    true
}

lazy_static! {
    static ref REGEX: Regex = Regex::new(r"\$([A-Z][A-Z0-9_]*)").unwrap();
}

/// Replaces an env var (if it is one) with `Some(v)`, else `None`. Not to be confused with
/// `env_var` which can return `Some(v)` even if something has not been replaced by an env
/// variable. This will _only_ return a replaced env variable string value.
pub fn env_var_check(ctx: &EnvironmentContext, value: &Value) -> Option<String> {
    let e = preprocess(value)?;
    if !check(ctx, &e, &*REGEX) {
        return None;
    }
    if REGEX.is_match(&e) {
        let res = replace(&e, &*REGEX, |v| ctx.env_var(v).map_err(|_| anyhow!("")));
        // At this point we know it's a string, given it matches a regex, etc, so convert the
        // value to a string.
        return Some(postprocess(res).to_string());
    }
    None
}

pub fn env_var(ctx: &EnvironmentContext, value: Value) -> Option<Value> {
    let env_string = preprocess(&value);
    if let Some(ref e) = env_string {
        if !check(ctx, e, &*REGEX) {
            return None;
        }
    }
    env_string
        .as_ref()
        .map(|s| replace(s, &*REGEX, |v| ctx.env_var(v).map_err(|_| anyhow!(""))))
        .map(postprocess)
        .or(Some(value))
}

pub fn env_var_strict(value: Value) -> Option<Value> {
    let env_string = preprocess(&value);
    if let Some(ref e) = env_string {
        if REGEX.is_match(e) {
            return None;
        }
    }
    Some(value)
}

////////////////////////////////////////////////////////////////////////////////
// tests
#[cfg(test)]
mod test {
    use super::*;
    use crate::environment::ExecutableKind;
    use crate::ConfigMap;

    #[test]
    fn test_env_var_mapper() {
        let ctx = EnvironmentContext::isolated(
            ExecutableKind::Test,
            "/tmp".into(),
            [("FFX_TEST_ENV_VAR_MAPPER".to_owned(), "test".to_owned())].into(),
            ConfigMap::default(),
            None,
            None,
            false,
        )
        .unwrap();
        let test = Value::String("$FFX_TEST_ENV_VAR_MAPPER".to_string());
        assert_eq!(env_var(&ctx, test), Some(Value::String("test".to_string())));
    }

    #[test]
    fn test_env_var_mapper_multiple() {
        let ctx = EnvironmentContext::isolated(
            ExecutableKind::Test,
            "/tmp".into(),
            [("FFX_TEST_ENV_VAR_MAPPER_MULTIPLE".to_owned(), "test".to_owned())].into(),
            ConfigMap::default(),
            None,
            None,
            false,
        )
        .unwrap();
        let test = Value::String(
            "$FFX_TEST_ENV_VAR_MAPPER_MULTIPLE/$FFX_TEST_ENV_VAR_MAPPER_MULTIPLE".to_string(),
        );
        assert_eq!(env_var(&ctx, test), Some(Value::String(format!("{}/{}", "test", "test"))));
    }

    #[test]
    fn test_env_var_mapper_returns_none() {
        let ctx = EnvironmentContext::isolated(
            ExecutableKind::Test,
            "/tmp".into(),
            Default::default(),
            ConfigMap::default(),
            None,
            None,
            false,
        )
        .unwrap();
        let test = Value::String("$ENVIRONMENT_VARIABLE_THAT_DOES_NOT_EXIST".to_string());
        assert_eq!(env_var(&ctx, test), None);
    }

    #[test]
    fn test_env_var_mapper_multiple_returns_none_if_one_does_not_exist() {
        let ctx = EnvironmentContext::isolated(
            ExecutableKind::Test,
            "/tmp".into(),
            Default::default(),
            ConfigMap::default(),
            None,
            None,
            false,
        )
        .unwrap();
        let test = Value::String("$HOME/$ENVIRONMENT_VARIABLE_THAT_DOES_NOT_EXIST".to_string());
        assert_eq!(env_var(&ctx, test), None);
    }

    #[test]
    fn test_env_var_mapper_escapes_dollar_sign() {
        let ctx = EnvironmentContext::isolated(
            ExecutableKind::Test,
            "/tmp".into(),
            Default::default(),
            ConfigMap::default(),
            None,
            None,
            false,
        )
        .unwrap();
        let test = Value::String("$$HOME".to_string());
        assert_eq!(env_var(&ctx, test), Some(Value::String("$HOME".to_string())));
    }

    #[test]
    fn test_env_var_returns_value_if_not_string() {
        let ctx = EnvironmentContext::isolated(
            ExecutableKind::Test,
            "/tmp".into(),
            Default::default(),
            ConfigMap::default(),
            None,
            None,
            false,
        )
        .unwrap();
        let test = Value::Bool(false);
        assert_eq!(env_var(&ctx, test), Some(Value::Bool(false)));
    }
}
