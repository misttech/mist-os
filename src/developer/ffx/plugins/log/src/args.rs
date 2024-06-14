// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Bypass warning about not using the argh and ffx_core crates, which are
// imported automatically by ffx_plugin. These can be removed when the plugin is
// no longer compiled as part of ffx.
use {argh as _, ffx_core as _};

pub use log_command::{DumpCommand, LogCommand, LogSubCommand, TimeFormat, WatchCommand};

// This does what the `ffx_command` proc macro would
// do if this type were implemented here.
pub type FfxPluginCommand = LogCommand;
