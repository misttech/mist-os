// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{collections::HashSet, fmt::Write, path::PathBuf, process::ExitStatus};

use crate::{Error, FfxCommandLine, MetricsSession, Result};
use async_trait::async_trait;
use ffx_config::EnvironmentContext;

/// Where the command was discovered
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FfxToolSource {
    /// built directly into the executable
    BuiltIn,
    /// discovered in a development tree or workspace tree
    Workspace,
    /// discovered in the currently active SDK
    Sdk,
}

/// Information about a tool for use in help output
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FfxToolInfo {
    /// Where the tool was found
    pub source: FfxToolSource,
    /// The short name of the tool
    pub name: String,
    /// A longer one-line description of the functionality of the tool
    pub description: String,
    /// A path to the executable for the tool, if there is one
    pub path: Option<PathBuf>,
}

impl FfxToolInfo {
    fn write_description(&self, out: &mut String) {
        crate::describe::write_description(out, &self.name, &self.description)
    }
}

impl From<&argh::CommandInfo> for FfxToolInfo {
    fn from(info: &argh::CommandInfo) -> Self {
        let source = FfxToolSource::BuiltIn;
        let name = info.name.to_owned();
        let description = info.description.to_owned();
        let path = None;
        FfxToolInfo { source, name, description, path }
    }
}

#[async_trait(?Send)]
pub trait ToolRunner {
    fn forces_stdout_log(&self) -> bool;
    async fn run(self: Box<Self>, metrics: MetricsSession) -> Result<ExitStatus, Error>;
}

/// Implements discovering and loading the subtools a particular ffx binary
/// is capable of running.
#[async_trait::async_trait(?Send)]
pub trait ToolSuite: Sized {
    /// Initializes the tool suite from the ffx invocation's environment.
    async fn from_env(env: &EnvironmentContext) -> Result<Self, Error>;

    /// Lists commands that should be available no matter how and where this tool
    /// is invoked.
    fn global_command_list() -> &'static [&'static argh::CommandInfo];

    /// Lists all commands reachable from the current context. Defaults to just
    /// the same set of commands as in [`Self::global_command_list`].
    async fn command_list(&self) -> Vec<FfxToolInfo> {
        Self::global_command_list().iter().copied().map(|cmd| cmd.into()).collect()
    }

    /// Parses the given command line information into a runnable command
    /// object.
    async fn try_from_args(
        &self,
        cmd: &FfxCommandLine,
    ) -> Result<Option<Box<dyn ToolRunner + '_>>, Error>;

    /// Parses the given command line information into a runnable command
    /// object, exiting and printing the early exit output if help is requested
    /// or an error occurs.
    async fn from_args(&self, cmd: &FfxCommandLine) -> Option<Box<dyn ToolRunner + '_>> {
        self.try_from_args(cmd).await.unwrap_or_else(|early_exit| {
            print!("{}", early_exit);

            std::process::exit(early_exit.exit_code())
        })
    }

    /// Prints out a list of the commands this suite has available
    async fn print_command_list(&self, w: &mut impl Write) -> Result<(), std::fmt::Error> {
        print_command_list(w, &self.command_list().await)
    }

    /// Finds the given tool by name in the available command list
    async fn find_tool_by_name(&self, name: &str) -> Option<FfxToolInfo> {
        self.command_list().await.into_iter().find(|cmd| cmd.name == name)
    }

    /// Use this with `map_err` on a result from [`crate::FfxCommandLine::from_env`]
    /// call to append the global command list to the help, if that's the kind of error
    /// this had.
    fn add_globals_to_help(err: Error) -> Error {
        match err {
            Error::Help { command, mut output, code } => {
                let cmd = command.join(" ");
                writeln!(&mut output).and_then(|_| {
                    writeln!(&mut output)?;
                    print_command_list(
                        &mut output,
                        &Vec::from_iter(
                            Self::global_command_list().iter().cloned().map(FfxToolInfo::from),
                        ),
                    )?;
                    writeln!(&mut output, "Note: There may be more commands available, use `{cmd} commands` for a complete list.")?;
                    writeln!(&mut output, "See '{cmd} <command> help' for more information on a specific command.")
                }).expect("Failed to append command list to help");
                Error::Help { command, output, code }
            }
            err => err,
        }
    }
}

fn print_command_list(w: &mut impl Write, commands: &[FfxToolInfo]) -> Result<(), std::fmt::Error> {
    let mut found = HashSet::new();
    let mut built_in = None;
    let mut workspace = None;
    let mut sdk = None;
    for cmd in commands {
        use FfxToolSource::*;
        if !found.contains(&cmd.name) {
            found.insert(cmd.name.clone());
            let kind = match cmd.source {
                BuiltIn => built_in.get_or_insert_with(String::new),
                Workspace => workspace.get_or_insert_with(String::new),
                Sdk => sdk.get_or_insert_with(String::new),
            };
            cmd.write_description(kind);
        }
    }

    if let Some(built_in) = built_in {
        writeln!(w, "Built-in Commands:\n{built_in}\n")?;
    }
    if let Some(workspace) = workspace {
        writeln!(w, "Workspace Commands:\n{workspace}\n")?;
    }
    if let Some(sdk) = sdk {
        writeln!(w, "SDK Commands:\n{sdk}\n")?;
    }
    Ok(())
}
