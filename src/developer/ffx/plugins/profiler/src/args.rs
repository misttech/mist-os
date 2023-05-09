// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use ffx_core::ffx_command;

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
/// Interact with the profiling subsystem.
#[argh(subcommand, name = "profiler")]
pub struct ProfilerCommand {
    #[argh(subcommand)]
    pub sub_cmd: ProfilerSubCommand,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
pub enum ProfilerSubCommand {
    Start(Start),
}

#[derive(FromArgs, PartialEq, Debug)]
/// Record a profile.
#[argh(subcommand, name = "start")]
pub struct Start {
    /// monikers to profile
    #[argh(option)]
    pub monikers: Vec<String>,

    /// pids to profile
    #[argh(option)]
    pub pids: Vec<u64>,

    /// tids to profile
    #[argh(option)]
    pub tids: Vec<u64>,

    /// jobs to profile
    #[argh(option)]
    pub job_ids: Vec<u64>,

    /// how long to profiler for. If unspecified, will interactively wait until <ENTER> is pressed.
    #[argh(option)]
    pub duration: Option<f64>,

    /// name of output trace file.  Defaults to profile.out.
    #[argh(option, default = "String::from(\"profile.out\")")]
    pub output: String,

    /// print stats about how the profiling session went
    #[argh(switch)]
    pub print_stats: bool,
}
