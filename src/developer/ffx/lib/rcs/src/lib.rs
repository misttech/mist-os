// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Result};
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_developer_ffx as ffx;
use fidl_fuchsia_developer_remotecontrol::{
    ConnectCapabilityError, IdentifyHostError, IdentifyHostResponse, RemoteControlMarker,
    RemoteControlProxy,
};
use fidl_fuchsia_overnet_protocol::NodeId;
use fidl_fuchsia_sys2::{
    ConfigOverrideMarker, ConfigOverrideProxy, LifecycleControllerMarker, LifecycleControllerProxy,
    RealmQueryMarker, RealmQueryProxy, RouteValidatorMarker, RouteValidatorProxy,
};
use futures::{StreamExt, TryFutureExt};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};
use timeout::{timeout, TimeoutError};

pub use fidl_fuchsia_io::OpenFlags;
pub use fidl_fuchsia_sys2::OpenDirType;

pub mod port_forward;
pub mod toolbox;

/// Note that this is only used for backwards compatibility. All new usages should prefer using the
/// toolbox moniker instead.
const REMOTE_CONTROL_MONIKER: &str = "core/remote-control";

const IDENTIFY_HOST_TIMEOUT_MILLIS: u64 = 10000;

#[derive(Debug, Clone)]
pub struct RcsConnection {
    pub node: Arc<overnet_core::Router>,
    pub proxy: RemoteControlProxy,
    pub overnet_id: NodeId,
}

impl Hash for RcsConnection {
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.overnet_id.id.hash(state)
    }
}

impl PartialEq for RcsConnection {
    fn eq(&self, other: &Self) -> bool {
        self.overnet_id == other.overnet_id
    }
}

impl Eq for RcsConnection {}

impl RcsConnection {
    pub fn new(node: Arc<overnet_core::Router>, id: &mut NodeId) -> Result<Self> {
        let (s, p) = fidl::Channel::create();
        let _result = RcsConnection::connect_to_service(Arc::clone(&node), id, s)?;
        let proxy = RemoteControlProxy::new(fidl::AsyncChannel::from_channel(p));

        Ok(Self { node, proxy, overnet_id: id.clone() })
    }

    pub fn copy_to_channel(&mut self, channel: fidl::Channel) -> Result<()> {
        RcsConnection::connect_to_service(Arc::clone(&self.node), &mut self.overnet_id, channel)
    }

    fn connect_to_service(
        node: Arc<overnet_core::Router>,
        overnet_id: &mut NodeId,
        channel: fidl::Channel,
    ) -> Result<()> {
        let overnet_id = (*overnet_id).into();
        // TODO(b/302394849): If this method were async we could return the
        // error instead of just logging it. This task used to be managed by
        // Hoist where we couldn't get to it, but now we have it right here
        // where it would be easy to factor out.
        fuchsia_async::Task::spawn(async move {
            if let Err(e) = node
                .connect_to_service(overnet_id, RemoteControlMarker::PROTOCOL_NAME, channel)
                .await
            {
                tracing::warn!("Error connecting to Rcs: {}", e)
            }
        })
        .detach();
        Ok(())
    }

    // Primarily For testing.
    pub fn new_with_proxy(
        node: Arc<overnet_core::Router>,
        proxy: RemoteControlProxy,
        id: &NodeId,
    ) -> Self {
        Self { node, proxy, overnet_id: id.clone() }
    }

    pub async fn identify_host(&self) -> Result<IdentifyHostResponse, RcsConnectionError> {
        tracing::debug!("Requesting host identity from overnet id {}", self.overnet_id.id);
        let identify_result = timeout(
            Duration::from_millis(IDENTIFY_HOST_TIMEOUT_MILLIS),
            self.proxy.identify_host(),
        )
        .await
        .map_err(|e| RcsConnectionError::ConnectionTimeoutError(e))?;

        let identify = match identify_result {
            Ok(res) => match res {
                Ok(target) => target,
                Err(e) => return Err(RcsConnectionError::RemoteControlError(e)),
            },
            Err(e) => return Err(RcsConnectionError::FidlConnectionError(e)),
        };

        Ok(identify)
    }
}

#[derive(Debug)]
pub enum RcsConnectionError {
    /// There is something wrong with the FIDL connection.
    FidlConnectionError(fidl::Error),
    /// There was a timeout trying to communicate with RCS.
    ConnectionTimeoutError(TimeoutError),
    /// There is an error from within Rcs itself.
    RemoteControlError(IdentifyHostError),

    /// There is an error with the output from Rcs.
    TargetError(anyhow::Error),
}

impl std::fmt::Display for RcsConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RcsConnectionError::FidlConnectionError(ferr) => {
                write!(f, "fidl connection error: {}", ferr)
            }
            RcsConnectionError::ConnectionTimeoutError(_) => write!(f, "timeout error"),
            RcsConnectionError::RemoteControlError(ierr) => write!(f, "internal error: {:?}", ierr),
            RcsConnectionError::TargetError(error) => write!(f, "general error: {}", error),
        }
    }
}

pub const RCS_KNOCK_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(thiserror::Error, Debug)]
pub enum KnockRcsError {
    #[error("FIDL error {0:?}")]
    FidlError(#[from] fidl::Error),
    #[error("Creating FIDL channel: {0:?}")]
    ChannelError(#[from] fidl::handle::Status),
    #[error("Connecting to RCS {0:?}")]
    RcsConnectCapabilityError(ConnectCapabilityError),
    #[error("Could not knock service from RCS")]
    FailedToKnock,
}

/// Attempts to "knock" RCS.
///
/// This can be used to verify whether it is up and running, or as a control flow to ensure that
/// RCS is up and running before continuing time-sensitive operations.
// TODO(b/339266778): Use non-FIDL error type.
pub async fn knock_rcs(rcs_proxy: &RemoteControlProxy) -> Result<(), ffx::TargetConnectionError> {
    knock_rcs_impl(rcs_proxy).await.map_err(|e| match e {
        KnockRcsError::FidlError(e) => {
            tracing::warn!("FIDL error: {:?}", e);
            ffx::TargetConnectionError::FidlCommunicationError
        }
        KnockRcsError::ChannelError(e) => {
            tracing::warn!("RCS connect channel err: {:?}", e);
            ffx::TargetConnectionError::FidlCommunicationError
        }
        KnockRcsError::RcsConnectCapabilityError(c) => {
            tracing::warn!("RCS failed connecting to itself for knocking: {:?}", c);
            ffx::TargetConnectionError::RcsConnectionError
        }
        KnockRcsError::FailedToKnock => ffx::TargetConnectionError::FailedToKnockService,
    })
}

async fn knock_rcs_impl(rcs_proxy: &RemoteControlProxy) -> Result<(), KnockRcsError> {
    let (knock_client, knock_remote) = fidl::handle::Channel::create();
    let res = rcs_proxy
        .open_capability(
            toolbox::MONIKER,
            OpenDirType::NamespaceDir,
            &format!("svc/{}", RemoteControlMarker::PROTOCOL_NAME),
            knock_remote,
            OpenFlags::empty(),
        )
        .await?;
    let knock_client = if res.is_ok() {
        knock_client
    } else {
        // Fallback to the legacy remote control moniker if toolbox doesn't contain the capability.
        let (knock_client, knock_remote) = fidl::handle::Channel::create();
        rcs_proxy
            .open_capability(
                REMOTE_CONTROL_MONIKER,
                OpenDirType::ExposedDir,
                RemoteControlMarker::PROTOCOL_NAME,
                knock_remote,
                OpenFlags::empty(),
            )
            .await?
            .map_err(|e| KnockRcsError::RcsConnectCapabilityError(e))?;
        knock_client
    };

    let knock_client = fuchsia_async::Channel::from_channel(knock_client);
    let knock_client = fidl::client::Client::<fidl::encoding::DefaultFuchsiaResourceDialect>::new(
        knock_client,
        "knock_client",
    );
    let mut event_receiver = knock_client.take_event_receiver();
    let res = timeout(RCS_KNOCK_TIMEOUT, event_receiver.next()).await;
    match res {
        // no events are expected -- the only reason we'll get an event is if
        // channel closes. So the only valid response here is a timeout.
        Err(_) => Ok(()),
        Ok(r) => r.ok_or(KnockRcsError::FailedToKnock).map(drop),
    }
}

pub async fn open_with_timeout_at(
    dur: Duration,
    moniker: &str,
    capability_set: OpenDirType,
    capability_name: &str,
    rcs_proxy: &RemoteControlProxy,
    server_end: fidl::Channel,
) -> Result<()> {
    let open_capability_fut = rcs_proxy.open_capability(
        moniker,
        capability_set,
        capability_name,
        server_end,
        OpenFlags::empty(),
    );
    timeout::timeout(dur, open_capability_fut
        .map_ok_or_else(|e| Result::<(), anyhow::Error>::Err(anyhow::anyhow!(e)), |fidl_result| {
            fidl_result.map(|_| ()).map_err(|e| {
                    match e {
                        ConnectCapabilityError::NoMatchingCapabilities => {
                            errors::ffx_error!(format!(
"The plugin service did not match any capabilities on the target for moniker '{moniker}' and
capability '{capability_name}'.

It is possible that the expected component is either not built into the system image, or that the
package server has not been setup.

For users, ensure your Fuchsia device is registered with ffx. To do this you can run:

$ ffx target repository register -r $IMAGE_TYPE --alias fuchsia.com

For plugin developers, it may be possible that the moniker you're attempting to connect to is
incorrect.
You can use `ffx component explore '<moniker>'` to explore the component topology
of your target device to fix this moniker if this is the case.

If you believe you have encountered a bug after walking through the above please report it at
https://fxbug.dev/new/ffx+User+Bug")).into()
                        }
                        _ => {
                            anyhow::anyhow!(
                                format!("This service dependency exists but connecting to it failed with error {e:?}. Moniker: {moniker}. Capability name: {capability_name}")
                            )
                        }
                    }
                })
        })).await.map_err(|_| errors::ffx_error!("Timed out connecting to capability: '{capability_name}'
with moniker: '{moniker}'.
This is likely due to a sudden shutdown or disconnect of the target.
If you have encountered what you think is a bug, Please report it at https://fxbug.dev/new/ffx+User+Bug

To diagnose the issue, use `ffx doctor`.").into()).and_then(|r| r)
}

pub async fn connect_with_timeout_at(
    dur: Duration,
    moniker: &str,
    capability_name: &str,
    rcs_proxy: &RemoteControlProxy,
    server_end: fidl::Channel,
) -> Result<()> {
    open_with_timeout_at(
        dur,
        moniker,
        OpenDirType::ExposedDir,
        capability_name,
        rcs_proxy,
        server_end,
    )
    .await
}

pub async fn connect_with_timeout<P: DiscoverableProtocolMarker>(
    dur: Duration,
    moniker: &str,
    rcs_proxy: &RemoteControlProxy,
    server_end: fidl::Channel,
) -> Result<()> {
    open_with_timeout_at(
        dur,
        moniker,
        OpenDirType::ExposedDir,
        P::PROTOCOL_NAME,
        rcs_proxy,
        server_end,
    )
    .await
}

pub async fn connect_to_protocol<P: DiscoverableProtocolMarker>(
    dur: Duration,
    moniker: &str,
    rcs_proxy: &RemoteControlProxy,
) -> Result<P::Proxy> {
    let (proxy, server_end) =
        fidl::endpoints::create_proxy::<P>().context("failed to create proxy")?;
    connect_with_timeout::<P>(dur, moniker, rcs_proxy, server_end.into_channel()).await?;
    Ok(proxy)
}

pub async fn open_with_timeout<P: DiscoverableProtocolMarker>(
    dur: Duration,
    moniker: &str,
    capability_set: OpenDirType,
    rcs_proxy: &RemoteControlProxy,
    server_end: fidl::Channel,
) -> Result<()> {
    open_with_timeout_at(dur, moniker, capability_set, P::PROTOCOL_NAME, rcs_proxy, server_end)
        .await
}

async fn get_cf_root_from_namespace<M: fidl::endpoints::DiscoverableProtocolMarker>(
    rcs_proxy: &RemoteControlProxy,
    timeout: Duration,
) -> Result<M::Proxy> {
    let (proxy, server_end) = fidl::endpoints::create_proxy::<M>()?;
    let start_time = Instant::now();
    let res = open_with_timeout_at(
        timeout,
        toolbox::MONIKER,
        OpenDirType::NamespaceDir,
        &format!("svc/{}.root", M::PROTOCOL_NAME),
        rcs_proxy,
        server_end.into_channel(),
    )
    .await;

    let proxy = if res.is_ok() {
        proxy
    } else {
        // Fallback to the legacy remote control moniker if toolbox doesn't contain the capability.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<M>()?;
        let timeout = timeout.saturating_sub(Instant::now() - start_time);
        open_with_timeout_at(
            timeout,
            REMOTE_CONTROL_MONIKER,
            OpenDirType::NamespaceDir,
            &format!("svc/{}.root", M::PROTOCOL_NAME),
            rcs_proxy,
            server_end.into_channel(),
        )
        .await?;
        proxy
    };
    Ok(proxy)
}

pub async fn kernel_stats(
    rcs_proxy: &RemoteControlProxy,
    timeout: Duration,
) -> Result<fidl_fuchsia_kernel::StatsProxy> {
    let (proxy, server_end) = fidl::endpoints::create_proxy::<fidl_fuchsia_kernel::StatsMarker>()?;
    let start_time = Instant::now();
    let res = open_with_timeout_at(
        timeout,
        toolbox::MONIKER,
        OpenDirType::NamespaceDir,
        &format!("svc/{}", fidl_fuchsia_kernel::StatsMarker::PROTOCOL_NAME),
        rcs_proxy,
        server_end.into_channel(),
    )
    .await;

    let proxy = if res.is_ok() {
        proxy
    } else {
        // Fallback to the legacy remote control moniker if toolbox doesn't contain the capability.
        let (proxy, server_end) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_kernel::StatsMarker>()?;
        let timeout = timeout.saturating_sub(Instant::now() - start_time);
        open_with_timeout_at(
            timeout,
            REMOTE_CONTROL_MONIKER,
            OpenDirType::NamespaceDir,
            &format!("svc/{}", fidl_fuchsia_kernel::StatsMarker::PROTOCOL_NAME),
            rcs_proxy,
            server_end.into_channel(),
        )
        .await?;
        proxy
    };
    Ok(proxy)
}

pub async fn root_config_override(
    rcs_proxy: &RemoteControlProxy,
    timeout: Duration,
) -> Result<ConfigOverrideProxy> {
    get_cf_root_from_namespace::<ConfigOverrideMarker>(rcs_proxy, timeout).await
}

pub async fn root_realm_query(
    rcs_proxy: &RemoteControlProxy,
    timeout: Duration,
) -> Result<RealmQueryProxy> {
    get_cf_root_from_namespace::<RealmQueryMarker>(rcs_proxy, timeout).await
}

pub async fn root_lifecycle_controller(
    rcs_proxy: &RemoteControlProxy,
    timeout: Duration,
) -> Result<LifecycleControllerProxy> {
    get_cf_root_from_namespace::<LifecycleControllerMarker>(rcs_proxy, timeout).await
}

pub async fn root_route_validator(
    rcs_proxy: &RemoteControlProxy,
    timeout: Duration,
) -> Result<RouteValidatorProxy> {
    get_cf_root_from_namespace::<RouteValidatorMarker>(rcs_proxy, timeout).await
}
