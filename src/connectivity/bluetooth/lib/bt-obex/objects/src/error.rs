// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use thiserror::Error;
use xml::{reader::Error as XmlReaderError, writer::Error as XmlWriterError};

/// The error types for packet parsing.
#[derive(Error, Debug)]
pub enum Error {
    /// Error encountered when trying to parse rust object from encoded data.
    #[error("Error writing XML data: {:?}", .0)]
    WriteXml(#[from] XmlWriterError),

    /// Error encountered when trying to read XML elements rust object from encoded data.
    #[error("Error reading from XML data: {:?}", .0)]
    ReadXml(#[from] XmlReaderError),

    /// Error encountered when invalid data was encountered.
    #[error("Invalid data: {:?}", .0)]
    InvalidData(String),

    /// Error encountered when required data is missing.
    #[error("Missing data: {:?}", .0)]
    MissingData(String),

    #[doc(hidden)]
    #[error("__Nonexhaustive error should never be created.")]
    __Nonexhaustive,
}
