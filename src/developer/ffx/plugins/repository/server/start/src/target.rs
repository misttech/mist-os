// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Target related functions used by the repository server.

use anyhow::{anyhow, Context};
use camino::Utf8Path;
use ffx_command_error::{return_user_error, Result};
use ffx_repository_server_start_args::StartCommand;
use ffx_target::RcsKnocker;
use ffx_target_net::TargetTcpStream;
use fidl_fuchsia_developer_ffx::TargetInfo;
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_pkg::RepositoryManagerMarker;
use fidl_fuchsia_pkg_ext::{
    RepositoryRegistrationAliasConflictMode, RepositoryStorageType, RepositoryTarget,
};
use fidl_fuchsia_pkg_rewrite::EngineMarker;
use fuchsia_repo::manager::RepositoryManager;
use fuchsia_repo::server::ConnectionStream;
use futures::channel::mpsc;
use futures::{pin_mut, select, FutureExt, SinkExt, Stream, StreamExt};
use pkg::repo;
use std::collections::BTreeSet;
use std::io::Write;
use std::pin::pin;
use std::sync::Arc;
use target_connector::Connector;
use target_holders::{RemoteControlProxyHolder, TargetInfoHolder};
use timeout::timeout;

const REPOSITORY_MANAGER_MONIKER: &str = "/core/pkg-resolver";
const ENGINE_MONIKER: &str = "/core/pkg-resolver";
const MAX_CONSECUTIVE_CONNECT_ATTEMPTS: u8 = 10;

async fn connect_to_target(
    target_spec: Option<String>,
    target_info: &TargetInfo,
    aliases: Vec<String>,
    storage_type: Option<RepositoryStorageType>,
    repo_server_listen_addr: std::net::SocketAddr,
    connect_timeout: std::time::Duration,
    repo_manager: Arc<RepositoryManager>,
    rcs_proxy: &RemoteControlProxy,
    alias_conflict_mode: RepositoryRegistrationAliasConflictMode,
    tunnel_addr: std::net::SocketAddr,
) -> Result<impl Stream<Item = anyhow::Result<TargetTcpStream>>, anyhow::Error> {
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

    let port_forward = ffx_target_net::SocketProvider::new_with_rcs(connect_timeout, &rcs_proxy)
        .await
        .with_context(|| format!("connecting to socket provider protocols {:?}", target_spec))?;

    let (repo_host, forwarding_stream) = repo::create_repo_host_and_listener(
        repo_server_listen_addr,
        target_info.ssh_host_address.as_ref(),
        &port_forward,
        tunnel_addr,
    )
    .await
    .with_context(|| format!("resolving repository rost on {:?}", target_spec))?;

    for (repo_name, repo) in repo_manager.repositories() {
        let repo_spec = repo.read().await.spec();
        let repo_target = RepositoryTarget {
            repo_name: repo_name.clone(),
            target_identifier: target_spec.clone(),
            aliases: if aliases.is_empty() {
                Some(repo_spec.aliases().iter().map(ToString::to_string).collect())
            } else {
                Some(BTreeSet::from_iter(aliases.iter().map(|a| a.clone())))
            },
            storage_type: storage_type.clone(),
        };

        // Construct RepositoryTarget from same args as `ffx target repository register`
        let repo_target_info = RepositoryTarget::try_from(repo_target)
            .map_err(|e| anyhow!("Failed to build RepositoryTarget: {:?}", e))?;

        repo::register_target_with_fidl_proxies(
            repo_proxy.clone(),
            engine_proxy.clone(),
            &repo_target_info,
            &repo_host,
            &repo,
            alias_conflict_mode.clone(),
        )
        .await
        .map_err(|e| anyhow!("Failed to register repository: {:?}", e))?;
    }
    Ok(forwarding_stream
        .map(|s| s.into_stream().map(|r| r.context("target tcp listener")).left_stream())
        .unwrap_or_else(|| futures::stream::pending().right_stream()))
}

async fn inner_connect_loop(
    ctx: &ffx_config::EnvironmentContext,
    cmd: &StartCommand,
    repo_path: &Utf8Path,
    server_addr: core::net::SocketAddr,
    connect_timeout: std::time::Duration,
    repo_manager: &Arc<RepositoryManager>,
    rcs_proxy: &Connector<RemoteControlProxyHolder>,
    target_info: &TargetInfoHolder,
    knocker: &impl RcsKnocker,
    writer: &mut impl Write,
    tunnel_addr: core::net::SocketAddr,
    connection_sink: &mut mpsc::UnboundedSender<anyhow::Result<ConnectionStream>>,
) -> Result<()> {
    let mut target_spec_from_rcs_proxy: Option<String> = None;
    let rcs_proxy = timeout(
        connect_timeout,
        rcs_proxy.try_connect(|target, _err| {
            log::info!(
                "RCS proxy: Waiting for target '{}' to return",
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
            fho::return_user_error!("Timeout connecting to rcs: {}", e);
        }
    };

    let connection = connect_to_target(
        target_spec_from_rcs_proxy.clone(),
        target_info,
        cmd.alias.clone(),
        cmd.storage_type.clone(),
        server_addr,
        connect_timeout,
        Arc::clone(&repo_manager),
        &rcs_proxy,
        cmd.alias_conflict_mode.clone(),
        tunnel_addr,
    )
    .await;
    match connection {
        Ok(proxy_stream) => {
            let s = match target_spec_from_rcs_proxy {
                Some(ref t) => format!(
                    "Serving repository '{repo_path}' to target '{t}' over address '{}'.",
                    server_addr
                ),
                None => {
                    format!("Serving repository '{repo_path}' over address '{server_addr}'.")
                }
            };
            if let Err(e) = writeln!(writer, "{}", s) {
                log::error!("Failed to write to output: {:?}", e);
            }
            log::info!("{}", s);

            let mut timer_knock = pin!(async {
                loop {
                    fuchsia_async::Timer::new(std::time::Duration::from_secs(10)).await;
                    match knocker.knock_rcs(target_spec_from_rcs_proxy.clone(), ctx).await {
                        Ok(_) => {
                            // Nothing to do, continue checking connection
                        }
                        Err(e) => {
                            let s = format!("Connection to target lost, retrying. Error: {}", e);
                            if let Err(e) = writeln!(writer, "{}", s) {
                                log::error!("Failed to write to output: {:?}", e);
                            }
                            log::warn!("{}", s);
                            break;
                        }
                    }
                }
            }
            .fuse());
            let mut proxy_drive = pin!(proxy_stream
                .map(|t| Ok(t.map(ConnectionStream::TargetTcp)))
                .forward(connection_sink.sink_map_err(|e| anyhow!("connection sink error: {e:?}")),)
                .fuse());
            select! {
                () = timer_knock => {},
                r = proxy_drive => {
                    log::error!("driving forwarded connections exited unexpectedly: {r:?}")
                }
            }
        }
        Err(e) => {
            return Err(fho::Error::User(e));
        }
    };
    Ok(())
}

pub(crate) async fn main_connect_loop(
    ctx: &ffx_config::EnvironmentContext,
    cmd: &StartCommand,
    repo_path: &Utf8Path,
    server_addr: core::net::SocketAddr,
    connect_timeout: std::time::Duration,
    repo_manager: Arc<RepositoryManager>,
    mut loop_stop_rx: futures::channel::mpsc::Receiver<()>,
    rcs_proxy: Connector<RemoteControlProxyHolder>,
    target_info: &TargetInfoHolder,
    knocker: &impl RcsKnocker,
    writer: &mut (impl Write + 'static),
    tunnel_addr: core::net::SocketAddr,
    mut connection_sink: mpsc::UnboundedSender<anyhow::Result<ConnectionStream>>,
) -> Result<()> {
    // We try to reconnect unless MAX_CONSECUTIVE_CONNECT_ATTEMPTS reconnect
    // attempts in immediate succession fail.
    let mut attempts = 0;

    // Outer connection loop, retries when disconnected.
    loop {
        if attempts >= MAX_CONSECUTIVE_CONNECT_ATTEMPTS {
            return_user_error!(
                "Stopping reconnecting after {attempts} consecutive failed attempts"
            );
        } else {
            attempts += 1;
        }

        let cancel = async {
            // Block until a loop stop request comes in
            loop_stop_rx.next().await;
        }
        .fuse();

        let connect = inner_connect_loop(
            ctx,
            cmd,
            repo_path,
            server_addr,
            connect_timeout,
            &repo_manager,
            &rcs_proxy,
            target_info,
            knocker,
            writer,
            tunnel_addr,
            &mut connection_sink,
        )
        .fuse();

        pin_mut!(cancel, connect);

        select! {
            () = cancel => {
                break Ok(());
            },
            r = connect => {
                match r {
                    // After successfully serving to the target, reset attempts counter before reconnect
                    Ok(()) => {
                        attempts = 0;
                    }
                    Err(e) => {
                        log::info!("Attempt {attempts}: {}", e);
                    }
                }
            },
        };
    }
}
