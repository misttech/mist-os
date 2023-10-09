// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_config::api::ConfigError;
use fidl_fuchsia_developer_ffx::{OpenTargetError, TargetConnectionError};
use fidl_fuchsia_developer_remotecontrol::{ConnectCapabilityError, IdentifyHostError};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LogError {
    #[error("Failed to open target: {:?}", error)]
    OpenTargetError { error: OpenTargetError },
    #[error("Failed to connect to target: {:?}", error)]
    TargetConnectionError { error: TargetConnectionError },
    #[error("Failed to identify host: {:?}", error)]
    IdentifyHostError { error: IdentifyHostError },
    #[error(transparent)]
    UnknownError(#[from] anyhow::Error),
    #[error(transparent)]
    ConfigError(#[from] ConfigError),
    #[error(transparent)]
    FidlError(#[from] fidl::Error),
    #[error("No boot timestamp")]
    NoBootTimestamp,
    #[error("failed to connect: {:?}", error)]
    ConnectCapabilityError { error: ConnectCapabilityError },
    #[error(transparent)]
    IOError(#[from] std::io::Error),
    #[error("Cannot use dump with --since now")]
    DumpWithSinceNow,
    #[error("No symbolizer configuration provided")]
    NoSymbolizerConfig,
    #[error(transparent)]
    LogCommandError(#[from] log_command::LogError),
    #[error("failed to connect to RealmQuery: {:?}", error)]
    RealmQueryConnectionFailed { error: i32 },
}

impl From<LogError> for fho::Error {
    fn from(value: LogError) -> Self {
        match value {
            LogError::DumpWithSinceNow => fho::Error::User(value.into()),
            LogError::LogCommandError(log_command::LogError::FfxError(err)) => err.into(),
            err => fho::Error::Unexpected(err.into()),
        }
    }
}

impl From<i32> for LogError {
    fn from(error: i32) -> Self {
        Self::RealmQueryConnectionFailed { error }
    }
}

impl From<ConnectCapabilityError> for LogError {
    fn from(error: ConnectCapabilityError) -> Self {
        Self::ConnectCapabilityError { error }
    }
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
