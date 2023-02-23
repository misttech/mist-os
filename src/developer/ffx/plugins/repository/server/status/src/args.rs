// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {argh::FromArgs, ffx_core::ffx_command};

/// Retrieves the status of the repository server.
///
/// If server is running will also return socket address. Note that all repositories under
/// `ffx repository list` will be running as subpaths at the returned address.
#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "status")]
pub struct StatusCommand {}
