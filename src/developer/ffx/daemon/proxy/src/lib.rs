// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use errors::{ffx_bail, ffx_error};
use fdomain_client::fidl::DiscoverableProtocolMarker;
use fdomain_fuchsia_developer_remotecontrol::{
    RemoteControlMarker as FRemoteControlMarker, RemoteControlProxy as FRemoteControlProxy,
};
use fdomain_fuchsia_io as fio_fdomain;
use ffx_command_error::FfxContext;
use ffx_config::EnvironmentContext;
use ffx_core::{downcast_injector_error, FfxInjectorError, Injector};
use ffx_daemon::{get_daemon_proxy_single_link, is_daemon_running_at_path, DaemonConfig};
use ffx_metrics::add_ffx_rcs_protocol_event;
use ffx_target::{get_remote_proxy, open_target_with_fut};
use fidl::endpoints::{ClientEnd, Proxy};
use fidl_fuchsia_developer_ffx::{DaemonError, DaemonProxy, TargetInfo, TargetProxy, VersionInfo};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_io::DirectoryMarker;
use futures::{FutureExt, SinkExt};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use target_errors::FfxTargetError;
use timeout::timeout;

/// The different ways to check the daemon's version against the local process' information
#[derive(Clone, Debug)]
pub enum DaemonVersionCheck {
    /// Compare the buildid, requires the daemon to have been spawned by the same executable.
    SameBuildId(String),
    /// Compare details from VersionInfo other than buildid, requires the daemon to have been
    /// spawned by the same overall build.
    SameVersionInfo(ffx_build_version::VersionInfo),
    /// Checks to see if the API level matches.
    CheckApiLevel(version_history::ApiLevel),
}

/// Lock-protected contents of [ProxyState]
enum ProxyStateInner<T: Proxy + Clone> {
    Uninitialized,
    Initialized(T),
    Failed,
}

impl<T: Proxy + Clone> ProxyStateInner<T> {
    /// See [ProxyState::get_or_try_init]
    async fn get_or_try_init<F: Future<Output = Result<T>>>(
        &mut self,
        mut f: impl FnMut(bool) -> F,
    ) -> Result<T> {
        if matches!(self, ProxyStateInner::Uninitialized) {
            *self = ProxyStateInner::Initialized(f(true).await?)
        }
        match self {
            ProxyStateInner::Uninitialized => unreachable!(),
            ProxyStateInner::Initialized(x) if !x.is_closed() => Ok(x.clone()),
            _ => {
                *self = ProxyStateInner::Failed;
                let proxy = f(false).await?;
                *self = ProxyStateInner::Initialized(proxy.clone());
                Ok(proxy)
            }
        }
    }
}

/// Container for a FIDL proxy which can be initialized lazily, and which will
/// re-initialize when the proxy is closed if possible.
struct ProxyState<T: Proxy + Clone>(futures::lock::Mutex<ProxyStateInner<T>>);

impl<T: Proxy + Clone> Default for ProxyState<T> {
    fn default() -> Self {
        ProxyState(futures::lock::Mutex::new(ProxyStateInner::Uninitialized))
    }
}

impl<T: Proxy + Clone> ProxyState<T> {
    /// Gets the proxy contained in this [`ProxyState`]. If the proxy hasn't
    /// been set, *or* it is in a closed state, the closure will be called to
    /// get a future which will construct a new proxy with which to initialize.
    async fn get_or_try_init<F: Future<Output = Result<T>>>(
        &self,
        f: impl FnMut(bool) -> F,
    ) -> Result<T> {
        self.0.lock().await.get_or_try_init(f).await
    }
}

pub struct Injection {
    env_context: EnvironmentContext,
    daemon_check: DaemonVersionCheck,
    target_spec: Option<String>,
    node: Arc<overnet_core::Router>,
    daemon_once: ProxyState<DaemonProxy>,
    remote_once: ProxyState<RemoteControlProxy>,
    fdomain: Mutex<
        Option<(
            Arc<fdomain_client::Client>,
            futures::channel::mpsc::Sender<ClientEnd<DirectoryMarker>>,
        )>,
    >,
}

pub const CONFIG_DAEMON_AUTOSTART: &str = "daemon.autostart";

impl std::fmt::Debug for Injection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Injection").finish()
    }
}

impl Injection {
    pub fn new(
        env_context: EnvironmentContext,
        daemon_check: DaemonVersionCheck,
        node: Arc<overnet_core::Router>,
        target_spec: Option<String>,
    ) -> Self {
        Self {
            env_context,
            daemon_check,
            node,
            target_spec,
            daemon_once: Default::default(),
            remote_once: Default::default(),
            fdomain: Mutex::new(None),
        }
    }

    #[tracing::instrument(skip(env_context))]
    pub async fn initialize_overnet(
        env_context: EnvironmentContext,
        router_interval: Option<Duration>,
        daemon_check: DaemonVersionCheck,
    ) -> ffx_command_error::Result<Injection> {
        tracing::debug!("Initializing Overnet");
        let node = overnet_core::Router::new(router_interval)
            .bug_context("Failed to initialize overnet")?;
        tracing::debug!("Getting target");
        let target_spec = ffx_target::get_target_specifier(&env_context).await?;
        tracing::debug!("Building Injection");
        Ok(Injection::new(env_context, daemon_check, node, target_spec))
    }

    #[tracing::instrument]
    async fn init_remote_proxy(
        &self,
        target_info: &mut Option<TargetInfo>,
    ) -> Result<RemoteControlProxy> {
        let daemon_proxy = self.daemon_factory().await?;
        let target_spec = self.target_spec.clone();
        let proxy_timeout = self.env_context.get_proxy_timeout().await?;
        get_remote_proxy(
            target_spec,
            daemon_proxy,
            proxy_timeout,
            Some(target_info),
            &self.env_context,
        )
        .await
    }

    #[tracing::instrument]
    async fn target_factory_inner(&self) -> Result<TargetProxy> {
        // See if we need to do local resolution. (Do it here not in
        // open_target_with_fut because o_t_w_f is not async)
        let target_spec = ffx_target::maybe_locally_resolve_target_spec(
            self.target_spec.clone(),
            &self.env_context,
        )
        .await?;
        let daemon_proxy = self.daemon_factory().await?;
        let (target_proxy, target_proxy_fut) = open_target_with_fut(
            target_spec,
            daemon_proxy.clone(),
            self.env_context.get_proxy_timeout().await?,
            &self.env_context,
        )?;
        target_proxy_fut.await?;
        Ok(target_proxy)
    }

    fn daemon_timeout_error(&self) -> FfxTargetError {
        FfxTargetError::DaemonError { err: DaemonError::Timeout, target: self.target_spec.clone() }
    }

    async fn daemon_factory_impl(
        &self,
        should_autostart: bool,
    ) -> Result<DaemonProxy, FfxInjectorError> {
        downcast_injector_error(
            self.daemon_once
                .get_or_try_init(|first_connection| {
                    let start_mode = if should_autostart {
                        DaemonStart::AutoStart
                    } else {
                        DaemonStart::DoNotAutoStart
                    };
                    init_daemon_proxy(
                        start_mode,
                        Arc::clone(&self.node),
                        self.env_context.clone(),
                        self.daemon_check.clone(),
                        first_connection,
                    )
                })
                .await,
        )
    }
}

#[async_trait(?Send)]
impl Injector for Injection {
    async fn daemon_factory_force_autostart(&self) -> Result<DaemonProxy, FfxInjectorError> {
        self.daemon_factory_impl(true).await
    }

    // This could get called multiple times by the plugin system via multiple threads - so make sure
    // the spawning only happens one thread at a time.
    #[tracing::instrument]
    async fn daemon_factory(&self) -> Result<DaemonProxy, FfxInjectorError> {
        let should_autostart =
            self.env_context.query(CONFIG_DAEMON_AUTOSTART).get().unwrap_or(true);
        self.daemon_factory_impl(should_autostart).await
    }

    #[tracing::instrument]
    async fn try_daemon(&self) -> Result<Option<DaemonProxy>> {
        let result = self
            .daemon_once
            .get_or_try_init(|first_connection| {
                init_daemon_proxy(
                    DaemonStart::DoNotAutoStart,
                    Arc::clone(&self.node),
                    self.env_context.clone(),
                    self.daemon_check.clone(),
                    first_connection,
                )
            })
            .await
            .ok();
        Ok(result)
    }

    #[tracing::instrument]
    async fn target_factory(&self) -> Result<TargetProxy> {
        let timeout_error = self.daemon_timeout_error();
        let proxy_timeout = self.env_context.get_proxy_timeout().await?;
        // We pin this in order to avoid the compiler reporting "error: large
        // future with a size of 16600 bytes".
        Box::pin(timeout(proxy_timeout, self.target_factory_inner())).await.map_err(|_| {
            tracing::warn!("Timed out getting Target proxy for: {:?}", self.target_spec);
            timeout_error
        })?
    }

    #[tracing::instrument]
    async fn remote_factory(&self) -> Result<RemoteControlProxy> {
        let timeout_error = self.daemon_timeout_error();
        // XXX Note: if we are doing local discovery, that will eat into this time.
        //     and if local discovery is _longer_ than the proxy timeout, we'll get
        //     a confusing error.
        let proxy_timeout = self.env_context.get_proxy_timeout().await?;
        let target_info = std::sync::Mutex::new(None);
        let proxy = Box::pin(timeout(proxy_timeout, async {
            self.remote_once
                .get_or_try_init(|_| async {
                    self.init_remote_proxy(&mut *target_info.lock().unwrap()).await
                })
                .await
        }))
        .await
        .map_err(|_| {
            tracing::warn!("Timed out getting remote control proxy for: {:?}", self.target_spec);
            match target_info.lock().unwrap().take() {
                Some(TargetInfo { nodename: Some(name), .. }) => {
                    FfxTargetError::DaemonError { err: DaemonError::Timeout, target: Some(name) }
                }
                _ => timeout_error,
            }
        })?;

        if let Ok(proxy) = proxy.as_ref() {
            let proto_fut = proxy.as_channel().get_channel_proxy_protocol();
            let proto_timeout = std::time::Duration::from_millis(500);

            match timeout(proto_timeout, proto_fut).await {
                Ok(Some(proto)) => {
                    let proto = proto.as_str();
                    if let Err(e) = add_ffx_rcs_protocol_event(proto).await {
                        tracing::warn!(error = ?e, "Problem sending protocol metrics");
                    }
                }
                // Peer seems to have closed. That'll be handled up the stack.
                Ok(None) => (),
                Err(_) => {
                    // This can happen if for some reason this channel isn't proxied by Overnet.
                    // That shouldn't ever happen but it's worth avoiding a hang.
                    tracing::warn!("Timed out waiting for protocol report from Overnet");
                }
            }
        }

        proxy
    }

    #[tracing::instrument]
    async fn remote_factory_fdomain(&self) -> Result<FRemoteControlProxy> {
        let rcs = self.remote_factory().await?;
        let toolbox = rcs::toolbox::open_toolbox(&rcs)
            .await?
            .into_client_end()
            .expect("Directory proxy couldn't become an endpoint despite being brand new!");

        let fdomain = self.fdomain.lock().unwrap().clone();
        let (fdomain, mut sender) = if let Some(fdomain) = fdomain {
            fdomain
        } else {
            let (sender, receiver) = futures::channel::mpsc::channel(1);
            let receiver = Mutex::new(receiver);
            let client = fdomain_local::local_client(move || {
                let ret = receiver
                    .lock()
                    .unwrap()
                    .try_next()
                    .map_err(|_| fidl::Status::INTERNAL)
                    .transpose()
                    .unwrap_or_else(|| Err(fidl::Status::INTERNAL));

                if ret.is_err() {
                    tracing::debug!(
                        "Somehow didn't get the proxy we created to answer this request."
                    )
                }
                ret
            });

            let mut fdomain = self.fdomain.lock().unwrap();
            fdomain.get_or_insert((client, sender)).clone()
        };

        sender.send(toolbox).await?;

        let namespace = fdomain.namespace().await?;
        let namespace =
            fdomain_client::fidl::ClientEnd::<fio_fdomain::DirectoryMarker>::new(namespace)
                .into_proxy();
        let (proxy, server_end) = fdomain.create_proxy::<FRemoteControlMarker>();
        namespace.open3(
            FRemoteControlMarker::PROTOCOL_NAME,
            fio_fdomain::Flags::PROTOCOL_SERVICE,
            &fio_fdomain::Options::default(),
            server_end.into_channel(),
        )?;
        Ok(proxy)
    }

    async fn is_experiment(&self, key: &str) -> bool {
        self.env_context.get(key).unwrap_or(false)
    }

    async fn build_info(&self) -> Result<VersionInfo> {
        let version_info = ffx_build_version::build_info();
        let ffx_version_info = VersionInfo {
            commit_hash: version_info.commit_hash,
            commit_timestamp: version_info.commit_timestamp,
            build_version: version_info.build_version,
            abi_revision: version_info.abi_revision,
            api_level: version_info.api_level,
            exec_path: version_info.exec_path,
            build_id: version_info.build_id,
            ..Default::default()
        };

        Ok(ffx_version_info)
    }
}

#[derive(PartialEq, Debug, Eq)]
enum DaemonStart {
    AutoStart,
    DoNotAutoStart,
}

#[tracing::instrument]
async fn init_daemon_proxy(
    autostart: DaemonStart,
    node: Arc<overnet_core::Router>,
    context: EnvironmentContext,
    version_check: DaemonVersionCheck,
    first_connection: bool,
) -> Result<DaemonProxy> {
    let ascendd_path = context.get_ascendd_path().await?;

    if cfg!(not(test)) && !is_daemon_running_at_path(&ascendd_path) {
        if autostart == DaemonStart::DoNotAutoStart {
            return Err(FfxInjectorError::DaemonAutostartDisabled.into());
        }
        ffx_daemon::spawn_daemon(&context).await?;
    }

    tracing::debug!("Daemon available, establishing Overnet link");
    let (nodeid, proxy, link) =
        get_daemon_proxy_single_link(&node, ascendd_path.clone(), None).await?;

    // Spawn off the link task, so that FIDL functions can be called (link IO makes progress).
    let link_task = fuchsia_async::Task::local(link.map(|_| ()));

    let daemon_version_info = timeout(context.get_proxy_timeout().await?, proxy.get_version_info())
        .await
        .context("timeout")
        .map_err(|_| {
            ffx_error!(
                "ffx was unable to query the version of the running ffx daemon. \
                                 Run `ffx doctor --restart-daemon` and try again."
            )
        })?
        .context("Getting hash from daemon")?;

    // Check the version against the given comparison scheme.
    tracing::debug!("Checking daemon version: {version_check:?}");
    tracing::debug!("Daemon version info: {daemon_version_info:?}");
    let matched_proxy = match (first_connection, version_check, daemon_version_info) {
        (false, _, _) => true,
        (_, DaemonVersionCheck::SameBuildId(ours), VersionInfo { build_id: Some(daemon), .. })
            if ours == daemon =>
        {
            true
        }
        (_, DaemonVersionCheck::SameVersionInfo(ours), daemon)
            if ours.build_version == daemon.build_version
                && ours.commit_hash == daemon.commit_hash
                && ours.commit_timestamp == daemon.commit_timestamp =>
        {
            true
        }
        (
            _,
            DaemonVersionCheck::CheckApiLevel(ours),
            VersionInfo { api_level: Some(daemon), .. },
        ) if {
            #[allow(deprecated)]
            ours.as_u64()
        } == daemon =>
        {
            true
        }
        _ => false,
    };

    if matched_proxy {
        tracing::debug!("Found matching daemon version, using it.");
        link_task.detach();
        return Ok(proxy);
    }

    tracing::info!("Daemon is a different version, attempting to restart");

    // Tell the daemon to quit, and wait for the link task to finish.
    // TODO(raggi): add a timeout on this, if the daemon quit fails for some
    // reason, the link task would hang indefinitely.
    let (quit_result, _) = futures::future::join(proxy.quit(), link_task).await;

    if !quit_result.is_ok() {
        ffx_bail!(
            "ffx daemon upgrade failed unexpectedly. \n\
            Try running `ffx doctor --restart-daemon` and then retry your \
            command.\n\nError was: {:?}",
            quit_result
        )
    }

    if cfg!(not(test)) {
        ffx_daemon::spawn_daemon(&context).await?;
    }

    let (_nodeid, proxy, link) =
        get_daemon_proxy_single_link(&node, ascendd_path, Some(vec![nodeid])).await?;

    fuchsia_async::Task::local(link.map(|_| ())).detach();

    Ok(proxy)
}

#[cfg(test)]
mod test {
    use super::*;
    use async_lock::Mutex;
    use fidl::endpoints::{DiscoverableProtocolMarker, RequestStream, ServerEnd};
    use fidl_fuchsia_developer_ffx::{
        DaemonMarker, DaemonRequest, DaemonRequestStream, TargetCollectionMarker,
        TargetCollectionRequest, TargetCollectionRequestStream, TargetConnectionError,
        TargetMarker, TargetRequest,
    };
    use fuchsia_async::Task;
    use futures::{AsyncReadExt, StreamExt, TryStreamExt};
    use netext::{TokioAsyncReadExt, UnixListenerStream};
    use std::path::PathBuf;
    use tokio::net::UnixListener;

    /// Retry a future until it succeeds or retries run out.
    async fn retry_with_backoff<E, F>(
        backoff0: Duration,
        max_backoff: Duration,
        mut f: impl FnMut() -> F,
    ) where
        F: futures::Future<Output = Result<(), E>>,
        E: std::fmt::Debug,
    {
        let mut backoff = backoff0;
        loop {
            match f().await {
                Ok(()) => {
                    backoff = backoff0;
                }
                Err(e) => {
                    tracing::warn!("Operation failed: {:?} -- retrying in {:?}", e, backoff);
                    fuchsia_async::Timer::new(backoff).await;
                    backoff = std::cmp::min(backoff * 2, max_backoff);
                }
            }
        }
    }

    fn start_socket_link(node: Arc<overnet_core::Router>, sockpath: PathBuf) -> Task<()> {
        Task::spawn(async move {
            let ascendd_path = sockpath.clone();
            let node = Arc::clone(&node);
            retry_with_backoff(Duration::from_millis(100), Duration::from_secs(3), || async {
                ffx_daemon::run_single_ascendd_link(Arc::clone(&node), ascendd_path.clone()).await
            })
            .await
        })
    }

    #[fuchsia::test]
    async fn test_init_daemon_proxy_link_lost() {
        let test_env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let sockpath = test_env.context.get_ascendd_path().await.expect("No ascendd path");

        // Start a listener that accepts and immediately closes the socket..
        let listener = UnixListener::bind(sockpath.to_owned()).unwrap();
        let _listen_task = Task::local(async move {
            loop {
                drop(listener.accept().await.unwrap());
            }
        });

        let res = init_daemon_proxy(
            DaemonStart::AutoStart,
            overnet_core::Router::new(None).unwrap(),
            test_env.context.clone(),
            DaemonVersionCheck::SameBuildId("testcurrenthash".to_owned()),
            true,
        )
        .await;
        let str = format!("{}", res.err().unwrap());
        assert!(str.contains("link lost"));
        assert!(str.contains("ffx doctor"));
    }

    #[fuchsia::test]
    async fn test_init_daemon_proxy_timeout_no_connection() {
        let test_env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let sockpath = test_env.context.get_ascendd_path().await.expect("No ascendd path");

        // Start a listener that never accepts the socket.
        let _listener = UnixListener::bind(sockpath.to_owned()).unwrap();

        let res = init_daemon_proxy(
            DaemonStart::AutoStart,
            overnet_core::Router::new(None).unwrap(),
            test_env.context.clone(),
            DaemonVersionCheck::SameBuildId("testcurrenthash".to_owned()),
            true,
        )
        .await;
        let str = format!("{}", res.err().unwrap());
        assert!(str.contains("Timed out"));
        assert!(str.contains("ffx doctor"));
    }

    async fn test_daemon_custom<F, R>(
        local_node: Arc<overnet_core::Router>,
        sockpath: PathBuf,
        build_id: &str,
        sleep_secs: u64,
        handler: F,
    ) -> Task<()>
    where
        F: Fn(DaemonRequest) -> R + 'static,
        F::Output: Future<Output = Result<(), fidl::Error>>,
    {
        let version_info = VersionInfo {
            exec_path: Some(std::env::current_exe().unwrap().to_string_lossy().to_string()),
            build_id: Some(build_id.to_owned()),
            ..Default::default()
        };
        let daemon = overnet_core::Router::new(None).unwrap();
        let listener = UnixListener::bind(&sockpath).unwrap();
        let local_link_task = start_socket_link(Arc::clone(&local_node), sockpath.clone());

        let (sender, mut receiver) = futures::channel::mpsc::unbounded();
        daemon
            .register_service(DaemonMarker::PROTOCOL_NAME.into(), move |chan| {
                let _ = sender.unbounded_send(chan);
                Ok(())
            })
            .await
            .unwrap();

        let link_tasks = Arc::new(Mutex::new(Vec::<Task<()>>::new()));
        let link_tasks1 = link_tasks.clone();

        let listen_task = Task::local(async move {
            // let (sock, _addr) = listener.accept().await.unwrap();
            let mut stream = UnixListenerStream(listener);
            while let Some(sock) = stream.try_next().await.unwrap_or(None) {
                fuchsia_async::Timer::new(Duration::from_secs(sleep_secs)).await;
                let node_clone = Arc::clone(&daemon);
                link_tasks1.lock().await.push(Task::local(async move {
                    let (mut rx, mut tx) = sock.into_multithreaded_futures_stream().split();
                    ascendd::run_stream(node_clone, &mut rx, &mut tx)
                        .map(|r| eprintln!("link error: {:?}", r))
                        .await;
                }));
            }
        });

        // Now that we've completed setting up everything, return a task for the main loop
        // of the fake daemon.
        Task::local(async move {
            while let Some(chan) = receiver.next().await {
                let link_tasks = link_tasks.clone();
                let mut stream =
                    DaemonRequestStream::from_channel(fidl::AsyncChannel::from_channel(chan));
                while let Some(request) = stream.try_next().await.unwrap_or(None) {
                    match request {
                        DaemonRequest::GetVersionInfo { responder, .. } => {
                            responder.send(&version_info).unwrap()
                        }
                        DaemonRequest::Quit { responder, .. } => {
                            std::fs::remove_file(sockpath).unwrap();
                            listen_task.cancel().await;
                            responder.send(true).unwrap();
                            // This is how long the daemon sleeps for, which
                            // is a workaround for the fact that we have no
                            // way to "flush" the response over overnet due
                            // to the constraints of mesh routing.
                            fuchsia_async::Timer::new(Duration::from_millis(20)).await;
                            link_tasks.lock().await.clear();
                            return;
                        }
                        _ => {
                            handler(request).await.unwrap();
                        }
                    }
                }
            }
            // Explicitly drop this in the task so it gets moved into it and isn't dropped
            // early.
            drop(local_link_task);
        })
    }

    async fn test_daemon(
        local_node: Arc<overnet_core::Router>,
        sockpath: PathBuf,
        build_id: &str,
        sleep_secs: u64,
    ) -> Task<()> {
        test_daemon_custom(local_node, sockpath, build_id, sleep_secs, |request| async move {
            panic!("unimplemented stub for request: {:?}", request);
        })
        .await
    }

    #[fuchsia::test]
    async fn test_init_daemon_proxy_hash_matches() {
        let test_env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let sockpath = test_env.context.get_ascendd_path().await.expect("No ascendd path");
        let local_node = overnet_core::Router::new(None).unwrap();

        let sockpath1 = sockpath.to_owned();
        let local_node1 = Arc::clone(&local_node);
        let daemons_task =
            test_daemon(local_node1, sockpath1.to_owned(), "testcurrenthash", 0).await;

        let proxy = init_daemon_proxy(
            DaemonStart::AutoStart,
            local_node,
            test_env.context.clone(),
            DaemonVersionCheck::SameBuildId("testcurrenthash".to_owned()),
            true,
        )
        .await
        .unwrap();
        proxy.quit().await.unwrap();
        daemons_task.await;
    }

    #[fuchsia::test]
    async fn test_init_daemon_proxy_upgrade() {
        let test_env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let sockpath = test_env.context.get_ascendd_path().await.expect("No ascendd path");
        let local_node = overnet_core::Router::new(None).unwrap();

        let sockpath1 = sockpath.to_owned();
        let local_node1 = Arc::clone(&local_node);

        // Spawn two daemons, the first out of date, the second is up to date.
        // spawn the first daemon directly so we know it's all started up before we proceed
        let first_daemon =
            test_daemon(Arc::clone(&local_node1), sockpath1.to_owned(), "oldhash", 0).await;
        let daemons_task = Task::local(async move {
            // wait for the first daemon to exit before starting the second
            first_daemon.await;
            // Note: testcurrenthash is explicitly expected by #cfg in get_daemon_proxy
            // Note: The double awaits are because test_daemon is an async function that returns a task
            test_daemon(local_node1, sockpath1.to_owned(), "testcurrenthash", 0).await.await;
        });

        let proxy = init_daemon_proxy(
            DaemonStart::AutoStart,
            local_node,
            test_env.context.clone(),
            DaemonVersionCheck::SameBuildId("testcurrenthash".to_owned()),
            true,
        )
        .await
        .unwrap();
        proxy.quit().await.unwrap();
        daemons_task.await;
    }

    #[fuchsia::test]
    async fn test_init_daemon_blocked_for_4s_succeeds() {
        let test_env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let sockpath = test_env.context.get_ascendd_path().await.expect("No ascendd path");
        let local_node = overnet_core::Router::new(None).unwrap();

        // Spawn two daemons, the first out of date, the second is up to date.
        let sockpath1 = sockpath.to_owned();
        let local_node1 = Arc::clone(&local_node);
        let daemon_task =
            test_daemon(local_node1, sockpath1.to_owned(), "testcurrenthash", 4).await;

        let proxy = init_daemon_proxy(
            DaemonStart::AutoStart,
            local_node,
            test_env.context.clone(),
            DaemonVersionCheck::SameBuildId("testcurrenthash".to_owned()),
            true,
        )
        .await
        .unwrap();
        proxy.quit().await.unwrap();
        daemon_task.await;
    }

    #[fuchsia::test]
    async fn test_init_daemon_blocked_for_long_timesout() {
        let test_env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let sockpath = test_env.context.get_ascendd_path().await.expect("No ascendd path");
        let local_node = overnet_core::Router::new(None).unwrap();

        // Spawn two daemons, the first out of date, the second is up to date.
        let sockpath1 = sockpath.to_owned();
        let local_node1 = Arc::clone(&local_node);
        let _daemon_task =
            test_daemon(local_node1, sockpath1.to_owned(), "testcurrenthash", 16).await;

        let err = init_daemon_proxy(
            DaemonStart::AutoStart,
            local_node,
            test_env.context.clone(),
            DaemonVersionCheck::SameBuildId("testcurrenthash".to_owned()),
            true,
        )
        .await;
        assert!(err.is_err());
        let str = format!("{:?}", err);
        assert!(str.contains("Timed out"));
        assert!(str.contains("ffx doctor"));
    }

    #[fuchsia::test]
    async fn test_remote_proxy_timeout() {
        let test_env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let sockpath = test_env.context.get_ascendd_path().await.expect("No ascendd path");
        let local_node = overnet_core::Router::new(None).unwrap();

        fn start_target_task(target_handle: ServerEnd<TargetMarker>) -> Task<()> {
            let mut stream = target_handle.into_stream();

            Task::local(async move {
                while let Some(request) = stream.try_next().await.unwrap() {
                    match request {
                        TargetRequest::Identity { responder } => {
                            responder
                                .send(&TargetInfo {
                                    nodename: Some("target_name".into()),
                                    ..TargetInfo::default()
                                })
                                .unwrap();
                        }
                        // Hang forever to trigger a timeout
                        request @ TargetRequest::OpenRemoteControl { .. } => {
                            Task::local(async move {
                                let _request = request;
                                futures::future::pending::<()>().await;
                            })
                            .detach();
                        }
                        _ => panic!("unhandled: {request:?}"),
                    }
                }
            })
        }

        fn start_target_collection_task(channel: fidl::AsyncChannel) -> Task<()> {
            let mut stream = TargetCollectionRequestStream::from_channel(channel);

            Task::local(async move {
                while let Some(request) = stream.try_next().await.unwrap() {
                    eprintln!("{request:?}");
                    match request {
                        TargetCollectionRequest::OpenTarget {
                            query: _,
                            target_handle,
                            responder,
                        } => {
                            start_target_task(target_handle).detach();

                            responder.send(Ok(())).unwrap();
                        }
                        _ => panic!("unhandled: {request:?}"),
                    }
                }
            })
        }

        let daemon_request_handler = move |request| async move {
            match request {
                DaemonRequest::ConnectToProtocol { name, server_end, responder }
                    if name == TargetCollectionMarker::PROTOCOL_NAME =>
                {
                    start_target_collection_task(fidl::AsyncChannel::from_channel(server_end))
                        .detach();

                    responder.send(Ok(()))?;
                }
                _ => panic!("unhandled request: {request:?}"),
            }
            Ok(())
        };

        let sockpath1 = sockpath.to_owned();
        let local_node1 = Arc::clone(&local_node);
        test_daemon_custom(
            local_node1,
            sockpath1.to_owned(),
            "testcurrenthash",
            0,
            daemon_request_handler,
        )
        .await
        .detach();

        let injection = Injection::new(
            test_env.context.clone(),
            DaemonVersionCheck::SameBuildId("testcurrenthash".to_owned()),
            local_node,
            Some("".into()),
        );

        let error = injection.remote_factory().await.unwrap_err();

        match error.downcast::<FfxTargetError>().unwrap() {
            FfxTargetError::DaemonError { err: DaemonError::Timeout, target } => {
                assert_eq!(target.as_deref(), Some(""));
            }
            err => panic!("Unexpected: {err}"),
        }
    }

    // These errors should ONLY be used with `test_rcs_connection_eventually_successful`.
    static ERRORS: std::sync::LazyLock<Arc<Mutex<Vec<TargetConnectionError>>>> =
        std::sync::LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

    #[fuchsia::test]
    async fn test_rcs_connection_eventually_successful() {
        let test_env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let sockpath = test_env.context.get_ascendd_path().await.expect("No ascendd path");
        let local_node = overnet_core::Router::new(None).unwrap();
        fn start_target_task(
            target_handle: ServerEnd<TargetMarker>,
            errors: Arc<Mutex<Vec<TargetConnectionError>>>,
        ) -> Task<()> {
            let mut stream = target_handle.into_stream();

            let errors = errors.clone();
            Task::local(async move {
                while let Some(request) = stream.try_next().await.unwrap() {
                    match request {
                        TargetRequest::Identity { responder } => {
                            responder
                                .send(&TargetInfo {
                                    nodename: Some("target_name".into()),
                                    ..TargetInfo::default()
                                })
                                .unwrap();
                        }
                        TargetRequest::OpenRemoteControl { remote_control, responder } => {
                            if let Some(err) = errors.lock().await.pop() {
                                responder.send(Err(err)).unwrap();
                                continue;
                            }
                            Task::local(async move {
                                let mut stream = remote_control.into_stream();
                                while let Ok(Some(request)) = stream.try_next().await {
                                    eprintln!("Got a request for RCS proxy: {request:?}");
                                }
                            })
                            .detach();
                            responder.send(Ok(())).unwrap();
                        }
                        TargetRequest::GetSshLogs { responder } => responder.send("").unwrap(),
                        _ => {
                            eprintln!("unhandled request: {request:?}");
                            panic!("unhandled: {request:?}")
                        }
                    }
                }
            })
        }

        fn start_target_collection_task(channel: fidl::AsyncChannel) -> Task<()> {
            let errors = ERRORS.clone();
            let mut stream = TargetCollectionRequestStream::from_channel(channel);
            Task::local(async move {
                while let Some(request) = stream.try_next().await.unwrap() {
                    eprintln!("{request:?}");
                    match request {
                        TargetCollectionRequest::OpenTarget {
                            query: _,
                            target_handle,
                            responder,
                        } => {
                            start_target_task(target_handle, errors.clone()).detach();

                            responder.send(Ok(())).unwrap();
                        }
                        _ => panic!("unhandled: {request:?}"),
                    }
                }
            })
        }

        let daemon_request_handler = move |request| async move {
            match request {
                DaemonRequest::ConnectToProtocol { name, server_end, responder }
                    if name == TargetCollectionMarker::PROTOCOL_NAME =>
                {
                    start_target_collection_task(fidl::AsyncChannel::from_channel(server_end))
                        .detach();
                    responder.send(Ok(()))?;
                }
                _ => panic!("unhandled request: {request:?}"),
            }
            Ok(())
        };

        let sockpath1 = sockpath.to_owned();
        let local_node1 = Arc::clone(&local_node);
        test_daemon_custom(
            local_node1,
            sockpath1.to_owned(),
            "testcurrenthash",
            0,
            daemon_request_handler,
        )
        .await
        .detach();

        let injection = Injection::new(
            test_env.context.clone(),
            DaemonVersionCheck::SameBuildId("testcurrenthash".to_owned()),
            local_node,
            Some("".into()),
        );
        let error_list = [
            TargetConnectionError::Timeout,
            TargetConnectionError::Timeout,
            TargetConnectionError::ConnectionRefused,
        ];
        ERRORS.lock().await.extend_from_slice(&error_list[..]);
        let mut target_info = None;
        assert!(injection.init_remote_proxy(&mut target_info).await.is_ok());
        // We should also get the target info here.
        assert_eq!(target_info.unwrap().nodename.unwrap(), "target_name".to_owned());

        let error_list = [
            TargetConnectionError::Timeout,
            TargetConnectionError::KeyVerificationFailure,
            TargetConnectionError::Timeout,
            TargetConnectionError::ConnectionRefused,
        ];
        ERRORS.lock().await.extend_from_slice(&error_list[..]);
        let mut target_info = None;
        let res = injection.init_remote_proxy(&mut target_info).await;
        assert!(res.is_err());
        let err = res.unwrap_err().downcast::<FfxTargetError>().unwrap();
        let FfxTargetError::TargetConnectionError { err, .. } = err else {
            panic!("Unexpected error: {err:?}");
        };
        assert_eq!(err, TargetConnectionError::KeyVerificationFailure);
        // We should still get the target info even during failure.
        assert_eq!(target_info.unwrap().nodename.unwrap(), "target_name".to_owned());
    }
}
