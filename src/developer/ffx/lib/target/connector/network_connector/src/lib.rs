// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_command_error::{bug, user_error, Error, Result};
use ffx_config::{EnvironmentContext, TryFromEnvContext};
use ffx_target::{get_target_specifier, Connection, ConnectionError, TargetConnector};
use fho::DirectConnector;
use futures::future::LocalBoxFuture;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};

async fn connect_helper<T: TryFromEnvContext + TargetConnector + 'static>(
    env: &EnvironmentContext,
    conn: &mut MutexGuard<'_, Option<Connection>>,
) -> Result<()> {
    match **conn {
        Some(_) => Ok(()),
        None => {
            let overnet_connector = T::try_from_env_context(env).await?;
            let c = match Connection::new(overnet_connector).await {
                Ok(c) => Ok(c),
                Err(ConnectionError::ConnectionStartError(cmd_info, error)) => {
                    tracing::info!("connector encountered start error: {cmd_info}, '{error}'");
                    Err(user_error!(
                        "Unable to connect to device via {}: {error}",
                        <T as TargetConnector>::CONNECTION_TYPE
                    ))
                }
                Err(e) => Err(bug!("{e}")),
            }?;
            **conn = Some(c);
            Ok(())
        }
    }
}

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
        let target_spec = get_target_specifier(env).await?;
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
            let mut conn = self.connection.lock().expect("maybe_connect: connection lock poisoned");
            connect_helper::<T>(&self.env, &mut conn).await
        })
    }
}

impl<T: TryFromEnvContext + TargetConnector + 'static> DirectConnector for NetworkConnector<T> {
    fn connect(&self) -> LocalBoxFuture<'_, Result<()>> {
        Box::pin(async {
            let mut conn = self.connection.lock().expect("connect: connection lock poisoned");
            if conn.is_some() {
                tracing::info!("Dropping current connection and reconnecting.");
            }
            drop(conn.take());
            connect_helper::<T>(&self.env, &mut conn).await
        })
    }

    fn wrap_connection_errors(&self, e: crate::Error) -> crate::Error {
        let oc = self.connection.lock().expect("warp_connection_errors: connection lock poisoned");
        if let Some(ref c) = *oc {
            return Error::User(c.wrap_connection_errors(e.into()));
        }
        e
    }

    fn device_address(&self) -> LocalBoxFuture<'_, Option<SocketAddr>> {
        Box::pin(async {
            let conn = self.connection.lock().expect("device_address: connection lock poisoned");
            conn.as_ref().and_then(|c| c.device_address())
        })
    }

    fn host_ssh_address(&self) -> LocalBoxFuture<'_, Option<String>> {
        Box::pin(async {
            let conn = self.connection.lock().expect("host_ssh_address: connection lock poisoned");
            conn.as_ref().and_then(|c| c.host_ssh_address()).map(|a| a.to_string())
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
    }
}
