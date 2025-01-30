// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_lock::Mutex;
use ffx_command_error::{FfxContext as _, Result};
use ffx_config::{EnvironmentContext, TryFromEnvContext};
use ffx_target::{Connection, TargetConnector};
use fho::DirectConnector;
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use futures::future::LocalBoxFuture;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::connect_helper;

/// Encapsulates a connection to a single fuchsia device, using fdomain or
/// overnet as the FIDL communication backend.
#[derive(Debug, Clone)]
pub struct NetworkConnector<T: TryFromEnvContext + TargetConnector> {
    env: EnvironmentContext,
    connection: Arc<Mutex<Option<Connection>>>,
    target_spec: Option<String>,
    _t: std::marker::PhantomData<T>,
}

impl<T: TryFromEnvContext + TargetConnector> NetworkConnector<T> {
    pub async fn new(env: &EnvironmentContext) -> Result<Self> {
        let target_spec = Option::<String>::try_from_env_context(env).await?;
        Ok(Self {
            env: env.clone(),
            connection: Default::default(),
            target_spec,
            _t: Default::default(),
        })
    }
}

impl<T: TryFromEnvContext + TargetConnector + 'static> NetworkConnector<T> {
    /// Attempts to connect. If already connected, this is a no-op.
    fn maybe_connect(&self) -> LocalBoxFuture<'_, Result<()>> {
        Box::pin(async {
            let mut conn = self.connection.lock().await;
            connect_helper::<T>(&self.env, &mut conn).await
        })
    }
}

impl<T: TryFromEnvContext + TargetConnector + 'static> DirectConnector for NetworkConnector<T> {
    fn connect(&self) -> LocalBoxFuture<'_, Result<()>> {
        Box::pin(async {
            let mut conn = self.connection.lock().await;
            if conn.is_some() {
                tracing::info!("Dropping current connection and reconnecting.");
            }
            drop(conn.take());
            connect_helper::<T>(&self.env, &mut conn).await
        })
    }

    fn rcs_proxy(&self) -> LocalBoxFuture<'_, Result<RemoteControlProxy>> {
        Box::pin(async {
            self.maybe_connect().await?;
            let conn = self.connection.lock().await;
            (*conn)
                .as_ref()
                .ok_or(crate::Error::Unexpected(anyhow::anyhow!("Connection not yet initialized")))?
                .rcs_proxy()
                .await
                .bug()
                .map_err(Into::into)
        })
    }

    fn wrap_connection_errors(&self, e: crate::Error) -> LocalBoxFuture<'_, crate::Error> {
        Box::pin(async {
            let conn = self.connection.lock().await;
            if let Some(c) = (*conn).as_ref() {
                crate::Error::User(c.wrap_connection_errors(e.into()))
            } else {
                e
            }
        })
    }

    fn device_address(&self) -> LocalBoxFuture<'_, Option<SocketAddr>> {
        Box::pin(async { self.connection.lock().await.as_ref().and_then(|c| c.device_address()) })
    }

    fn host_ssh_address(&self) -> LocalBoxFuture<'_, Option<String>> {
        Box::pin(async {
            self.connection
                .lock()
                .await
                .as_ref()
                .and_then(|c| c.host_ssh_address())
                .map(|a| a.to_string())
        })
    }

    fn target_spec(&self) -> Option<String> {
        self.target_spec.clone()
    }

    fn connection(&self) -> LocalBoxFuture<'_, Result<Arc<Mutex<Option<Connection>>>>> {
        Box::pin(async {
            self.maybe_connect().await?;
            Ok(self.connection.clone())
        })
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
        connector.connect().await.unwrap();
        assert_eq!(
            connector.rcs_proxy().await.unwrap().echo_string("foobar").await.unwrap(),
            "foobar"
        );
    }

    #[fuchsia::test]
    async fn test_connection_works_without_explicit_connect() {
        let test_env = ffx_config::test_init().await.unwrap();
        let env = &test_env.context;
        let connector = NetworkConnector::<RegularFakeOvernet>::new(env).await.unwrap();
        assert_eq!(
            connector.rcs_proxy().await.unwrap().echo_string("foobar").await.unwrap(),
            "foobar"
        );
    }
}
