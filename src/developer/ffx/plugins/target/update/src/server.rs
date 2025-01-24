// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_config::EnvironmentContext;
use fho::{
    bug, return_bug, return_user_error, user_error, Deferred, FfxMain, Result,
    VerifiedMachineWriter,
};
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_developer_ffx::{
    RepositoryRegistrationAliasConflictMode, RepositoryRegistryProxy,
};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_io::OpenFlags;
use fidl_fuchsia_pkg::RepositoryManagerProxy;
use fidl_fuchsia_pkg_rewrite::EngineProxy;
use fidl_fuchsia_pkg_rewrite_ext::{do_transaction, Rule};
use fidl_fuchsia_sys2::OpenDirType;
use fuchsia_async::{Task, Timer};
use std::io::{Error, ErrorKind};
use std::net::Ipv6Addr;
use std::path::PathBuf;
use std::process;
use std::time::Duration;
use target_connector::Connector;
use target_holders::TargetProxyHolder;
use timeout::timeout;
use zx_status::Status;

const REPOSITORY_MANAGER_MONIKER: &str = "/core/pkg-resolver";

struct LogWriter {
    prefix: String,
}

impl LogWriter {
    pub fn new(prefix: &str) -> Self {
        Self { prefix: prefix.to_string() }
    }
}

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let strings = std::str::from_utf8(buf).map_err(|e| {
            Error::new(ErrorKind::InvalidData, format!("Could not convert to UTF8: {e}"))
        })?;
        for s in strings.lines() {
            tracing::info!("{}{}", self.prefix, s);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) struct PackageServerTask {
    pub(crate) repo_name: String,
    pub(crate) task: Task<Result<()>>,
}

pub(crate) async fn package_server_task(
    target_proxy_connector: Connector<TargetProxyHolder>,
    rcs_proxy_connector: Connector<RemoteControlProxy>,
    repos: Deferred<RepositoryRegistryProxy>,
    context: EnvironmentContext,
    product_bundle: PathBuf,
    repo_port: u16,
) -> Result<Option<PackageServerTask>> {
    tracing::info!("starting package server for {product_bundle:?}");

    // Make the name mostly unique, that way it is easier to remove this update source.
    let repo_name_prefix = "pb-update-source-";
    let repo_name = format!("{repo_name_prefix}{}", process::id());

    let cmd = ffx_repository_server_start_args::StartCommand {
        // Start a server on the given port.
        address: Some((Ipv6Addr::UNSPECIFIED, repo_port).into()),
        foreground: true,
        // Give it a name. This is actually a prefix of the name when running a product bundle.
        repository: Some(repo_name.clone()),

        product_bundle: Some(
            camino::Utf8PathBuf::from_path_buf(product_bundle)
                .map_err(|e| user_error!("Could not encode {e:?} as UTF-8"))?,
        ),

        // Replace all other alias rules so the update uses this server.
        alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,

        // These are defaults, nothing special needed.
        background: false,
        daemon: false,
        disconnected: false,
        trusted_root: None,
        repo_path: None,
        alias: vec![],
        storage_type: None,
        port_path: None,
        no_device: false,
        refresh_metadata: false,
        auto_publish: None,
    };

    // Check that there is not an update source that has the same prefix. This is extremely
    // unlikely, and would mean that process ids are being reused on the host computer.
    if is_server_registered(&repo_name, rcs_proxy_connector.clone(), Duration::from_secs(60))
        .await?
    {
        return_user_error!(
            "Product bundle repository server name collision detected.\
         Please deregister repostory servers starting with {repo_name_prefix}"
        )
    }

    // Create a local task that runs the repo package server. This is done in-process since it
    // easier to manage cleaning up the current process vs. having to manage multiple processes.
    // Task::spawn would be a better alternative, but the Connector<> is not "Send", so it cannot
    // be used across threads.
    let connector = rcs_proxy_connector.clone();
    let task = fuchsia_async::Task::local(async move {
        let tool = ffx_repository_server_start::ServerStartTool {
            cmd,
            repos,
            context,
            target_proxy_connector: target_proxy_connector.clone(),
            rcs_proxy_connector: connector.clone(),
        };

        let stdout = LogWriter::new("repo_server stdout");
        let stderr = LogWriter::new("repo_server stderr");

        let server_writer = VerifiedMachineWriter::new_buffers(None, stdout, stderr);

        let server_result = tool.main(server_writer).await;
        tracing::info!("product bundle server exited: {server_result:?}");
        server_result
    });
    Ok(Some(PackageServerTask { repo_name, task }))
}

pub(crate) async fn wait_for_device_task(
    repo_name: String,
    rcs_proxy_connector: Connector<RemoteControlProxy>,
) -> Result<()> {
    // Once the server task is running, wait until the registration appears on the device.
    let registered = timeout::<_, fho::Result<()>>(Duration::from_secs(30), async {
        loop {
            fuchsia_async::Timer::new(std::time::Duration::from_secs(1)).await;
            if is_server_registered(
                &repo_name,
                rcs_proxy_connector.clone(),
                Duration::from_secs(30),
            )
            .await?
            {
                return Ok(());
            }
        }
    })
    .await
    .map_err(|e| bug!("{e:?}"))?;

    if let Err(e) = registered {
        return_user_error!("Product bundle server was not registered on the device: {e}")
    }
    Ok(())
}

/// unregisters all servers that have the given prefix.
/// This uses the Connector for the rcs_proxy to potentially
/// reconnect to the device since post-update the device may
/// be rebooting.
pub(crate) async fn unregister_pb_repo_server(
    repo_name_prefix: &str,
    rcs_proxy_connector: Connector<RemoteControlProxy>,
) -> Result<()> {
    let mut retry = true;

    let mut repo_manager_proxy =
        match get_repo_manager_proxy(rcs_proxy_connector.clone(), Duration::from_secs(500)).await {
            Ok(proxy) => proxy,
            Err(err) => {
                tracing::info!("repo manager proxy closed, retrying");
                if retry {
                    fuchsia_async::Timer::new(std::time::Duration::from_secs(1)).await;
                    get_repo_manager_proxy(rcs_proxy_connector.clone(), Duration::from_secs(500))
                        .await?
                } else {
                    return_bug!("Could not list servers on device: {err}")
                }
            }
        };

    let mut names: Vec<String> = vec![];

    retry = true;

    loop {
        let (repo_iterator, repo_iterator_server) = fidl::endpoints::create_proxy();
        match repo_manager_proxy.list(repo_iterator_server) {
            Ok(_) => {
                loop {
                    let repos = repo_iterator.next().await.map_err(|e| bug!(e))?;
                    if repos.is_empty() {
                        break;
                    }
                    names.extend(repos.iter().filter_map(|r| {
                        if let Some(repo_url) = &r.repo_url {
                            if repo_url.starts_with(&format!("fuchsia-pkg://{repo_name_prefix}")) {
                                Some(repo_url.to_string())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }));
                }
                break;
            }
            Err(err) => {
                if err.is_closed() {
                    tracing::info!("repo manager proxy closed, retrying");
                    if retry {
                        retry = false;
                        repo_manager_proxy = get_repo_manager_proxy(
                            rcs_proxy_connector.clone(),
                            Duration::from_secs(500),
                        )
                        .await?;
                    } else {
                        return_bug!("Could not list servers on device: {err}")
                    }
                }
            }
        };
    }
    for name in names {
        deregister_standalone(&name, rcs_proxy_connector.clone(), Duration::from_secs(500)).await?
    }
    Ok(())
}

/// Inspects the on-device registrations and looks for a
/// server that has the given prefix.
async fn is_server_registered(
    repo_name: &str,
    rcs_proxy_connector: Connector<RemoteControlProxy>,
    time_to_wait: Duration,
) -> Result<bool> {
    let repo_manager_proxy = get_repo_manager_proxy(rcs_proxy_connector, time_to_wait).await?;

    let (repo_iterator, repo_iterator_server) = fidl::endpoints::create_proxy();
    repo_manager_proxy.list(repo_iterator_server).map_err(|e| bug!(e))?;
    loop {
        let repos = repo_iterator.next().await.map_err(|e| bug!(e))?;
        if repos.is_empty() {
            break;
        }
        if repos.iter().any(|r| {
            if let Some(repo_url) = &r.repo_url {
                repo_url.starts_with(&format!("fuchsia-pkg://{repo_name}"))
            } else {
                false
            }
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn deregister_standalone(
    repo_name: &str,
    rcs_proxy_connector: Connector<RemoteControlProxy>,
    time_to_wait: Duration,
) -> Result<()> {
    let repo_url = if repo_name.starts_with("fuchsia-pkg://") {
        repo_name
    } else {
        &format!("fuchsia-pkg://{repo_name}")
    };
    tracing::info!("Removing server {repo_url}");

    let repo_proxy: RepositoryManagerProxy =
        match get_repo_manager_proxy(rcs_proxy_connector.clone(), time_to_wait).await {
            Ok(proxy) => proxy,
            Err(e) => {
                tracing::warn!("Got error getting repo_manager_proxy: {e}, retrying");
                Timer::new(Duration::from_secs(1)).await;
                get_repo_manager_proxy(rcs_proxy_connector.clone(), time_to_wait).await?
            }
        };

    match repo_proxy.remove(repo_url).await {
        Ok(Ok(())) => (),
        Ok(Err(err)) => {
            let status = Status::from_raw(err);
            if status != Status::NOT_FOUND {
                let message = format!(
                    "failed to remove registration for {repo_url}: {:#?}",
                    Status::from_raw(err)
                );
                tracing::error!("{message}");
                return_bug!("{message}");
            } else {
                tracing::info!("registration for {repo_url} was not found. Ignoring.")
            }
        }
        Err(err) => {
            let message =
                format!("failed to remove registrtation  due to communication error: {:#?}", err);
            tracing::error!("{message}");
            return_bug!("{message}");
        }
    };
    // Remove any alias rules.
    let rewrite_proxy: EngineProxy =
        match get_rewrite_proxy(rcs_proxy_connector.clone(), time_to_wait).await {
            Ok(proxy) => proxy,
            Err(e) => {
                tracing::warn!("Got error getting rewrite_proxy: {e}, retrying");
                Timer::new(Duration::from_secs(1)).await;
                get_rewrite_proxy(rcs_proxy_connector.clone(), time_to_wait).await?
            }
        };

    remove_aliases(repo_name, rewrite_proxy).await
}

async fn remove_aliases(repo_url: &str, rewrite_proxy: EngineProxy) -> Result<()> {
    tracing::info!("Removing aliases for {repo_url}");
    // Check flag here for "overwrite" style
    do_transaction(&rewrite_proxy, |transaction| async {
        // Prepend the alias rules to the front so they take priority.
        let mut rules: Vec<Rule> = vec![];

        // These are rules to re-evaluate...
        let repo_rules_state = transaction.list_dynamic().await?;
        rules.extend(repo_rules_state);

        // Clear the list, since we'll be adding it back later.
        transaction.reset_all()?;

        // Keep rules that do not match the repo being removed.
        rules.retain(|r| r.host_replacement() != repo_url);

        // Add the rules back into the transaction. We do it in reverse, because `.add()`
        // always inserts rules into the front of the list.
        for rule in rules.into_iter().rev() {
            transaction.add(rule).await?
        }

        Ok(transaction)
    })
    .await
    .map_err(|err| {
        tracing::warn!("failed to create transactions: {:#?}", err);
        bug!("Failed to create transaction for aliases: {err}")
    })?;
    Ok(())
}

async fn get_rewrite_proxy(
    rcs_proxy_connector: Connector<RemoteControlProxy>,
    time_to_wait: Duration,
) -> Result<EngineProxy> {
    let rcs_proxy = try_rcs_proxy_connection(rcs_proxy_connector, time_to_wait).await?;

    let (rewrite_proxy, rewrite_server) =
        fidl::endpoints::create_proxy::<<EngineProxy as fidl::endpoints::Proxy>::Protocol>();
    rcs_proxy
        .deprecated_open_capability(
            &REPOSITORY_MANAGER_MONIKER,
            OpenDirType::ExposedDir,
            fidl_fuchsia_pkg_rewrite::EngineMarker::PROTOCOL_NAME,
            rewrite_server.into_channel(),
            OpenFlags::empty(),
        )
        .await
        .map_err(|e| bug!(e))?
        .map_err(|err| {
            bug!(
                "Attempting to connect to moniker {REPOSITORY_MANAGER_MONIKER} failed with {err:?}",
            )
        })?;

    Ok(rewrite_proxy)
}

async fn get_repo_manager_proxy(
    rcs_proxy_connector: Connector<RemoteControlProxy>,
    time_to_wait: Duration,
) -> Result<RepositoryManagerProxy> {
    let rcs_proxy = try_rcs_proxy_connection(rcs_proxy_connector, time_to_wait).await?;
    let (repo_manager_proxy, repo_manager_server) = fidl::endpoints::create_proxy::<
        <RepositoryManagerProxy as fidl::endpoints::Proxy>::Protocol,
    >();
    rcs_proxy
        .deprecated_open_capability(
            &REPOSITORY_MANAGER_MONIKER,
            OpenDirType::ExposedDir,
            fidl_fuchsia_pkg::RepositoryManagerMarker::PROTOCOL_NAME,
            repo_manager_server.into_channel(),
            OpenFlags::empty(),
        )
        .await
        .map_err(|e| bug!(e))?
        .map_err(|err| {
            bug!(
                "Attempting to connect to moniker {REPOSITORY_MANAGER_MONIKER} failed with {err:?}",
            )
        })?;
    Ok(repo_manager_proxy)
}

async fn try_rcs_proxy_connection(
    rcs_proxy_connector: Connector<RemoteControlProxy>,
    time_to_wait: Duration,
) -> Result<RemoteControlProxy> {
    let rcs_proxy = timeout(
        time_to_wait,
        rcs_proxy_connector.try_connect(|target, _err| {
            tracing::info!(
                "RCS proxy: Waiting for target '{}' to return",
                match target {
                    Some(s) => s,
                    _ => "None",
                }
            );
            Ok(())
        }),
    )
    .await;
    match rcs_proxy {
        Ok(r) => r,
        Err(e) => fho::return_user_error!("Timeout connecting to rcs: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_config::TestEnv;
    use ffx_target::TargetProxy;
    use fho::testing::ToolEnv;
    use fho::TryFromEnv as _;
    use fidl_fuchsia_developer_remotecontrol::{ConnectCapabilityError, RemoteControlRequest};
    use fidl_fuchsia_pkg::{
        RepositoryConfig, RepositoryIteratorRequest, RepositoryManagerMarker,
        RepositoryManagerRequest, RepositoryManagerRequestStream,
    };
    use fidl_fuchsia_pkg_rewrite::{
        EditTransactionRequest, EngineMarker, EngineRequest, EngineRequestStream,
        RuleIteratorRequest,
    };
    use futures::channel::mpsc;
    use futures::{SinkExt as _, StreamExt as _, TryStreamExt as _};
    use std::sync::{Arc, Mutex};

    struct FakeTestEnv {
        pub context: EnvironmentContext,
        pub rcs_proxy_connector: Connector<RemoteControlProxy>,
        pub target_proxy_connector: Connector<TargetProxyHolder>,
        pub repo_proxy: Deferred<RepositoryRegistryProxy>,
    }

    impl FakeTestEnv {
        async fn new(test_env: &TestEnv) -> Self {
            let fake_rcs_proxy: RemoteControlProxy =
                fho::testing::fake_proxy(move |req| handle_rcs_proxy_request(req));
            let fake_target_proxy: TargetProxy =
                fho::testing::fake_proxy(move |req| panic!("Unexpected request: {:?}", req));
            let fake_repo_proxy = Deferred::from_output(Ok(fho::testing::fake_proxy(move |req| {
                panic!("Unexpected request: {:?}", req)
            })));
            let tool_env = ToolEnv::new()
                .remote_factory_closure(move || {
                    let value = fake_rcs_proxy.clone();
                    async move { Ok(value) }
                })
                .target_factory_closure(move || {
                    let value = fake_target_proxy.clone();
                    async { Ok(value) }
                });
            let fho_env = tool_env.make_environment(test_env.context.clone());

            let target_proxy_connector = Connector::try_from_env(&fho_env)
                .await
                .expect("Could not make target proxy test connector");
            let rcs_proxy_connector =
                Connector::try_from_env(&fho_env).await.expect("Could not make RCS test connector");
            Self {
                context: test_env.context.clone(),
                rcs_proxy_connector,
                target_proxy_connector,
                repo_proxy: fake_repo_proxy,
            }
        }
    }

    #[derive(Clone)]
    struct FakeRepositoryManager {
        sender: mpsc::Sender<()>,
    }

    impl FakeRepositoryManager {
        fn new() -> (Self, mpsc::Receiver<()>) {
            let (sender, rx) = futures::channel::mpsc::channel::<()>(1);

            (Self { sender }, rx)
        }

        fn spawn(&self, mut stream: RepositoryManagerRequestStream) {
            let sender = self.sender.clone();

            Task::local(async move {
                while let Some(Ok(req)) = stream.next().await {
                    match req {
                        RepositoryManagerRequest::Add { responder, .. } => {
                            let mut sender = sender.clone();

                            Task::local(async move {
                                responder.send(Ok(())).unwrap();
                                let _send = sender.send(()).await.unwrap();
                            })
                            .detach();
                        }
                        RepositoryManagerRequest::Remove { responder, .. } => {
                            responder.send(Ok(())).unwrap();
                        }
                        RepositoryManagerRequest::List {
                            iterator,
                            control_handle: _control_handle,
                        } => {
                            let mut stream = iterator.into_stream();
                            let mut sent = false;
                            while let Some(RepositoryIteratorRequest::Next { responder }) =
                                stream.try_next().await.expect("next try_next")
                            {
                                if !sent {
                                    responder
                                        .send(&[RepositoryConfig {
                                            repo_url: Some(
                                                "fuchsia-pkg://registered_test_repo".into(),
                                            ),
                                            ..Default::default()
                                        }])
                                        .expect("next send");
                                    sent = true;
                                } else {
                                    responder.send(&[]).expect("next send");
                                }
                            }
                        }
                        _ => panic!("unexpected request: {:?}", req),
                    }
                }
            })
            .detach();
        }
    }

    #[derive(Clone)]
    struct FakeEngine {
        sender: mpsc::Sender<()>,
    }

    impl FakeEngine {
        fn new() -> (Self, mpsc::Receiver<()>) {
            let (sender, rx) = futures::channel::mpsc::channel::<()>(1);
            (Self { sender }, rx)
        }

        fn spawn(&self, mut stream: EngineRequestStream) {
            let rules: Arc<Mutex<Vec<Rule>>> = Arc::new(Mutex::new(Vec::<Rule>::new()));
            let sender = self.sender.clone();

            Task::local(async move {
                while let Some(Ok(req)) = stream.next().await {
                    match req {
                        EngineRequest::StartEditTransaction { transaction, control_handle: _ } => {
                            let mut sender = sender.clone();
                            let rules = Arc::clone(&rules);

                            Task::local(async move {
                                let mut stream = transaction.into_stream();
                                while let Some(request) = stream.next().await {
                                    let request = request.unwrap();
                                    match request {
                                        EditTransactionRequest::ResetAll { control_handle: _ } => {}
                                        EditTransactionRequest::ListDynamic {
                                            iterator,
                                            control_handle: _,
                                        } => {
                                            let mut stream = iterator.into_stream();

                                            let mut rules =
                                                rules.lock().unwrap().clone().into_iter();

                                            while let Some(req) = stream.try_next().await.unwrap() {
                                                let RuleIteratorRequest::Next { responder } = req;

                                                if let Some(rule) = rules.next() {
                                                    responder.send(&[rule.into()]).unwrap();
                                                } else {
                                                    responder.send(&[]).unwrap();
                                                }
                                            }
                                        }
                                        EditTransactionRequest::Add { rule: _, responder } => {
                                            responder.send(Ok(())).unwrap()
                                        }
                                        EditTransactionRequest::Commit { responder } => {
                                            let res = responder.send(Ok(())).unwrap();
                                            let _send = sender.send(()).await.unwrap();
                                            res
                                        }
                                    }
                                }
                            })
                            .detach();
                        }
                        _ => panic!("unexpected request: {:?}", req),
                    }
                }
            })
            .detach();
        }
    }

    fn handle_rcs_proxy_request(req: RemoteControlRequest) {
        let (repo_manager, _) = FakeRepositoryManager::new();
        let (engine, _) = FakeEngine::new();
        match req {
            RemoteControlRequest::DeprecatedOpenCapability {
                capability_name,
                server_channel,
                responder,
                ..
            } => {
                match capability_name.as_str() {
                    RepositoryManagerMarker::PROTOCOL_NAME => {
                        repo_manager.spawn(
                            fidl::endpoints::ServerEnd::<RepositoryManagerMarker>::new(
                                server_channel,
                            )
                            .into_stream(),
                        );
                        responder.send(Ok(())).expect("Could not send response")
                    }
                    EngineMarker::PROTOCOL_NAME => {
                        engine.spawn(
                            fidl::endpoints::ServerEnd::<EngineMarker>::new(server_channel)
                                .into_stream(),
                        );
                        responder.send(Ok(())).expect("Could not send response")
                    }
                    _ => {
                        responder.send(Err(ConnectCapabilityError::NoMatchingCapabilities)).unwrap()
                    }
                };
            }
            _ => panic!("Unexpected request: {:?}", req),
        }
    }

    #[fuchsia::test]
    async fn test_package_server_task() {
        let test_env = ffx_config::test_init().await.expect("test env");
        let fake_env = FakeTestEnv::new(&test_env).await;

        let product_bundle = PathBuf::from("/path/to/product_bundle");

        let result = package_server_task(
            fake_env.target_proxy_connector,
            fake_env.rcs_proxy_connector,
            fake_env.repo_proxy,
            fake_env.context,
            product_bundle,
            0,
        )
        .await;
        assert!(result.is_ok(), "got {:?}", result.err());
    }
    #[fuchsia::test]
    async fn test_wait_for_device_task() {
        let test_env = ffx_config::test_init().await.expect("test env");
        let fake_env = FakeTestEnv::new(&test_env).await;

        let repo_name = "registered_test_repo".into();

        let result = wait_for_device_task(repo_name, fake_env.rcs_proxy_connector).await;
        assert!(result.is_ok(), "got {:?}", result.err());
    }
    #[fuchsia::test]
    async fn test_unregister_pb_repo_server() {
        let test_env = ffx_config::test_init().await.expect("test env");
        let fake_env = FakeTestEnv::new(&test_env).await;

        let result =
            unregister_pb_repo_server("repo_name_prefix", fake_env.rcs_proxy_connector).await;
        assert!(result.is_ok(), "got {:?}", result.err());
    }
    #[fuchsia::test]
    async fn test_is_server_registered() {
        let test_env = ffx_config::test_init().await.expect("test env");
        let fake_env = FakeTestEnv::new(&test_env).await;

        let repo_name = "registered_test_repo";
        let time_to_wait = Duration::from_secs(5);

        let result =
            is_server_registered(repo_name, fake_env.rcs_proxy_connector.clone(), time_to_wait)
                .await;
        match &result {
            Ok(is_registered) => {
                assert!(is_registered, "Expected server to be registered, but it was not")
            }
            Err(e) => assert!(result.is_ok(), "got {e:?}"),
        };
        let result =
            is_server_registered("unregistered_repo", fake_env.rcs_proxy_connector, time_to_wait)
                .await;
        match &result {
            Ok(is_registered) => {
                assert!(!is_registered, "Expected server NOT to be registered, but it was")
            }
            Err(e) => assert!(result.is_ok(), "got {e:?}"),
        };
    }

    #[fuchsia::test]
    async fn test_deregister_standalone() {
        let test_env = ffx_config::test_init().await.expect("test env");
        let fake_env = FakeTestEnv::new(&test_env).await;

        let result = deregister_standalone(
            "repo_name",
            fake_env.rcs_proxy_connector,
            Duration::from_secs(30),
        )
        .await;
        assert!(result.is_ok(), "got {:?}", result.err());
    }
}
