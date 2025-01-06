// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subtool::{FfxTool, ToolCommand};
use crate::FhoEnvironment;
use argh::FromArgs;
use async_trait::async_trait;
use fdomain_fuchsia_developer_remotecontrol::RemoteControlProxy as FRemoteControlProxy;
use ffx_command::{FfxCommandLine, Result};
use ffx_config::EnvironmentContext;
use ffx_core::{downcast_injector_error, FfxInjectorError, Injector};
use fidl_fuchsia_developer_ffx::{DaemonProxy, TargetProxy, VersionInfo};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

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
    factory_func!(remote_factory_closure_f, FRemoteControlProxy);
    factory_func!(target_factory_closure, TargetProxy);
    factory_func!(build_info_closure, VersionInfo);

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

    pub fn make_environment(self, context: EnvironmentContext) -> FhoEnvironment {
        let injector = Arc::new(self.injector);
        #[allow(deprecated)] // injector field.
        FhoEnvironment {
            ffx: self.ffx_cmd_line,
            context,
            behavior: crate::FhoConnectionBehavior::DaemonConnector(injector),
            lookup: Arc::new(crate::from_env::DeviceLookupDefaultImpl),
        }
    }

    pub async fn build_tool<T: FfxTool>(self, context: EnvironmentContext) -> Result<T> {
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

    pub async fn build_tool_from_cmd<T: FfxTool>(
        self,
        cmd: T::Command,
        context: EnvironmentContext,
    ) -> Result<T> {
        let env = self.make_environment(context);
        T::from_env(env, cmd).await
    }
}

pub struct FakeInjector {
    daemon_factory_force_autostart_closure:
        Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<DaemonProxy>>>>>,
    daemon_factory_closure:
        Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<DaemonProxy>>>>>,
    try_daemon_closure:
        Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<Option<DaemonProxy>>>>>>,
    remote_factory_closure:
        Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<RemoteControlProxy>>>>>,
    remote_factory_closure_f:
        Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<FRemoteControlProxy>>>>>,
    target_factory_closure:
        Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<TargetProxy>>>>>,
    is_experiment_closure: Box<dyn Fn(&str) -> Pin<Box<dyn Future<Output = bool>>>>,
    build_info_closure: Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<VersionInfo>>>>>,
}

impl Default for FakeInjector {
    fn default() -> Self {
        Self {
            daemon_factory_closure: Box::new(|| Box::pin(async { unimplemented!() })),
            daemon_factory_force_autostart_closure: Box::new(|| {
                Box::pin(async { unimplemented!() })
            }),
            try_daemon_closure: Box::new(|| Box::pin(async { unimplemented!() })),
            remote_factory_closure: Box::new(|| Box::pin(async { unimplemented!() })),
            remote_factory_closure_f: Box::new(|| Box::pin(async { unimplemented!() })),
            target_factory_closure: Box::new(|| Box::pin(async { unimplemented!() })),
            is_experiment_closure: Box::new(|_| Box::pin(async { unimplemented!() })),
            build_info_closure: Box::new(|| Box::pin(async { unimplemented!() })),
        }
    }
}

#[async_trait(?Send)]
impl Injector for FakeInjector {
    async fn daemon_factory_force_autostart(
        &self,
    ) -> anyhow::Result<DaemonProxy, FfxInjectorError> {
        downcast_injector_error((self.daemon_factory_force_autostart_closure)().await)
    }

    async fn daemon_factory(&self) -> anyhow::Result<DaemonProxy, FfxInjectorError> {
        downcast_injector_error((self.daemon_factory_closure)().await)
    }

    async fn try_daemon(&self) -> anyhow::Result<Option<DaemonProxy>> {
        (self.try_daemon_closure)().await
    }

    async fn remote_factory(&self) -> anyhow::Result<RemoteControlProxy> {
        (self.remote_factory_closure)().await
    }

    async fn remote_factory_fdomain(&self) -> anyhow::Result<FRemoteControlProxy> {
        (self.remote_factory_closure_f)().await
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
}

/// Sets up a fake proxy of type `T` handing requests to the given callback and returning
/// their responses.
///
/// This is basically the same thing as `ffx_plugin` used to generate for
/// each proxy argument, but uses a generic instead of text replacement.
pub fn fake_proxy<T: fidl::endpoints::Proxy>(
    mut handle_request: impl FnMut(fidl::endpoints::Request<T::Protocol>) + 'static,
) -> T {
    use futures::TryStreamExt;
    let (proxy, mut stream) = fidl::endpoints::create_proxy_and_stream::<T::Protocol>();
    fuchsia_async::Task::local(async move {
        while let Ok(Some(req)) = stream.try_next().await {
            handle_request(req);
        }
    })
    .detach();
    proxy
}

/// Sets up a fake FDomain proxy of type `T` handing requests to the given
/// callback and returning their responses.
///
/// This is basically the same thing as `ffx_plugin` used to generate for
/// each proxy argument, but uses a generic instead of text replacement.
pub async fn fake_proxy_f<T: fdomain_client::fidl::Proxy>(
    client: Arc<fdomain_client::Client>,
    mut handle_request: impl FnMut(fdomain_client::fidl::Request<T::Protocol>) + 'static,
) -> T {
    use futures::TryStreamExt;
    let (proxy, mut stream) = client.create_proxy_and_stream::<T::Protocol>().await.unwrap();
    fuchsia_async::Task::local(async move {
        // Capture the client so it doesn't go out of scope
        let _client = client;
        while let Ok(Some(req)) = stream.try_next().await {
            handle_request(req);
        }
    })
    .detach();
    proxy
}

#[cfg(test)]
mod internal {
    use super::*;
    use crate::from_env::CheckEnv;
    use crate::{self as fho, FfxMain, ToolIO, TryFromEnv};
    use argh::{ArgsInfo, FromArgs};
    use std::cell::RefCell;

    #[derive(Debug)]
    pub struct NewTypeString(String);

    #[async_trait(?Send)]
    impl TryFromEnv for NewTypeString {
        async fn try_from_env(_env: &FhoEnvironment) -> Result<Self> {
            Ok(Self(String::from("foobar")))
        }
    }

    #[derive(Debug, ArgsInfo, FromArgs)]
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
        async fn check_env(self, _env: &FhoEnvironment) -> Result<()> {
            SIMPLE_CHECK_COUNTER.with(|counter| *counter.borrow_mut() += 1);
            if self.0 {
                Ok(())
            } else {
                Err(anyhow::anyhow!("SimpleCheck was false").into())
            }
        }
    }

    #[derive(fho_macro::FfxTool, Debug)]
    #[check(SimpleCheck(true))]
    pub struct FakeTool {
        from_env_string: NewTypeString,
        #[command]
        fake_command: FakeCommand,
    }

    #[async_trait(?Send)]
    impl FfxMain for FakeTool {
        type Writer = ffx_writer::SimpleWriter;
        async fn main(self, mut writer: Self::Writer) -> Result<()> {
            assert_eq!(self.from_env_string.0, "foobar");
            assert_eq!(self.fake_command.stuff, "stuff");
            writer.line("junk-line").unwrap();
            Ok(())
        }
    }
}
#[cfg(test)]
pub(crate) use internal::*;
