// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use ffx_core::ffx_command;

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq, Clone)]
#[argh(
    subcommand,
    name = "get-time",
    description = "Gets the monotonic time on the target device",
    example = "To get the target time:

    $ ffx target get-time
"
)]

pub struct GetTimeCommand {}
