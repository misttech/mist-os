// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use arc_swap::ArcSwapOption;
use ffx_command_error::{bug, user_error, Error, Result};
use ffx_config::{EnvironmentContext, TryFromEnvContext};
use ffx_target::fho::connector::DirectConnector;
use ffx_target::{get_target_specifier, Connection, ConnectionError, TargetConnector};
use futures::future::LocalBoxFuture;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Semaphore;

async fn connect_helper<T: TryFromEnvContext + TargetConnector + 'static>(
    env: &EnvironmentContext,
) -> Result<Connection> {
    let target_connector = T::try_from_env_context(env).await?;
    match Connection::new(target_connector).await {
        Ok(c) => Ok(c),
        Err(ConnectionError::ConnectionStartError(cmd_info, error)) => {
            log::info!("connector encountered start error: {cmd_info}, '{error}'");
            Err(user_error!(
                "Unable to connect to device via {}: {error}",
                <T as TargetConnector>::CONNECTION_TYPE
            ))
        }
        Err(e) => Err(bug!("{e}")),
    }
}

/// Encapsulates a connection to a single fuchsia device, using fdomain or
/// overnet as the FIDL communication backend.
#[derive(Debug)]
pub struct NetworkConnector<T: TryFromEnvContext + TargetConnector> {
    env: EnvironmentContext,
    connection: ArcSwapOption<Connection>,
    target_spec: Option<String>,
    connect_gate: Semaphore,
    _t: std::marker::PhantomData<T>,
}

impl<T: TryFromEnvContext + TargetConnector> NetworkConnector<T> {
    pub async fn new(env: &EnvironmentContext) -> Result<Self> {
        let target_spec = get_target_specifier(env).await?;
        Ok(Self {
            env: env.clone(),
            connection: Default::default(),
            target_spec,
            connect_gate: Semaphore::new(1),
            _t: Default::default(),
        })
    }
}

impl<T: TryFromEnvContext + TargetConnector + 'static> NetworkConnector<T> {
    /// Attempts to connect. If already connected, this is a no-op.
    fn maybe_connect(&self) -> LocalBoxFuture<'_, Result<Arc<Connection>>> {
        Box::pin(async {
            if let Some(conn) = self.connection.load_full() {
                return Ok(conn);
            }
            // If there are multiple tasks trying to connect, ensure only one tries at a time.
            let _permit = self
                .connect_gate
                .acquire()
                .await
                .map_err(|_| bug!("connection semaphore was closed??"));
            // Check again, in case someone else grabbed it while we were getting the permit
            if let Some(conn) = self.connection.load_full() {
                return Ok(conn);
            }
            let conn = Arc::new(connect_helper::<T>(&self.env).await?);
            self.connection.store(Some(conn.clone()));
            Ok(conn)
        })
    }
}

impl<T: TryFromEnvContext + TargetConnector + 'static> DirectConnector for NetworkConnector<T> {
    /// TODO(b/432297777)
    #[allow(clippy::await_holding_lock)]
    fn connect(&self) -> LocalBoxFuture<'_, Result<()>> {
        Box::pin(async {
            if self.connection.load().is_some() {
                log::info!("Dropping current connection and reconnecting.");
                self.connection.store(None);
            }
            let _ = self.maybe_connect().await?;
            Ok(())
        })
    }
    fn wrap_connection_errors(&self, e: crate::Error) -> crate::Error {
        let guard = self.connection.load();
        if let Some(c) = guard.as_ref() {
            return Error::User(c.wrap_connection_errors(e.into()));
        }
        e
    }

    /// TODO(b/432297777)
    #[allow(clippy::await_holding_lock)]
    fn device_address(&self) -> LocalBoxFuture<'_, Option<SocketAddr>> {
        Box::pin(async {
            let guard = self.connection.load();
            guard.as_ref().and_then(|c| c.device_address())
        })
    }

    /// TODO(b/432297777)
    #[allow(clippy::await_holding_lock)]
    fn host_ssh_address(&self) -> LocalBoxFuture<'_, Option<String>> {
        Box::pin(async {
            let guard = self.connection.load();
            guard.as_ref().and_then(|c| c.host_ssh_address()).map(|a| a.to_string())
        })
    }

    fn target_spec(&self) -> Option<String> {
        self.target_spec.clone()
    }

    fn connection(&self) -> LocalBoxFuture<'_, Result<Arc<Connection>>> {
        Box::pin(async { Ok(self.maybe_connect().await?) })
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::RegularFakeOvernet;

    use super::*;

    #[fuchsia::test]
    async fn test_connection_works_after_connecting() {
        let test_env = ffx_config::test_init().await.unwrap();
        let env = &test_env.context;
        let connector = NetworkConnector::<RegularFakeOvernet>::new(env).await.unwrap();
        connector.connection().await.unwrap();
    }
}
