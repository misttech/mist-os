// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use errors::ffx_bail;
use ffx_config::environment::EnvironmentKind;
use ffx_config::EnvironmentContext;
use ffx_repository_serve_args::ServeCommand;
use ffx_target::{knock_target, TargetProxy};
use fho::{AvailabilityFlag, Connector, FfxMain, FfxTool, Result, SimpleWriter};
use fidl_fuchsia_developer_ffx::{
    RepositoryStorageType, RepositoryTarget as FfxCliRepositoryTarget, TargetInfo,
};
use fidl_fuchsia_developer_ffx_ext::{
    RepositoryRegistrationAliasConflictMode as FfxRepositoryRegistrationAliasConflictMode,
    RepositoryTarget as FfxDaemonRepositoryTarget,
};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_pkg::RepositoryManagerMarker;
use fidl_fuchsia_pkg_rewrite::EngineMarker;
use fuchsia_async as fasync;
use fuchsia_repo::manager::RepositoryManager;
use fuchsia_repo::repo_client::RepoClient;
use fuchsia_repo::repository::{PmRepository, RepoProvider};
use fuchsia_repo::server::RepositoryServer;
use futures::executor::block_on;
use futures::{SinkExt, StreamExt};
use package_tool::{cmd_repo_publish, RepoPublishCommand};
use pkg::repo::register_target_with_fidl_proxies;
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::fs;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use timeout::timeout;
use tuf::metadata::RawSignedMetadata;

const REPO_CONNECT_TIMEOUT_CONFIG: &str = "repository.connect_timeout_secs";
const DEFAULT_CONNECTION_TIMEOUT_SECS: u64 = 120;
const MAX_CONSECUTIVE_CONNECT_ATTEMPTS: u8 = 10;
const REPO_BACKGROUND_FEATURE_FLAG: &str = "repository.server.enabled";
const REPO_FOREGROUND_FEATURE_FLAG: &str = "repository.foreground.enabled";
const REPOSITORY_MANAGER_MONIKER: &str = "/core/pkg-resolver";
const ENGINE_MONIKER: &str = "/core/pkg-resolver";
const DEFAULT_REPO_NAME: &str = "devhost";
const REPO_PATH_RELATIVE_TO_BUILD_DIR: &str = "amber-files";

#[derive(FfxTool)]
#[check(AvailabilityFlag(REPO_FOREGROUND_FEATURE_FLAG))]
pub struct ServeTool {
    #[command]
    cmd: ServeCommand,
    context: EnvironmentContext,
    target_proxy_connector: Connector<TargetProxy>,
    rcs_proxy_connector: Connector<RemoteControlProxy>,
}

fho::embedded_plugin!(ServeTool);

#[tracing::instrument(skip(conn_quit_tx, server_quit_tx))]
fn start_signal_monitoring(
    mut conn_quit_tx: futures::channel::mpsc::Sender<()>,
    mut server_quit_tx: futures::channel::mpsc::Sender<()>,
) {
    tracing::debug!("Starting monitoring for SIGHUP, SIGINT, SIGTERM");
    let mut signals = Signals::new(&[SIGHUP, SIGINT, SIGTERM]).unwrap();
    // Can't use async here, as signals.forever() is blocking.
    std::thread::spawn(move || {
        if let Some(signal) = signals.forever().next() {
            match signal {
                SIGINT | SIGHUP | SIGTERM => {
                    tracing::info!("Received signal {signal}, quitting");
                    let _ = block_on(conn_quit_tx.send(())).ok();
                    let _ = block_on(server_quit_tx.send(())).ok();
                }
                _ => unreachable!(),
            }
        }
    });
}

async fn connect_to_target(
    target_spec: Option<String>,
    target_info: TargetInfo,
    aliases: Option<Vec<String>>,
    storage_type: Option<RepositoryStorageType>,
    repo_server_listen_addr: std::net::SocketAddr,
    connect_timeout: std::time::Duration,
    repo_manager: Arc<RepositoryManager>,
    rcs_proxy: &RemoteControlProxy,
    alias_conflict_mode: FfxRepositoryRegistrationAliasConflictMode,
) -> Result<(), anyhow::Error> {
    let repo_proxy = rcs::connect_to_protocol::<RepositoryManagerMarker>(
        connect_timeout,
        REPOSITORY_MANAGER_MONIKER,
        &rcs_proxy,
    )
    .await
    .with_context(|| format!("connecting to repository manager on {:?}", target_spec))?;

    let engine_proxy =
        rcs::connect_to_protocol::<EngineMarker>(connect_timeout, ENGINE_MONIKER, &rcs_proxy)
            .await
            .with_context(|| format!("binding engine to stream on {:?}", target_spec))?;

    for (repo_name, repo) in repo_manager.repositories() {
        let repo_target = FfxCliRepositoryTarget {
            repo_name: Some(repo_name),
            target_identifier: target_spec.clone(),
            aliases: aliases.clone(),
            storage_type,
            ..Default::default()
        };

        // Construct RepositoryTarget from same args as `ffx target repository register`
        let repo_target_info = FfxDaemonRepositoryTarget::try_from(repo_target)
            .map_err(|e| anyhow!("Failed to build RepositoryTarget: {:?}", e))?;

        register_target_with_fidl_proxies(
            repo_proxy.clone(),
            engine_proxy.clone(),
            &repo_target_info,
            &target_info,
            repo_server_listen_addr,
            &repo,
            alias_conflict_mode.clone(),
        )
        .await
        .map_err(|e| anyhow!("Failed to register repository: {:?}", e))?;
    }
    Ok(())
}

// Constructs a repo client with an explicitly passed trusted
// root, or defaults to the trusted root of the repository if
// none is provided.
async fn repo_client_from_optional_trusted_root(
    trusted_root: Option<Utf8PathBuf>,
    repository: impl RepoProvider + 'static,
) -> Result<RepoClient<Box<dyn RepoProvider>>, anyhow::Error> {
    let repo_client = if let Some(ref trusted_root_path) = trusted_root {
        let buf = async_fs::read(&trusted_root_path)
            .await
            .with_context(|| format!("reading trusted root {trusted_root_path}"))?;

        let trusted_root = RawSignedMetadata::new(buf);

        RepoClient::from_trusted_root(&trusted_root, Box::new(repository) as Box<_>)
            .await
            .with_context(|| {
                format!("Creating repo client using trusted root {trusted_root_path}")
            })?
    } else {
        RepoClient::from_trusted_remote(Box::new(repository) as Box<_>)
            .await
            .with_context(|| format!("Creating repo client using default trusted root"))?
    };
    Ok(repo_client)
}

/// Refreshes repository metadata, in the same way running a shell command
/// `ffx repository publish /path/to/repository` would
async fn refresh_repository_metadata(path: &Utf8PathBuf) -> Result<()> {
    let rf = RepoPublishCommand {
        signing_keys: None,
        trusted_keys: None,
        trusted_root: None,
        package_manifests: vec![],
        package_list_manifests: vec![],
        package_archives: vec![],
        product_bundle: vec![],
        time_versioning: false,
        metadata_current_time: chrono::Utc::now(),
        refresh_root: false,
        clean: false,
        depfile: None,
        copy_mode: fuchsia_repo::repository::CopyMode::Copy,
        delivery_blob_type: 1,
        watch: false,
        ignore_missing_packages: false,
        blob_manifest: None,
        blob_repo_dir: None,
        repo_path: path.clone(),
    };
    cmd_repo_publish(rf)
        .await
        .map_err(|e| fho::user_error!(format!("failed publishing to repo {}: {}", path, e)))
}

async fn main_connect_loop(
    cmd: &ServeCommand,
    repo_path: &Utf8Path,
    server_addr: core::net::SocketAddr,
    connect_timeout: std::time::Duration,
    repo_manager: Arc<RepositoryManager>,
    mut loop_stop_rx: futures::channel::mpsc::Receiver<()>,
    rcs_proxy: Connector<RemoteControlProxy>,
    target_proxy: Connector<TargetProxy>,
    mut writer: impl Write + 'static,
) -> Result<()> {
    // We try to reconnect unless MAX_CONSECUTIVE_CONNECT_ATTEMPTS reconnect
    // attempts in immediate succession fail.
    let mut attempts = 0;

    // Outer connection loop, retries when disconnected.
    loop {
        // Check if we want to exit before starting to (re-)connect.
        if let Ok(Some(())) = loop_stop_rx.try_next() {
            return Ok(());
        }
        if attempts >= MAX_CONSECUTIVE_CONNECT_ATTEMPTS {
            ffx_bail!("Stopping reconnecting after {attempts} consecutive failed attempts");
        } else {
            attempts += 1;
        }

        let mut target_spec_from_rcs_proxy: Option<String> = None;
        let rcs_proxy = timeout(
            connect_timeout,
            rcs_proxy.try_connect(|target| {
                tracing::info!(
                    "Waiting for target '{}' to return",
                    match target {
                        Some(s) => s,
                        _ => "None",
                    }
                );
                target_spec_from_rcs_proxy = target.clone();
                Ok(())
            }),
        )
        .await;
        let rcs_proxy = match rcs_proxy {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                return Err(e);
            }
            Err(e) => {
                tracing::warn!("Attempt #{attempts}: failed to connect to rcs, retrying: {e}");
                continue;
            }
        };
        let mut target_spec_from_target_proxy: Option<String> = None;
        let target_proxy = target_proxy
            .try_connect(|target| {
                tracing::info!(
                    "Waiting for target '{}' to return",
                    match target {
                        Some(s) => s,
                        _ => "None",
                    }
                );
                target_spec_from_target_proxy = target.clone();
                Ok(())
            })
            .await?;

        // This catches an edge case where the environment is not populated consistently.
        if target_spec_from_rcs_proxy != target_spec_from_target_proxy {
            tracing::warn!(
                "Attempt #{}: RCS and target proxies do not match: '{:?}', '{:?}', retrying.",
                attempts,
                target_spec_from_rcs_proxy,
                target_spec_from_target_proxy,
            );
            continue;
        }

        let target_info: TargetInfo = timeout(Duration::from_secs(2), target_proxy.identity())
            .await
            .context("Timed out getting target identity")?
            .context("Failed to get target identity")?;

        let connection = connect_to_target(
            target_spec_from_rcs_proxy.clone(),
            target_info,
            Some(cmd.alias.clone()),
            cmd.storage_type,
            server_addr,
            connect_timeout,
            Arc::clone(&repo_manager),
            &rcs_proxy,
            cmd.alias_conflict_mode.into(),
        )
        .await;
        match connection {
            Ok(()) => {
                attempts = 0;
                let s = match target_spec_from_rcs_proxy {
                    Some(t) => format!(
                        "Serving repository '{repo_path}' to target '{t}' over address '{}'.",
                        server_addr
                    ),
                    None => {
                        format!("Serving repository '{repo_path}' over address '{server_addr}'.")
                    }
                };
                if let Err(e) = writeln!(writer, "{}", s) {
                    tracing::error!("Failed to write to output: {:?}", e);
                }
                tracing::info!("{}", s);
                loop {
                    fuchsia_async::Timer::new(std::time::Duration::from_secs(10)).await;
                    // Check for an exit request before knocking the target
                    if let Ok(Some(())) = loop_stop_rx.try_next() {
                        return Ok(());
                    }
                    match knock_target(&target_proxy).await {
                        Ok(()) => {
                            // Nothing to do, continue checking connection
                        }
                        Err(e) => {
                            let s = format!("Connection to target lost, retrying. Error: {}", e);
                            if let Err(e) = writeln!(writer, "{}", s) {
                                tracing::error!("Failed to write to output: {:?}", e);
                            }
                            tracing::warn!(s);
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Cannot connect to target: {:?}, retrying.", e);
                continue;
            }
        };
    }
}

#[async_trait(?Send)]
impl FfxMain for ServeTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: Self::Writer) -> Result<()> {
        serve_impl(
            self.target_proxy_connector,
            self.rcs_proxy_connector,
            self.cmd,
            self.context,
            writer,
        )
        .await?;
        Ok(())
    }
}

async fn serve_impl<W: Write + 'static>(
    target_proxy: Connector<TargetProxy>,
    rcs_proxy: Connector<RemoteControlProxy>,
    cmd: ServeCommand,
    context: EnvironmentContext,
    mut writer: W,
) -> Result<()> {
    let bg: bool =
        context.get(REPO_BACKGROUND_FEATURE_FLAG).context("checking for background server flag")?;
    if bg {
        ffx_bail!(
            r#"The ffx setting '{}' and the foreground server '{}' are mutually incompatible.
Please disable background serving by running the following commands:
$ ffx config remove repository.server.enabled
$ ffx doctor --restart-daemon"#,
            REPO_BACKGROUND_FEATURE_FLAG,
            REPO_FOREGROUND_FEATURE_FLAG,
        );
    }

    let connect_timeout =
        context.get(REPO_CONNECT_TIMEOUT_CONFIG).unwrap_or(DEFAULT_CONNECTION_TIMEOUT_SECS);
    let connect_timeout = std::time::Duration::from_secs(connect_timeout);

    let repo_manager: Arc<RepositoryManager> = RepositoryManager::new();

    let repo_path = match (cmd.repo_path.clone(), cmd.product_bundle.clone()) {
        (Some(_), Some(_)) => {
            ffx_bail!("Cannot specify both --repo-path and --product-bundle");
        }
        (None, Some(product_bundle)) => {
            if cmd.repository.is_some() {
                ffx_bail!("--repository is not supported with --product-bundle");
            }
            let repositories = sdk_metadata::get_repositories(product_bundle.clone())
                .with_context(|| {
                    format!("getting repositories from product bundle {product_bundle}")
                })?;
            for repository in repositories {
                let repo_name = repository.aliases().first().unwrap().clone();

                let repo_client = RepoClient::from_trusted_remote(Box::new(repository) as Box<_>)
                    .await
                    .with_context(|| format!("Creating a repo client for {repo_name}"))?;
                repo_manager.add(repo_name, repo_client);
            }

            if cmd.refresh_metadata {
                tracing::warn!(
                    "--refresh-metadata is not supported with product bundles, ignoring"
                );
            }

            product_bundle
        }
        (repo_path, None) => {
            let repo_path = if let Some(repo_path) = repo_path {
                repo_path
            } else if let EnvironmentKind::InTree { build_dir: Some(build_dir), .. } =
                context.env_kind()
            {
                let build_dir = Utf8Path::from_path(build_dir)
                    .with_context(|| format!("converting repo path to UTF-8 {:?}", repo_path))?;

                build_dir.join(REPO_PATH_RELATIVE_TO_BUILD_DIR)
            } else {
                ffx_bail!("Either --repo-path or --product-bundle need to be specified");
            };

            // Create PmRepository and RepoClient
            let repo_path = repo_path
                .canonicalize_utf8()
                .with_context(|| format!("canonicalizing repo path {:?}", repo_path))?;
            let repository = PmRepository::new(repo_path.clone());

            let mut repo_client =
                repo_client_from_optional_trusted_root(cmd.trusted_root.clone(), repository)
                    .await?;

            repo_client.update().await.context("updating the repository metadata")?;

            let repo_name = cmd.repository.clone().unwrap_or_else(|| DEFAULT_REPO_NAME.to_string());
            repo_manager.add(repo_name, repo_client);

            if cmd.refresh_metadata {
                refresh_repository_metadata(&repo_path).await?;
            }

            repo_path
        }
    };

    // Serve RepositoryManager over a RepositoryServer
    let (server_fut, _, server) = RepositoryServer::builder(cmd.address, Arc::clone(&repo_manager))
        .start()
        .await
        .with_context(|| format!("starting repository server"))?;

    // Write port file if needed
    if let Some(port_path) = cmd.port_path.clone() {
        let port = server.local_addr().port().to_string();

        fs::write(port_path, port.clone())
            .with_context(|| format!("creating port file for port {}", port))?;
    };

    let server_addr = server.local_addr().clone();

    let server_task = fasync::Task::local(server_fut);
    let (mut server_stop_tx, mut server_stop_rx) = futures::channel::mpsc::channel::<()>(1);
    let (loop_stop_tx, loop_stop_rx) = futures::channel::mpsc::channel::<()>(1);

    // Register signal handler and monitor for server requests.
    let _server_stop_task = fasync::Task::local(async move {
        if let Some(_) = server_stop_rx.next().await {
            server.stop();
        }
    });
    start_signal_monitoring(loop_stop_tx.clone(), server_stop_tx.clone());

    let result = if cmd.no_device {
        let s = format!("Serving repository '{repo_path}' over address '{}'.", server_addr);
        writeln!(writer, "{}", s).map_err(|e| anyhow!("Failed to write to output: {:?}", e))?;
        tracing::info!("{}", s);
        Ok(())
    } else {
        let r = main_connect_loop(
            &cmd,
            &repo_path,
            server_addr,
            connect_timeout,
            repo_manager,
            loop_stop_rx,
            rcs_proxy,
            target_proxy,
            writer,
        )
        .await;
        if r.is_err() {
            let _ = server_stop_tx.send(()).await;
        }
        r
    };

    // Wait for the server to shut down.
    server_task.await;

    result
}

///////////////////////////////////////////////////////////////////////////////
// tests
#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use ffx_config::keys::TARGET_DEFAULT_KEY;
    use ffx_config::{ConfigLevel, TestEnv};
    use fho::macro_deps::ffx_writer::TestBuffer;
    use fho::testing::ToolEnv;
    use fho::TryFromEnv;
    use fidl::endpoints::DiscoverableProtocolMarker;
    use fidl_fuchsia_developer_ffx::{
        RemoteControlState, RepositoryRegistrationAliasConflictMode, SshHostAddrInfo,
        TargetAddrInfo, TargetIpPort, TargetRequest, TargetState,
    };
    use fidl_fuchsia_developer_remotecontrol as frcs;
    use fidl_fuchsia_net::{IpAddress, Ipv4Address};
    use fidl_fuchsia_pkg::{
        MirrorConfig, RepositoryConfig, RepositoryManagerRequest, RepositoryManagerRequestStream,
    };
    use fidl_fuchsia_pkg_rewrite::{
        EditTransactionRequest, EngineRequest, EngineRequestStream, RuleIteratorRequest,
    };
    use fidl_fuchsia_pkg_rewrite_ext::Rule;
    use frcs::RemoteControlMarker;
    use fuchsia_repo::repo_builder::RepoBuilder;
    use fuchsia_repo::repo_keys::RepoKeys;
    use fuchsia_repo::repository::HttpRepository;
    use fuchsia_repo::test_utils;
    use futures::channel::mpsc;
    use futures::TryStreamExt;
    use std::collections::BTreeSet;
    use std::sync::Mutex;
    use std::time;
    use tuf::crypto::Ed25519PrivateKey;
    use tuf::metadata::Metadata;
    use url::Url;

    const REPO_NAME: &str = "some-repo";
    const REPO_IPV4_ADDR: [u8; 4] = [127, 0, 0, 1];
    const REPO_ADDR: &str = "127.0.0.1";
    const REPO_PORT: u16 = 0;
    const DEVICE_PORT: u16 = 5;
    const HOST_ADDR: &str = "1.2.3.4";
    const TARGET_NODENAME: &str = "some-target";
    const EMPTY_REPO_PATH: &str =
        concat!(env!("ROOT_OUT_DIR"), "/test_data/ffx_lib_pkg/empty-repo");

    macro_rules! rule {
        ($host_match:expr => $host_replacement:expr,
         $path_prefix_match:expr => $path_prefix_replacement:expr) => {
            Rule::new($host_match, $host_replacement, $path_prefix_match, $path_prefix_replacement)
                .unwrap()
        };
    }

    fn to_target_info(nodename: String) -> TargetInfo {
        let device_addr = TargetAddrInfo::IpPort(TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
            port: DEVICE_PORT,
        });

        TargetInfo {
            nodename: Some(nodename),
            addresses: Some(vec![device_addr.clone()]),
            ssh_address: Some(device_addr.clone()),
            ssh_host_address: Some(SshHostAddrInfo { address: HOST_ADDR.to_string() }),
            age_ms: Some(101),
            rcs_state: Some(RemoteControlState::Up),
            target_state: Some(TargetState::Unknown),
            ..Default::default()
        }
    }

    struct FakeRcs;

    impl FakeRcs {
        fn new(repo_manager: FakeRepositoryManager, engine: FakeEngine) -> RemoteControlProxy {
            let fake_rcs_proxy: RemoteControlProxy =
                fho::testing::fake_proxy(move |req| match req {
                    frcs::RemoteControlRequest::OpenCapability {
                        moniker: _,
                        capability_set: _,
                        capability_name,
                        server_channel,
                        flags: _,
                        responder,
                    } => {
                        match capability_name.as_str() {
                            RepositoryManagerMarker::PROTOCOL_NAME => repo_manager.spawn(
                                fidl::endpoints::ServerEnd::<RepositoryManagerMarker>::new(
                                    server_channel,
                                )
                                .into_stream()
                                .unwrap(),
                            ),
                            EngineMarker::PROTOCOL_NAME => engine.spawn(
                                fidl::endpoints::ServerEnd::<EngineMarker>::new(server_channel)
                                    .into_stream()
                                    .unwrap(),
                            ),
                            _ => {
                                unreachable!();
                            }
                        }
                        responder.send(Ok(())).unwrap();
                    }
                    _ => panic!("unexpected request: {:?}", req),
                });

            fake_rcs_proxy
        }
    }

    #[derive(Debug, PartialEq)]
    enum TargetEvent {
        Identity,
        OpenRemoteControl,
    }
    struct FakeTarget;

    impl FakeTarget {
        fn new(knock_skip: Option<Vec<u32>>) -> (Self, TargetProxy, mpsc::Receiver<()>) {
            let (sender, target_rx) = mpsc::channel::<()>(1);
            let events = Arc::new(Mutex::new(Vec::new()));
            let events_closure = Arc::clone(&events);

            let mut knock_counter = 0;
            let knock_skip = if let Some(k) = knock_skip { k } else { vec![] };

            let target_proxy: TargetProxy = fho::testing::fake_proxy(move |req| match req {
                TargetRequest::Identity { responder, .. } => {
                    let mut sender = sender.clone();
                    let events_closure = events_closure.clone();

                    fasync::Task::local(async move {
                        events_closure.lock().unwrap().push(TargetEvent::Identity);
                        responder.send(&to_target_info(TARGET_NODENAME.to_string())).unwrap();
                        let _send = sender.send(()).await.unwrap();
                    })
                    .detach();
                }
                TargetRequest::OpenRemoteControl { remote_control, responder } => {
                    let mut s = remote_control.into_stream().unwrap();
                    let knock_skip = knock_skip.clone();
                    fasync::Task::local(async move {
                        if let Ok(Some(req)) = s.try_next().await {
                            match req {
                                frcs::RemoteControlRequest::OpenCapability {
                                    moniker: _,
                                    capability_set: _,
                                    capability_name,
                                    server_channel,
                                    flags: _,
                                    responder,
                                } => {
                                    match capability_name.as_str() {
                                        RemoteControlMarker::PROTOCOL_NAME => {
                                            // Serve the periodic knock whether the fake target is alive
                                            // By knock_rcs_impl() in
                                            // src/developer/ffx/lib/rcs/src/lib.rs
                                            // a knock is considered unsuccessful if there is no
                                            // channel connection available within the timeout.
                                            knock_counter += 1;
                                            if knock_skip.contains(&knock_counter) { // Do not respond
                                            } else {
                                                let mut stream = fidl::endpoints::ServerEnd::<
                                                    RemoteControlMarker,
                                                >::new(
                                                    server_channel
                                                )
                                                .into_stream()
                                                .unwrap();
                                                fasync::Task::local(async move {
                                                    while let Some(Ok(_)) = stream.next().await {
                                                        // Do nada, just await the request, this is required for target knocking
                                                    }
                                                })
                                                .detach();
                                                let _ = responder.send(Ok(())).unwrap();
                                            }
                                        }
                                        e => {
                                            panic!("Requested capability not implemented: {}", e);
                                        }
                                    };
                                }
                                _ => panic!("unexpected request: {:?}", req),
                            };
                        }
                    })
                    .detach();

                    knock_counter += 1;
                    if knock_counter != 200 {
                        let mut sender = sender.clone();
                        let events_closure = events_closure.clone();

                        fasync::Task::local(async move {
                            events_closure.lock().unwrap().push(TargetEvent::OpenRemoteControl);
                            responder.send(Ok(())).unwrap();
                            let _send = sender.send(()).await.unwrap();
                        })
                        .detach();
                    }
                }
                _ => panic!("unexpected request: {:?}", req),
            });
            (Self, target_proxy, target_rx)
        }
    }

    #[derive(Debug, PartialEq)]
    enum RepositoryManagerEvent {
        Add { repo: RepositoryConfig },
    }

    #[derive(Clone)]
    struct FakeRepositoryManager {
        events: Arc<Mutex<Vec<RepositoryManagerEvent>>>,
        sender: mpsc::Sender<()>,
    }

    impl FakeRepositoryManager {
        fn new() -> (Self, mpsc::Receiver<()>) {
            let (sender, rx) = futures::channel::mpsc::channel::<()>(1);
            let events = Arc::new(Mutex::new(Vec::new()));

            (Self { events, sender }, rx)
        }

        fn spawn(&self, mut stream: RepositoryManagerRequestStream) {
            let sender = self.sender.clone();
            let events_closure = Arc::clone(&self.events);

            fasync::Task::local(async move {
                while let Some(Ok(req)) = stream.next().await {
                    match req {
                        RepositoryManagerRequest::Add { repo, responder } => {
                            let mut sender = sender.clone();
                            let events_closure = events_closure.clone();

                            fasync::Task::local(async move {
                                events_closure
                                    .lock()
                                    .unwrap()
                                    .push(RepositoryManagerEvent::Add { repo });
                                responder.send(Ok(())).unwrap();
                                let _send = sender.send(()).await.unwrap();
                            })
                            .detach();
                        }
                        _ => panic!("unexpected request: {:?}", req),
                    }
                }
            })
            .detach();
        }

        fn take_events(&self) -> Vec<RepositoryManagerEvent> {
            self.events.lock().unwrap().drain(..).collect::<Vec<_>>()
        }
    }

    #[derive(Debug, PartialEq)]
    enum RewriteEngineEvent {
        ResetAll,
        ListDynamic,
        IteratorNext,
        EditTransactionAdd { rule: Rule },
        EditTransactionCommit,
    }

    #[derive(Clone)]
    struct FakeEngine {
        events: Arc<Mutex<Vec<RewriteEngineEvent>>>,
        sender: mpsc::Sender<()>,
    }

    impl FakeEngine {
        fn new() -> (Self, mpsc::Receiver<()>) {
            let (sender, rx) = futures::channel::mpsc::channel::<()>(1);
            let events = Arc::new(Mutex::new(Vec::new()));

            (Self { events, sender }, rx)
        }

        fn spawn(&self, mut stream: EngineRequestStream) {
            let rules: Arc<Mutex<Vec<Rule>>> = Arc::new(Mutex::new(Vec::<Rule>::new()));
            let sender = self.sender.clone();
            let events_closure = Arc::clone(&self.events);

            fasync::Task::local(async move {
                while let Some(Ok(req)) = stream.next().await {
                    match req {
                        EngineRequest::StartEditTransaction { transaction, control_handle: _ } => {
                            let mut sender = sender.clone();
                            let rules = Arc::clone(&rules);
                            let events_closure = Arc::clone(&events_closure);

                            fasync::Task::local(async move {
                                let mut stream = transaction.into_stream().unwrap();
                                while let Some(request) = stream.next().await {
                                    let request = request.unwrap();
                                    match request {
                                        EditTransactionRequest::ResetAll { control_handle: _ } => {
                                            events_closure
                                                .lock()
                                                .unwrap()
                                                .push(RewriteEngineEvent::ResetAll);
                                        }
                                        EditTransactionRequest::ListDynamic {
                                            iterator,
                                            control_handle: _,
                                        } => {
                                            events_closure
                                                .lock()
                                                .unwrap()
                                                .push(RewriteEngineEvent::ListDynamic);
                                            let mut stream = iterator.into_stream().unwrap();

                                            let mut rules =
                                                rules.lock().unwrap().clone().into_iter();

                                            while let Some(req) = stream.try_next().await.unwrap() {
                                                let RuleIteratorRequest::Next { responder } = req;
                                                events_closure
                                                    .lock()
                                                    .unwrap()
                                                    .push(RewriteEngineEvent::IteratorNext);

                                                if let Some(rule) = rules.next() {
                                                    responder.send(&[rule.into()]).unwrap();
                                                } else {
                                                    responder.send(&[]).unwrap();
                                                }
                                            }
                                        }
                                        EditTransactionRequest::Add { rule, responder } => {
                                            events_closure.lock().unwrap().push(
                                                RewriteEngineEvent::EditTransactionAdd {
                                                    rule: rule.try_into().unwrap(),
                                                },
                                            );
                                            responder.send(Ok(())).unwrap()
                                        }
                                        EditTransactionRequest::Commit { responder } => {
                                            events_closure
                                                .lock()
                                                .unwrap()
                                                .push(RewriteEngineEvent::EditTransactionCommit);
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

        fn take_events(&self) -> Vec<RewriteEngineEvent> {
            self.events.lock().unwrap().drain(..).collect::<Vec<_>>()
        }
    }

    async fn get_test_env() -> TestEnv {
        let test_env = ffx_config::test_init().await.expect("test initialization");

        test_env
            .context
            .query(REPO_FOREGROUND_FEATURE_FLAG)
            .level(Some(ConfigLevel::User))
            .set("true".into())
            .await
            .unwrap();

        test_env
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_register() {
        async fn run_test_start_register(refresh_metadata: bool) {
            let test_env = get_test_env().await;

            test_env
                .context
                .query(TARGET_DEFAULT_KEY)
                .level(Some(ConfigLevel::User))
                .set(TARGET_NODENAME.into())
                .await
                .unwrap();

            let (fake_repo, mut fake_repo_rx) = FakeRepositoryManager::new();
            let (fake_engine, mut fake_engine_rx) = FakeEngine::new();
            let (_, fake_target_proxy, mut fake_target_rx) = FakeTarget::new(None);

            let frc = fake_repo.clone();
            let fec = fake_engine.clone();

            let tool_env = ToolEnv::new()
                .remote_factory_closure(move || {
                    let fake_repo = frc.clone();
                    let fake_engine = fec.clone();
                    async move { Ok(FakeRcs::new(fake_repo, fake_engine)) }
                })
                .target_factory_closure(move || {
                    let fake_target_proxy = fake_target_proxy.clone();
                    async { Ok(fake_target_proxy) }
                });

            let env = tool_env.make_environment(test_env.context.clone());

            let tmp_port_file = tempfile::NamedTempFile::new().unwrap();

            let serve_tool = ServeTool {
                cmd: ServeCommand {
                    repository: Some(REPO_NAME.to_string()),
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: Some(EMPTY_REPO_PATH.into()),
                    product_bundle: None,
                    alias: vec!["example.com".into(), "fuchsia.com".into()],
                    storage_type: Some(RepositoryStorageType::Ephemeral),
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
                    port_path: Some(tmp_port_file.path().to_owned()),
                    no_device: false,
                    refresh_metadata: refresh_metadata,
                },
                context: env.context.clone(),
                target_proxy_connector: Connector::try_from_env(&env)
                    .await
                    .expect("Could not make target proxy test connector"),
                rcs_proxy_connector: Connector::try_from_env(&env)
                    .await
                    .expect("Could not make RCS test connector"),
            };

            let test_stdout = TestBuffer::default();
            let writer = SimpleWriter::new_buffers(test_stdout.clone(), Vec::new());

            // Run main in background
            let _task = fasync::Task::local(async move { serve_tool.main(writer).await.unwrap() });

            // Future resolves once repo server communicates with them.
            let _timeout = timeout(time::Duration::from_secs(10), async {
                let _ = fake_target_rx.next().await.unwrap();
                let _ = fake_repo_rx.next().await.unwrap();
                let _ = fake_engine_rx.next().await.unwrap();
            })
            .await
            .unwrap();

            // Get dynamic port
            let dynamic_repo_port =
                fs::read_to_string(tmp_port_file.path()).unwrap().parse::<u16>().unwrap();
            tmp_port_file.close().unwrap();

            let repo_url = format!("http://{REPO_ADDR}:{dynamic_repo_port}/{REPO_NAME}");

            assert_eq!(
                fake_repo.take_events(),
                vec![RepositoryManagerEvent::Add {
                    repo: RepositoryConfig {
                        mirrors: Some(vec![MirrorConfig {
                            mirror_url: Some(repo_url.clone()),
                            subscribe: Some(true),
                            ..Default::default()
                        }]),
                        repo_url: Some(format!("fuchsia-pkg://{}", REPO_NAME)),
                        root_keys: Some(vec![fuchsia_repo::test_utils::repo_key().into()]),
                        root_version: Some(1),
                        root_threshold: Some(1),
                        use_local_mirror: Some(false),
                        storage_type: Some(fidl_fuchsia_pkg::RepositoryStorageType::Ephemeral),
                        ..Default::default()
                    }
                }],
            );

            assert_eq!(
                fake_engine.take_events(),
                vec![
                    RewriteEngineEvent::ListDynamic,
                    RewriteEngineEvent::IteratorNext,
                    RewriteEngineEvent::ResetAll,
                    RewriteEngineEvent::EditTransactionAdd {
                        rule: rule!("example.com" => REPO_NAME, "/" => "/"),
                    },
                    RewriteEngineEvent::EditTransactionAdd {
                        rule: rule!("fuchsia.com" => REPO_NAME, "/" => "/"),
                    },
                    RewriteEngineEvent::EditTransactionCommit,
                ],
            );

            // Check repository state.
            let http_repo = HttpRepository::new(
                fuchsia_hyper::new_client(),
                Url::parse(&repo_url).unwrap(),
                Url::parse(&format!("{repo_url}/blobs")).unwrap(),
                BTreeSet::new(),
            );
            let mut repo_client = RepoClient::from_trusted_remote(http_repo).await.unwrap();

            assert_matches!(repo_client.update().await, Ok(true));
        }

        let test_cases = [false, true];
        for tc in test_cases {
            run_test_start_register(tc).await;
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_auto_reconnect() {
        let test_env = get_test_env().await;

        test_env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NODENAME.into())
            .await
            .unwrap();

        let (fake_repo, mut fake_repo_rx) = FakeRepositoryManager::new();
        let (fake_engine, mut fake_engine_rx) = FakeEngine::new();
        // Create a target where the second and third knock requests are not answered, triggering the reconnect
        // loops in both the repository serve plugin and the Connect<TargetProxy>.
        let (_, fake_target_proxy, mut fake_target_rx) = FakeTarget::new(Some(vec![2, 3]));

        let frc = fake_repo.clone();
        let fec = fake_engine.clone();

        let tool_env = ToolEnv::new()
            .remote_factory_closure(move || {
                let fake_repo = frc.clone();
                let fake_engine = fec.clone();
                async move { Ok(FakeRcs::new(fake_repo, fake_engine)) }
            })
            .target_factory_closure(move || {
                let fake_target_proxy = fake_target_proxy.clone();
                async { Ok(fake_target_proxy) }
            });

        let env = tool_env.make_environment(test_env.context.clone());

        let tmp_port_file = tempfile::NamedTempFile::new().unwrap();
        let tmp_port_file_path = tmp_port_file.path().to_owned();

        let test_stdout = TestBuffer::default();
        let writer = SimpleWriter::new_buffers(test_stdout.clone(), Vec::new());

        // Run main in background
        let _task = fasync::Task::local(async move {
            serve_impl(
                Connector::try_from_env(&env)
                    .await
                    .expect("Could not make target proxy test connector"),
                Connector::try_from_env(&env).await.expect("Could not make RCS test connector"),
                ServeCommand {
                    repository: Some(REPO_NAME.to_string()),
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: Some(EMPTY_REPO_PATH.into()),
                    product_bundle: None,
                    alias: vec!["example.com".into(), "fuchsia.com".into()],
                    storage_type: Some(RepositoryStorageType::Ephemeral),
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
                    port_path: Some(tmp_port_file_path),
                    no_device: false,
                    refresh_metadata: false,
                },
                env.context.clone(),
                writer,
            )
            .await
            .unwrap()
        });

        // Future resolves once repo server communicates with them.
        let _timeout = timeout(time::Duration::from_secs(10), async {
            let _ = fake_target_rx.next().await.unwrap();
            let _ = fake_repo_rx.next().await.unwrap();
            let _ = fake_engine_rx.next().await.unwrap();
        })
        .await
        .unwrap();

        // Get dynamic port
        let dynamic_repo_port =
            fs::read_to_string(tmp_port_file.path()).unwrap().parse::<u16>().unwrap();
        tmp_port_file.close().unwrap();

        let repo_url = format!("http://{REPO_ADDR}:{dynamic_repo_port}/{REPO_NAME}");

        assert_eq!(
            fake_repo.take_events(),
            vec![RepositoryManagerEvent::Add {
                repo: RepositoryConfig {
                    mirrors: Some(vec![MirrorConfig {
                        mirror_url: Some(repo_url.clone()),
                        subscribe: Some(true),
                        ..Default::default()
                    }]),
                    repo_url: Some(format!("fuchsia-pkg://{}", REPO_NAME)),
                    root_keys: Some(vec![fuchsia_repo::test_utils::repo_key().into()]),
                    root_version: Some(1),
                    root_threshold: Some(1),
                    use_local_mirror: Some(false),
                    storage_type: Some(fidl_fuchsia_pkg::RepositoryStorageType::Ephemeral),
                    ..Default::default()
                }
            }],
        );

        assert_eq!(
            fake_engine.take_events(),
            vec![
                RewriteEngineEvent::ListDynamic,
                RewriteEngineEvent::IteratorNext,
                RewriteEngineEvent::ResetAll,
                RewriteEngineEvent::EditTransactionAdd {
                    rule: rule!("example.com" => REPO_NAME, "/" => "/"),
                },
                RewriteEngineEvent::EditTransactionAdd {
                    rule: rule!("fuchsia.com" => REPO_NAME, "/" => "/"),
                },
                RewriteEngineEvent::EditTransactionCommit,
            ],
        );

        // Check repository state.
        let http_repo = HttpRepository::new(
            fuchsia_hyper::new_client(),
            Url::parse(&repo_url).unwrap(),
            Url::parse(&format!("{repo_url}/blobs")).unwrap(),
            BTreeSet::new(),
        );
        let mut repo_client = RepoClient::from_trusted_remote(http_repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));

        // Wait for the sequence of outputs on stdout to indicate
        // serving a repo, then reconnecting, then serving again.
        let expected_outputs =
            vec!["Serving repository", "Connection to target lost", "Serving repository"];
        for expected in expected_outputs {
            let mut output_found = "";
            'attempts: for _ in 0..30 {
                let out = test_stdout.clone().into_string();
                for line in out.split("\n") {
                    if line.starts_with(expected) {
                        output_found = expected;
                        break 'attempts;
                    }
                }
                fasync::Timer::new(time::Duration::from_secs(1)).await;
            }
            assert_eq!(output_found, expected);
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_no_device() {
        let test_env = get_test_env().await;

        let tmp_port_file = tempfile::NamedTempFile::new().unwrap();
        let tmp_port_file_path = tmp_port_file.path().to_owned();

        let (fake_repo, _fake_repo_rx) = FakeRepositoryManager::new();
        let (fake_engine, _fake_engine_rx) = FakeEngine::new();
        let (_, fake_target_proxy, _) = FakeTarget::new(None);

        let frc = fake_repo.clone();
        let fec = fake_engine.clone();

        let tool_env = ToolEnv::new()
            .remote_factory_closure(move || {
                let fake_repo = frc.clone();
                let fake_engine = fec.clone();
                async move { Ok(FakeRcs::new(fake_repo, fake_engine)) }
            })
            .target_factory_closure(move || {
                let fake_target_proxy = fake_target_proxy.clone();
                async { Ok(fake_target_proxy) }
            });

        let env = tool_env.make_environment(test_env.context.clone());

        let test_stdout = TestBuffer::default();
        let writer = SimpleWriter::new_buffers(test_stdout.clone(), Vec::new());

        // Run main in background
        let _task = fasync::Task::local(async move {
            serve_impl(
                Connector::try_from_env(&env)
                    .await
                    .expect("Could not make target proxy test connector"),
                Connector::try_from_env(&env).await.expect("Could not make RCS test connector"),
                ServeCommand {
                    repository: Some(REPO_NAME.to_string()),
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: Some(EMPTY_REPO_PATH.into()),
                    product_bundle: None,
                    alias: vec!["example.com".into(), "fuchsia.com".into()],
                    storage_type: Some(RepositoryStorageType::Ephemeral),
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
                    port_path: Some(tmp_port_file_path),
                    no_device: false,
                    refresh_metadata: false,
                },
                env.context.clone(),
                writer,
            )
            .await
            .unwrap()
        });

        // Wait for the "Serving repository ..." output
        for _ in 0..10 {
            if !test_stdout.clone().into_string().is_empty() {
                break;
            }
            fasync::Timer::new(time::Duration::from_millis(100)).await;
        }

        // Get dynamic port
        let dynamic_repo_port =
            fs::read_to_string(tmp_port_file.path()).unwrap().parse::<u16>().unwrap();
        tmp_port_file.close().unwrap();

        let repo_url = format!("http://{REPO_ADDR}:{dynamic_repo_port}/{REPO_NAME}");

        // Check repository state.
        let http_repo = HttpRepository::new(
            fuchsia_hyper::new_client(),
            Url::parse(&repo_url).unwrap(),
            Url::parse(&format!("{repo_url}/blobs")).unwrap(),
            BTreeSet::new(),
        );
        let mut repo_client = RepoClient::from_trusted_remote(http_repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));
    }

    async fn write_product_bundle(pb_dir: &Utf8Path) {
        let blobs_dir = pb_dir.join("blobs");

        let mut repositories = vec![];
        for repo_name in ["fuchsia.com", "example.com"] {
            let metadata_path = pb_dir.join(repo_name);
            fuchsia_repo::test_utils::make_repo_dir(metadata_path.as_ref(), blobs_dir.as_ref())
                .await;
            repositories.push(sdk_metadata::Repository {
                name: repo_name.into(),
                metadata_path,
                blobs_path: blobs_dir.clone(),
                delivery_blob_type: 1,
                root_private_key_path: None,
                targets_private_key_path: None,
                snapshot_private_key_path: None,
                timestamp_private_key_path: None,
            });
        }

        let pb = sdk_metadata::ProductBundle::V2(sdk_metadata::ProductBundleV2 {
            product_name: "test".into(),
            product_version: "test-product-version".into(),
            partitions: assembly_partitions_config::PartitionsConfig::default(),
            sdk_version: "test-sdk-version".into(),
            system_a: None,
            system_b: None,
            system_r: None,
            repositories,
            update_package_hash: None,
            virtual_devices_path: None,
        });
        pb.write(&pb_dir).unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_serve_product_bundle() {
        let test_env = get_test_env().await;

        test_env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NODENAME.into())
            .await
            .unwrap();

        let tmp_pb_dir = tempfile::tempdir().unwrap();
        let pb_dir = Utf8Path::from_path(tmp_pb_dir.path()).unwrap().canonicalize_utf8().unwrap();
        write_product_bundle(&pb_dir).await;

        let tmp_port_file = tempfile::NamedTempFile::new().unwrap();
        let tmp_port_file_path = tmp_port_file.path().to_owned();

        let (fake_repo, mut fake_repo_rx) = FakeRepositoryManager::new();
        let (fake_engine, _fake_engine_rx) = FakeEngine::new();
        let (_, fake_target_proxy, _) = FakeTarget::new(None);

        let frc = fake_repo.clone();
        let fec = fake_engine.clone();
        let ftpc = fake_target_proxy.clone();

        let tool_env = ToolEnv::new()
            .remote_factory_closure(move || {
                let fake_repo = frc.clone();
                let fake_engine = fec.clone();
                async move { Ok(FakeRcs::new(fake_repo, fake_engine)) }
            })
            .target_factory_closure(move || {
                let fake_target_proxy = ftpc.clone();
                async { Ok(fake_target_proxy) }
            });

        let env = tool_env.make_environment(test_env.context.clone());

        // Future resolves once fake target exists
        let _timeout = timeout(time::Duration::from_secs(10), async {
            let target = fake_target_proxy.identity().await.unwrap();
            assert_eq!(target, to_target_info(TARGET_NODENAME.to_string()));
        })
        .await
        .unwrap();

        let test_stdout = TestBuffer::default();
        let writer = SimpleWriter::new_buffers(test_stdout.clone(), Vec::new());

        // Run main in background
        let _task = fasync::Task::local(async move {
            serve_impl(
                Connector::try_from_env(&env)
                    .await
                    .expect("Could not make target proxy test connector"),
                Connector::try_from_env(&env).await.expect("Could not make RCS test connector"),
                ServeCommand {
                    repository: None,
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: None,
                    product_bundle: Some(pb_dir),
                    alias: vec![],
                    storage_type: Some(RepositoryStorageType::Ephemeral),
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
                    port_path: Some(tmp_port_file_path),
                    no_device: false,
                    refresh_metadata: false,
                },
                test_env.context.clone(),
                writer,
            )
            .await
            .unwrap()
        });

        // Future resolves once repo server communicates with them.
        let _timeout = timeout(time::Duration::from_secs(10), async {
            let _ = fake_repo_rx.next().await.unwrap();
        })
        .await
        .unwrap();

        // Get dynamic port
        let dynamic_repo_port =
            fs::read_to_string(tmp_port_file.path()).unwrap().parse::<u16>().unwrap();
        tmp_port_file.close().unwrap();

        assert_eq!(
            fake_repo.take_events(),
            ["example.com", "fuchsia.com"].map(|repo_name| RepositoryManagerEvent::Add {
                repo: RepositoryConfig {
                    mirrors: Some(vec![MirrorConfig {
                        mirror_url: Some(format!(
                            "http://{REPO_ADDR}:{dynamic_repo_port}/{repo_name}"
                        )),
                        subscribe: Some(true),
                        ..Default::default()
                    }]),
                    repo_url: Some(format!("fuchsia-pkg://{}", repo_name)),
                    root_keys: Some(vec![fuchsia_repo::test_utils::repo_key().into()]),
                    root_version: Some(1),
                    root_threshold: Some(1),
                    use_local_mirror: Some(false),
                    storage_type: Some(fidl_fuchsia_pkg::RepositoryStorageType::Ephemeral),
                    ..Default::default()
                }
            },)
        );

        // Check repository state.
        for repo_name in ["example.com", "fuchsia.com"] {
            let repo_url = format!("http://{REPO_ADDR}:{dynamic_repo_port}/{repo_name}");
            let http_repo = HttpRepository::new(
                fuchsia_hyper::new_client(),
                Url::parse(&repo_url).unwrap(),
                Url::parse(&format!("{repo_url}/blobs")).unwrap(),
                BTreeSet::new(),
            );
            let mut repo_client = RepoClient::from_trusted_remote(http_repo).await.unwrap();

            assert_matches!(repo_client.update().await, Ok(true));
        }
    }

    fn generate_ed25519_private_key() -> Ed25519PrivateKey {
        Ed25519PrivateKey::from_pkcs8(&Ed25519PrivateKey::pkcs8().unwrap()).unwrap()
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_trusted_root_file() {
        let test_env = get_test_env().await;

        let tmp_port_file = tempfile::NamedTempFile::new().unwrap();

        // Set up a simple test repository
        let tmp_repo = tempfile::tempdir().unwrap();
        let tmp_repo_path = Utf8Path::from_path(tmp_repo.path()).unwrap();
        let tmp_pm_repo = test_utils::make_pm_repository(tmp_repo_path).await;
        let mut tmp_repo_client = RepoClient::from_trusted_remote(&tmp_pm_repo).await.unwrap();
        tmp_repo_client.update().await.unwrap();

        // Generate a newer set of keys.
        let repo_keys_new = RepoKeys::builder()
            .add_root_key(Box::new(generate_ed25519_private_key()))
            .add_targets_key(Box::new(generate_ed25519_private_key()))
            .add_snapshot_key(Box::new(generate_ed25519_private_key()))
            .add_timestamp_key(Box::new(generate_ed25519_private_key()))
            .build();

        // Generate new metadata that trusts the new keys, but signs it with the old keys.
        let repo_signing_keys = tmp_pm_repo.repo_keys().unwrap();
        RepoBuilder::from_database(
            tmp_repo_client.remote_repo(),
            &repo_keys_new,
            tmp_repo_client.database(),
        )
        .signing_repo_keys(&repo_signing_keys)
        .commit()
        .await
        .unwrap();

        assert_eq!(tmp_repo_client.database().trusted_timestamp().unwrap().version(), 1);

        // Delete 1.root.json to ensure it can't be accessed when initializing
        // root of trust with 2.root.json
        std::fs::remove_file(tmp_repo_path.join("repository").join("1.root.json")).unwrap();
        // Move root.json and 2.root.json out of the repository/ dir, to verify
        // we can pass the root of trust file from anywhere to a repo constructor
        let tmp_root = tempfile::tempdir().unwrap();
        let tmp_root_dir = Utf8Path::from_path(tmp_root.path()).unwrap();
        let trusted_root_path = tmp_root_dir.join("2.root.json");
        std::fs::rename(
            tmp_repo_path.join("repository").join("root.json"),
            tmp_root_dir.join("root.json"),
        )
        .unwrap();
        std::fs::rename(tmp_repo_path.join("repository").join("2.root.json"), &trusted_root_path)
            .unwrap();

        let (fake_repo, _fake_repo_rx) = FakeRepositoryManager::new();
        let (fake_engine, _fake_engine_rx) = FakeEngine::new();
        let (_, fake_target_proxy, _) = FakeTarget::new(None);
        let frc = fake_repo.clone();
        let fec = fake_engine.clone();

        let tool_env = ToolEnv::new()
            .remote_factory_closure(move || {
                let fake_repo = frc.clone();
                let fake_engine = fec.clone();
                async move { Ok(FakeRcs::new(fake_repo, fake_engine)) }
            })
            .target_factory_closure(move || {
                let fake_target_proxy = fake_target_proxy.clone();
                async { Ok(fake_target_proxy) }
            });
        let env = tool_env.make_environment(test_env.context.clone());

        // Prepare serving the repo without passing the trusted root, and
        // passing of the trusted root 2.root.json explicitly
        let serve_cmd_without_root = ServeCommand {
            repository: Some(REPO_NAME.to_string()),
            trusted_root: None,
            address: (REPO_IPV4_ADDR, REPO_PORT).into(),
            repo_path: Some(tmp_repo_path.into()),
            product_bundle: None,
            alias: vec![],
            storage_type: None,
            alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
            port_path: Some(tmp_port_file.path().to_owned()),
            no_device: true,
            refresh_metadata: false,
        };
        let mut serve_cmd_with_root = serve_cmd_without_root.clone();
        serve_cmd_with_root.trusted_root = trusted_root_path.clone().into();

        // Serving the repo should error out since it does not find root.json and
        // and can't initialize root of trust 1.root.json.
        assert_eq!(
            serve_impl(
                Connector::try_from_env(&env)
                    .await
                    .expect("Could not make target proxy test connector"),
                Connector::try_from_env(&env).await.expect("Could not make RCS test connector"),
                serve_cmd_without_root,
                test_env.context.clone(),
                SimpleWriter::new()
            )
            .await
            .is_err(),
            true
        );

        let test_stdout = TestBuffer::default();
        let writer = SimpleWriter::new_buffers(test_stdout.clone(), Vec::new());

        // Run main in background
        let _task = fasync::Task::local(async move {
            serve_impl(
                Connector::try_from_env(&env)
                    .await
                    .expect("Could not make target proxy test connector"),
                Connector::try_from_env(&env).await.expect("Could not make RCS test connector"),
                serve_cmd_with_root,
                test_env.context.clone(),
                writer,
            )
            .await
            .unwrap()
        });

        // Wait for the "Serving repository ..." output
        for _ in 0..10 {
            if !test_stdout.clone().into_string().is_empty() {
                break;
            }
            fasync::Timer::new(time::Duration::from_millis(100)).await;
        }

        // Get dynamic port
        let dynamic_repo_port =
            fs::read_to_string(tmp_port_file.path()).unwrap().parse::<u16>().unwrap();
        tmp_port_file.close().unwrap();

        let repo_url = format!("http://{REPO_ADDR}:{dynamic_repo_port}/{REPO_NAME}");

        // Check repository state.
        let http_repo = HttpRepository::new(
            fuchsia_hyper::new_client(),
            Url::parse(&repo_url).unwrap(),
            Url::parse(&format!("{repo_url}/blobs")).unwrap(),
            BTreeSet::new(),
        );

        // As there was no key rotation since we created 2.root.json above,
        // and we removed root.json out of the repo, creating a repo client via
        // RepoClient::from_trusted_remote would error out trying to find root.json.
        // Hence we need to initialize the http client with 2.root.json, too.
        let mut repo_client =
            repo_client_from_optional_trusted_root(Some(trusted_root_path), http_repo)
                .await
                .unwrap();

        // The repo metadata should be at version 2
        assert_matches!(repo_client.update().await, Ok(true));
        assert_eq!(repo_client.database().trusted_timestamp().unwrap().version(), 2);
    }
}
