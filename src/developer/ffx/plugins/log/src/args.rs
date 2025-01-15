// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub use log_command::{DumpCommand, LogCommand, LogSubCommand, TimeFormat, WatchCommand};

// This does what the `ffx_command` proc macro would
// do if this type were implemented here.
pub type FfxPluginCommand = LogCommand;
