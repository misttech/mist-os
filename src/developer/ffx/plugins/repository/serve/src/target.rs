// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Target related functions used by the repository server.

use anyhow::{anyhow, Context};
use camino::Utf8Path;
use ffx_command_error::{return_user_error, Result};
use ffx_repository_serve_args::ServeCommand;
use ffx_target::{knock_target, TargetProxy};
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
use fuchsia_repo::manager::RepositoryManager;
use futures::{pin_mut, select, FutureExt, StreamExt};
use pkg::repo::register_target_with_fidl_proxies;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use target_connector::Connector;
use timeout::timeout;

const REPOSITORY_MANAGER_MONIKER: &str = "/core/pkg-resolver";
const ENGINE_MONIKER: &str = "/core/pkg-resolver";
const MAX_CONSECUTIVE_CONNECT_ATTEMPTS: u8 = 10;

async fn connect_to_target(
    target_spec: Option<String>,
    target_info: TargetInfo,
    aliases: Vec<String>,
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
        let repo_spec = repo.read().await.spec();
        let repo_target = FfxCliRepositoryTarget {
            repo_name: Some(repo_name),
            target_identifier: target_spec.clone(),
            aliases: if aliases.is_empty() {
                Some(repo_spec.aliases().iter().map(ToString::to_string).collect())
            } else {
                Some(aliases.clone())
            },
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

async fn inner_connect_loop(
    cmd: &ServeCommand,
    repo_path: &Utf8Path,
    server_addr: core::net::SocketAddr,
    connect_timeout: std::time::Duration,
    repo_manager: &Arc<RepositoryManager>,
    rcs_proxy: &Connector<RemoteControlProxy>,
    target_proxy: &Connector<TargetProxy>,
    writer: &mut impl Write,
) -> Result<()> {
    let mut target_spec_from_rcs_proxy: Option<String> = None;
    let rcs_proxy = timeout(
        connect_timeout,
        rcs_proxy.try_connect(|target, _err| {
            tracing::info!(
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
    let mut target_spec_from_target_proxy: Option<String> = None;
    let target_proxy = target_proxy
        .try_connect(|target, _err| {
            tracing::info!(
                "Target proxy: Waiting for target '{}' to return",
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
        fho::return_user_error!(
            "RCS and target proxies do not match: '{:?}', '{:?}'",
            target_spec_from_rcs_proxy,
            target_spec_from_target_proxy,
        );
    }

    let target_info: TargetInfo = timeout(Duration::from_secs(2), target_proxy.identity())
        .await
        .context("Timed out getting target identity")?
        .context("Failed to get target identity")?;

    let connection = connect_to_target(
        target_spec_from_rcs_proxy.clone(),
        target_info,
        cmd.alias.clone(),
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
            return Err(fho::Error::User(e));
        }
    };
    Ok(())
}

pub(crate) async fn main_connect_loop(
    cmd: &ServeCommand,
    repo_path: &Utf8Path,
    server_addr: core::net::SocketAddr,
    connect_timeout: std::time::Duration,
    repo_manager: Arc<RepositoryManager>,
    mut loop_stop_rx: futures::channel::mpsc::Receiver<()>,
    rcs_proxy: Connector<RemoteControlProxy>,
    target_proxy: Connector<TargetProxy>,
    writer: &mut (impl Write + 'static),
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
            cmd,
            repo_path,
            server_addr,
            connect_timeout,
            &repo_manager,
            &rcs_proxy,
            &target_proxy,
            writer,
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
                        tracing::info!("Attempt {attempts}: {}", e);
                    }
                }
            },
        };
    }
}
