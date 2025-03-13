// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::html::HtmlCommand;
use crate::print::PrintCommand;
use crate::process::ProcessCommand;
use anyhow::Result;
use argh::FromArgs;

mod html;
mod print;
mod process;
mod types;

#[derive(FromArgs)]
/// collect and generate stats on Fuchsia packages
struct Args {
    #[argh(subcommand)]
    cmd: CommandArgs,
}

#[derive(FromArgs)]
#[argh(subcommand)]
enum CommandArgs {
    Process(ProcessCommand),
    Html(HtmlCommand),
    Print(PrintCommand),
}

#[fuchsia::main]
fn main() -> Result<()> {
    let args: Args = argh::from_env();

    match args.cmd {
        CommandArgs::Process(cmnd) => cmnd.execute(),
        CommandArgs::Html(cmnd) => cmnd.execute(),
        CommandArgs::Print(cmnd) => cmnd.execute(),
    }
}
