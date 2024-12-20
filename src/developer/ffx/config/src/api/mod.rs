// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use serde_json::Value;
use thiserror::Error;

pub mod query;
pub mod value;

pub type ConfigResult = Result<ConfigValue>;
pub use query::ConfigQuery;
pub use value::ConfigValue;
use value::TryConvert;

#[derive(Debug, Error)]
#[error("Configuration error")]
pub enum ConfigError {
    #[error("{}", .0)]
    Error(#[from] anyhow::Error),
    #[error("Config key not found")]
    KeyNotFound,
    #[error("Can't remove empty key")]
    EmptyKey,
    #[error("Bad value: {value}: {reason}")]
    BadValue { value: Value, reason: String },
}

impl ConfigError {
    pub fn new(e: anyhow::Error) -> Self {
        Self::Error(e)
    }
}

pub(crate) fn validate_type<T>(value: Value) -> Option<Value>
where
    T: TryConvert,
{
    let result = T::try_convert(ConfigValue(Some(value.clone())));
    match result {
        Ok(_) => Some(value),
        Err(_) => None,
    }
}

impl From<ConfigError> for std::convert::Infallible {
    fn from(value: ConfigError) -> Self {
        panic!("cannot convert value into `Infallible`: {value:#}")
    }
}
