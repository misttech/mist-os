// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};

#[derive(ArgsInfo, FromArgs, Clone, Debug, PartialEq)]
#[argh(subcommand, name = "filter")]
/// commands for configuring packet filtering
pub struct Filter {
    #[argh(subcommand)]
    pub filter_cmd: FilterEnum,
}

#[derive(ArgsInfo, FromArgs, Clone, Debug, PartialEq)]
#[argh(subcommand)]
pub enum FilterEnum {
    List(List),
}

/// A command to list filtering configuration.
#[derive(Clone, Debug, ArgsInfo, FromArgs, PartialEq)]
#[argh(subcommand, name = "list")]
pub struct List {}
