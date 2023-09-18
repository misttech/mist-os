// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{FromArgs, SubCommands};
use errors::ffx_error;
use ffx_command::{
    Error, FfxCommandLine, FfxToolInfo, MetricsSession, Result, ToolRunner, ToolSuite,
};
use ffx_config::{environment::ExecutableKind, EnvironmentContext};
use ffx_daemon_proxy::{DaemonVersionCheck, Injection};
use ffx_lib_args::FfxBuiltIn;
use ffx_lib_sub_command::SubCommand;
use fho_search::ExternalSubToolSuite;
use std::{os::unix::process::ExitStatusExt, process::ExitStatus, sync::Arc};

/// The command to be invoked and everything it needs to invoke
struct FfxSubCommand {
    app: FfxCommandLine,
    context: EnvironmentContext,
    cmd: FfxBuiltIn,
}

/// The suite of commands FFX supports.
struct FfxSuite {
    context: EnvironmentContext,
    external_commands: ExternalSubToolSuite,
}

const CIRCUIT_REFRESH_RATE: std::time::Duration = std::time::Duration::from_millis(500);

#[async_trait::async_trait(?Send)]
impl ToolSuite for FfxSuite {
    async fn from_env(env: &EnvironmentContext) -> Result<Self> {
        let context = env.clone();

        let external_commands = ExternalSubToolSuite::from_env(env).await?;

        Ok(Self { context, external_commands })
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
    ) -> Result<Option<Box<(dyn ToolRunner + '_)>>> {
        let context = self.context.clone();
        let app = ffx_cmd.clone();
        let args = Vec::from_iter(app.global.subcommand.iter().map(String::as_str));
        match args.first().copied() {
            Some("commands") => {
                let mut output = String::new();
                self.print_command_list(&mut output).await.ok();
                let code = 0;
                Err(Error::Help { command: ffx_cmd.command.clone(), output, code })
            }
            Some(name) if SubCommand::COMMANDS.iter().any(|c| c.name == name) => {
                let cmd = FfxBuiltIn::from_args(&Vec::from_iter(ffx_cmd.cmd_iter()), &args)
                    .map_err(|err| Error::from_early_exit(&ffx_cmd.command, err))?;
                Ok(Some(Box::new(FfxSubCommand { cmd, context, app })))
            }
            _ => self.external_commands.try_from_args(ffx_cmd).await,
        }
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

    async fn run(self: Box<Self>, metrics: MetricsSession) -> Result<ExitStatus> {
        if self.app.global.machine.is_some()
            && !ffx_lib_suite::ffx_plugin_is_machine_supported(&self.cmd)
        {
            Err(ffx_error!("The machine flag is not supported for this subcommand").into())
        } else {
            metrics.print_notice(&mut std::io::stderr()).await?;
            let redacted_args = ffx_lib_suite::ffx_plugin_redact_args(&self.app, &self.cmd);
            let res = run_legacy_subcommand(self.app, self.context, self.cmd)
                .await
                .map(|_| ExitStatus::from_raw(0));
            metrics.command_finished(res.is_ok(), &redacted_args).await.and(res)
        }
    }
}

async fn run_legacy_subcommand(
    app: FfxCommandLine,
    context: EnvironmentContext,
    subcommand: FfxBuiltIn,
) -> Result<()> {
    let router_interval = if is_daemon(&subcommand) { Some(CIRCUIT_REFRESH_RATE) } else { None };
    let daemon_version_string = DaemonVersionCheck::SameBuildId(context.daemon_version_string()?);
    tracing::debug!("initializing overnet");
    let injector = Injection::initialize_overnet(
        context,
        router_interval,
        daemon_version_string,
        app.global.machine,
        app.global.target().await?,
    )
    .await?;
    tracing::debug!("Overnet initialized, creating injector");
    let injector: Arc<dyn ffx_core::Injector> = Arc::new(injector);
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
    let result = ffx_command::run::<FfxSuite>(ExecutableKind::MainFfx).await;
    ffx_command::exit(result).await
}
