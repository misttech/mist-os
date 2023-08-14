// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use argh::SubCommand;
use log_command as log_utils;
pub use log_utils::DumpCommand;
pub use log_utils::LogCommand;
pub use log_utils::LogSubCommand;
pub use log_utils::TimeFormat;
pub use log_utils::WatchCommand;

use ffx_core::ffx_command;

#[ffx_command()]
#[derive(Clone, Debug, PartialEq)]
pub struct FfxLogCommand {
    pub cmd: LogCommand,
}

impl SubCommand for FfxLogCommand {
    const COMMAND: &'static argh::CommandInfo = LogCommand::COMMAND;
}

impl FromArgs for FfxLogCommand {
    fn from_args(command_name: &[&str], args: &[&str]) -> Result<Self, argh::EarlyExit> {
        Ok(Self { cmd: LogCommand::from_args(command_name, args)? })
    }
}
