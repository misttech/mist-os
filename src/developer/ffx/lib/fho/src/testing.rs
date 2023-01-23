// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subtool::FfxTool;
use crate::subtool::ToolCommand;
use crate::FhoEnvironment;
use argh::FromArgs;
use async_trait::async_trait;
use ffx_command::{FfxCommandLine, Result};
use ffx_config::EnvironmentContext;
use ffx_core::Injector;
use ffx_writer::Writer;
use fidl_fuchsia_developer_ffx::{DaemonProxy, FastbootProxy, TargetProxy, VersionInfo};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use std::future::Future;
use std::pin::Pin;

pub struct ToolEnv {
    injector: FakeInjector,
    ffx_cmd_line: FfxCommandLine,
}

impl Default for ToolEnv {
    fn default() -> Self {
        Self {
            injector: Default::default(),
            ffx_cmd_line: FfxCommandLine::new(None, &["please", "set", "me"]).unwrap(),
        }
    }
}

macro_rules! factory_func {
    ($func:ident, $output:ty $(,)?) => {
        pub fn $func<F, Fut>(mut self, closure: F) -> Self
        where
            F: Fn() -> Fut + 'static,
            Fut: Future<Output = anyhow::Result<$output>> + 'static,
        {
            self.injector.$func = Box::new(move || Box::pin(closure()));
            self
        }
    };
}

impl ToolEnv {
    pub fn new() -> Self {
        Self::default()
    }

    factory_func!(daemon_factory_closure, DaemonProxy);
    factory_func!(try_daemon_closure, Option<DaemonProxy>);
    factory_func!(remote_factory_closure, RemoteControlProxy);
    factory_func!(fastboot_factory_closure, FastbootProxy);
    factory_func!(target_factory_closure, TargetProxy);
    factory_func!(build_info_closure, VersionInfo);
    factory_func!(writer_closure, Writer);

    pub fn is_experiment_closure<F, Fut>(mut self, closure: F) -> Self
    where
        F: Fn() -> Fut + 'static,
        Fut: Future<Output = bool> + 'static,
    {
        self.injector.is_experiment_closure = Box::new(move |_| Box::pin(closure()));
        self
    }

    pub fn set_ffx_cmd(mut self, cmd: FfxCommandLine) -> Self {
        self.ffx_cmd_line = cmd;
        self
    }

    pub fn take_injector(self) -> FakeInjector {
        self.injector
    }

    pub async fn build_tool<'a, T: FfxTool>(
        &'a self,
        context: &'a EnvironmentContext,
    ) -> Result<T::Main<'a>> {
        let tool_cmd = ToolCommand::<T>::from_args(
            &Vec::from_iter(self.ffx_cmd_line.cmd_iter()),
            &Vec::from_iter(self.ffx_cmd_line.subcmd_iter()),
        )
        .unwrap();
        let crate::subtool::FhoHandler::Standalone(cmd) = tool_cmd.subcommand else {
            panic!("Not testing metadata generation");
        };
        self.build_tool_from_cmd::<T>(cmd, context).await
    }

    pub async fn build_tool_from_cmd<'a, T: FfxTool>(
        &'a self,
        cmd: T::Command,
        context: &'a EnvironmentContext,
    ) -> Result<T::Main<'a>> {
        let env = FhoEnvironment { injector: &self.injector, context, ffx: &self.ffx_cmd_line };
        T::from_env(env, cmd).await
    }
}

pub struct FakeInjector {
    daemon_factory_closure:
        Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<DaemonProxy>>>>>,
    try_daemon_closure:
        Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<Option<DaemonProxy>>>>>>,
    remote_factory_closure:
        Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<RemoteControlProxy>>>>>,
    fastboot_factory_closure:
        Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<FastbootProxy>>>>>,
    target_factory_closure:
        Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<TargetProxy>>>>>,
    is_experiment_closure: Box<dyn Fn(&str) -> Pin<Box<dyn Future<Output = bool>>>>,
    build_info_closure: Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<VersionInfo>>>>>,
    writer_closure: Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<Writer>>>>>,
}

impl Default for FakeInjector {
    fn default() -> Self {
        Self {
            daemon_factory_closure: Box::new(|| Box::pin(async { unimplemented!() })),
            try_daemon_closure: Box::new(|| Box::pin(async { unimplemented!() })),
            remote_factory_closure: Box::new(|| Box::pin(async { unimplemented!() })),
            fastboot_factory_closure: Box::new(|| Box::pin(async { unimplemented!() })),
            target_factory_closure: Box::new(|| Box::pin(async { unimplemented!() })),
            is_experiment_closure: Box::new(|_| Box::pin(async { unimplemented!() })),
            build_info_closure: Box::new(|| Box::pin(async { unimplemented!() })),
            writer_closure: Box::new(|| Box::pin(async { unimplemented!() })),
        }
    }
}

#[async_trait(?Send)]
impl Injector for FakeInjector {
    async fn daemon_factory(&self) -> anyhow::Result<DaemonProxy> {
        (self.daemon_factory_closure)().await
    }

    async fn try_daemon(&self) -> anyhow::Result<Option<DaemonProxy>> {
        (self.try_daemon_closure)().await
    }

    async fn remote_factory(&self) -> anyhow::Result<RemoteControlProxy> {
        (self.remote_factory_closure)().await
    }

    async fn fastboot_factory(&self) -> anyhow::Result<FastbootProxy> {
        (self.fastboot_factory_closure)().await
    }

    async fn target_factory(&self) -> anyhow::Result<TargetProxy> {
        (self.target_factory_closure)().await
    }

    async fn is_experiment(&self, key: &str) -> bool {
        (self.is_experiment_closure)(key).await
    }

    async fn build_info(&self) -> anyhow::Result<VersionInfo> {
        (self.build_info_closure)().await
    }

    async fn writer(&self) -> anyhow::Result<Writer> {
        (self.writer_closure)().await
    }
}

#[cfg(test)]
mod internal {
    use super::*;
    use crate::{self as fho, CheckEnv, FfxMain, FhoEnvironment, Result, TryFromEnv};
    use argh::FromArgs;
    use std::cell::RefCell;

    #[derive(Debug)]
    pub struct NewTypeString(String);

    #[async_trait(?Send)]
    impl TryFromEnv for NewTypeString {
        async fn try_from_env(_env: &FhoEnvironment<'_>) -> Result<Self> {
            Ok(Self(String::from("foobar")))
        }
    }

    #[derive(Debug, FromArgs)]
    #[argh(subcommand, name = "fake", description = "fake command")]
    pub struct FakeCommand {
        #[argh(positional)]
        /// just needs a doc here so the macro doesn't complain.
        stuff: String,
    }

    thread_local! {
        pub static SIMPLE_CHECK_COUNTER: RefCell<u64> = RefCell::new(0);
    }

    pub struct SimpleCheck(pub bool);

    #[async_trait(?Send)]
    impl CheckEnv for SimpleCheck {
        async fn check_env(self, _env: &FhoEnvironment<'_>) -> Result<()> {
            SIMPLE_CHECK_COUNTER.with(|counter| *counter.borrow_mut() += 1);
            if self.0 {
                Ok(())
            } else {
                Err(anyhow::anyhow!("SimpleCheck was false").into())
            }
        }
    }

    #[derive(fho_macro::FfxTool, Debug)]
    #[ffx(forces_stdout_logs)]
    #[check(SimpleCheck(true))]
    pub struct FakeTool {
        from_env_string: NewTypeString,
        #[command]
        fake_command: FakeCommand,
    }

    #[async_trait(?Send)]
    impl FfxMain for FakeTool {
        type Writer = ffx_writer::Writer;
        async fn main(self, writer: &Self::Writer) -> Result<()> {
            assert_eq!(self.from_env_string.0, "foobar");
            assert_eq!(self.fake_command.stuff, "stuff");
            writer.line("junk-line").unwrap();
            Ok(())
        }
    }
}
#[cfg(test)]
pub(crate) use internal::*;
