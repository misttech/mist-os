// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::HashMap,
    fmt::{Display, Write},
    os::unix::process::CommandExt,
};

use anyhow::Context;
use argh::FromArgs;
use ffx_command::{Ffx, FFX_WRAPPER_INVOKE};
use ffx_config::environment::ExecutableKind::MainFfx;

use camino::Utf8Path;

const NOT_FOUND_CODE: u8 = 127;

struct Exit {
    message: Box<dyn Display>,
    code: u8,
}

impl From<Exit> for u8 {
    fn from(value: Exit) -> Self {
        value.code
    }
}

impl From<anyhow::Error> for Exit {
    fn from(value: anyhow::Error) -> Self {
        let message = Box::new(value);
        let code = NOT_FOUND_CODE;
        Self { message, code }
    }
}

impl From<ffx_command::Error> for Exit {
    fn from(value: ffx_command::Error) -> Self {
        let message = Box::new(value);
        let code = NOT_FOUND_CODE;
        Self { message, code }
    }
}

impl Exit {
    fn from_early_exit(mut error: argh::EarlyExit, cmd: &str) -> Self {
        let code;
        match error.status {
            Ok(()) => {
                // an ok early exit from argh means it was help output from the ffx
                // parse, to which we'll want to add a note about how to see the
                // full list of commands
                Ffx::more_commands_help(&mut error.output, cmd).unwrap();
                code = 0;
            }
            Err(()) => {
                // an error early exit from argh means it was an error parsing
                // arguments and we should add a note saying that this was run
                // through a helper script and it's possible that it doesn't
                // know about a new ffx argument.
                write!(
                    &mut error.output,
                    "\nNote: This command was run through the `fuchsia-sdk-run` binary,\n\
                    which may not recognize ffx command line arguments added after it\n\
                    was built. You may need to update your copy of `fuchsia-sdk-run\n\
                    if the given command line should have parsed correctly."
                )
                .unwrap();
                code = 1;
            }
        }
        let message = Box::new(error.output);
        Self { message, code }
    }

    fn from_failed_exec(sdk: &ffx_config::Sdk, exe_path: &str, err: std::io::Error) -> Self {
        let message = Box::new(format!(
            "fuchsia-sdk-run: Failed to execute tool binary from the active sdk:\n\
            SDK Path: {}\n\
            Tool Path: {exe_path}\n\
            \n\
            Error: {err}",
            sdk.get_path_prefix().to_string_lossy(),
        ));
        let code = NOT_FOUND_CODE;
        Self { message, code }
    }
}

impl Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

struct SdkToolRunner {
    ffx: Ffx,
    arg0: String,
    cmd: String,
    args: Vec<String>,
}

impl SdkToolRunner {
    fn from_args(args: impl IntoIterator<Item = String>) -> Result<Self, Exit> {
        let mut args = args.into_iter();
        let arg0 =
            args.next().context("No arguments provided to fuchsia-sdk-run, nothing to run.")?;
        let cmd = Utf8Path::new(&arg0)
            .file_name()
            .with_context(|| format!("fuchsia-sdk-run: '{arg0}' is not a valid host tool name"))?;

        let args = Vec::from_iter(args);
        let ffx = match cmd {
            "fuchsia-sdk-run" => return Self::from_args(args),
            "ffx" => parse_ffx_args(cmd, &args)?,
            _ => Ffx::default(),
        };
        Ok(Self { ffx, arg0: arg0.to_string(), cmd: cmd.to_string(), args })
    }
}

fn parse_ffx_args(cmd: &str, args: &[impl AsRef<str>]) -> Result<Ffx, Exit> {
    let ffx_args = Vec::from_iter(args.iter().map(AsRef::as_ref));
    Ffx::from_args(&[&cmd], &ffx_args).map_err(|err| Exit::from_early_exit(err, cmd))
}

async fn run(
    args: impl IntoIterator<Item = String>,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<(), Exit> {
    let runner = SdkToolRunner::from_args(args)?;
    let env = runner.ffx.load_context_with_env(MainFfx, HashMap::from_iter(env))?;
    let sdk = env.get_sdk().await?;
    let mut command = sdk.get_host_tool_command(&runner.cmd)?;
    command.args(&runner.args);
    command.env(FFX_WRAPPER_INVOKE, runner.arg0);
    let exe_path = command.get_program().to_string_lossy().into_owned();
    // [`CommandExt::exec`] doesn't return if successful, the current process is
    // replaced, so this just always returns an error if anything at all.
    Err(Exit::from_failed_exec(&sdk, &exe_path, command.exec()))
}

#[fuchsia::main]
async fn main() {
    let exit_code = match run(std::env::args(), std::env::vars()).await {
        Ok(()) => {
            eprintln!("ERROR: fuchsia-sdk-run exited without error or running a host tool!");
            NOT_FOUND_CODE
        }
        Err(err) => {
            eprintln!("{err}");
            err.code
        }
    };
    std::process::exit(exit_code.into())
}
