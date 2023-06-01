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
    fuchsia_async as fasync,
    fuchsia_hyper::{new_https_client, HttpsClient},
    fuchsia_repo::{manager::RepositoryManager, server::RepositoryServer},
    futures::FutureExt as _,
    protocols::prelude::Context,
    std::{net::SocketAddr, sync::Arc, time::Duration},
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
