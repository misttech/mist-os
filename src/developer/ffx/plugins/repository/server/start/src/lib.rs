// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use daemonize::daemonize;
use ffx_config::EnvironmentContext;
use ffx_repository_server_start_args::StartCommand;
use ffx_writer::VerifiedMachineWriter;
use fho::{bug, deferred, user_error, Deferred, FfxMain, FfxTool, Result};
use fidl_fuchsia_developer_ffx as ffx;
use pkg::config::DEFAULT_REPO_NAME;
use pkg::ServerMode;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::net::SocketAddr;
use std::time::Duration;
use target_connector::Connector;
use target_holders::{daemon_protocol, RemoteControlProxyHolder, TargetProxyHolder};

mod server;
mod server_impl;
mod target;

use server_impl::serve_impl_validate_args;

// The output is untagged and OK is flattened to match
// the legacy output. One day, we'll update the schema and
// worry about migration then.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[serde(untagged)]
pub enum CommandStatus {
    /// Successful execution with an optional informational string.
    Ok {
        #[serde(flatten)]
        address: ServerInfo,
    },
    /// Unexpected error with string.
    UnexpectedError { error_message: String },
    /// A known kind of error that can be reported usefully to the user
    UserError { error_message: String },
}

#[derive(FfxTool)]
pub struct ServerStartTool {
    #[command]
    pub cmd: StartCommand,
    #[with(deferred(daemon_protocol()))]
    pub repos: Deferred<ffx::RepositoryRegistryProxy>,
    pub context: EnvironmentContext,
    pub target_proxy_connector: Connector<TargetProxyHolder>,
    pub rcs_proxy_connector: Connector<RemoteControlProxyHolder>,
}

fho::embedded_plugin!(ServerStartTool);

#[async_trait(?Send)]
impl FfxMain for ServerStartTool {
    type Writer = VerifiedMachineWriter<CommandStatus>;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let new_logname = self.log_basename();

        let result: Result<Option<SocketAddr>> =
            match (self.cmd.background, self.cmd.foreground || self.cmd.disconnected) {
                // Foreground server mode
                (false, true) | (false, false) => {
                    let mode = if self.cmd.disconnected {
                        ServerMode::Background
                    } else {
                        ServerMode::Foreground
                    };
                    return Box::pin(server::run_foreground_server(
                        self.cmd,
                        self.context,
                        self.target_proxy_connector,
                        self.rcs_proxy_connector,
                        self.repos,
                        writer,
                        mode,
                    ))
                    .await;
                }
                // Background server mode
                (true, false) => {
                    // Validate the cmd args before processing. This allows good error messages to
                    // be presented to the user when running in Background mode. If the server is
                    // already running, this returns Ok.
                    if let Some(running) = serve_impl_validate_args(
                        &self.cmd,
                        &self.rcs_proxy_connector,
                        self.repos,
                        &self.context,
                    )
                    .await?
                    {
                        // The server that matches the cmd is already running.
                        writeln!(
                            writer,
                            "A server named {} is serving on address {} the repo path: {}",
                            running.name,
                            running.address,
                            running.repo_path_display()
                        )
                        .map_err(|e| bug!(e))?;
                        Ok(Some(running.address.clone()))
                    } else {
                        let mut args = vec![
                            "repository".to_string(),
                            "server".to_string(),
                            "start".to_string(),
                            "--disconnected".to_string(),
                        ];
                        args.extend(server::to_argv(&self.cmd));

                        if let Some(log_basename) = new_logname {
                            let wait_for_start_timeout: u64 = match self
                                .context
                                .get::<u64, _>("repository.background_startup_timeout")
                            {
                                Ok(v) => v.into(),
                                Err(e) => {
                                    tracing::warn!("Error reading startup timeout: {e}");
                                    60
                                }
                            };

                            daemonize(&args, log_basename, self.context.clone(), true)
                                .await
                                .map_err(|e| bug!(e))?;

                            match server::wait_for_start(
                                self.context.clone(),
                                self.cmd,
                                Duration::from_secs(wait_for_start_timeout),
                            )
                            .await
                            {
                                Ok(addr) => {
                                    tracing::debug!("Daemonized server started successfully");
                                    Ok(Some(addr))
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Daemonized server did not start successfully: {e}"
                                    );
                                    Err(e)
                                }
                            }
                        } else {
                            Err(bug!(
                                "Cannot daemonize repository server without a log file basename"
                            ))
                        }
                    }
                }
                // Invalid switch combinations.
                (true, true) => {
                    Err(user_error!("--background and --foreground are mutually exclusive"))
                }
            };

        match result {
            Ok(server_addr) => {
                if let Some(server_addr) = server_addr {
                    writer.machine_or(
                        &CommandStatus::Ok { address: ServerInfo { address: server_addr } },
                        format!("Repository server is listening on {server_addr:?}"),
                    )?;
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn log_basename(&self) -> Option<String> {
        let basename = format!(
            "repo_{}",
            self.cmd.repository.clone().unwrap_or_else(|| DEFAULT_REPO_NAME.into())
        );
        Some(basename)
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ServerInfo {
    address: std::net::SocketAddr,
}
#[cfg(test)]
mod tests {}
