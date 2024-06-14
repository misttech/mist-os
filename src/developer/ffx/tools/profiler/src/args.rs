// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
/// Interact with the profiling subsystem.
#[argh(subcommand, name = "profiler")]
pub struct ProfilerCommand {
    #[argh(subcommand)]
    pub sub_cmd: ProfilerSubCommand,
}

#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
pub enum ProfilerSubCommand {
    Attach(Attach),
    Launch(Launch),
}

#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
/// Profile a running task or component
#[argh(subcommand, name = "attach")]
#[derive(Default)]
pub struct Attach {
    /// url of a component to profile. If there is no matching component, wait for one to appear.
    #[argh(option)]
    pub url: Option<String>,

    /// moniker of a component to profile. If there is no matching component, the profiler will
    #[argh(option)]
    pub moniker: Option<String>,

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
    pub duration: Option<u64>,

    /// name of output trace file. Defaults to profile.pb.
    #[argh(option, default = "String::from(\"profile.pb\")")]
    pub output: String,

    /// print stats about how the profiling session went
    #[argh(switch)]
    pub print_stats: bool,

    /// if false, output the raw sample file instead of attempting to symbolize it
    #[argh(option, default = "true")]
    pub symbolize: bool,

    /// if false, output the raw symbolized sample file instead of attempting to convert to the
    /// pprof format. Ignored if --symbolize is false.
    #[argh(option, default = "true")]
    pub pprof_conversion: bool,

    /// how frequently to take a sample
    #[argh(option, default = "10000")]
    pub sample_period_us: u64,
}

#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
/// Record a profile.
#[argh(subcommand, name = "launch")]
#[derive(Default)]
pub struct Launch {
    /// url of a component to launch and profile
    #[argh(option)]
    pub url: String,

    /// moniker of a component to attach to and profile. If specified in combination with `--url`,
    /// will attempt to launch the component at the given moniker.
    #[argh(option)]
    pub moniker: Option<String>,

    /// how long in seconds to profile for. If unspecified, will interactively wait until <ENTER> is pressed.
    #[argh(option)]
    pub duration: Option<u64>,

    /// name of output trace file. Defaults to profile.pb.
    #[argh(option, default = "String::from(\"profile.pb\")")]
    pub output: String,

    /// print stats about how the profiling session went
    #[argh(switch)]
    pub print_stats: bool,

    /// if false, output the raw sample file instead of attempting to symbolize it
    #[argh(option, default = "true")]
    pub symbolize: bool,

    /// if false, output the raw symbolized sample file instead of attempting to convert to the
    /// pprof format. Ignored if --symbolize is false.
    #[argh(option, default = "true")]
    pub pprof_conversion: bool,

    /// how frequently to take a sample
    #[argh(option, default = "10000")]
    pub sample_period_us: u64,
}
