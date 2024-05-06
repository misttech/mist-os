// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_component_sandbox as fsandbox;
use fuchsia_zircon_status as zx;
use std::{
    fmt::{self, Debug, Display},
    sync::Arc,
};
use thiserror::Error;

/// The error type returned by bedrock operations.
#[derive(Debug, Error, Clone)]
pub enum RouterError {
    #[error("could not find capability: {0}")]
    NotFound(Arc<dyn Explain>),

    #[error("invalid arguments")]
    InvalidArgs,

    #[error("unknown error")]
    Unknown,
}

impl From<fsandbox::RouterError> for RouterError {
    fn from(err: fsandbox::RouterError) -> Self {
        match err {
            fsandbox::RouterError::NotFound => Self::NotFound(Arc::new(ExternalNotFoundError {})),
            fsandbox::RouterError::InvalidArgs => Self::InvalidArgs,
            fsandbox::RouterErrorUnknown!() => Self::Unknown,
        }
    }
}

impl From<RouterError> for fsandbox::RouterError {
    fn from(err: RouterError) -> Self {
        match err {
            RouterError::NotFound(_) => Self::NotFound,
            RouterError::InvalidArgs => Self::InvalidArgs,
            RouterError::Unknown => Self::unknown(),
        }
    }
}

#[derive(Debug, Error, Clone)]
struct ExternalNotFoundError {}

impl Explain for ExternalNotFoundError {
    fn as_zx_status(&self) -> zx::Status {
        zx::Status::NOT_FOUND
    }
}

impl fmt::Display for ExternalNotFoundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "external not found error")
    }
}

/// All detailed error objects must implement the [`Explain`] trait, since:
///
/// - Some operations are not yet refactored into bedrock.
/// - Some operations fundamentally are not fit for bedrock.
///
/// The detailed errors are hidden, but users may get strings or codes for debugging.
pub trait Explain: std::error::Error + Debug + Display + Send + Sync + sealed::AnyCast {
    fn as_zx_status(&self) -> zx::Status;
}

impl Explain for RouterError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            Self::NotFound(err) => err.as_zx_status(),
            Self::InvalidArgs => zx::Status::INVALID_ARGS,
            Self::Unknown => zx::Status::INTERNAL,
        }
    }
}

/// To test the error case of e.g. a `Router` implementation, it will be helpful
/// to cast the erased error back to an expected error type and match on it.
///
/// Do not use this in production as conditioning behavior on error cases is
/// extremely fragile.
pub trait DowncastErrorForTest {
    /// For tests only. Downcast the erased error to `E` or panic if fails.
    fn downcast_for_test<E: Explain>(&self) -> &E;
}

impl DowncastErrorForTest for dyn Explain {
    fn downcast_for_test<E: Explain>(&self) -> &E {
        match self.as_any().downcast_ref::<E>() {
            Some(value) => value,
            None => {
                let expected = std::any::type_name::<E>();
                panic!("Cannot downcast `{self}` to the {expected} error type!");
            }
        }
    }
}

mod sealed {
    use std::any::Any;

    pub trait AnyCast: Any {
        fn as_any(&self) -> &dyn Any;
    }

    impl<T: Any> AnyCast for T {
        fn as_any(&self) -> &dyn Any {
            self
        }
    }
}
