// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{CommandInfo, FromArgs, SubCommand, SubCommands};
use async_trait::async_trait;
use ffx_command::{
    Error, FfxCommandLine, FfxContext, MetricsSession, Result, ToolRunner, ToolSuite,
};
use ffx_config::environment::ExecutableKind;
use ffx_config::EnvironmentContext;
use ffx_daemon_proxy::{DaemonVersionCheck, Injection};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::Arc;
use std::{fs::File, path::PathBuf};

use crate::{FhoEnvironment, FhoToolMetadata, TryFromEnv};

/// The main trait for defining an ffx tool. This is not intended to be implemented directly
/// by the user, but instead derived via `#[derive(FfxTool)]`.
#[async_trait(?Send)]
pub trait FfxTool: FfxMain + Sized {
    type Command: FromArgs + SubCommand;

    fn forces_stdout_log(&self) -> bool;
    fn supports_machine_output(&self) -> bool;

    async fn from_env(env: FhoEnvironment, cmd: Self::Command) -> Result<Self>;

    /// Executes the tool. This is intended to be invoked by the user in main.
    async fn execute_tool() {
        let result = ffx_command::run::<FhoSuite<Self>>(ExecutableKind::Subtool).await;
        ffx_command::exit(result).await;
    }
}

#[async_trait(?Send)]
pub trait FfxMain: Sized {
    type Writer: TryFromEnv;

    /// The entrypoint of the tool. Once FHO has set up the environment for the tool, this is
    /// invoked. Should not be invoked directly unless for testing.
    async fn main(self, writer: Self::Writer) -> Result<()>;
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub(crate) enum FhoHandler<M: FfxTool> {
    //FhoVersion1(M),
    /// Run the tool as if under ffx
    Standalone(M::Command),
    /// Print out the subtool's metadata json
    Metadata(MetadataCmd),
}

#[derive(FromArgs)]
#[argh(subcommand, name = "metadata", description = "Print out this subtool's FHO metadata json")]
pub(crate) struct MetadataCmd {
    #[argh(positional)]
    output_path: Option<PathBuf>,
}

#[derive(FromArgs)]
/// Fuchsia Host Objects Runner
pub(crate) struct ToolCommand<M: FfxTool> {
    #[argh(subcommand)]
    pub(crate) subcommand: FhoHandler<M>,
}

pub struct FhoSuite<M> {
    context: EnvironmentContext,
    _p: std::marker::PhantomData<fn(M) -> ()>,
}

impl<M> Clone for FhoSuite<M> {
    fn clone(&self) -> Self {
        Self { context: self.context.clone(), _p: self._p.clone() }
    }
}

struct FhoTool<M: FfxTool> {
    env: FhoEnvironment,
    redacted_args: Vec<String>,
    main: M,
}

struct MetadataRunner {
    cmd: MetadataCmd,
    info: &'static CommandInfo,
}

#[async_trait(?Send)]
impl ToolRunner for MetadataRunner {
    fn forces_stdout_log(&self) -> bool {
        false
    }

    async fn run(self: Box<Self>, _metrics: MetricsSession) -> Result<ExitStatus> {
        // We don't ever want to emit metrics for a metadata query, it's a tool-level
        // command
        let meta = FhoToolMetadata::new(self.info.name, self.info.description);
        match &self.cmd.output_path {
            Some(path) => serde_json::to_writer_pretty(
                &File::create(path).with_user_message(|| {
                    format!("Failed to create metadata file {}", path.display())
                })?,
                &meta,
            ),
            None => serde_json::to_writer_pretty(&std::io::stdout(), &meta),
        }
        .user_message("Failed writing metadata")?;
        Ok(ExitStatus::from_raw(0))
    }
}

#[async_trait(?Send)]
impl<T: FfxTool> ToolRunner for FhoTool<T> {
    fn forces_stdout_log(&self) -> bool {
        self.main.forces_stdout_log()
    }

    async fn run(self: Box<Self>, metrics: MetricsSession) -> Result<ExitStatus> {
        metrics.print_notice(&mut std::io::stderr()).await?;
        let writer = TryFromEnv::try_from_env(&self.env).await?;
        let res = self.main.main(writer).await.map(|_| ExitStatus::from_raw(0));
        metrics.command_finished(res.is_ok(), &self.redacted_args).await.and(res)
    }
}

impl<T: FfxTool> FhoTool<T> {
    async fn build(
        context: &EnvironmentContext,
        ffx: FfxCommandLine,
        tool: T::Command,
    ) -> Result<Box<Self>> {
        let is_machine_output = ffx.global.machine.is_some();
        let build_info = context.build_info();
        let injector = Injection::initialize_overnet(
            context.clone(),
            None,
            DaemonVersionCheck::SameVersionInfo(build_info),
            ffx.global.machine,
            ffx.global.target().await?,
        )
        .await?;
        let redacted_args = ffx.redact_subcmd(&tool);
        let env = FhoEnvironment { ffx, context: context.clone(), injector: Arc::new(injector) };
        let main = T::from_env(env.clone(), tool).await?;

        if !main.supports_machine_output() && is_machine_output {
            return Err(Error::User(anyhow::anyhow!(
                "The machine flag is not supported for this subcommand"
            )));
        }

        let found = FhoTool { env, redacted_args, main };
        Ok(Box::new(found))
    }
}

#[async_trait::async_trait(?Send)]
impl<M: FfxTool> ToolSuite for FhoSuite<M> {
    async fn from_env(context: &EnvironmentContext) -> Result<Self> {
        let context = context.clone();
        Ok(Self { context: context, _p: Default::default() })
    }

    fn global_command_list() -> &'static [&'static argh::CommandInfo] {
        FhoHandler::<M>::COMMANDS
    }

    async fn try_from_args(
        &self,
        ffx: &FfxCommandLine,
    ) -> Result<Option<Box<dyn ToolRunner + '_>>> {
        let args = Vec::from_iter(ffx.global.subcommand.iter().map(String::as_str));
        let command = ToolCommand::<M>::from_args(&Vec::from_iter(ffx.cmd_iter()), &args)
            .map_err(|err| Error::from_early_exit(&ffx.command, err))?;

        let res: Box<dyn ToolRunner> = match command.subcommand {
            FhoHandler::Metadata(cmd) => {
                Box::new(MetadataRunner { cmd, info: M::Command::COMMAND })
            }
            FhoHandler::Standalone(tool) => {
                FhoTool::<M>::build(&self.context, ffx.clone(), tool).await?
            }
        };
        Ok(Some(res))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // This keeps the macros from having compiler errors.
    use crate as fho;
    use crate::{testing::*, FhoDetails, Only, SimpleWriter};
    use async_trait::async_trait;
    use fho_macro::FfxTool;

    // The main testing part will happen in the `main()` function of the tool.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_run_fake_tool() {
        let config_env = ffx_config::test_init().await.unwrap();
        let tool_env = fho::testing::ToolEnv::new()
            .set_ffx_cmd(FfxCommandLine::new(None, &["ffx", "fake", "stuff"]).unwrap());
        let writer = SimpleWriter::new_buffers(Vec::new(), Vec::new());
        let fake_tool = tool_env.build_tool::<FakeTool>(config_env.context.clone()).await.unwrap();
        assert_eq!(
            SIMPLE_CHECK_COUNTER.with(|counter| *counter.borrow()),
            1,
            "tool pre-check should have been called once"
        );
        fake_tool.main(writer).await.unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn negative_precheck_fails() {
        #[derive(Debug, FfxTool)]
        #[check(SimpleCheck(false))]
        struct FakeToolWillFail {
            #[command]
            _fake_command: FakeCommand,
        }
        #[async_trait(?Send)]
        impl FfxMain for FakeToolWillFail {
            type Writer = SimpleWriter;
            async fn main(self, _writer: Self::Writer) -> Result<()> {
                panic!("This should never get called")
            }
        }

        let config_env = ffx_config::test_init().await.unwrap();
        let tool_env = fho::testing::ToolEnv::new()
            .set_ffx_cmd(FfxCommandLine::new(None, &["ffx", "fake", "stuff"]).unwrap());
        tool_env
            .build_tool::<FakeToolWillFail>(config_env.context.clone())
            .await
            .expect_err("Should not have been able to create tool with a negative pre-check");
        assert_eq!(
            SIMPLE_CHECK_COUNTER.with(|counter| *counter.borrow()),
            1,
            "tool pre-check should have been called once"
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn present_metadata() {
        let test_env = ffx_config::test_init().await.expect("Test env initialization failed");
        let tmpdir = tempfile::tempdir().expect("tempdir");

        let output_path = tmpdir.path().join("metadata.json");
        let cmd = MetadataCmd { output_path: Some(output_path.clone()) };
        let tool = Box::new(MetadataRunner { cmd, info: FakeCommand::COMMAND });
        let metrics = MetricsSession::start(&test_env.context).await.expect("Session start");

        tool.run(metrics).await.expect("running metadata command");

        let read_metadata: FhoToolMetadata =
            serde_json::from_reader(File::open(output_path).expect("opening metadata"))
                .expect("parsing metadata");
        assert_eq!(
            read_metadata,
            FhoToolMetadata {
                name: "fake".to_owned(),
                description: "fake command".to_owned(),
                requires_fho: 0,
                fho_details: FhoDetails::FhoVersion0 { version: Only },
            }
        );
    }
}
