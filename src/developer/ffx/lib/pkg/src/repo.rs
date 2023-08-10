// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::config as pkg_config,
    crate::metrics,
    crate::tunnel::TunnelManager,
    async_lock::RwLock,
    fidl_fuchsia_developer_ffx as ffx,
    fidl_fuchsia_developer_ffx_ext::{
        RepositoryError, RepositoryRegistrationAliasConflictMode, RepositoryTarget,
    },
    fidl_fuchsia_pkg::RepositoryManagerProxy,
    fidl_fuchsia_pkg_rewrite::EngineProxy,
    fidl_fuchsia_pkg_rewrite_ext::{do_transaction, Rule},
    fuchsia_async as fasync,
    fuchsia_hyper::{new_https_client, HttpsClient},
    fuchsia_repo::{
        manager::RepositoryManager,
        repo_client::RepoClient,
        repository::{self, RepoProvider},
        server::RepositoryServer,
    },
    fuchsia_url::RepositoryUrl,
    fuchsia_zircon_status::Status,
    futures::FutureExt as _,
    protocols::prelude::Context,
    std::{
        collections::{BTreeSet, HashSet},
        net::SocketAddr,
        sync::Arc,
        time::Duration,
    },
};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(PartialEq)]
pub enum SaveConfig {
    Save,
    DoNotSave,
}

// TODO(fxbug/127781) Remove `pub` once library centralized here.
#[derive(Debug)]
pub struct ServerInfo {
    // TODO(fxbug/127781) Remove `pub` once library centralized here.
    pub server: RepositoryServer,
    // TODO(fxbug/127781) Remove `pub` once library centralized here.
    pub task: fasync::Task<()>,
    // TODO(fxbug/127781) Remove `pub` once library centralized here.
    pub tunnel_manager: TunnelManager,
}

impl ServerInfo {
    async fn new(
        listen_addr: SocketAddr,
        manager: Arc<RepositoryManager>,
    ) -> std::io::Result<Self> {
        tracing::debug!("Starting repository server on {}", listen_addr);

        let (server_fut, sink, server) =
            RepositoryServer::builder(listen_addr, Arc::clone(&manager)).start().await?;

        tracing::info!("Started repository server on {}", server.local_addr());

        // Spawn the server future in the background to process requests from clients.
        let task = fasync::Task::local(server_fut);

        let tunnel_manager = TunnelManager::new(server.local_addr(), sink);

        Ok(ServerInfo { server, task, tunnel_manager })
    }
}

// TODO(fxbug/127781) Remove `pub` once library centralized here.
#[derive(Debug)]
pub enum ServerState {
    Running(ServerInfo),
    Stopped,
    Disabled,
}

impl ServerState {
    pub async fn start_tunnel(&self, cx: &Context, target_nodename: &str) -> anyhow::Result<()> {
        match self {
            ServerState::Running(ref server_info) => {
                server_info.tunnel_manager.start_tunnel(cx, target_nodename.to_string()).await
            }
            _ => Ok(()),
        }
    }

    pub async fn stop(&mut self) -> Result<(), ffx::RepositoryError> {
        match std::mem::replace(self, ServerState::Disabled) {
            ServerState::Running(server_info) => {
                *self = ServerState::Stopped;

                tracing::debug!("Stopping the repository server");

                server_info.server.stop();

                futures::select! {
                    () = server_info.task.fuse() => {
                        tracing::debug!("Stopped the repository server");
                    },
                    () = fasync::Timer::new(SHUTDOWN_TIMEOUT).fuse() => {
                        tracing::error!("Timed out waiting for the repository server to shut down");
                    },
                }

                Ok(())
            }
            state => {
                *self = state;

                Err(ffx::RepositoryError::ServerNotRunning)
            }
        }
    }

    /// Returns the address is running on. Returns None if the server is not
    /// running, or is unconfigured.
    pub fn listen_addr(&self) -> Option<SocketAddr> {
        match self {
            ServerState::Running(x) => Some(x.server.local_addr()),
            _ => None,
        }
    }
}

pub struct RepoInner {
    // TODO(fxbug/127781) Remove `pub` once library centralized here.
    pub manager: Arc<RepositoryManager>,
    // TODO(fxbug/127781) Remove `pub` once library centralized here.
    pub server: ServerState,
    // TODO(fxbug/127781) Remove `pub` once library centralized here.
    pub https_client: HttpsClient,
}

// RepoInner can move.
impl RepoInner {
    pub fn new() -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(RepoInner {
            manager: RepositoryManager::new(),
            server: ServerState::Disabled,
            https_client: new_https_client(),
        }))
    }

    pub async fn start_server(
        &mut self,
        socket_address: Option<SocketAddr>,
    ) -> Result<Option<SocketAddr>, RepositoryError> {
        // Exit early if the server is disabled.
        let server_enabled = pkg_config::get_repository_server_enabled().await.map_err(|err| {
            tracing::error!("failed to read save server enabled flag: {:#?}", err);
            RepositoryError::InternalError
        })?;

        if !server_enabled {
            return Ok(None);
        }

        // Exit early if we're already running on this address.
        let listen_addr = match &self.server {
            ServerState::Disabled => {
                return Ok(None);
            }
            ServerState::Running(info) => {
                return Ok(Some(info.server.local_addr()));
            }
            ServerState::Stopped => match {
                if let Some(addr) = socket_address {
                    Ok(Some(addr))
                } else {
                    pkg_config::repository_listen_addr().await
                }
            } {
                Ok(Some(addr)) => addr,
                Ok(None) => {
                    tracing::error!(
                        "repository.server.listen address not configured, not starting server"
                    );

                    metrics::server_disabled_event().await;
                    return Ok(None);
                }
                Err(err) => {
                    tracing::error!("Failed to read server address from config: {:#}", err);
                    return Ok(None);
                }
            },
        };

        match ServerInfo::new(listen_addr, Arc::clone(&self.manager)).await {
            Ok(info) => {
                let local_addr = info.server.local_addr();
                self.server = ServerState::Running(info);
                pkg_config::set_repository_server_last_address_used(local_addr.to_string())
                    .await
                    .map_err(|err| {
                    tracing::error!(
                        "failed to save server last address used flag to config: {:#?}",
                        err
                    );
                    ffx::RepositoryError::InternalError
                })?;
                metrics::server_started_event().await;
                Ok(Some(local_addr))
            }
            Err(err) => {
                tracing::error!("failed to start repository server: {:#?}", err);
                metrics::server_failed_to_start_event(&err.to_string()).await;

                match err.kind() {
                    std::io::ErrorKind::AddrInUse => {
                        Err(RepositoryError::ServerAddressAlreadyInUse)
                    }
                    _ => Err(RepositoryError::IoError),
                }
            }
        }
    }

    pub async fn stop_server(&mut self) -> Result<(), ffx::RepositoryError> {
        tracing::debug!("Stopping repository protocol");

        self.server.stop().await?;

        // Drop all repositories.
        self.manager.clear();

        tracing::info!("Repository protocol has been stopped");

        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
pub trait Registrar {
    async fn register_target(
        &self,
        cx: &Context,
        mut target_info: RepositoryTarget,
        save_config: SaveConfig,
        inner: Arc<RwLock<RepoInner>>,
        alias_conflict_mode: RepositoryRegistrationAliasConflictMode,
    ) -> Result<(), ffx::RepositoryError>;

    async fn register_target_with_fidl(
        &self,
        cx: &Context,
        mut target_info: RepositoryTarget,
        save_config: SaveConfig,
        inner: Arc<RwLock<RepoInner>>,
        alias_conflict_mode: RepositoryRegistrationAliasConflictMode,
    ) -> Result<(), ffx::RepositoryError>;

    async fn register_target_with_ssh(
        &self,
        cx: &Context,
        mut target_info: RepositoryTarget,
        save_config: SaveConfig,
        inner: Arc<RwLock<RepoInner>>,
        alias_conflict_mode: RepositoryRegistrationAliasConflictMode,
    ) -> Result<(), ffx::RepositoryError>;
}

pub async fn update_repository(
    repo_name: &str,
    repo: &RwLock<RepoClient<Box<dyn RepoProvider>>>,
) -> Result<bool, ffx::RepositoryError> {
    repo.write().await.update().await.map_err(|err| {
        tracing::error!("Unable to update repository {}: {:#?}", repo_name, err);

        match err {
            repository::Error::Tuf(tuf::Error::ExpiredMetadata(_)) => {
                ffx::RepositoryError::ExpiredRepositoryMetadata
            }
            _ => ffx::RepositoryError::IoError,
        }
    })
}

pub async fn register_target_with_fidl_proxies(
    repo_proxy: RepositoryManagerProxy,
    rewrite_engine_proxy: EngineProxy,
    repo_target_info: &RepositoryTarget,
    target: &ffx::TargetInfo,
    target_nodename: &str,
    inner: &Arc<RwLock<RepoInner>>,
    alias_conflict_mode: RepositoryRegistrationAliasConflictMode,
) -> Result<(), ffx::RepositoryError> {
    let repo_name: &str = &repo_target_info.repo_name;

    tracing::info!(
        "Registering repository {:?} for target {:?}",
        repo_name,
        repo_target_info.target_identifier
    );

    let repo = inner
        .read()
        .await
        .manager
        .get(repo_name)
        .ok_or_else(|| ffx::RepositoryError::NoMatchingRepository)?;

    let listen_addr = match inner.read().await.server.listen_addr() {
        Some(listen_addr) => listen_addr,
        None => {
            tracing::error!("repository server is not running");
            return Err(ffx::RepositoryError::ServerNotRunning);
        }
    };

    // Before we register the repository, we need to decide which address the
    // target device should use to reach the repository. If the server is
    // running on a loopback device, then we need to create a tunnel for the
    // device to access the server.
    let (_, repo_host) = create_repo_host(
        listen_addr,
        target.ssh_host_address.clone().ok_or_else(|| {
            tracing::error!(
                "target {:?} does not have a host address",
                repo_target_info.target_identifier
            );
            ffx::RepositoryError::TargetCommunicationFailure
        })?,
    );

    // Make sure the repository is up to date.
    update_repository(repo_name, &repo).await?;

    let repo_url = RepositoryUrl::parse_host(repo_name.to_owned()).map_err(|err| {
        tracing::error!("failed to parse repository name {}: {:#}", repo_name, err);
        ffx::RepositoryError::InvalidUrl
    })?;

    let mirror_url = format!("http://{}/{}", repo_host, repo_name);
    let mirror_url = mirror_url.parse().map_err(|err| {
        tracing::error!("failed to parse mirror url {}: {:#}", mirror_url, err);
        ffx::RepositoryError::InvalidUrl
    })?;

    let (config, aliases) = {
        let repo = repo.read().await;

        let config = repo
            .get_config(
                repo_url,
                mirror_url,
                repo_target_info
                    .storage_type
                    .as_ref()
                    .map(|storage_type| storage_type.clone().into()),
            )
            .map_err(|e| {
                tracing::error!("failed to get config: {}", e);
                return ffx::RepositoryError::RepositoryManagerError;
            })?;

        // Use the repository aliases if the registration doesn't have any.
        let aliases = if let Some(aliases) = &repo_target_info.aliases {
            aliases.clone()
        } else {
            repo.aliases().clone()
        };

        // Checking for registration alias conflicts.
        let check_alias_conflict = pkg_config::check_registration_alias_conflict(
            repo_name,
            target_nodename,
            aliases.clone().into_iter().collect(),
        )
        .await
        .map_err(|e| {
            tracing::error!("{e}");
            ffx::RepositoryError::ConflictingRegistration
        });
        if alias_conflict_mode == RepositoryRegistrationAliasConflictMode::ErrorOut {
            check_alias_conflict?
        }

        (config, aliases)
    };

    match repo_proxy.add(&config.into()).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::error!("failed to add config: {:#?}", Status::from_raw(err));
            return Err(ffx::RepositoryError::RepositoryManagerError);
        }
        Err(err) => {
            tracing::error!("failed to add config: {:#?}", err);
            return Err(ffx::RepositoryError::TargetCommunicationFailure);
        }
    }

    if !aliases.is_empty() {
        let () = create_aliases_fidl(rewrite_engine_proxy, repo_name, &aliases).await?;
    }

    Ok(())
}

pub fn aliases_to_rules(
    repo_name: &str,
    aliases: &BTreeSet<String>,
) -> Result<Vec<Rule>, ffx::RepositoryError> {
    let rules = aliases
        .iter()
        .map(|alias| {
            let mut split_alias = alias.split("/").collect::<Vec<&str>>();
            let host_match = split_alias.remove(0);
            let path_prefix = split_alias.join("/");
            Rule::new(
                host_match.to_string(),
                repo_name.to_string(),
                format!("/{path_prefix}"),
                format!("/{path_prefix}"),
            )
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| {
            tracing::warn!("failed to construct rule: {:#?}", err);
            ffx::RepositoryError::RewriteEngineError
        })?;

    Ok(rules)
}

async fn create_aliases_fidl(
    rewrite_proxy: EngineProxy,
    repo_name: &str,
    aliases: &BTreeSet<String>,
) -> Result<(), ffx::RepositoryError> {
    let alias_rules = aliases_to_rules(repo_name, &aliases)?;

    // Check flag here for "overwrite" style
    do_transaction(&rewrite_proxy, |transaction| async {
        // Prepend the alias rules to the front so they take priority.
        let mut rules = alias_rules.iter().cloned().rev().collect::<Vec<_>>();

        // These are rules to re-evaluate...
        let repo_rules_state = transaction.list_dynamic().await?;
        rules.extend(repo_rules_state);

        // Clear the list, since we'll be adding it back later.
        transaction.reset_all()?;

        // Remove duplicated rules while preserving order.
        let mut unique_rules = HashSet::new();
        rules.retain(|r| unique_rules.insert(r.clone()));

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
        ffx::RepositoryError::RewriteEngineError
    })?;

    Ok(())
}

/// Decide which repo host we should use when creating a repository config, and
/// whether or not we need to create a tunnel in order for the device to talk to
/// the repository.
pub fn create_repo_host(
    listen_addr: SocketAddr,
    host_address: ffx::SshHostAddrInfo,
) -> (bool, String) {
    // We need to decide which address the target device should use to reach the
    // repository. If the server is running on a loopback device, then we need
    // to create a tunnel for the device to access the server.
    if listen_addr.ip().is_loopback() {
        return (true, listen_addr.to_string());
    }

    // However, if it's not a loopback address, then configure the device to
    // communicate by way of the ssh host's address. This is helpful when the
    // device can access the repository only through a specific interface.

    // FIXME(fxbug.dev/87439): Once the tunnel bug is fixed, we may
    // want to default all traffic going through the tunnel. Consider
    // creating an ffx config variable to decide if we want to always
    // tunnel, or only tunnel if the server is on a loopback address.

    // IPv6 addresses can contain a ':', IPv4 cannot.
    let repo_host = if host_address.address.contains(':') {
        if let Some(pos) = host_address.address.rfind('%') {
            let ip = &host_address.address[..pos];
            let scope_id = &host_address.address[pos + 1..];
            format!("[{}%25{}]:{}", ip, scope_id, listen_addr.port())
        } else {
            format!("[{}]:{}", host_address.address, listen_addr.port())
        }
    } else {
        format!("{}:{}", host_address.address, listen_addr.port())
    };

    (false, repo_host)
}
