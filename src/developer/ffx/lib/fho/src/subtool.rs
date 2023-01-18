// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{CommandInfo, FromArgs, SubCommand, SubCommands};
use async_trait::async_trait;
use errors::{ffx_bail, ffx_error};
use ffx_command::{
    DaemonVersionCheck, Error, FfxCommandLine, FfxContext, Result, ToolRunner, ToolSuite,
};
use ffx_config::EnvironmentContext;
use ffx_core::Injector;
use ffx_fidl::DaemonError;
use fidl::endpoints::Proxy;
use fidl_fuchsia_developer_ffx as ffx_fidl;
use selectors::{self, VerboseError};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::time::Duration;
use std::{fs::File, path::PathBuf, rc::Rc, sync::Arc};

use crate::FhoToolMetadata;

#[async_trait(?Send)]
pub trait FfxTool: Sized {
    type Command: FromArgs + SubCommand;
    type Main<'a>: FfxMain;

    fn forces_stdout_log() -> bool;
    async fn from_env(env: FhoEnvironment<'_>, cmd: Self::Command) -> Result<Self::Main<'_>>;

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
    fn machine_writer_output() -> String {
        String::from("Not supported")
    }

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

pub struct FhoEnvironment<'a> {
    pub ffx: &'a FfxCommandLine,
    pub context: &'a EnvironmentContext,
    pub injector: &'a dyn Injector,
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

    async fn run(self: Box<Self>) -> Result<ExitStatus> {
        match self.command.subcommand {
            FhoHandler::Metadata(metadata) => metadata.print(T::Command::COMMAND),
            FhoHandler::Standalone(tool) => {
                let cache_path = self.suite.context.get_cache_path()?;
                let hoist_cache_dir = std::fs::create_dir_all(&cache_path)
                    .and_then(|_| tempfile::tempdir_in(&cache_path))
                    .with_user_message(|| format!(
                        "Could not create hoist cache root in {}. Do you have permission to write to its parent?",
                        cache_path.display()
                    ))?;
                let build_info = self.suite.context.build_info();
                let injector = self
                    .ffx
                    .global
                    .initialize_overnet(
                        hoist_cache_dir.path(),
                        None,
                        DaemonVersionCheck::SameVersionInfo(build_info),
                    )
                    .await?;
                let env = FhoEnvironment {
                    ffx: &self.ffx,
                    context: &self.suite.context,
                    injector: &injector,
                };
                let writer = TryFromEnv::try_from_env(&env).await?;
                let main = T::from_env(env, tool).await?;
                main.main(&writer).await.map(|_| ExitStatus::from_raw(0))
            }
        }
    }
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

    fn redact_arg_values(&self, cmd: &FfxCommandLine) -> Result<Vec<String>> {
        let args = Vec::from_iter(cmd.global.subcommand.iter().map(String::as_str));
        let cmd_vec = Vec::from_iter(cmd.cmd_iter());
        ToolCommand::<M>::redact_arg_values(&cmd_vec, &args)
            .map_err(|err| Error::from_early_exit(&cmd.command, err))
    }
}

#[async_trait(?Send)]
pub trait TryFromEnv: Sized {
    async fn try_from_env(env: &FhoEnvironment<'_>) -> Result<Self>;
}

#[async_trait(?Send)]
pub trait CheckEnv {
    async fn check_env(self, env: &FhoEnvironment<'_>) -> Result<()>;
}

#[async_trait(?Send)]
impl<T> TryFromEnv for Arc<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment<'_>) -> Result<Self> {
        T::try_from_env(env).await.map(Arc::new)
    }
}

#[async_trait(?Send)]
impl<T> TryFromEnv for Rc<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment<'_>) -> Result<Self> {
        T::try_from_env(env).await.map(Rc::new)
    }
}

#[async_trait(?Send)]
impl<T> TryFromEnv for Box<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment<'_>) -> Result<Self> {
        T::try_from_env(env).await.map(Box::new)
    }
}

#[async_trait(?Send)]
impl<T> TryFromEnv for Option<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment<'_>) -> Result<Self> {
        Ok(T::try_from_env(env).await.ok())
    }
}

#[async_trait(?Send)]
impl<T> TryFromEnv for Result<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment<'_>) -> Result<Self> {
        Ok(T::try_from_env(env).await)
    }
}

/// Checks if the experimental config flag is set. This gates the execution of the command.
/// If the flag is set to `true`, this returns `Ok(())`, else returns an error.
pub struct AvailabilityFlag<T>(pub T);

#[async_trait(?Send)]
impl<T: AsRef<str>> CheckEnv for AvailabilityFlag<T> {
    async fn check_env(self, _env: &FhoEnvironment<'_>) -> Result<()> {
        let flag = self.0.as_ref();
        if ffx_config::get(flag).await.unwrap_or(false) {
            Ok(())
        } else {
            ffx_bail!(
                "This is an experimental subcommand.  To enable this subcommand run 'ffx config set {} true'",
                flag
            );
        }
    }
}

/// A trait for looking up a Fuchsia component when using the Protocol struct.
///
/// Example usage;
/// ```rust
/// struct FooSelector;
/// impl FuchsiaComponentSelector for FooSelector {
///     const SELECTOR: &'static str = "core/selector/thing";
/// }
///
/// #[derive(FfxTool)]
/// struct Tool {
///     foo_proxy: Protocol<FooProxy, FooSelector>,
/// }
/// ```
pub trait FuchsiaComponentSelector {
    const SELECTOR: &'static str;
}

/// A wrapper type used to look up protocols on a Fuchsia target. Whatever has been set as the
/// default target in the environment will be where the proxy is connected.
#[derive(Debug, Clone)]
pub struct Protocol<P: Clone, S> {
    proxy: P,
    _s: std::marker::PhantomData<fn(S) -> ()>,
}

impl<P: Clone, S> Protocol<P, S> {
    pub fn new(proxy: P) -> Self {
        Self { proxy, _s: Default::default() }
    }
}

impl<P: Clone, S> std::ops::Deref for Protocol<P, S> {
    type Target = P;

    fn deref(&self) -> &Self::Target {
        &self.proxy
    }
}

#[async_trait(?Send)]
impl<P: Proxy + Clone, S: FuchsiaComponentSelector> TryFromEnv for Protocol<P, S>
where
    P::Protocol: fidl::endpoints::DiscoverableProtocolMarker,
{
    async fn try_from_env(env: &FhoEnvironment<'_>) -> Result<Self> {
        let (proxy, server_end) = fidl::endpoints::create_proxy::<P::Protocol>()
            .with_user_message(|| format!("Failed creating proxy for selector {}", S::SELECTOR))?;
        let _ = selectors::parse_selector::<VerboseError>(S::SELECTOR)
            .with_bug_context(|| format!("Parsing selector {}", S::SELECTOR))?;
        let retry_count = 1;
        let mut tries = 0;
        // TODO(fxbug.dev/113143): Remove explicit retries/timeouts here so they can be
        // configurable instead.
        let rcs_instance = loop {
            tries += 1;
            let res = env.injector.remote_factory().await;
            if res.is_ok() || tries > retry_count {
                break res;
            }
        }?;
        rcs::connect_with_timeout(
            Duration::from_secs(15),
            S::SELECTOR,
            &rcs_instance,
            server_end.into_channel(),
        )
        .await?;
        Ok(Protocol::new(proxy))
    }
}

#[derive(Debug, Clone)]
pub struct DaemonProtocol<P: Clone> {
    proxy: P,
}

impl<P: Clone> DaemonProtocol<P> {
    pub fn new(proxy: P) -> Self {
        Self { proxy }
    }
}

impl<P: Clone> DaemonProtocol<P> {
    pub fn into_inner(self) -> P {
        self.proxy
    }
}

impl<P: Clone> std::ops::Deref for DaemonProtocol<P> {
    type Target = P;

    fn deref(&self) -> &Self::Target {
        &self.proxy
    }
}

fn map_daemon_error(svc_name: &str, err: DaemonError) -> Error {
    match err {
        DaemonError::ProtocolNotFound => ffx_error!(
            "The daemon protocol '{svc_name}' did not match any protocols on the daemon
If you are not developing this plugin or the protocol it connects to, then this is a bug

Please report it at http://fxbug.dev/new/ffx+User+Bug."
        ),
        DaemonError::ProtocolOpenError => ffx_error!(
            "The daemon protocol '{svc_name}' failed to open on the daemon.

If you are developing the protocol, there may be an internal failure when invoking the start
function. See the ffx.daemon.log for details at `ffx config get log.dir -p sub`.

If you are NOT developing this plugin or the protocol it connects to, then this is a bug.

Please report it at http://fxbug.dev/new/ffx+User+Bug."
        ),
        unexpected => ffx_error!(
"While attempting to open the daemon protocol '{svc_name}', received an unexpected error:

{unexpected:?}

This is not intended behavior and is a bug.
Please report it at http://fxbug.dev/new/ffx+User+Bug."

        ),
    }
    .into()
}

#[async_trait(?Send)]
impl<P: Proxy + Clone> TryFromEnv for DaemonProtocol<P>
where
    P::Protocol: fidl::endpoints::DiscoverableProtocolMarker,
{
    async fn try_from_env(env: &FhoEnvironment<'_>) -> Result<Self> {
        let svc_name = <P::Protocol as fidl::endpoints::DiscoverableProtocolMarker>::PROTOCOL_NAME;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<P::Protocol>()
            .with_user_message(|| format!("Failed creating proxy for service {}", svc_name))?;
        let daemon = env.injector.daemon_factory().await?;

        daemon
            .connect_to_protocol(svc_name, server_end.into_channel())
            .await
            .bug_context("Connecting to protocol")?
            .map_err(|err| map_daemon_error(svc_name, err))
            .map(|_| DaemonProtocol { proxy })
    }
}

#[async_trait(?Send)]
impl TryFromEnv for ffx_fidl::DaemonProxy {
    async fn try_from_env(env: &FhoEnvironment<'_>) -> Result<Self> {
        env.injector.daemon_factory().await.user_message("Failed to create daemon proxy")
    }
}

#[async_trait(?Send)]
impl TryFromEnv for ffx_fidl::TargetProxy {
    async fn try_from_env(env: &FhoEnvironment<'_>) -> Result<Self> {
        env.injector.target_factory().await.user_message("Failed to create target proxy")
    }
}

#[async_trait(?Send)]
impl TryFromEnv for ffx_fidl::FastbootProxy {
    async fn try_from_env(env: &FhoEnvironment<'_>) -> Result<Self> {
        env.injector.fastboot_factory().await.user_message("Failed to create fastboot proxy")
    }
}

#[async_trait(?Send)]
impl TryFromEnv for ffx_writer::Writer {
    async fn try_from_env(env: &FhoEnvironment<'_>) -> Result<Self> {
        env.injector.writer().await.user_message("Failed to create writer")
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
    use async_trait::async_trait;
    use ffx_writer::Writer;
    use fho_macro::FfxTool;

    // The main testing part will happen in the `main()` function of the tool.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_run_fake_tool() {
        let context = ffx_config::EnvironmentContext::default();
        let (ffx, injector, tool_cmd) = setup_fho_items::<FakeTool>();
        let fho_env = FhoEnvironment { ffx: &ffx, context: &context, injector: &injector };
        let writer = Writer::try_from_env(&fho_env).await.expect("creating writer");

        assert_eq!(
            SIMPLE_CHECK_COUNTER.with(|counter| *counter.borrow()),
            0,
            "tool pre-check should not have been called yet"
        );
        let fake_tool = match tool_cmd.subcommand {
            FhoHandler::Standalone(t) => FakeTool::from_env(fho_env, t).await.unwrap(),
            FhoHandler::Metadata(_) => panic!("Not testing metadata generation"),
        };
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

        let context = ffx_config::EnvironmentContext::default();
        let (ffx, injector, tool_cmd) = setup_fho_items::<FakeToolWillFail>();
        let fho_env = FhoEnvironment { ffx: &ffx, context: &context, injector: &injector };

        assert_eq!(
            SIMPLE_CHECK_COUNTER.with(|counter| *counter.borrow()),
            0,
            "tool pre-check should not have been called yet"
        );
        match tool_cmd.subcommand {
            FhoHandler::Standalone(t) => FakeToolWillFail::from_env(fho_env, t)
                .await
                .expect_err("Should not have been able to create tool with a negative pre-check"),
            FhoHandler::Metadata(_) => panic!("Not testing metadata generation"),
        };
        assert_eq!(
            SIMPLE_CHECK_COUNTER.with(|counter| *counter.borrow()),
            1,
            "tool pre-check should have been called once"
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn present_metadata() {
        let tmpdir = tempfile::tempdir().expect("tempdir");

        let ffx = FfxCommandLine::new(None, &["ffx"]).expect("ffx command line to parse");
        let context = EnvironmentContext::default();
        let suite: FhoSuite<FakeTool> = FhoSuite { context, _p: Default::default() };
        let output_path = tmpdir.path().join("metadata.json");
        let subcommand =
            FhoHandler::Metadata(MetadataCmd { output_path: Some(output_path.clone()) });
        let command = ToolCommand { subcommand };
        let tool = Box::new(FhoTool { ffx, suite, command });

        tool.run().await.expect("running metadata command");

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
