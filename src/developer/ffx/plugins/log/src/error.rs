// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::transactional_symbolizer::ReadError;
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
    #[error(
        "SDK not available, please use --symbolize off to disable symbolization. Reason: {msg}"
    )]
    SdkNotAvailable { msg: &'static str },
    #[error("failed to connect: {:?}", error)]
    ConnectCapabilityError { error: ConnectCapabilityError },
    #[error(transparent)]
    IOError(#[from] std::io::Error),
    #[error("Cannot use dump with --since now")]
    DumpWithSinceNow,
    #[error("No symbolizer configuration provided")]
    NoSymbolizerConfig,
    #[error("failed to connect to RealmQuery: {:?}", error)]
    RealmQueryConnectionFailed { error: i32 },
    #[error(transparent)]
    SymbolizerError(#[from] ReadError),
    #[error("Daemon connection was lost and retries are disabled.")]
    DaemonRetriesDisabled,
}

impl From<log_command::LogError> for LogError {
    fn from(value: log_command::LogError) -> Self {
        use log_command::LogError::*;
        match value {
            UnknownError(err) => Self::UnknownError(err),
            NoBootTimestamp => Self::NoBootTimestamp,
            IOError(err) => Self::IOError(err),
            DumpWithSinceNow => Self::DumpWithSinceNow,
            NoSymbolizerConfig => Self::NoSymbolizerConfig,
            FfxError(err) => Self::UnknownError(err.into()),
            Utf8Error(err) => Self::UnknownError(err.into()),
            FidlError(err) => Self::UnknownError(err.into()),
        }
    }
}

impl From<LogError> for fho::Error {
    fn from(value: LogError) -> Self {
        use LogError::*;
        match value {
            // anyhow errors may carry ffx user errors, so let the normal translation deal with that.
            UnknownError(err) => err.into(),
            // these errors have useful, actionable errors for users
            DumpWithSinceNow
            | NoBootTimestamp
            | NoSymbolizerConfig
            | RealmQueryConnectionFailed { .. }
            | OpenTargetError { .. }
            | TargetConnectionError { .. }
            | IdentifyHostError { .. }
            | SdkNotAvailable { .. }
            | ConnectCapabilityError { .. } => fho::Error::User(value.into()),
            // these errors are probably an unexpected problem with no actionable error output.
            FidlError(err) => fho::Error::Unexpected(err.into()),
            IOError(err) => fho::Error::Unexpected(err.into()),
            ConfigError(err) => fho::Error::Unexpected(err.into()),
            SymbolizerError(err) => fho::Error::Unexpected(err.into()),
            DaemonRetriesDisabled => fho::Error::Unexpected(value.into()),
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
