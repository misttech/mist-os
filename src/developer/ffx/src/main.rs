// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{FromArgs, SubCommands};
use errors::ffx_error;
use ffx_command::{
    argh_to_ffx_err, DaemonVersionCheck, Error, Ffx, FfxCommandLine, FfxContext, FfxToolInfo,
    Result, ToolRunner, ToolSuite,
};
use ffx_config::EnvironmentContext;
use ffx_lib_args::FfxBuiltIn;
use ffx_lib_sub_command::SubCommand;
use fho::ExternalSubToolSuite;
use std::{os::unix::process::ExitStatusExt, process::ExitStatus};

/// The command to be invoked and everything it needs to invoke
struct FfxSubCommand {
    app: Ffx,
    context: EnvironmentContext,
    cmd: FfxBuiltIn,
}

/// The suite of commands FFX supports.
struct FfxSuite {
    app: Ffx,
    context: EnvironmentContext,
    external_commands: ExternalSubToolSuite,
}

const CIRCUIT_REFRESH_RATE: std::time::Duration = std::time::Duration::from_millis(500);

#[async_trait::async_trait(?Send)]
impl ToolSuite for FfxSuite {
    fn from_env(app: &Ffx, env: &EnvironmentContext) -> Result<Self> {
        let app = app.clone();
        let context = env.clone();

        let external_commands = ExternalSubToolSuite::from_env(&app, env)?;

        Ok(Self { app, context, external_commands })
    }

    fn global_command_list() -> &'static [&'static argh::CommandInfo] {
        SubCommand::COMMANDS
    }

    async fn command_list(&self) -> Vec<FfxToolInfo> {
        let builtin_commands = SubCommand::COMMANDS.iter().copied().map(FfxToolInfo::from);

        builtin_commands.chain(self.external_commands.command_list().await.into_iter()).collect()
    }

    async fn try_from_args(
        &self,
        ffx_cmd: &FfxCommandLine,
        args: &[&str],
    ) -> Result<Option<Box<(dyn ToolRunner + '_)>>> {
        let context = self.context.clone();
        let app = self.app.clone();
        match args.first().copied() {
            Some("commands") => {
                let mut output = String::new();
                self.print_command_list(&mut output).await.ok();
                let code = 0;
                Err(Error::Help { output, code })
            }
            Some(name) if SubCommand::COMMANDS.iter().any(|c| c.name == name) => {
                let cmd = FfxBuiltIn::from_args(&Vec::from_iter(ffx_cmd.cmd_iter()), args)
                    .map_err(argh_to_ffx_err)?;
                Ok(Some(Box::new(FfxSubCommand { cmd, context, app })))
            }
            Some(name) => match self.external_commands.try_from_args(ffx_cmd, args).await? {
                Some(tool) => Ok(Some(tool)),
                _ => {
                    let mut output = format!(
                        "Unknown ffx tool `{name}`. Did you mean one of the following?\n\n"
                    );
                    self.print_command_list(&mut output).await.ok();
                    let code = 1;
                    return Err(Error::Help { output, code });
                }
            },
            None => {
                let help_res = Ffx::from_args(&Vec::from_iter(ffx_cmd.cmd_iter()), &["help"]);
                match help_res {
                    Ok(_) => Ok(None),
                    Err(help_err) => {
                        let mut output = help_err.output;
                        let code = help_err.status.map_or(1, |_| 0);
                        self.print_command_list(&mut output).await.ok();
                        Err(Error::Help { output, code })
                    }
                }
            }
        }
    }

    fn redact_arg_values(&self, ffx_cmd: &FfxCommandLine, args: &[&str]) -> Result<Vec<String>> {
        let cmd_vec = Vec::from_iter(ffx_cmd.cmd_iter());
        FfxBuiltIn::redact_arg_values(&cmd_vec, args).map_err(argh_to_ffx_err)
    }
}

#[async_trait::async_trait(?Send)]
impl ToolRunner for FfxSubCommand {
    /// Whether the given subcommand forces logging to stdout
    fn forces_stdout_log(&self) -> bool {
        match &self.cmd {
            subcommand @ FfxBuiltIn { subcommand: Some(_) } if is_daemon(subcommand) => true,
            _ => false,
        }
    }

    async fn run(self: Box<Self>) -> Result<ExitStatus> {
        use SubCommand::FfxSchema;
        match self.cmd {
            FfxBuiltIn { subcommand: Some(FfxSchema(_)) } => {
                ffx_lib_suite::ffx_plugin_writer_all_output(0);
                Ok(ExitStatus::from_raw(0))
            }
            subcommand => {
                if self.app.machine.is_some()
                    && !ffx_lib_suite::ffx_plugin_is_machine_supported(&subcommand)
                {
                    Err(ffx_error!("The machine flag is not supported for this subcommand").into())
                } else {
                    run_legacy_subcommand(self.app, self.context, subcommand)
                        .await
                        .map(|_| ExitStatus::from_raw(0))
                        .map_err(Error::from)
                }
            }
        }
    }
}

async fn run_legacy_subcommand(
    app: Ffx,
    context: EnvironmentContext,
    subcommand: FfxBuiltIn,
) -> Result<()> {
    let router_interval = if is_daemon(&subcommand) { Some(CIRCUIT_REFRESH_RATE) } else { None };
    let cache_path = context.get_cache_path()?;
    let hoist_cache_dir = std::fs::create_dir_all(&cache_path)
        .and_then(|_| tempfile::tempdir_in(&cache_path))
        .user_message("Unable to create hoist cache directory")?;
    let injector = app
        .initialize_overnet(
            hoist_cache_dir.path(),
            router_interval,
            DaemonVersionCheck::SameBuildId(context.daemon_version_string()?),
        )
        .await?;
    ffx_lib_suite::ffx_plugin_impl(&injector, subcommand).await
}

fn is_daemon(subcommand: &FfxBuiltIn) -> bool {
    use ffx_daemon_plugin_args::FfxPluginCommand;
    use ffx_daemon_plugin_sub_command::SubCommand::FfxDaemonStart;
    use SubCommand::FfxDaemonPlugin;
    matches!(
        subcommand,
        FfxBuiltIn {
            subcommand: Some(FfxDaemonPlugin(FfxPluginCommand { subcommand: FfxDaemonStart(_) }))
        }
    )
}

#[fuchsia_async::run_singlethreaded]
async fn main() {
    let result = ffx_command::run::<FfxSuite>().await;
    ffx_command::exit(result).await
}
