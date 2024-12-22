// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api::query::{ConfigQuery, SelectMode};
use crate::api::ConfigError;
use crate::mapping::{filter, flatten};
use crate::nested::RecursiveMap;
use anyhow::anyhow;
use serde_json::{Map, Value};
use std::path::PathBuf;

const ADDITIVE_RETURN_ERR: &str =
    "Additive mode can only be used with an array or Value return type.";
const _ADDITIVE_LEVEL_ERR: &str =
    "Additive mode can only be used if config level is not specified.";

#[derive(Debug, Clone)]
pub struct ConfigValue(pub(crate) Option<Value>);

// See RecursiveMap for why the value version is the main implementation.
impl RecursiveMap for ConfigValue {
    type Output = ConfigValue;
    fn recursive_map<T: Fn(Value) -> Option<Value>>(self, mapper: &T) -> ConfigValue {
        ConfigValue(self.0.recursive_map(mapper))
    }
}
impl RecursiveMap for &ConfigValue {
    type Output = ConfigValue;
    fn recursive_map<T: Fn(Value) -> Option<Value>>(self, mapper: &T) -> ConfigValue {
        ConfigValue(self.0.clone()).recursive_map(mapper)
    }
}

pub trait ValueStrategy {
    fn handle_arrays(value: Value) -> Option<Value> {
        flatten(value)
    }

    fn validate_query(query: &ConfigQuery<'_>) -> Result<(), ConfigError> {
        match query.select {
            SelectMode::First => Ok(()),
            SelectMode::All => Err(anyhow!(ADDITIVE_RETURN_ERR).into()),
        }
    }
}

impl From<ConfigValue> for Option<Value> {
    fn from(value: ConfigValue) -> Self {
        value.0
    }
}

impl From<Option<Value>> for ConfigValue {
    fn from(value: Option<Value>) -> Self {
        ConfigValue(value)
    }
}

impl ValueStrategy for Value {
    fn handle_arrays(value: Value) -> Option<Value> {
        Some(value)
    }

    fn validate_query(_query: &ConfigQuery<'_>) -> Result<(), ConfigError> {
        Ok(())
    }
}

pub trait TryConvert: Sized {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError>;
}

impl<T> TryConvert for T
where
    T: From<ConfigValue>,
{
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        Ok(value.into())
    }
}

impl TryConvert for Value {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        value.0.ok_or_else(|| anyhow!("no value").into())
    }
}

impl ValueStrategy for Option<Value> {
    fn handle_arrays(value: Value) -> Option<Value> {
        Some(value)
    }

    fn validate_query(_query: &ConfigQuery<'_>) -> Result<(), ConfigError> {
        Ok(())
    }
}

impl ValueStrategy for String {}

impl TryConvert for String {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        value
            .0
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .ok_or_else(|| anyhow!("no configuration String value found").into())
    }
}

impl ValueStrategy for Option<String> {}

impl TryConvert for Option<String> {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        Ok(value.0.and_then(|v| v.as_str().map(|s| s.to_string())))
    }
}

impl ValueStrategy for usize {}

impl TryConvert for usize {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        value
            .0
            .and_then(|v| {
                v.as_u64().and_then(|v| usize::try_from(v).ok()).or_else(|| {
                    if let Value::String(s) = v {
                        s.parse().ok()
                    } else {
                        None
                    }
                })
            })
            .ok_or_else(|| anyhow!("no configuration usize value found").into())
    }
}

impl ValueStrategy for u64 {}

impl TryConvert for u64 {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        value
            .0
            .and_then(|v| {
                v.as_u64().or_else(|| if let Value::String(s) = v { s.parse().ok() } else { None })
            })
            .ok_or_else(|| anyhow!("no configuration Number value found").into())
    }
}

impl ValueStrategy for Option<u64> {}

impl TryConvert for Option<u64> {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        Ok(value.0.and_then(|v| {
            v.as_u64().or_else(|| if let Value::String(s) = v { s.parse().ok() } else { None })
        }))
    }
}

impl ValueStrategy for u16 {}

impl TryConvert for u16 {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        value
            .0
            .and_then(|v| {
                v.as_u64().or_else(|| if let Value::String(s) = v { s.parse().ok() } else { None })
            })
            .and_then(|v| u16::try_from(v).ok())
            .ok_or_else(|| anyhow!("no configuration Number value found").into())
    }
}

impl ValueStrategy for i64 {}

impl TryConvert for i64 {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        value
            .0
            .and_then(|v| {
                v.as_i64().or_else(|| if let Value::String(s) = v { s.parse().ok() } else { None })
            })
            .ok_or_else(|| anyhow!("no configuration Number value found").into())
    }
}

impl ValueStrategy for bool {}

impl TryConvert for bool {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        value
            .0
            .and_then(|v| {
                v.as_bool().or_else(|| if let Value::String(s) = v { s.parse().ok() } else { None })
            })
            .ok_or_else(|| anyhow!("no configuration Boolean value found").into())
    }
}

impl ValueStrategy for Option<bool> {}

impl TryConvert for Option<bool> {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        Ok(value.0.and_then(|v| {
            v.as_bool().or_else(|| if let Value::String(s) = v { s.parse().ok() } else { None })
        }))
    }
}

impl ValueStrategy for PathBuf {}

impl TryConvert for PathBuf {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        value
            .0
            .and_then(|v| v.as_str().map(|s| PathBuf::from(s.to_string())))
            .ok_or_else(|| anyhow!("no configuration PathBuf value found").into())
    }
}

impl ValueStrategy for Option<PathBuf> {}

impl TryConvert for Option<PathBuf> {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        Ok(value.0.and_then(|v| v.as_str().map(|s| PathBuf::from(s.to_string()))))
    }
}

impl<T> ValueStrategy for Vec<T> {
    fn handle_arrays(value: Value) -> Option<Value> {
        filter(value)
    }

    fn validate_query(_query: &ConfigQuery<'_>) -> Result<(), ConfigError> {
        Ok(())
    }
}

impl<T: TryConvert> TryConvert for Vec<T> {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        value
            .0
            .and_then(|val| match val.as_array() {
                Some(v) => {
                    let result: Vec<T> = v
                        .iter()
                        .filter_map(|i| T::try_convert(ConfigValue(Some(i.clone()))).ok())
                        .collect();
                    if result.len() > 0 {
                        Some(result)
                    } else {
                        None
                    }
                }
                None => T::try_convert(ConfigValue(Some(val))).map(|x| vec![x]).ok(),
            })
            .ok_or_else(|| anyhow!("no configuration Vec<> value found").into())
    }
}

impl ValueStrategy for f64 {}

impl TryConvert for f64 {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        value
            .0
            .and_then(|v| {
                v.as_f64().or_else(|| if let Value::String(s) = v { s.parse().ok() } else { None })
            })
            .ok_or_else(|| anyhow!("no configuration Number value found").into())
    }
}

impl ValueStrategy for Option<f64> {}

impl TryConvert for Option<f64> {
    fn try_convert(value: ConfigValue) -> Result<Self, ConfigError> {
        Ok(value.0.and_then(|v| {
            v.as_f64().or_else(|| if let Value::String(s) = v { s.parse().ok() } else { None })
        }))
    }
}

/// Merges [`Map`] b into [`Map`] a.
pub fn merge_map(a: &mut Map<String, Value>, b: &Map<String, Value>) {
    for (k, v) in b.iter() {
        self::merge(a.entry(k.clone()).or_insert(Value::Null), v);
    }
}

/// Merge's `Value` b into `Value` a.
pub fn merge(a: &mut Value, b: &Value) {
    match (a, b) {
        (&mut Value::Object(ref mut a), &Value::Object(ref b)) => merge_map(a, b),
        (a, b) => *a = b.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_merge() {
        let mut proto = Value::Null;
        let a = json!({
            "list": [ "first", "second" ],
            "string": "This is a string",
            "object" : {
                "foo" : "foo-prime",
                "bar" : "bar-prime"
            }
        });
        let b = json!({
            "list": [ "third" ],
            "title": "This is a title",
            "otherObject" : {
                "yourHonor" : "I object!"
            }
        });
        merge(&mut proto, &a);
        assert_eq!(proto["list"].as_array().unwrap()[0].as_str().unwrap(), "first");
        assert_eq!(proto["list"].as_array().unwrap()[1].as_str().unwrap(), "second");
        assert_eq!(proto["string"].as_str().unwrap(), "This is a string");
        assert_eq!(proto["object"]["foo"].as_str().unwrap(), "foo-prime");
        assert_eq!(proto["object"]["bar"].as_str().unwrap(), "bar-prime");
        merge(&mut proto, &b);
        assert_eq!(proto["list"].as_array().unwrap()[0].as_str().unwrap(), "third");
        assert_eq!(proto["title"].as_str().unwrap(), "This is a title");
        assert_eq!(proto["string"].as_str().unwrap(), "This is a string");
        assert_eq!(proto["object"]["foo"].as_str().unwrap(), "foo-prime");
        assert_eq!(proto["object"]["bar"].as_str().unwrap(), "bar-prime");
        assert_eq!(proto["otherObject"]["yourHonor"].as_str().unwrap(), "I object!");
    }
}
