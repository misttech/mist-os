// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use errors::FfxError;
use ffx_command_error::{return_bug, Error, Result};
use ffx_target::ssh_connector::SshConnector;
use fho::{DirectConnector, FhoConnectionBehavior, FhoEnvironment, TryFromEnv};
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_developer_ffx as ffx_fidl;
use std::sync::Arc;
use std::time::Duration;
use target_holders::{init_connection_behavior, DaemonProxyHolder, DeviceLookupDefaultImpl};
use target_network_connector::NetworkConnector;

/// A connector lets a tool make multiple attempts to connect to an object. It
/// retains the environment in the tool body to allow this.
#[derive(Clone)]
pub struct Connector<T: TryFromEnv> {
    env: FhoEnvironment,
    _connects_to: std::marker::PhantomData<T>,
}

impl<T: TryFromEnv> Connector<T> {
    const OPEN_TARGET_TIMEOUT: Duration = Duration::from_millis(500);
    const KNOCK_TARGET_TIMEOUT: Duration = ffx_target::DEFAULT_RCS_KNOCK_TIMEOUT;

    /// Try to get a `T` from the environment. Will wait for the target to
    /// appear if it is non-responsive. If that occurs, `log_target_wait` will
    /// be called prior to waiting.
    pub async fn try_connect(
        &self,
        mut log_target_wait: impl FnMut(&Option<String>, &Option<Error>) -> Result<()>,
    ) -> Result<T> {
        if let Some(behavior) = self.env.behavior().await {
            match behavior {
                FhoConnectionBehavior::DaemonConnector(_) => {
                    daemon_try_connect(
                        &self.env,
                        &mut log_target_wait,
                        Self::OPEN_TARGET_TIMEOUT,
                        Self::KNOCK_TARGET_TIMEOUT,
                    )
                    .await
                }
                FhoConnectionBehavior::DirectConnector(ref dc) => {
                    direct_connector_try_connect::<T>(&self.env, dc, &mut log_target_wait).await
                }
            }
        } else {
            return_bug!("Behavior must be initialized at this point")
        }
    }
}

#[async_trait(?Send)]
impl<T: TryFromEnv> TryFromEnv for Connector<T> {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        if env.behavior().await.is_none() {
            let b = init_connection_behavior(env.environment_context()).await?;
            env.set_behavior(b).await;
        }
        if env.lookup().await.is_none() {
            env.set_lookup(Box::new(DeviceLookupDefaultImpl)).await
        }
        Ok(Connector { env: env.clone(), _connects_to: Default::default() })
    }
}

pub struct DirectTargetConnector<T: TryFromEnv> {
    pub inner: Connector<T>,
    connector: Arc<NetworkConnector<SshConnector>>,
}

#[async_trait(?Send)]
impl<T: TryFromEnv> TryFromEnv for DirectTargetConnector<T> {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        // Configure the environment to use a direct connector
        let connector: Arc<NetworkConnector<SshConnector>> = Arc::new(
            NetworkConnector::<ffx_target::ssh_connector::SshConnector>::new(
                &env.environment_context(),
            )
            .await?,
        );

        let direct_env = env.clone();
        direct_env.set_behavior(FhoConnectionBehavior::DirectConnector(connector.clone())).await;
        if direct_env.lookup().await.is_none() {
            direct_env.set_lookup(Box::new(DeviceLookupDefaultImpl)).await
        }
        Ok(DirectTargetConnector {
            connector,
            inner: Connector { env: direct_env, _connects_to: Default::default() },
        })
    }
}

/// This is prototype code for the daemonless direct-non-strict connection.
impl<T: TryFromEnv> DirectTargetConnector<T> {
    /// Try to get a `T` from the environment. Will wait for the target to
    /// appear if it is non-responsive. If that occurs, `log_target_wait` will
    /// be called prior to waiting.
    #[allow(dead_code)]
    pub async fn try_connect(
        &self,
        log_target_wait: impl FnMut(&Option<String>, &Option<Error>) -> Result<()>,
    ) -> Result<T> {
        self.inner.try_connect(log_target_wait).await
    }

    #[allow(dead_code)]
    pub async fn get_address(&self) -> Option<std::net::SocketAddr> {
        self.connector.device_address().await
    }
    #[allow(dead_code)]
    pub async fn get_ssh_host_address(&self) -> Option<String> {
        self.connector.host_ssh_address().await
    }
}

async fn knock_rcs(
    target: &Option<String>,
    tc_proxy: &ffx_fidl::TargetCollectionProxy,
    open_target_timeout: Duration,
    knock_target_timeout: Duration,
) -> Result<()> {
    loop {
        match ffx_target::knock_target_by_name(
            target,
            tc_proxy,
            open_target_timeout,
            knock_target_timeout,
        )
        .await
        {
            Ok(()) => break,
            Err(ffx_target::KnockError::CriticalError(e)) => return Err(e.into()),
            Err(ffx_target::KnockError::NonCriticalError(_)) => {
                // Should we log the error? It'll spam like hell.
            }
        };
    }
    Ok(())
}

async fn daemon_try_connect<T: TryFromEnv>(
    env: &FhoEnvironment,
    log_target_wait: &mut impl FnMut(&Option<String>, &Option<Error>) -> Result<()>,
    open_target_timeout: Duration,
    knock_target_timeout: Duration,
) -> Result<T> {
    loop {
        return match T::try_from_env(env).await {
            Err(ffx_command_error::Error::User(e)) => {
                match e.downcast::<target_errors::FfxTargetError>() {
                    Ok(target_errors::FfxTargetError::DaemonError {
                        err: ffx_fidl::DaemonError::Timeout,
                        target,
                        ..
                    }) => {
                        let Ok(daemon_proxy) = DaemonProxyHolder::try_from_env(env).await else {
                            // Let the initial try_from_env detect this error.
                            continue;
                        };
                        let (tc_proxy, server_end) =
                            fidl::endpoints::create_proxy::<ffx_fidl::TargetCollectionMarker>();
                        let Ok(Ok(())) = daemon_proxy
                            .connect_to_protocol(
                                ffx_fidl::TargetCollectionMarker::PROTOCOL_NAME,
                                server_end.into_channel(),
                            )
                            .await
                        else {
                            // Let the rcs_proxy_connector detect this error too.
                            continue;
                        };
                        log_target_wait(&target, &None)?;
                        // The daemon version of this check uses a "knock" against RCS, which is
                        // essentially: keep a channel open to RCS for about a second, and if no
                        // error events come in on the channel during that time, we consider it
                        // "safe." This isn't something strictly necessary (and is not being used
                        // in the daemonless version). This was implemented when reliability with
                        // overnet was pretty spotty (when it was primarily a mesh network), and
                        // was a means to determine if a connection was "real" or if it was
                        // something stale.
                        //
                        // For non-daemon connections this isn't necessary, and we
                        // can operate under the assumption that if we have connected to an
                        // instance of an RCS proxy, we are therefore able to use it.t
                        knock_rcs(&target, &tc_proxy, open_target_timeout, knock_target_timeout)
                            .await?;
                        continue;
                    }
                    Ok(other) => return Err(Into::<FfxError>::into(other).into()),
                    Err(e) => return Err(e.into()),
                }
            }
            other => other,
        };
    }
}

async fn direct_connector_try_connect<T: TryFromEnv>(
    env: &FhoEnvironment,
    dc: &Arc<dyn DirectConnector>,
    log_target_wait: &mut impl FnMut(&Option<String>, &Option<Error>) -> Result<()>,
) -> Result<T> {
    loop {
        match dc.connect().await {
            Ok(()) => {}
            Err(err) => {
                let e = err.downcast_non_fatal()?;
                tracing::debug!("error when attempting to connect with connector: {e}");
                log_target_wait(&dc.target_spec(), &Some(Error::User(e)))?;
                // This is just a small wait to prevent busy-looping. The delay is arbitrary.
                fuchsia_async::Timer::new(Duration::from_millis(50)).await;
                continue;
            }
        }
        return match T::try_from_env(env).await {
            Err(conn_error) => {
                let e = conn_error.downcast_non_fatal()?;
                tracing::debug!("error when trying to connect using TryFromEnv: {e}");
                log_target_wait(&dc.target_spec(), &Some(Error::User(e)))?;
                continue;
            }
            Ok(res) => Ok(res),
        };
    }
}

#[cfg(test)]
mod tests {
    use std::marker::PhantomData;

    use super::*;
    use async_lock::Mutex;
    use ffx_command_error::{bug, NonFatalError};
    use ffx_config::{EnvironmentContext, TryFromEnvContext};
    use ffx_target::connection::testing::{FakeOvernet, FakeOvernetBehavior};
    use ffx_target::{TargetConnection, TargetConnectionError, TargetConnector};
    use fho::MockDirectConnector;
    use futures::future::LocalBoxFuture;
    use target_holders::RemoteControlProxyHolder;

    #[fuchsia::test]
    async fn test_connector_try_connect_fail_after_critical_connection_error() {
        let config_env = ffx_config::test_init().await.unwrap();
        let mut mock_connector = MockDirectConnector::new();
        mock_connector.expect_connect().times(1).returning(|| {
            Box::pin(async { Err(Error::Unexpected(anyhow::anyhow!("we're doomed!").into())) })
        });

        let fho_env =
            FhoEnvironment::new_with_args(&config_env.context, &["some", "connector", "test"]);
        fho_env
            .set_behavior(FhoConnectionBehavior::DirectConnector(Arc::new(mock_connector)))
            .await;

        let connector =
            Connector::<RemoteControlProxyHolder>::try_from_env(&fho_env).await.unwrap();
        let res = connector.try_connect(|_, _| Ok(())).await;
        assert!(res.is_err(), "Expected failure: {:?}", res);
    }

    #[fuchsia::test]
    async fn test_connector_try_connect_fail_reconnect_and_rcs_eventual_success() {
        let config_env = ffx_config::test_init().await.unwrap();

        let mut mock_connector = MockDirectConnector::new();
        mock_connector.expect_device_address().returning(|| Box::pin(async { None }));
        mock_connector.expect_target_spec().returning(|| None);
        let mut seq = mockall::Sequence::new();
        mock_connector.expect_connect().times(3).in_sequence(&mut seq).returning(|| {
            Box::pin(async {
                Err(Error::User(NonFatalError(anyhow::anyhow!("we just need to try again")).into()))
            })
        });
        mock_connector.expect_connect().returning(|| Box::pin(async { Ok(()) }));

        let fho_env =
            FhoEnvironment::new_with_args(&config_env.context, &["some", "connector", "test"]);
        fho_env
            .set_behavior(FhoConnectionBehavior::DirectConnector(Arc::new(mock_connector)))
            .await;

        let connector = Connector::<PhantomData<String>>::try_from_env(&fho_env).await.unwrap();
        let res = connector.try_connect(|_, _| Ok(())).await;
        assert!(res.is_ok(), "Expected success: {:?}", res);
    }

    #[fuchsia::test]
    async fn test_connector_try_connect_fail_after_successful_connection() {
        let config_env = ffx_config::test_init().await.unwrap();
        let mut mock_connector = MockDirectConnector::new();
        mock_connector.expect_device_address().returning(|| Box::pin(async { None }));
        mock_connector.expect_target_spec().returning(|| None);
        let mut seq = mockall::Sequence::new();
        mock_connector
            .expect_connect()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|| Box::pin(async { Ok(()) }));
        mock_connector
            .expect_connection()
            .times(1)
            .returning(|| Box::pin(async { Ok(Arc::new(Mutex::new(None))) }));

        let fho_env =
            FhoEnvironment::new_with_args(&config_env.context, &["some", "connector", "test"]);
        fho_env
            .set_behavior(FhoConnectionBehavior::DirectConnector(Arc::new(mock_connector)))
            .await;

        let connector =
            Connector::<RemoteControlProxyHolder>::try_from_env(&fho_env).await.unwrap();
        let res = connector.try_connect(|_, _| Ok(())).await;
        assert!(res.is_err(), "Expected failure: {:?}", res);
    }

    #[fuchsia::test]
    async fn test_connection_fails_when_overnet_connector_cannot_be_allocated() {
        let test_env = ffx_config::test_init().await.unwrap();
        let env = &test_env.context;
        let connector = NetworkConnector::<FromContextFailer>::new(env).await.unwrap();
        assert!(connector.connect().await.is_err());
        assert!(connector.connect().await.is_err());
        let err = bug!("foo");
        assert_eq!(err.to_string(), connector.wrap_connection_errors(err).await.to_string());
    }

    // This is a bit of a hack, but there needs to be a way to set the behavior that also doesn't
    // require locking every test sequentially.
    #[derive(Debug)]
    pub(crate) struct RegularFakeOvernet(FakeOvernet);

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
}
