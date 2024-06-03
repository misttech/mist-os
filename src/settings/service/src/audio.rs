// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub use self::audio_default_settings::build_audio_default_settings;
#[cfg(test)]
pub(crate) use self::audio_default_settings::create_default_audio_stream;
pub(crate) use self::audio_default_settings::{
    create_default_modified_counters, AudioInfoLoader, ModifiedCounters,
};
pub use self::stream_volume_control::StreamVolumeControl;
pub mod audio_controller;
pub mod types;

mod audio_default_settings;
mod audio_fidl_handler;
mod stream_volume_control;

/// Mod containing utility functions for audio-related functionality.
pub(crate) mod utils;
