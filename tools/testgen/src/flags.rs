// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// NOTE: The doc comments on `Flags` and its fields appear as the helptext of
/// `fx testgen`. Please run that command to make sure the output looks correct before
/// submitting changes.
use argh::FromArgs;

/// testgen generates a Fuchsia test.
#[derive(FromArgs, Debug)]
pub(crate) struct Flags {
    #[argh(subcommand)]
    pub subcommand: Subcommand,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
pub(crate) enum Subcommand {
    IntegrationTest(crate::cmd_integration_test::IntegrationTestCmd),
}
