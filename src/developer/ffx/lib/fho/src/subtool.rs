// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{CommandInfo, FromArgs, SubCommand, SubCommands};
use async_trait::async_trait;
use ffx_command::{
    DaemonVersionCheck, Error, FfxCommandLine, FfxContext, MetricsSession, Result, ToolRunner,
    ToolSuite,
};
use ffx_config::EnvironmentContext;
use ffx_core::Injector;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::Arc;
use std::{fs::File, path::PathBuf};

use crate::{FhoToolMetadata, TryFromEnv};

/// The main trait for defining an ffx tool. This is not intended to be implemented directly
/// by the user, but instead derived via `#[derive(FfxTool)]`.
#[async_trait(?Send)]
pub trait FfxTool: Sized {
    type Command: FromArgs + SubCommand;
    type Main: FfxMain;

    fn forces_stdout_log() -> bool;
    async fn from_env(env: FhoEnvironment, cmd: Self::Command) -> Result<Self::Main>;

    /// Executes the tool. This is intended to be invoked by the user in main.
    async fn execute_tool() {
        let result = ffx_command::run::<FhoSuite<Self>>().await;
        ffx_command::exit(result).await;
    }
}

#[async_trait(?Send)]
pub trait FfxMain: Sized {
    type Writer: FfxToolIo + TryFromEnv;

    /// The entrypoint of the tool. Once FHO has set up the environment for the tool, this is
    /// invoked. Should not be invoked directly unless for testing.
    async fn main(self, writer: &Self::Writer) -> Result<()>;
}

pub trait FfxToolIo {
    fn is_machine_supported() -> bool {
        false
    }
}
impl FfxToolIo for ffx_writer::Writer {}

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
    suite: FhoSuite<M>,
    ffx: FfxCommandLine,
    command: ToolCommand<M>,
}

#[derive(Clone)]
pub struct FhoEnvironment {
    pub ffx: FfxCommandLine,
    pub context: EnvironmentContext,
    pub injector: Arc<dyn Injector>,
}

impl MetadataCmd {
    fn print(&self, info: &CommandInfo) -> Result<ExitStatus> {
        let meta = FhoToolMetadata::new(info.name, info.description);
        match &self.output_path {
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
        T::forces_stdout_log()
    }

    async fn run(self: Box<Self>, metrics: MetricsSession) -> Result<ExitStatus> {
        match self.command.subcommand {
            FhoHandler::Metadata(metadata) => metadata.print(T::Command::COMMAND),
            FhoHandler::Standalone(tool) => {
                metrics.print_notice(&mut std::io::stderr()).await?;
                let redacted_args = self.ffx.redact_subcmd(&tool);
                let res = run_main(self.suite, self.ffx, tool).await;
                metrics.command_finished(res.is_ok(), &redacted_args).await.and(res)
            }
        }
    }
}

async fn run_main<T: FfxTool>(
    suite: FhoSuite<T>,
    cmd: FfxCommandLine,
    tool: T::Command,
) -> Result<ExitStatus> {
    let cache_path = suite.context.get_cache_path()?;
    let hoist_cache_dir = std::fs::create_dir_all(&cache_path)
        .and_then(|_| tempfile::tempdir_in(&cache_path))
        .with_user_message(|| format!(
            "Could not create hoist cache root in {}. Do you have permission to write to its parent?",
            cache_path.display()
        ))?;
    let build_info = suite.context.build_info();
    let injector = cmd
        .global
        .initialize_overnet(
            hoist_cache_dir.path(),
            None,
            DaemonVersionCheck::SameVersionInfo(build_info),
        )
        .await?;
    let env = FhoEnvironment { ffx: cmd, context: suite.context, injector: Arc::new(injector) };
    let writer = TryFromEnv::try_from_env(&env).await?;
    let main = T::from_env(env, tool).await?;
    main.main(&writer).await.map(|_| ExitStatus::from_raw(0))
}

#[async_trait::async_trait(?Send)]
impl<M: FfxTool> ToolSuite for FhoSuite<M> {
    fn from_env(context: &EnvironmentContext) -> Result<Self> {
        let context = context.clone();
        Ok(Self { context: context, _p: Default::default() })
    }

    fn global_command_list() -> &'static [&'static argh::CommandInfo] {
        FhoHandler::<M>::COMMANDS
    }

    async fn try_from_args(
        &self,
        cmd: &FfxCommandLine,
    ) -> Result<Option<Box<dyn ToolRunner + '_>>> {
        let args = Vec::from_iter(cmd.global.subcommand.iter().map(String::as_str));
        let found = FhoTool {
            suite: self.clone(),
            ffx: cmd.clone(),
            command: ToolCommand::<M>::from_args(&Vec::from_iter(cmd.cmd_iter()), &args)
                .map_err(|err| Error::from_early_exit(&cmd.command, err))?,
        };
        Ok(Some(Box::new(found)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // This keeps the macros from having compiler errors.
    use crate as fho;
    use crate::testing::*;
    use crate::FhoDetails;
    use crate::Only;
    use crate::SimpleWriter;
    use async_trait::async_trait;
    use fho_macro::FfxTool;

    // The main testing part will happen in the `main()` function of the tool.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_run_fake_tool() {
        let config_env = ffx_config::test_init().await.unwrap();
        let tool_env = fho::testing::ToolEnv::new()
            .set_ffx_cmd(FfxCommandLine::new(None, &["ffx", "fake", "stuff"]).unwrap());
        let writer = SimpleWriter::new_test(None);
        let fake_tool = tool_env.build_tool::<FakeTool>(config_env.context.clone()).await.unwrap();
        assert_eq!(
            SIMPLE_CHECK_COUNTER.with(|counter| *counter.borrow()),
            1,
            "tool pre-check should have been called once"
        );
        fake_tool.main(&writer).await.unwrap();
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
            type Writer = ffx_writer::Writer;
            async fn main(self, _writer: &Self::Writer) -> Result<()> {
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

        let ffx = FfxCommandLine::new(None, &["ffx"]).expect("ffx command line to parse");
        let suite: FhoSuite<FakeTool> =
            FhoSuite { context: test_env.context.clone(), _p: Default::default() };
        let output_path = tmpdir.path().join("metadata.json");
        let subcommand =
            FhoHandler::Metadata(MetadataCmd { output_path: Some(output_path.clone()) });
        let command = ToolCommand { subcommand };
        let tool = Box::new(FhoTool { ffx, suite, command });
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
