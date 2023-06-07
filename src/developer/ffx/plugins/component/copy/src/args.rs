// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use ffx_core::ffx_command;

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "copy",
    description = "copies files to/from a component's namespace. \n\
                   Paths may be any combination of local or remote paths.",
    example = "To copy from a component to a local path: \n\
               ffx component copy /some/moniker::/path/file./txt /local/file.txt \n\n\
               To copy from a local path to a component : \n\
               ffx component copy /local/file.txt /some/moniker::/dir/file.txt\n\n\
               To copy between two components: \n\
               ffx component copy /some/moniker::/dir/file.txt /some/moniker::/dir/file.txt\n\n\
               To copy multiple files: \n\
               ffx component copy /some/moniker::/dir/* /some/local/dir\n\
               ffx component copy /file/one.txt /file/two.txt ... /some/moniker::/dir/",
    note = "To learn more about the command see https://fuchsia.dev/fuchsia-src/development/sdk/ffx/copy-files-to-and-from-a-component"
)]
pub struct CopyComponentCommand {
    #[argh(positional)]
    /// paths to copy where the last argument is the destination, formatted as one of:
    /// a local path (/some/dir/file.txt), or a remote component namespace path (/some/component/moniker::/some/file.txt)
    pub paths: Vec<String>,
    /// verbose output: outputs a line for each file copied.
    #[argh(switch, short = 'v')]
    pub verbose: bool,
}
