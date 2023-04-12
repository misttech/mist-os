// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_config::api::ConfigError;
use ffx_core::macro_deps::fidl;
use fidl_fuchsia_developer_ffx::{OpenTargetError, TargetConnectionError};
use fidl_fuchsia_developer_remotecontrol::IdentifyHostError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LogError {
    #[error("Failed to open target: {:?}", error)]
    OpenTargetError { error: OpenTargetError },
    #[error("Failed to connect to target: {:?}", error)]
    TargetConnectionError { error: TargetConnectionError },
    #[error("Failed to identify host: {:?}", error)]
    IdentifyHostError { error: IdentifyHostError },
    #[error("Unknown error: {}", error)]
    UnknownError {
        #[from]
        error: anyhow::Error,
    },
    #[error("No RCS proxy available")]
    NoRcsProxyAvailable,
    #[error("Failed to get log settings: {:?}", error)]
    ConfigError {
        #[from]
        error: ConfigError,
    },
    #[error("FIDL error: {:?}", error)]
    FidlError {
        #[from]
        error: fidl::Error,
    },
    #[error("No hostname available")]
    NoHostname,
}

impl From<OpenTargetError> for LogError {
    fn from(error: OpenTargetError) -> Self {
        LogError::OpenTargetError { error }
    }
}

impl From<TargetConnectionError> for LogError {
    fn from(error: TargetConnectionError) -> Self {
        LogError::TargetConnectionError { error }
    }
}

impl From<IdentifyHostError> for LogError {
    fn from(error: IdentifyHostError) -> Self {
        LogError::IdentifyHostError { error }
    }
}
