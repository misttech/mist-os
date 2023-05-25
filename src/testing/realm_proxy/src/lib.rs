// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod client;
pub mod service;

use fidl_fuchsia_testing_harness as fharness;

/// A wrapper for errors returned by fuchsia.testing.harness FIDL protocol methods.
/// This implements std::error::Error.
///
/// Example usage:
///
/// ```
/// realm_factory.create_realm(..)
///     .await?
///     .map_err(realm_proxy::Error::from)?;
/// ```
#[derive(Debug)]
pub enum Error {
    OperationError(fharness::OperationError),
}

impl From<fharness::OperationError> for Error {
    fn from(value: fharness::OperationError) -> Self {
        Error::OperationError(value)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::OperationError(e) => write!(f, "{:?}", e),
        }
    }
}

impl std::error::Error for Error {
    fn description(&self) -> &str {
        "description() is deprecated; use Display"
    }

    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}
