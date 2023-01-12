// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This is designed to be imported by users as `use crate::log::*` so we should be judicious when
// adding to this module.

pub use tracing::{debug, error, info, warn};

pub trait AsValue<'a> {
    type ValueType;

    fn as_value(&'a self) -> Self::ValueType;
}

impl<'a> AsValue<'a> for anyhow::Error {
    type ValueType = &'a (dyn std::error::Error + 'static);

    fn as_value(&'a self) -> Self::ValueType {
        self.as_ref()
    }
}

#[cfg(target_os = "fuchsia")]
mod fuchsia {
    use super::AsValue;

    impl<'a> AsValue<'a> for fuchsia_zircon::Status {
        type ValueType = &'a (dyn std::error::Error + 'static);

        fn as_value(&'a self) -> Self::ValueType {
            self
        }
    }

    impl<'a> AsValue<'a> for fidl::Error {
        type ValueType = &'a (dyn std::error::Error + 'static);

        fn as_value(&'a self) -> Self::ValueType {
            self
        }
    }
}

#[cfg(target_os = "fuchsia")]
pub use self::fuchsia::*;
