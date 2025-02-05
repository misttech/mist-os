// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io;
use std::path::PathBuf;
use thiserror::Error;

/// Error encountered while executing test binary
#[derive(Debug, Error)]
pub enum TestRunError {
    #[error("Error launching test binary '{0:?}': {1:?}")]
    Spawn(std::ffi::OsString, io::Error),

    #[error("Error reading stdout: {0:?}")]
    StdoutRead(#[source] io::Error),

    #[error("Error reading stderr: {0:?}")]
    StderrRead(#[source] io::Error),

    #[error("Error writing stdout: {0:?}")]
    StdoutWrite(#[source] io::Error),

    #[error("Error writing stderr: {0:?}")]
    StderrWrite(#[source] io::Error),
}

/// Error encountered validating config
#[derive(Debug, Error)]
pub enum BuildError {
    #[error("Schema file {path} could not be opened for reading")]
    FailedToOpenSchema {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("Failure attempting to read schema {path}")]
    FailedToReadSchema {
        path: PathBuf,
        #[source]
        source: serde_json5::Error,
    },

    #[error("Failure attempting to parse schema {path}")]
    FailedToParseSchema {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("Schema not well-formed: {0}")]
    InvalidSchema(String),

    #[error("Incorrect usage")]
    IncorrectUsage(#[source] UsageError),
}

impl From<UsageError> for BuildError {
    fn from(value: UsageError) -> Self {
        BuildError::IncorrectUsage(value)
    }
}
/// Error encountered processing command line arguments, environment variables, or
/// included JSON files.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum UsageError {
    #[error("Wrong type: expected type {expected}, got \"{got}\" for parameter {parameter}")]
    TypeMismatch { expected: String, got: String, parameter: String },

    #[error("Multiple values supplied for non-array parameter {parameter}: \"{got}\"")]
    CommasNotAllowed { parameter: String, got: String },

    #[error("Parameters of type object (such as {0}) are not allowed in command lines or environment variables")]
    ObjectNotAllowed(String),

    #[error(
        "Parameters of type array must have simple items in command lines or environment variables"
    )]
    ArrayOfComplexTypeNotAllowed(String),
}
