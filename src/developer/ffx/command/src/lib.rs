// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use errors::{ffx_error, IntoExitCode};
use fuchsia_async::TimeoutExt;
use itertools::Itertools;
use std::fs::File;
use std::io::Write;
use std::process::ExitStatus;
use std::str::FromStr;
use std::time::Duration;

mod describe;
mod error;
mod ffx;
mod metrics;
mod tools;

pub use error::*;
pub use ffx::*;
pub use metrics::*;
pub use tools::*;

fn stamp_file(stamp: &Option<String>) -> anyhow::Result<Option<File>> {
    if let Some(stamp) = stamp {
        Ok(Some(File::create(stamp)?))
    } else {
        Ok(None)
    }
}

fn write_exit_code<W: Write>(res: &Result<ExitStatus>, out: &mut W) {
    let exit_code = match res {
        Ok(status) => status.code().unwrap_or(1),
        Err(err) => err.exit_code(),
    };
    write!(out, "{}\n", exit_code).ok();
}

/// If given an error result, prints a user-meaningful interpretation of it
/// to the given output handle
pub async fn report_user_error(err: &Error) -> anyhow::Result<()> {
    // Report BUG errors as crash events
    if let Error::Unexpected(err) = err {
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
    Ok(())
}

pub async fn run<T: ToolSuite>() -> Result<ExitStatus> {
    let cmd = ffx::FfxCommandLine::from_env().map_err(T::add_globals_to_help)?;
    let app = &cmd.global;

    let context = app.load_context()?;

    ffx_config::init(&context).await?;

    context.env_file_path().map_err(|e| {
        let output = format!("ffx could not determine the environment configuration path: {}\nEnsure that $HOME is set, or pass the --env option to specify an environment configuration path", e);
        let code = 1;
        Error::Help { command: cmd.command.clone(), output, code }
    })?;

    let tools = T::from_env(&context)?;

    let tool = tools.try_from_args(&cmd).await?;
    // If the line above succeeds, then this will succeed as well.
    let sanitized_args = match tools.redact_arg_values(&cmd) {
        Ok(a) => format!("{} {}", cmd.redact_args_flags_only(), a[1..].join(" ")).trim().to_owned(),
        Err(e) => format!("{e}"),
    };

    let log_to_stdio = tool.as_ref().map(|tool| tool.forces_stdout_log()).unwrap_or(false);
    ffx_config::logging::init(log_to_stdio || app.verbose, !log_to_stdio).await?;
    tracing::info!("starting command: {:?}", Vec::from_iter(cmd.all_iter()));

    // Since this is invoking the config, this must be run _after_ ffx_config::init.
    let log_level = app.log_level().await?;
    simplelog::LevelFilter::from_str(log_level.as_str()).with_user_message(|| {
        ffx_error!("'{log_level}' is not a valid log level. Supported log levels are 'Off', 'Error', 'Warn', 'Info', 'Debug', and 'Trace'")
    })?;

    let metrics = MetricsSession::start(&context).await?;
    metrics.print_notice(&mut std::io::stderr()).await?;

    let stamp = stamp_file(&app.stamp)?;
    let res = if let Some(tool) = tool {
        tool.run().await
    } else {
        Err(ffx_error!(
            "No configured tool suites were able to handle the command line: {}",
            cmd.all_iter().join(" ")
        )
        .into())
    };

    metrics.command_finished(res.is_ok(), sanitized_args).await?;

    // Write to our stamp file if it was requested
    if let Some(mut stamp) = stamp {
        write_exit_code(&res, &mut stamp);
        stamp.sync_all().bug_context("Syncing exit code stamp write")?;
    }

    res
}

/// Terminates the process, outputting errors as appropriately and with the indicated exit code.
pub async fn exit(res: Result<ExitStatus>) -> ! {
    let exit_code = res.exit_code();
    match res {
        Err(Error::Help { output, .. }) => {
            writeln!(&mut std::io::stdout(), "{output}").unwrap();
        }
        Err(Error::Config(err)) => {
            let mut out = std::io::stderr();
            // abort hard on a failure to print the user error somehow
            writeln!(&mut out, "{err}").unwrap();
            // note: we don't try to report this error or print a log hint
            // because we don't expect to have enough information to do either
            // of those things here.
        }
        Err(err) => {
            let mut out = std::io::stderr();
            // abort hard on a failure to print the user error somehow
            writeln!(&mut out, "{err}").unwrap();
            report_user_error(&err).await.unwrap();
            ffx_config::print_log_hint(&mut out).await;
        }
        _ => (),
    }
    std::process::exit(exit_code);
}

#[cfg(test)]
mod test {
    use super::*;
    use std::{io::BufWriter, os::unix::process::ExitStatusExt};
    use tempfile;

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
