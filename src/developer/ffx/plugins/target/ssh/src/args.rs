// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use ffx_core::ffx_command;

#[ffx_command()]
#[derive(FromArgs, Default, Debug, Eq, PartialEq)]
#[argh(
    subcommand,
    name = "ssh",
    description = "SSH to a target device",
    example = "To ssh to a specific device:

    $ ffx -t fuchsia-EEEE-NNNN target ssh

To ssh and run a command:

    $ ffx target ssh 'echo $USER'
"
)]
pub struct SshCommand {
    // Custom ssh config file
    #[argh(option, description = "path to the custom ssh config file to use.")]
    pub sshconfig: Option<String>,

    // Custom private key
    #[argh(
        option,
        description = "path to the private key file - will default to the value configured for \
           `ssh.pub` key in ffx config."
    )]
    pub private_key: Option<String>,

    #[argh(positional, description = "command to run on the target. If blank drops into a shell")]
    pub command: Vec<String>,
}
