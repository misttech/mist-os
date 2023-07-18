// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod dynamic_writer;
mod machine_writer;
mod simple_writer;
mod test_buffer;
mod tool_io;

pub use dynamic_writer::*;
pub use machine_writer::*;
pub use simple_writer::*;
pub use test_buffer::*;
pub use tool_io::*;

#[derive(thiserror::Error, Debug)]
#[error("Error while presenting output")]
pub enum Error {
    #[error("Error on the underlying IO stream")]
    Io(#[from] std::io::Error),
    #[error("Error formatting JSON output")]
    Json(#[from] serde_json::Error),
    #[error("Error parsing utf8 from buffer")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("`{0}` is not a valid machine format")]
    InvalidFormat(String),
}

type Result<O, E = Error> = std::result::Result<O, E>;

/// The valid formats possible to output for machine consumption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    JsonPretty,
}

impl std::str::FromStr for Format {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_ref() {
            "json-pretty" => Ok(Format::JsonPretty),
            "json" | "j" => Ok(Format::Json),
            other => Err(Error::InvalidFormat(other.to_owned())),
        }
    }
}

impl From<Error> for ffx_command_error::Error {
    fn from(error: Error) -> Self {
        use ffx_command_error::Error::*;
        use Error::*;
        match error {
            error @ (Io(_) | Json(_) | Utf8(_)) => Unexpected(error.into()),
            error @ InvalidFormat(_) => User(error.into()),
        }
    }
}
