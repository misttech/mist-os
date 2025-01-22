// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use ffx_config::environment::EnvironmentKind;
use ffx_config::EnvironmentContext;
use ffx_repository_serve_args::ServeCommand;
use ffx_target::TargetProxy;
use fho::{
    bug, daemon_protocol, deferred, return_bug, return_user_error, Connector, Deferred, FfxMain,
    FfxTool, Result, SimpleWriter,
};
use fidl_fuchsia_developer_ffx::{self as ffx, RepositoryRegistryProxy, ServerStatus};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fuchsia_async as fasync;
use fuchsia_repo::manager::RepositoryManager;
use fuchsia_repo::repo_client::RepoClient;
use fuchsia_repo::repository::{PmRepository, RepoProvider};
use fuchsia_repo::server::RepositoryServer;
use futures::executor::block_on;
use futures::{SinkExt, StreamExt};
use package_tool::{cmd_repo_publish, RepoPublishCommand};
use pkg::{
    write_instance_info, PkgServerInfo, PkgServerInstanceInfo as _, PkgServerInstances, ServerMode,
};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::fs;
use std::io::Write;
use std::sync::Arc;
use target_errors::FfxTargetError;
use tuf::metadata::RawSignedMetadata;

// LINT.IfChange
/// Default name used for package repositories in ffx. It is expected that there is no need to
/// change this constant. But in case this is changed, ensure that it is consistent with the ffx
/// developer documentation, see
/// https://cs.opensource.google/search?q=devhost&sq=&ss=fuchsia%2Ffuchsia:src%2Fdeveloper%2Fffx%2F
pub const DEFAULT_REPO_NAME: &str = "devhost";
// LINT.ThenChange(args.rs)

mod target;

const REPO_CONNECT_TIMEOUT_CONFIG: &str = "repository.connect_timeout_secs";
const DEFAULT_CONNECTION_TIMEOUT_SECS: u64 = 120;
const REPO_BACKGROUND_FEATURE_FLAG: &str = "repository.server.enabled";
const REPO_PATH_RELATIVE_TO_BUILD_DIR: &str = "amber-files";
const CONFIG_KEY_DEFAULT_REPOSITORY: &str = "repository.default";

#[derive(FfxTool)]
pub struct ServeTool {
    #[command]
    pub cmd: ServeCommand,
    pub context: EnvironmentContext,
    pub target_proxy_connector: Connector<TargetProxy>,
    pub rcs_proxy_connector: Connector<RemoteControlProxy>,
    #[with(deferred(daemon_protocol()))]
    pub repos: Deferred<ffx::RepositoryRegistryProxy>,
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

#[async_trait(?Send)]
impl FfxMain for ServeTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: Self::Writer) -> Result<()> {
        /* This check is specific to the `ffx repository serve` command and should be ignored
        if the entry point is `ffx repository server start`.
         */
        let bg: bool = self
            .context
            .get(REPO_BACKGROUND_FEATURE_FLAG)
            .context("checking for daemon server flag")?;
        if bg {
            return_user_error!(
                r#"The ffx setting '{}' and the foreground server are mutually incompatible.
Please disable background serving by running the following commands:
$ ffx config remove repository.server.enabled
$ ffx doctor --restart-daemon"#,
                REPO_BACKGROUND_FEATURE_FLAG,
            );
        }

        serve_impl(
            self.target_proxy_connector,
            self.rcs_proxy_connector,
            self.repos,
            self.cmd,
            self.context,
            writer,
            ServerMode::Foreground,
        )
        .await?;
        Ok(())
    }
}

pub fn get_repo_base_name(
    cmd_line: &Option<String>,
    context: &EnvironmentContext,
) -> Result<String> {
    if let Some(repo_name) = cmd_line.as_ref() {
        return Ok(repo_name.to_string());
    } else {
        if let Some(repo_name) = context
            .get::<Option<String>, _>(CONFIG_KEY_DEFAULT_REPOSITORY)
            .map_err(|e| bug!("{e}"))?
        {
            return Ok(repo_name);
        }
    }
    Ok(DEFAULT_REPO_NAME.to_string())
}

pub async fn serve_impl_validate_args(
    cmd: &ServeCommand,
    rcs_proxy_connector: &Connector<RemoteControlProxy>,
    repos: Deferred<RepositoryRegistryProxy>,
    context: &EnvironmentContext,
) -> Result<Option<PkgServerInfo>> {
    /* This check makes sure there is not a daemon based server running, which causes
     a lot of confusion.
    */
    let bg: bool =
        context.get(REPO_BACKGROUND_FEATURE_FLAG).context("checking for daemon server flag")?;
    if bg {
        return_user_error!(
            r#"The ffx setting '{}' and the standalone server are mutually incompatible.
Please disable the daemon based serving by running the following command:

ffx config remove repository.server.enabled && ffx doctor --restart-daemon
"#,
            REPO_BACKGROUND_FEATURE_FLAG,
        );
    }
    // Check that there is a target device identified, it is OK if it is not online.
    if !cmd.no_device {
        let res = rcs_proxy_connector
            .try_connect(|target, err| {
                tracing::info!(
            "Validating RCS proxy: Waiting for target '{target:?}' to return error: {err:?}"
        );
                if target.is_none() {
                    return Err(Into::<errors::FfxError>::into(FfxTargetError::OpenTargetError {
                        err: fidl_fuchsia_developer_ffx::OpenTargetError::TargetNotFound,
                        target: None,
                    })
                    .into());
                } else {
                    Ok(())
                }
            })
            .await;
        match res {
            //  For validating the arguments to this command, we're only checking for there being
            // 1 device identified as the target. If there is more than one or zero, print an error.
            Err(fho::Error::User(e)) => {
                if let Some(ee) = e.downcast_ref::<FfxTargetError>() {
                    match ee {
                        FfxTargetError::OpenTargetError { err, .. } => {
                            if err == &fidl_fuchsia_developer_ffx::OpenTargetError::TargetNotFound
                                || err
                                    == &fidl_fuchsia_developer_ffx::OpenTargetError::QueryAmbiguous
                            {
                                return_user_error!("{e} To disable auto-registration use the `--no-device` option.")
                            }
                        }
                        _ => (),
                    };
                }
            }
            _ => (),
        };
    }

    let repo_base_name = get_repo_base_name(&cmd.repository, context)?;
    // Validate the repo-path vs. product bundle.
    // Product bundles may contain multiple repositories, So return a list.
    let repo_name_paths: Vec<(String, Utf8PathBuf)> = match (
        cmd.repo_path.clone(),
        cmd.product_bundle.clone(),
    ) {
        (Some(_), Some(_)) => {
            return_user_error!("Cannot specify both --repo-path and --product-bundle");
        }
        (None, Some(product_bundle)) => {
            if !product_bundle.exists() {
                return_user_error!("product bundle {product_bundle:?} does not exist");
            }
            let repositories = sdk_metadata::get_repositories(product_bundle.clone())
                .with_context(|| {
                    format!("getting repositories from product bundle {product_bundle}")
                })?;
            let mut pb_repo_name_paths = vec![];
            for r in repositories {
                if let Some(first_alias) = r.aliases().clone().first() {
                    pb_repo_name_paths
                        .push((format!("{repo_base_name}.{first_alias}"), product_bundle.clone()));
                } else {
                    return_bug!("Invalid repository configuration in the product bundle {product_bundle}. No aliases defined for a repository");
                }
            }
            pb_repo_name_paths
        }
        (repo_path, None) => {
            if let Some(path) = repo_path {
                if !path.exists() {
                    return_user_error!("repo-path {path:?} does not exist");
                }
                vec![(repo_base_name, path)]
            // TODO(b/359927881): Use the configuration to read repo-path
            // vs. constructing it from the build dir. This way it works with other EnvironmentKinds.
            } else if let EnvironmentKind::InTree { build_dir: Some(build_dir), .. } =
                context.env_kind()
            {
                let path = build_dir.join(REPO_PATH_RELATIVE_TO_BUILD_DIR);
                if !path.exists() {
                    return_user_error!("build directory relative path {path:?} does not exist");
                }
                vec![(
                    repo_base_name,
                    Utf8Path::from_path(&path)
                        .with_context(|| format!("converting repo path to UTF-8 {:?}", repo_path))?
                        .into(),
                )]
            } else {
                tracing::warn!("repo-path not found in env: {:?}", context.env_kind());
                return_user_error!("Either --repo-path or --product-bundle need to be specified");
            }
        }
    };

    if let Some(package_manifest) = &cmd.auto_publish {
        if !package_manifest.exists() {
            let msg = format!("package manifest {package_manifest:?} does not exist");
            tracing::error!("{msg}");
            return_user_error!("{msg}");
        }
    }

    // Compare against running instances.
    let instance_root =
        context.get("repository.process_dir").map_err(|e: ffx_config::api::ConfigError| bug!(e))?;
    let mgr = PkgServerInstances::new(instance_root);
    let mut running_instances = mgr.list_instances()?;

    // Check for any instances that are daemon based, and if they are, check the status of the daemon
    // based server. This is only done if needed to avoid starting the daemon unnecessarily.
    let daemon_running =
        if running_instances.iter().any(|instance| instance.server_mode == ServerMode::Daemon) {
            daemon_repo_is_running(repos.await?).await?
        } else {
            false
        };

    // Filter the daemon based instances if the daemon is not running
    if !daemon_running {
        running_instances.retain(|instance| instance.server_mode != ServerMode::Daemon);
    }

    // Check all the name/path pairs for conflicts. If there is an exact match, return it as
    // an indicator that the server is already running and does not need to be started again.
    let mut already_running_instance: Option<PkgServerInfo> = None;
    for (repo_name, repo_path) in repo_name_paths {
        let addr = cmd.address.clone();
        let duplicate = running_instances.iter().find(|instance| instance.address == addr);
        if let Some(duplicate) = duplicate {
            // if we're starting using a product bundle, the name will be different so compare the repo_path
            // which is the path to the product bundle
            if let Some(pb_path) = &cmd.product_bundle {
                if *pb_path != duplicate.repo_path_display() {
                    return_user_error!("Repository address conflict. \
                Cannot start a server named {repo_name} serving {repo_path:?}. \
                Repository server  \"{}\" is already running on {addr} serving a different path: {}\n\
                Use `ffx  repository server list` to list running servers",
                 duplicate.name, duplicate.repo_path_display());
                }
            } else {
                if repo_name != duplicate.name {
                    return_user_error!("Repository address conflict. \
                Cannot start a server named {repo_name} serving {repo_path:?}. \
                Repository server  \"{}\" is already running on {addr} serving a different path: {}\n\
                Use `ffx  repository server list` to list running servers",
                 duplicate.name, duplicate.repo_path_display());
                }
            }
            if already_running_instance.is_none() {
                already_running_instance = Some(duplicate.clone());
            }
        }
        let duplicate = running_instances.iter().find(|instance| instance.name == repo_name);
        if let Some(duplicate) = duplicate {
            if addr != duplicate.address {
                return_user_error!(
                "Repository name conflict. \
            Cannot start a server named {repo_name} serving {repo_path:?}. \
            Repository server  \"{dupe_name}\" is already running on {dupe_addr} serving a different path: {dupe_path}\n\
            Use `ffx  repository server list` to list running servers",
                dupe_name=duplicate.name,
                dupe_addr=duplicate.address,
                dupe_path=duplicate.repo_path_display()
            );
            }
            if already_running_instance.is_none() {
                already_running_instance = Some(duplicate.clone());
            }
        }
    }
    Ok(already_running_instance)
}

async fn daemon_repo_is_running(repos: ffx::RepositoryRegistryProxy) -> Result<bool> {
    let status = repos.server_status().await.map_err(|e| bug!(e))?;
    let running = match status {
        ServerStatus::Running(_) => true,
        _ => false,
    };
    Ok(running)
}

pub async fn serve_impl<W: Write + 'static>(
    target_proxy: Connector<TargetProxy>,
    rcs_proxy: Connector<RemoteControlProxy>,
    repos: Deferred<RepositoryRegistryProxy>,
    cmd: ServeCommand,
    context: EnvironmentContext,
    mut writer: W,
    mode: ServerMode,
) -> Result<()> {
    // Validate the cmd args before processing. This allows good error messages to be presented
    // to the user when running in Background mode. If the server is already running, this returns
    // Ok.
    if let Some(running) = serve_impl_validate_args(&cmd, &rcs_proxy, repos, &context).await? {
        // The server that matches the cmd is already running.
        writeln!(
            writer,
            "A server named {} is already serving on address {} the repo path: {}",
            running.name,
            running.address,
            running.repo_path_display()
        )
        .map_err(|e| bug!(e))?;
        return Ok(());
    }

    let repo_base_name = get_repo_base_name(&cmd.repository, &context)?;

    let connect_timeout =
        context.get(REPO_CONNECT_TIMEOUT_CONFIG).unwrap_or(DEFAULT_CONNECTION_TIMEOUT_SECS);

    let connect_timeout = std::time::Duration::from_secs(connect_timeout);
    let repo_manager: Arc<RepositoryManager> = RepositoryManager::new();

    let repo_path = match (cmd.repo_path.clone(), cmd.product_bundle.clone()) {
        (Some(_), Some(_)) => {
            return_user_error!("Cannot specify both --repo-path and --product-bundle");
        }
        (None, Some(product_bundle)) => {
            let repositories = sdk_metadata::get_repositories(product_bundle.clone())
                .with_context(|| {
                    format!("getting repositories from product bundle {product_bundle}")
                })?;
            for repository in repositories {
                let aliases = repository.aliases().clone();
                let repo_name = format!(
                    "{repo_base_name}.{first_alias}",
                    first_alias = aliases.first().unwrap().clone()
                );

                let repo_client = RepoClient::from_trusted_remote(Box::new(repository) as Box<_>)
                    .await
                    .with_context(|| format!("Creating a repo client for {repo_name}"))?;
                repo_manager.add(&repo_name, repo_client);
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
                // TODO(b/359927881): Use the configuration to read repo-path
                // vs. constructing it from the build dir. This way it works with other EnvironmentKinds.
                let build_dir = Utf8Path::from_path(build_dir)
                    .with_context(|| format!("converting repo path to UTF-8 {:?}", repo_path))?;

                build_dir.join(REPO_PATH_RELATIVE_TO_BUILD_DIR)
            } else {
                tracing::warn!("repo-path not found in env: {:?}", context.env_kind());
                return_user_error!("Either --repo-path or --product-bundle need to be specified");
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

            repo_manager.add(&repo_base_name, repo_client);

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
    let storage_type: Option<fidl_fuchsia_pkg_ext::RepositoryStorageType> = match cmd.storage_type {
        Some(fidl_fuchsia_developer_ffx::RepositoryStorageType::Ephemeral) => {
            Some(fidl_fuchsia_pkg_ext::RepositoryStorageType::Ephemeral)
        }
        Some(fidl_fuchsia_developer_ffx::RepositoryStorageType::Persistent) => {
            Some(fidl_fuchsia_pkg_ext::RepositoryStorageType::Persistent)
        }
        None => None,
    };

    // Write out the instance data
    for (name, repo_client) in repo_manager.repositories() {
        let repo_name = cmd.repository.clone().unwrap_or_else(|| DEFAULT_REPO_NAME.to_string());
        let repo_url =
            fuchsia_url::RepositoryUrl::parse_host(repo_name.clone()).map_err(|e| bug!(e))?;
        let mirror_url = format!("http://{server_addr}/{repo_name}")
            .parse()
            .map_err(|e: http::uri::InvalidUri| bug!(e))?;
        let repo_config = repo_client
            .read()
            .await
            .get_config(repo_url, mirror_url, storage_type.clone())
            .map_err(|e| bug!("{e}"))?;
        let repo_spec = repo_client.read().await.spec();
        if let Err(e) = write_instance_info(
            Some(context.clone()),
            mode.clone(),
            &name,
            &server_addr,
            repo_spec,
            storage_type.clone().unwrap_or(fidl_fuchsia_pkg_ext::RepositoryStorageType::Ephemeral),
            match cmd.alias_conflict_mode {
                fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::ErrorOut => {
                    fidl_fuchsia_pkg_ext::RepositoryRegistrationAliasConflictMode::ErrorOut
                }
                fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::Replace => {
                    fidl_fuchsia_pkg_ext::RepositoryRegistrationAliasConflictMode::Replace
                }
            },
            repo_config,
        )
        .await
        {
            tracing::error!(
                "failed to write repo server instance information for {repo_name}: {e:?}"
            );
        }
    }

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

    // If auto-publishing, start the task in the background.
    if let Some(package_manifest) = &cmd.auto_publish {
        let publish_cmd = RepoPublishCommand {
            signing_keys: None,
            trusted_keys: None,
            trusted_root: cmd.trusted_root.clone(),
            package_manifests: vec![],
            package_list_manifests: vec![package_manifest.clone()],
            package_archives: vec![],
            product_bundle: vec![],
            time_versioning: true,
            metadata_current_time: chrono::Utc::now(),
            refresh_root: false,
            clean: false,
            depfile: None,
            copy_mode: fuchsia_repo::repository::CopyMode::Copy,
            delivery_blob_type: 1,
            watch: true,
            ignore_missing_packages: true,
            blob_manifest: None,
            blob_repo_dir: None,
            repo_path: repo_path.clone(),
        };

        let auto_publisher = fasync::Task::local(async move {
            let publish_result = cmd_repo_publish(publish_cmd).await;
            tracing::warn!("Auto-publishing exited: {publish_result:?}");
        });

        auto_publisher.detach();
    }

    let result = if cmd.no_device {
        let s = format!("Serving repository '{repo_path}' over address '{}'.", server_addr);
        writeln!(writer, "{}", s).map_err(|e| anyhow!("Failed to write to output: {:?}", e))?;
        tracing::info!("{}", s);
        Ok(())
    } else {
        let r = target::main_connect_loop(
            &cmd,
            &repo_path,
            server_addr,
            connect_timeout,
            repo_manager,
            loop_stop_rx,
            rcs_proxy,
            target_proxy,
            &mut writer,
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
    use fho::{user_error, TryFromEnv};
    use fidl::endpoints::DiscoverableProtocolMarker;
    use fidl_fuchsia_developer_ffx::{
        RemoteControlState, RepositoryError, RepositoryRegistrationAliasConflictMode,
        RepositoryStorageType, SshHostAddrInfo, TargetAddrInfo, TargetInfo, TargetIpPort,
        TargetRequest, TargetState,
    };
    use fidl_fuchsia_developer_remotecontrol as frcs;
    use fidl_fuchsia_net::{IpAddress, Ipv4Address};
    use fidl_fuchsia_pkg::{
        MirrorConfig, RepositoryConfig, RepositoryManagerMarker, RepositoryManagerRequest,
        RepositoryManagerRequestStream,
    };
    use fidl_fuchsia_pkg_ext::RepositoryConfigBuilder;
    use fidl_fuchsia_pkg_rewrite::{
        EditTransactionRequest, EngineMarker, EngineRequest, EngineRequestStream,
        RuleIteratorRequest,
    };
    use fidl_fuchsia_pkg_rewrite_ext::Rule;
    use frcs::RemoteControlMarker;
    use fuchsia_repo::repo_builder::RepoBuilder;
    use fuchsia_repo::repo_keys::RepoKeys;
    use fuchsia_repo::repository::HttpRepository;
    use fuchsia_repo::test_utils;
    use futures::channel::mpsc;
    use futures::channel::oneshot::channel;
    use futures::TryStreamExt;
    use std::collections::BTreeSet;
    use std::sync::Mutex;
    use std::time;
    use timeout::timeout;
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
                    frcs::RemoteControlRequest::DeprecatedOpenCapability {
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
                                .into_stream(),
                            ),
                            EngineMarker::PROTOCOL_NAME => engine.spawn(
                                fidl::endpoints::ServerEnd::<EngineMarker>::new(server_channel)
                                    .into_stream(),
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
                    let mut s = remote_control.into_stream();
                    let knock_skip = knock_skip.clone();
                    fasync::Task::local(async move {
                        if let Ok(Some(req)) = s.try_next().await {
                            match req {
                                frcs::RemoteControlRequest::DeprecatedOpenCapability {
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
                                                .into_stream();
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
                                let mut stream = transaction.into_stream();
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
                                            let mut stream = iterator.into_stream();

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
        ffx_config::test_init().await.expect("test initialization")
    }

    async fn make_fake_rcs_proxy_connector(test_env: &TestEnv) -> Connector<RemoteControlProxy> {
        let (fake_repo, _) = FakeRepositoryManager::new();
        let (fake_engine, _content) = FakeEngine::new();
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

        let fho_env = tool_env.make_environment(test_env.context.clone());
        Connector::try_from_env(&fho_env).await.expect("Could not make RCS test connector")
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_serve_impl_validate_args() {
        let env = get_test_env().await;

        let pb_path = env.isolate_root.path().join("some-pb");
        fs::create_dir_all(&pb_path).expect("pb temp dir");
        let bundle_path = Utf8PathBuf::from_path_buf(pb_path).expect("utf8 path");
        write_product_bundle(&bundle_path).await;

        let test_cases: Vec<(ServeCommand, Result<Option<PkgServerInfo>>)> = vec![
            (
                ServeCommand {
                    repository: None,
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: Some("/some/repo/path".into()),
                    product_bundle: Some(bundle_path.clone()),
                    alias: vec![],
                    storage_type: None,
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::ErrorOut,
                    port_path: None,
                    no_device: false,
                    refresh_metadata: false,
                    auto_publish: None,
                },
                Err(user_error!("Cannot specify both --repo-path and --product-bundle")),
            ),
            (
                ServeCommand {
                    repository: Some("repo-with-name".into()),
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: None,
                    product_bundle: Some(bundle_path.clone()),
                    alias: vec![],
                    storage_type: None,
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::ErrorOut,
                    port_path: None,
                    no_device: false,
                    refresh_metadata: false,
                    auto_publish: None,
                },
                Ok(None),
            ),
            (
                ServeCommand {
                    repository: None,
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: None,
                    product_bundle: Some("/missing/product/bundle".into()),
                    alias: vec![],
                    storage_type: None,
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::ErrorOut,
                    port_path: None,
                    no_device: false,
                    refresh_metadata: false,
                    auto_publish: None,
                },
                Err(user_error!("product bundle \"/missing/product/bundle\" does not exist")),
            ),
            (
                ServeCommand {
                    repository: None,
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: Some("/missing/repo/path".into()),
                    product_bundle: None,
                    alias: vec![],
                    storage_type: None,
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::ErrorOut,
                    port_path: None,
                    no_device: false,
                    refresh_metadata: false,
                    auto_publish: None,
                },
                Err(user_error!("repo-path \"/missing/repo/path\" does not exist")),
            ),
            (
                ServeCommand {
                    repository: None,
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: None,
                    product_bundle: None,
                    alias: vec![],
                    storage_type: None,
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::ErrorOut,
                    port_path: None,
                    no_device: false,
                    refresh_metadata: false,
                    auto_publish: None,
                },
                Err(user_error!("Either --repo-path or --product-bundle need to be specified")),
            ),
            (
                ServeCommand {
                    repository: None,
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: None,
                    product_bundle: Some(bundle_path.clone()),
                    alias: vec![],
                    storage_type: None,
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::ErrorOut,
                    port_path: None,
                    no_device: false,
                    refresh_metadata: false,
                    auto_publish: Some(Utf8PathBuf::from("/missing/package-list")),
                },
                Err(user_error!("package manifest \"/missing/package-list\" does not exist")),
            ),
            (
                ServeCommand {
                    repository: None,
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: Some(
                        Utf8PathBuf::from_path_buf(env.isolate_root.path().to_path_buf())
                            .expect("repo path"),
                    ),
                    product_bundle: None,
                    alias: vec![],
                    storage_type: None,
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::ErrorOut,
                    port_path: None,
                    no_device: false,
                    refresh_metadata: false,
                    auto_publish: Some(Utf8PathBuf::from("/missing/package-list")),
                },
                Err(user_error!("package manifest \"/missing/package-list\" does not exist")),
            ),
        ];

        let rcs_proxy_connector = make_fake_rcs_proxy_connector(&env).await;

        for (cmd, expected) in test_cases {
            let (sender, _receiver) = channel();
            let mut sender = Some(sender);
            let repos = Deferred::from_output(Ok(fho::testing::fake_proxy(move |req| match req {
                ffx::RepositoryRegistryRequest::ServerStatus { responder } => {
                    sender.take().unwrap().send(()).unwrap();
                    responder
                        .send(&fidl_fuchsia_developer_ffx_ext::ServerStatus::Stopped.into())
                        .unwrap()
                }
                other => panic!("Unexpected request: {:?}", other),
            })));
            let result =
                serve_impl_validate_args(&cmd, &rcs_proxy_connector, repos, &env.context).await;
            match expected {
                Ok(Some(pkg_server_info)) => {
                    if let Some(actual_info) = result.ok().expect("Ok result") {
                        assert_eq!(actual_info, pkg_server_info)
                    } else {
                        assert!(false, "Expected {pkg_server_info:?}, got None");
                    }
                }
                Ok(None) => {
                    if let Some(actual_info) = result.ok().expect("Ok result") {
                        assert!(false, "Expected None, got {actual_info:?}");
                    }
                }
                Err(e) => {
                    if let Some(actual_err) = result.as_ref().err() {
                        assert_eq!(actual_err.to_string(), e.to_string())
                    } else {
                        assert!(false, "Expected {e}, got no error: {result:?}")
                    }
                }
            };
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_serve_impl_validate_args_in_tree() {
        let build_dir = tempfile::tempdir().expect("temp dir");
        let env = ffx_config::test_init_in_tree(build_dir.path()).await.expect("in-tree test env");
        let cmd = ServeCommand {
            repository: None,
            trusted_root: None,
            address: (REPO_IPV4_ADDR, REPO_PORT).into(),
            repo_path: None,
            product_bundle: None,
            alias: vec![],
            storage_type: None,
            alias_conflict_mode: RepositoryRegistrationAliasConflictMode::ErrorOut,
            port_path: None,
            no_device: false,
            refresh_metadata: false,
            auto_publish: None,
        };
        let expected: Result<Option<PkgServerInfo>> = Err(user_error!(
            "build directory relative path {:?} does not exist",
            build_dir.path().join("amber-files")
        ));

        let fake_rcs_proxy_connector = make_fake_rcs_proxy_connector(&env).await;

        let (sender, _receiver) = channel();
        let mut sender = Some(sender);
        let repos = Deferred::from_output(Ok(fho::testing::fake_proxy(move |req| match req {
            ffx::RepositoryRegistryRequest::ServerStart { responder, address: None } => {
                sender.take().unwrap().send(()).unwrap();
                responder.send(Err(RepositoryError::ServerNotRunning)).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        })));

        let result =
            serve_impl_validate_args(&cmd, &fake_rcs_proxy_connector, repos, &env.context).await;
        match expected {
            Ok(Some(pkg_server_info)) => {
                if let Some(actual_info) = result.ok().expect("Ok result") {
                    assert_eq!(actual_info, pkg_server_info)
                } else {
                    assert!(false, "Expected {pkg_server_info:?}, got None");
                }
            }
            Ok(None) => {
                if let Some(actual_info) = result.ok().expect("Ok result") {
                    assert!(false, "Expected None, got {actual_info:?}");
                }
            }
            Err(e) => {
                if let Some(actual_err) = result.as_ref().err() {
                    assert_eq!(actual_err.to_string(), e.to_string())
                } else {
                    assert!(false, "Expected {e}, got no error: {result:?}")
                }
            }
        };
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_serve_impl_validate_args_running_servers() {
        let env = get_test_env().await;

        let instance_root = env.isolate_root.path().join("repo_instances");
        fs::create_dir_all(&instance_root).expect("instance root dir");

        let repo_path = env.isolate_root.path().join("repo_path");
        fs::create_dir_all(&repo_path).expect("repo path dir");

        let instance_name = "devhost";
        let repo_config =
            RepositoryConfigBuilder::new(format!("fuchsia-pkg://{instance_name}").parse().unwrap())
                .build();

        env.context
            .query("repository.process_dir")
            .level(Some(ConfigLevel::User))
            .set(instance_root.to_string_lossy().into())
            .await
            .expect("setting instance root config");

        let server_info = PkgServerInfo {
            name: instance_name.into(),
            address: (REPO_IPV4_ADDR, REPO_PORT).into(),
            repo_spec: fuchsia_repo::repository::RepositorySpec::Pm {
                path: Utf8PathBuf::new(),
                aliases: BTreeSet::new(),
            },
            registration_storage_type: fidl_fuchsia_pkg_ext::RepositoryStorageType::Ephemeral,
            registration_alias_conflict_mode:
                fidl_fuchsia_pkg_ext::RepositoryRegistrationAliasConflictMode::ErrorOut,
            server_mode: ServerMode::Background,
            pid: std::process::id(),
            repo_config,
        };

        let mgr = PkgServerInstances::new(instance_root);
        mgr.write_instance(&server_info).expect("test instance written");

        let test_cases: Vec<(ServeCommand, Result<Option<PkgServerInfo>>)> = vec![
            (
         ServeCommand {
            repository: Some("another-name".into()),
            trusted_root: None,
            address: (REPO_IPV4_ADDR, REPO_PORT).into(),
            repo_path: Some(Utf8PathBuf::from_path_buf(repo_path.clone()).expect("utf8 repo_path")),
            product_bundle: None,
            alias: vec![],
            storage_type: None,
            alias_conflict_mode: RepositoryRegistrationAliasConflictMode::ErrorOut,
            port_path: None,
            no_device: false,
            refresh_metadata: false,
            auto_publish: None,
        },
            Err(user_error!("Repository address conflict. \
            Cannot start a server named another-name serving {repo_path:?}. \
            Repository server  \"{name}\" is already running on {addr} serving a different path: {dupe_path}\n\
            Use `ffx  repository server list` to list running servers",
             addr=server_info.address, name=server_info.name, dupe_path=server_info.repo_path_display()))
    ),
    (
        ServeCommand {
           repository: Some(instance_name.into()),
           trusted_root: None,
           address: (REPO_IPV4_ADDR, REPO_PORT).into(),
           repo_path: Some(Utf8PathBuf::from_path_buf(repo_path.clone()).expect("utf8 repo_path")),
           product_bundle: None,
           alias: vec![],
           storage_type: None,
           alias_conflict_mode: RepositoryRegistrationAliasConflictMode::ErrorOut,
           port_path: None,
           no_device: false,
           refresh_metadata: false,
           auto_publish: None,
       },
           Ok(Some(server_info.clone()))
   ),
   (
    ServeCommand {
       repository: Some(instance_name.into()),
       trusted_root: None,
       address: (REPO_IPV4_ADDR, 8888).into(),
       repo_path: Some(Utf8PathBuf::from_path_buf(repo_path.clone()).expect("utf8 repo_path")),
       product_bundle: None,
       alias: vec![],
       storage_type: None,
       alias_conflict_mode: RepositoryRegistrationAliasConflictMode::ErrorOut,
       port_path: None,
       no_device: false,
       refresh_metadata: false,
       auto_publish: None,
   },
       Err(user_error!(
        "Repository name conflict. \
    Cannot start a server named {name} serving {repo_path:?}. \
    Repository server  \"{dupe_name}\" is already running on {addr} serving a different path: {dupe_path}\n\
    Use `ffx  repository server list` to list running servers",
        name=instance_name,
        dupe_name=instance_name,
        addr=server_info.address,
        dupe_path=server_info.repo_path_display()
       ))
   )
    ];

        let rcs_proxy_connector = make_fake_rcs_proxy_connector(&env).await;

        for (cmd, expected) in test_cases {
            let (sender, _receiver) = channel();
            let mut sender = Some(sender);
            let repos = Deferred::from_output(Ok(fho::testing::fake_proxy(move |req| match req {
                ffx::RepositoryRegistryRequest::ServerStart { responder, address: None } => {
                    sender.take().unwrap().send(()).unwrap();
                    responder.send(Err(RepositoryError::ServerNotRunning)).unwrap()
                }
                other => panic!("Unexpected request: {:?}", other),
            })));
            let result =
                serve_impl_validate_args(&cmd, &rcs_proxy_connector, repos, &env.context).await;
            match expected {
                Ok(Some(pkg_server_info)) => {
                    if let Some(actual_info) = match result {
                        Ok(info) => info,
                        Err(e) => {
                            assert!(false, " unexpected error {e}");
                            None
                        }
                    } {
                        assert_eq!(actual_info, pkg_server_info)
                    } else {
                        assert!(false, "Expected {pkg_server_info:?}, got None");
                    }
                }
                Ok(None) => {
                    if let Some(actual_info) = result.ok().expect("Ok result") {
                        assert!(false, "Expected None, got {actual_info:?}");
                    }
                }
                Err(e) => {
                    if let Some(actual_err) = result.as_ref().err() {
                        assert_eq!(actual_err.to_string(), e.to_string())
                    } else {
                        assert!(false, "Expected {e}, got no error: {result:?}")
                    }
                }
            };
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_register() {
        async fn run_test_start_register(refresh_metadata: bool) {
            let test_env = get_test_env().await;
            test_env
                .context
                .query("repository.process_dir")
                .level(Some(ConfigLevel::User))
                .set(test_env.isolate_root.path().to_string_lossy().into())
                .await
                .expect("Setting process dir");

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

            let (sender, _receiver) = channel();
            let mut sender = Some(sender);
            let repos = Deferred::from_output(Ok(fho::testing::fake_proxy(move |req| match req {
                ffx::RepositoryRegistryRequest::ServerStart { responder, address: None } => {
                    sender.take().unwrap().send(()).unwrap();
                    responder.send(Err(RepositoryError::ServerNotRunning)).unwrap()
                }
                other => panic!("Unexpected request: {:?}", other),
            })));

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

            // Use a tmp repo to allow metadata updates
            let tmp_repo = tempfile::tempdir().unwrap();
            let tmp_repo_path = Utf8Path::from_path(tmp_repo.path()).unwrap();
            test_utils::make_empty_pm_repo_dir(tmp_repo_path);

            let serve_tool = ServeTool {
                cmd: ServeCommand {
                    repository: Some(REPO_NAME.to_string()),
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: Some(tmp_repo_path.into()),
                    product_bundle: None,
                    alias: vec!["example.com".into(), "fuchsia.com".into()],
                    storage_type: Some(RepositoryStorageType::Ephemeral),
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
                    port_path: Some(tmp_port_file.path().to_owned()),
                    no_device: false,
                    refresh_metadata: refresh_metadata,
                    auto_publish: None,
                },
                repos,
                context: env.environment_context().clone(),
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

        let (sender, _receiver) = channel();
        let mut sender = Some(sender);
        let repos = Deferred::from_output(Ok(fho::testing::fake_proxy(move |req| match req {
            ffx::RepositoryRegistryRequest::ServerStatus { responder } => {
                sender.take().unwrap().send(()).unwrap();
                responder
                    .send(&fidl_fuchsia_developer_ffx_ext::ServerStatus::Stopped.into())
                    .unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        })));

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
            // Use a tmp repo to allow metadata updates
            let tmp_repo = tempfile::tempdir().unwrap();
            let tmp_repo_path = Utf8Path::from_path(tmp_repo.path()).unwrap();
            test_utils::make_empty_pm_repo_dir(tmp_repo_path);

            serve_impl(
                Connector::try_from_env(&env)
                    .await
                    .expect("Could not make target proxy test connector"),
                Connector::try_from_env(&env).await.expect("Could not make RCS test connector"),
                repos,
                ServeCommand {
                    repository: Some(REPO_NAME.to_string()),
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: Some(tmp_repo_path.into()),
                    product_bundle: None,
                    alias: vec!["example.com".into(), "fuchsia.com".into()],
                    storage_type: Some(RepositoryStorageType::Ephemeral),
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
                    port_path: Some(tmp_port_file_path),
                    no_device: false,
                    refresh_metadata: false,
                    auto_publish: None,
                },
                env.environment_context().clone(),
                writer,
                ServerMode::Foreground,
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
        test_env
            .context
            .query("repository.process_dir")
            .level(Some(ConfigLevel::User))
            .set(test_env.isolate_root.path().to_string_lossy().into())
            .await
            .expect("Setting process dir");

        let tmp_port_file = tempfile::NamedTempFile::new().unwrap();
        let tmp_port_file_path = tmp_port_file.path().to_owned();

        let (fake_repo, _fake_repo_rx) = FakeRepositoryManager::new();
        let (fake_engine, _fake_engine_rx) = FakeEngine::new();
        let (_, fake_target_proxy, _) = FakeTarget::new(None);

        let frc = fake_repo.clone();
        let fec = fake_engine.clone();

        let (sender, _receiver) = channel();
        let mut sender = Some(sender);
        let repos = Deferred::from_output(Ok(fho::testing::fake_proxy(move |req| match req {
            ffx::RepositoryRegistryRequest::ServerStart { responder, address: None } => {
                sender.take().unwrap().send(()).unwrap();
                responder.send(Err(RepositoryError::ServerNotRunning)).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        })));

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
            // Use a tmp repo to allow metadata updates
            let tmp_repo = tempfile::tempdir().unwrap();
            let tmp_repo_path = Utf8Path::from_path(tmp_repo.path()).unwrap();
            test_utils::make_empty_pm_repo_dir(tmp_repo_path);

            serve_impl(
                Connector::try_from_env(&env)
                    .await
                    .expect("Could not make target proxy test connector"),
                Connector::try_from_env(&env).await.expect("Could not make RCS test connector"),
                repos,
                ServeCommand {
                    repository: Some(REPO_NAME.to_string()),
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: Some(tmp_repo_path.into()),
                    product_bundle: None,
                    alias: vec!["example.com".into(), "fuchsia.com".into()],
                    storage_type: Some(RepositoryStorageType::Ephemeral),
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
                    port_path: Some(tmp_port_file_path),
                    no_device: false,
                    refresh_metadata: false,
                    auto_publish: None,
                },
                env.environment_context().clone(),
                writer,
                ServerMode::Foreground,
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
            fuchsia_repo::test_utils::make_repo_dir(
                metadata_path.as_ref(),
                blobs_dir.as_ref(),
                None,
            )
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

        let (sender, _receiver) = channel();
        let mut sender = Some(sender);
        let repos = Deferred::from_output(Ok(fho::testing::fake_proxy(move |req| match req {
            ffx::RepositoryRegistryRequest::ServerStatus { responder } => {
                sender.take().unwrap().send(()).unwrap();
                responder
                    .send(&fidl_fuchsia_developer_ffx_ext::ServerStatus::Stopped.into())
                    .unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        })));
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
                repos,
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
                    auto_publish: None,
                },
                test_env.context.clone(),
                writer,
                ServerMode::Foreground,
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
        let repo_base_name = "devhost";
        assert_eq!(
            fake_repo.take_events(),
            ["example.com", "fuchsia.com"].map(|repo_name| RepositoryManagerEvent::Add {
                repo: RepositoryConfig {
                    mirrors: Some(vec![MirrorConfig {
                        mirror_url: Some(format!(
                            "http://{REPO_ADDR}:{dynamic_repo_port}/{repo_base_name}.{repo_name}"
                        )),
                        subscribe: Some(true),
                        ..Default::default()
                    }]),
                    repo_url: Some(format!("fuchsia-pkg://{repo_base_name}.{repo_name}")),
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
            let repo_url =
                format!("http://{REPO_ADDR}:{dynamic_repo_port}/{repo_base_name}.{repo_name}");
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
        test_env
            .context
            .query("repository.process_dir")
            .level(Some(ConfigLevel::User))
            .set(test_env.isolate_root.path().to_string_lossy().into())
            .await
            .expect("Setting process dir");

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

        let (sender, _receiver) = channel();
        let mut sender = Some(sender);
        let repos = Deferred::from_output(Ok(fho::testing::fake_proxy(move |req| match req {
            ffx::RepositoryRegistryRequest::ServerStart { responder, address: None } => {
                sender.take().unwrap().send(()).unwrap();
                responder.send(Err(RepositoryError::ServerNotRunning)).unwrap()
            }
            other => panic!("Unexpected request: {:?}", other),
        })));

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
            auto_publish: None,
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
                repos,
                serve_cmd_without_root,
                test_env.context.clone(),
                SimpleWriter::new(),
                ServerMode::Foreground
            )
            .await
            .is_err(),
            true
        );

        let test_stdout = TestBuffer::default();
        let writer = SimpleWriter::new_buffers(test_stdout.clone(), Vec::new());

        // Run main in background
        let _task = fasync::Task::local(async move {
            let (sender, _receiver) = channel();
            let mut sender = Some(sender);
            let repos = Deferred::from_output(Ok(fho::testing::fake_proxy(move |req| match req {
                ffx::RepositoryRegistryRequest::ServerStart { responder, address: None } => {
                    sender.take().unwrap().send(()).unwrap();
                    responder.send(Err(RepositoryError::ServerNotRunning)).unwrap()
                }
                other => panic!("Unexpected request: {:?}", other),
            })));
            serve_impl(
                Connector::try_from_env(&env)
                    .await
                    .expect("Could not make target proxy test connector"),
                Connector::try_from_env(&env).await.expect("Could not make RCS test connector"),
                repos,
                serve_cmd_with_root,
                test_env.context.clone(),
                writer,
                ServerMode::Foreground,
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
