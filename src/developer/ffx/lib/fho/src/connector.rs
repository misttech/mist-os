// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{FfxContext, Result};
use async_lock::{Mutex, MutexGuard};
use fdomain_fuchsia_developer_remotecontrol::RemoteControlProxy as FRemoteControlProxy;
use ffx_config::EnvironmentContext;
use ffx_target::connection::{Connection, ConnectionError};
use ffx_target::ssh_connector::SshConnector;
use ffx_target::{Resolution, TargetConnector};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use futures::future::LocalBoxFuture;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::sync::Arc;

/// An object used for connecting to a Fuchsia Device. This represents the entire underlying
/// connection to a fuchsia device. If this object is dropped, then all FIDL protocols will be
/// closed with `PEER_CLOSED` errors as a result.
pub trait DirectConnector: Debug {
    // A note on the shape of this trait: the object must _not_ be sized in order for it to be used as
    // a trait object, due to the bounds around object safety in rust. Furthermore, a safe trait object
    // cannot return `impl Trait` of some kind, so using `impl Future` and making this a trait object
    // is not feasible. This is functionally what `async_trait` already does, but as a whole the tools
    // team is moving away from its usage where possible.

    /// Attempts to connect to the Fuchsia device. This function can be run until it succeeds, but
    /// once it succeeds running this function again will takes the current connection, tear it
    /// down, then create a new connection.
    fn connect(&self) -> LocalBoxFuture<'_, Result<()>>;

    /// Gets the RCS proxy from the device via the underlying connector. Starts a connection if one
    /// hasn't been initiated.
    fn rcs_proxy(&self) -> LocalBoxFuture<'_, Result<RemoteControlProxy>>;

    /// Gets the RCS proxy from the device via the underlying connector. Starts a connection if one
    /// hasn't been initiated.
    fn rcs_proxy_fdomain(&self) -> LocalBoxFuture<'_, Result<FRemoteControlProxy>>;

    /// Attempts to pull any errors off of the connection and wrap the passed error in one larger
    /// error encompassing the entire connection failure. This is usually done after something
    /// else depending on the connection fails (e.g. a failure in the operation of a FIDL
    /// protocol).
    ///
    /// In the event that `Some(_)` the enclosed vector will always be of size 1 or larger.
    fn wrap_connection_errors(&self, e: crate::Error) -> LocalBoxFuture<'_, crate::Error>;

    /// Returns the device address (if there currently is one).
    fn device_address(&self) -> LocalBoxFuture<'_, Option<SocketAddr>>;

    /// Returns the host address of the ssh connection from the device perspective.
    fn host_ssh_address(&self) -> LocalBoxFuture<'_, Option<String>>;

    /// Returns the spec of the target to which we are connecting/connected.
    fn target_spec(&self) -> Option<String>;
}

pub trait TryFromEnvContext: Sized + Debug {
    fn try_from_env_context<'a>(env: &'a EnvironmentContext) -> LocalBoxFuture<'a, Result<Self>>;
}

// This is an implementation for the "target_spec", which is just an `Option<String>` (it should
// really just be a newtype, but that requires a lot of existing code to change).
impl TryFromEnvContext for Option<String> {
    fn try_from_env_context<'a>(env: &'a EnvironmentContext) -> LocalBoxFuture<'a, Result<Self>> {
        Box::pin(async { ffx_target::get_target_specifier(env).await.bug().map_err(Into::into) })
    }
}

impl TryFromEnvContext for ffx_target::Resolution {
    fn try_from_env_context<'a>(env: &'a EnvironmentContext) -> LocalBoxFuture<'a, Result<Self>> {
        Box::pin(async {
            let unspecified_target = ffx_target::UNSPECIFIED_TARGET_NAME.to_owned();
            let target_spec = Option::<String>::try_from_env_context(env).await?;
            let target_spec_unwrapped = if env.is_strict() {
                target_spec.as_ref().ok_or(ffx_command::user_error!(
                    "You must specify a target via `-t <target_name>` before any command arguments"
                ))?
            } else {
                target_spec.as_ref().unwrap_or(&unspecified_target)
            };
            tracing::trace!("resolving target spec address from {}", target_spec_unwrapped);
            let resolution = ffx_target::resolve_target_address(&target_spec, env)
                .await
                .map_err(|e| ffx_command::Error::User(crate::NonFatalError(e).into()))?;
            Ok(resolution)
        })
    }
}

impl TryFromEnvContext for SshConnector {
    fn try_from_env_context<'a>(env: &'a EnvironmentContext) -> LocalBoxFuture<'a, Result<Self>> {
        Box::pin(async {
            let resolution = Resolution::try_from_env_context(env).await?;
            let res = resolution.addr().map_err(|_| {
                ffx_command::user_error!(
                    "query did not resolve an IP address. Resolved the following: {:?}",
                    resolution,
                )
            })?;
            tracing::debug!("connecting to address {res}");
            SshConnector::new(res, env).await.bug().map_err(Into::into)
        })
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
                    Err(crate::user_error!(
                        "Unable to connect to device via {}: {error}",
                        <T as TargetConnector>::CONNECTION_TYPE
                    ))
                }
                Err(e) => Err(crate::bug!("{e}")),
            }?;
            **conn = Some(c);
            Ok(())
        }
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

    fn rcs_proxy_fdomain(&self) -> LocalBoxFuture<'_, Result<FRemoteControlProxy>> {
        Box::pin(async {
            self.maybe_connect().await?;
            let conn = self.connection.lock().await;
            (*conn)
                .as_ref()
                .ok_or(crate::Error::Unexpected(anyhow::anyhow!("Connection not yet initialized")))?
                .rcs_proxy_fdomain()
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

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_target::connection::testing::{FakeOvernet, FakeOvernetBehavior};
    use ffx_target::{TargetConnection, TargetConnectionError};

    // This is a bit of a hack, but there needs to be a way to set the behavior that also doesn't
    // require locking every test sequentially.
    #[derive(Debug)]
    struct RegularFakeOvernet(FakeOvernet);

    impl TargetConnector for RegularFakeOvernet {
        const CONNECTION_TYPE: &'static str = "fake";

        async fn connect(&mut self) -> Result<TargetConnection, TargetConnectionError> {
            self.0.connect().await
        }
    }

    impl TryFromEnvContext for RegularFakeOvernet {
        fn try_from_env_context<'a>(
            _env: &'a EnvironmentContext,
        ) -> LocalBoxFuture<'a, Result<Self>> {
            let (_sender, receiver) = async_channel::unbounded();
            let circuit_node = overnet_core::Router::new(None).unwrap();
            Box::pin(async {
                Ok(Self(FakeOvernet::new(circuit_node, receiver, FakeOvernetBehavior::KeepRcsOpen)))
            })
        }
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

    #[derive(Debug)]
    struct FromContextFailer(FakeOvernet);

    impl TargetConnector for FromContextFailer {
        const CONNECTION_TYPE: &'static str = "fake";
        async fn connect(&mut self) -> Result<TargetConnection, TargetConnectionError> {
            self.0.connect().await
        }
    }

    impl TryFromEnvContext for FromContextFailer {
        fn try_from_env_context<'a>(
            _env: &'a EnvironmentContext,
        ) -> LocalBoxFuture<'_, Result<Self>> {
            Box::pin(async {
                Err(crate::Error::Unexpected(anyhow::anyhow!("Oh no it broke!")).into())
            })
        }
    }

    #[fuchsia::test]
    async fn test_connection_fails_when_overnet_connector_cannot_be_allocated() {
        let test_env = ffx_config::test_init().await.unwrap();
        let env = &test_env.context;
        let connector = NetworkConnector::<FromContextFailer>::new(env).await.unwrap();
        assert!(connector.connect().await.is_err());
        assert!(connector.connect().await.is_err());
        assert!(connector.rcs_proxy().await.is_err());
        let err = crate::Error::Unexpected(anyhow::anyhow!("foo"));
        assert_eq!(err.to_string(), connector.wrap_connection_errors(err).await.to_string());
    }
}
