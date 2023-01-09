// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    clonable_error::ClonableError,
    cm_moniker::{InstancedAbsoluteMoniker, InstancedExtendedMoniker, InstancedRelativeMoniker},
    thiserror::Error,
};

#[derive(Debug, Error, Clone)]
pub enum OpenResourceError {
    #[error("Failed to open path `{}` in outgoing directory of `{}`: {}", path, moniker, err)]
    OpenOutgoingFailed {
        moniker: InstancedAbsoluteMoniker,
        path: String,
        #[source]
        err: ClonableError,
    },
    #[error("Failed to open path `{}` in component manager's namespace: {}", path, err)]
    OpenComponentManagerNamespaceFailed {
        path: String,
        #[source]
        err: ClonableError,
    },
    #[error(
        "Failed to open path `{}`, in storage directory for `{}` backed by `{}`: {}",
        path,
        relative_moniker,
        moniker,
        err
    )]
    OpenStorageFailed {
        moniker: InstancedExtendedMoniker,
        relative_moniker: InstancedRelativeMoniker,
        path: String,
        #[source]
        err: ClonableError,
    },
}

impl OpenResourceError {
    pub fn open_outgoing_failed(
        moniker: &InstancedAbsoluteMoniker,
        path: impl Into<String>,
        err: impl Into<Error>,
    ) -> Self {
        Self::OpenOutgoingFailed {
            moniker: moniker.clone(),
            path: path.into(),
            err: err.into().into(),
        }
    }

    pub fn open_component_manager_namespace_failed(
        path: impl Into<String>,
        err: impl Into<Error>,
    ) -> Self {
        Self::OpenComponentManagerNamespaceFailed { path: path.into(), err: err.into().into() }
    }

    pub fn open_storage_failed(
        moniker: &InstancedExtendedMoniker,
        relative_moniker: &InstancedRelativeMoniker,
        path: impl Into<String>,
        err: impl Into<Error>,
    ) -> Self {
        Self::OpenStorageFailed {
            moniker: moniker.clone(),
            relative_moniker: relative_moniker.clone(),
            path: path.into(),
            err: err.into().into(),
        }
    }
}
