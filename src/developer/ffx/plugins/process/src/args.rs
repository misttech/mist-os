// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use ffx_core::ffx_command;

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "process", description = "Processes related commands")]
pub struct ProcessCommand {
    #[argh(subcommand)]
    pub arg: Args,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
pub enum Args {
    List(ListArg),
    Filter(FilterArg),
    GenerateFuchsiaMap(GenerateFuchsiaMapArg),
    Kill(KillArg),
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(
    subcommand,
    name = "list",
    description = "outputs a list containing the name and koid of all processes"
)]
pub struct ListArg {
    #[argh(
        switch,
        description = "outputs all processes and the kernel objects owned by each of them"
    )]
    pub verbose: bool,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(
    subcommand,
    name = "filter",
    description = "outputs information about the processes that correspond to the koids input"
)]
pub struct FilterArg {
    #[argh(positional, description = "process koids")]
    pub process_koids: Vec<u64>,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(
    subcommand,
    name = "generate-fuchsia-map",
    description = "outputs the json required to generate a map of all processes and channels"
)]
pub struct GenerateFuchsiaMapArg {}

#[derive(PartialEq, Debug)]
pub enum TaskToKill {
    Koid(u64),
    ProcessName(String),
}

fn parse_task(arg: &str) -> Result<TaskToKill, String> {
    Ok(if let Ok(koid) = arg.parse::<u64>() {
        TaskToKill::Koid(koid)
    } else {
        TaskToKill::ProcessName(String::from(arg))
    })
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "kill", description = "Attempts to kill a process by its KOID")]
pub struct KillArg {
    #[argh(positional, from_str_fn(parse_task), description = "koid of process to kill.")]
    pub task_to_kill: TaskToKill,
}
