// Copyright 2023 The Fuchsia Authors. All rights reserved.
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
#[derive(Debug, Error, Eq, PartialEq)]
pub enum UsageError {
    // TODO(b/393444515): replace 'detail' fields below with actual errors and use #[source].
    #[error("Schema file {path} could not be opened for reading: {detail}")]
    FailedToOpenSchema { path: PathBuf, detail: String },

    #[error("Failure attempting to parse schema {path}: {detail}")]
    FailedToParseSchema { path: PathBuf, detail: String },

    #[error("Schema not well-formed: {0}")]
    InvalidSchema(String),

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
