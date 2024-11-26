// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use errors::IntoExitCode;
pub use ffx_command_error::*;
use ffx_config::environment::ExecutableKind;
use fuchsia_async::TimeoutExt;
use std::fs::File;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::time::Duration;

mod args_info;
mod describe;
mod ffx;
mod metrics;
mod tools;

pub use args_info::*;
pub use ffx::*;
pub use metrics::*;
pub use tools::*;

fn stamp_file(stamp: &Option<String>) -> Result<Option<File>> {
    let Some(stamp) = stamp else { return Ok(None) };
    File::create(stamp)
        .with_bug_context(|| format!("Failure creating stamp file '{stamp}'"))
        .map(Some)
}

fn write_exit_code<W: Write>(res: &Result<ExitStatus>, out: &mut W) {
    let exit_code = match res {
        Ok(status) => status.code().unwrap_or(1),
        Err(err) => err.exit_code(),
    };
    write!(out, "{}\n", exit_code).ok();
}

/// Tries to report the given unexpected error to analytics if appropriate
#[tracing::instrument(skip(err))]
pub async fn report_bug(err: &impl std::fmt::Display) {
    // TODO(66918): make configurable, and evaluate chosen time value.
    if let Err(e) = analytics::add_crash_event(&format!("{}", err), None)
        .on_timeout(Duration::from_secs(2), || {
            tracing::error!("analytics timed out reporting crash event");
            Ok(())
        })
        .await
    {
        tracing::error!("analytics failed to submit crash event: {}", e);
    }
}

#[tracing::instrument]
pub async fn run<T: ToolSuite>(exe_kind: ExecutableKind) -> Result<ExitStatus> {
    let mut return_args_info = false;
    let mut return_help: Option<Error> = None;
    let cmd = match ffx::FfxCommandLine::from_env() {
        Ok(c) => c,
        Err(Error::Help { command, output, code }) => {
            // Check for machine json output and  help
            // This is a little bit messy since the command line is not returned
            // when a help error is returned. So look for the `--machine json` flag
            // and either `help` or `--help` or `-h`.
            let argv = Vec::from_iter(std::env::args());
            let c = ffx::FfxCommandLine::from_args_for_help(&argv)?;
            if find_machine_and_help(&c).is_some() {
                return_args_info = true;
                c
            } else {
                return_help = Some(Error::Help { command, output, code });
                c
            }
        }

        Err(e) => return Err(e),
    };
    let app = &cmd.global;

    let context = app.load_context(exe_kind)?;

    ffx_config::init(&context).await?;

    // Everything that needs to use the config must be after loading the config.
    if !context.has_no_environment() {
        context.env_file_path().map_err(|e| {
            let output = format!("ffx could not determine the environment configuration path: {}\nEnsure that $HOME is set, or pass the --env option to specify an environment configuration path", e);
            let code = 1;
            Error::Help { command: cmd.command.clone(), output, code }
        })?;
    }

    let tools = T::from_env(&context).await?;

    if return_args_info {
        // This handles the top level ffx command information and prints the information
        // for all subcommands.
        let args = tools.get_args_info().await?;
        let output = match cmd.global.machine.unwrap() {
            ffx_writer::Format::Json => serde_json::to_string(&args),
            ffx_writer::Format::JsonPretty => serde_json::to_string_pretty(&args),
        };
        println!("{}", output.bug_context("Error serializing args")?);
        return Ok(ExitStatus::from_raw(0));
    }
    match return_help {
        Some(Error::Help { command, output, code }) => {
            let mut commands: String = Default::default();
            tools
                .print_command_list(&mut commands)
                .await
                .bug_context("Error getting command list")?;
            let full_output = format!("{output}\n{commands}");
            return Err(Error::Help { command, output: full_output, code });
        }
        _ => (),
    };

    // If the schema is requested, then find the tool runner for the requested tool
    // and return the schema. try_from_args() can't be used since it is common for
    // commands to have positional arguments and they are present if the schema is
    // requested.
    let tool = if app.schema && app.machine.is_some() {
        tools.try_runner_from_name(&cmd).await?
    } else {
        match tools.try_from_args(&cmd).await {
            Ok(t) => t,
            Err(Error::Help { command, output, code }) => {
                // TODO(b/303088345): Enhance argh to support custom help better.
                // Check for machine json output and  help.
                // This handles the sub command of ffx information.
                if let Some(machine_format) = find_machine_and_help(&cmd) {
                    let all_info = tools.get_args_info().await?;
                    // Tools will return the top level args info, so
                    //iterate over the subcommands to get to the right level
                    let mut info: CliArgsInfo = all_info;
                    for c in cmd.subcmd_iter() {
                        if c.starts_with("-") {
                            continue;
                        }
                        if info.name == c {
                            continue;
                        }
                        info = info
                            .commands
                            .iter()
                            .find(|s| s.name == c)
                            .map(|s| s.command.clone().into())
                            .unwrap_or(info);
                    }
                    let output = match machine_format {
                        ffx_writer::Format::Json => serde_json::to_string(&info),
                        ffx_writer::Format::JsonPretty => serde_json::to_string_pretty(&info),
                    };
                    println!("{}", output.bug_context("Error serializing args")?);
                    return Ok(ExitStatus::from_raw(0));
                } else {
                    return Err(Error::Help { command, output, code });
                }
            }

            Err(e) => return Err(e),
        }
    };

    // Yecch, this is unreasonably specific. But it is to preserve compatibility
    // with the daemon, which is going away, at which point this code can also
    // go away.
    let log_dest = if app.subcommand.len() >= 2
        && app.subcommand[0..2] == ["daemon", "start"]
        && app.log_destination.is_none()
    {
        // The daemon should by default produce output on stdout, not ffx.log,
        // because integrators who turn off daemon.autostart are expecting to
        // manage the output.
        Some(ffx_config::logging::LogDestination::Stdout)
    } else {
        app.log_destination.clone()
    };

    ffx_config::logging::init(&context, app.verbose, &log_dest)?;
    tracing::info!("starting command: {:?}", Vec::from_iter(cmd.all_iter()));
    tracing::info!("with context: {kind:#?}", kind = context.env_kind());

    let metrics = MetricsSession::start(&context).await?;
    tracing::debug!("metrics session started");

    let stamp = stamp_file(&app.stamp)?;
    tracing::debug!("stamp file created, running tool");
    let res = match tool {
        Some(tool) => tool.run(metrics).await,
        // since we didn't run a subtool, do the metrics ourselves
        None => Err(cmd.no_handler_help(metrics, &tools).await?),
    };

    // Write to our stamp file if it was requested
    if let Some(mut stamp) = stamp {
        write_exit_code(&res, &mut stamp);
        if !context.is_isolated() {
            stamp.sync_all().bug_context("Error syncing exit code stamp write")?;
        }
    }

    res
}

/// Terminates the process, outputting errors as appropriately and with the indicated exit code.
pub async fn exit(res: Result<ExitStatus>) -> ! {
    const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

    let exit_code = res.exit_code();
    match res {
        Err(Error::Help { output, .. }) => {
            writeln!(&mut std::io::stdout(), "{output}").unwrap();
        }
        Err(err @ Error::Config(_)) | Err(err @ Error::User(_)) => {
            let mut out = std::io::stderr();
            // abort hard on a failure to print the user error somehow
            writeln!(&mut out, "{err}").unwrap();
        }
        Err(err @ Error::Unexpected(_)) => {
            let mut out = std::io::stderr();
            // abort hard on a failure to print the unexpected error somehow
            writeln!(&mut out, "{err}").unwrap();
            report_bug(&err).await;
            ffx_config::print_log_hint(&mut out).await;
        }
        Ok(_) | Err(Error::ExitWithCode(_)) => (),
    }

    if timeout::timeout(SHUTDOWN_TIMEOUT, fuchsia_async::emulated_handle::shut_down_handles())
        .await
        .is_err()
    {
        tracing::warn!("Timed out shutting down handles");
    };

    std::process::exit(exit_code);
}

/// look through the command line args for `--machine <format>`
/// and --help or help or -h. This is used to indicate the
/// JSON arg info should be returned.
fn find_machine_and_help(cmd: &FfxCommandLine) -> Option<ffx_writer::Format> {
    if cmd.subcmd_iter().any(|c| c == "help" || c == "--help" || c == "-h") {
        cmd.global.machine
    } else {
        None
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::io::BufWriter;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stamp_file_creation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stamp").into_os_string().into_string().ok();
        let stamp = stamp_file(&path);

        assert!(stamp.unwrap().is_some());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stamp_file_no_create() {
        let no_stamp = stamp_file(&None);
        assert!(no_stamp.unwrap().is_none());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_write_exit_code() {
        let mut out = BufWriter::new(Vec::new());
        write_exit_code(&Ok(ExitStatus::from_raw(0)), &mut out);
        assert_eq!(String::from_utf8(out.into_inner().unwrap()).unwrap(), "0\n");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_write_exit_code_on_failure() {
        let mut out = BufWriter::new(Vec::new());
        write_exit_code(&Result::<ExitStatus>::Err(Error::from(anyhow::anyhow!("fail"))), &mut out);
        assert_eq!(String::from_utf8(out.into_inner().unwrap()).unwrap(), "1\n")
    }
}
