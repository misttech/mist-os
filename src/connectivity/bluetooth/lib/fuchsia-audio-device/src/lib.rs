// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "256"]

pub use crate::types::{Error, Result};

/// Generic types
#[macro_use]
mod types;

/// Software Audio Driver
pub mod driver;

/// Audio Frame Stream (output stream)
pub mod audio_frame_stream;

pub use audio_frame_stream::AudioFrameStream;

/// Audio Frame Sink (input sink)
pub mod audio_frame_sink;

pub use audio_frame_sink::AudioFrameSink;

/// Frame VMO Helper
mod frame_vmo;
