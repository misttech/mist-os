// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/76549): Replace with GN config once available in an ffx_plugin.
#![deny(unused_results)]

use anyhow::Context as _;
use errors::FfxError;
use ffx_core::ffx_plugin;
use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_developer_remotecontrol as fremotecontrol;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_net_debug as fdebug;
use fidl_fuchsia_net_dhcp as fdhcp;
use fidl_fuchsia_net_filter as ffilter;
use fidl_fuchsia_net_interfaces as finterfaces;
use fidl_fuchsia_net_name as fname;
use fidl_fuchsia_net_neighbor as fneighbor;
use fidl_fuchsia_net_root as froot;
use fidl_fuchsia_net_routes as froutes;
use fidl_fuchsia_net_stack as fstack;

const NETSTACK_MONIKER_SUFFIX: &str = "/netstack";
const DHCPD_MONIKER_SUFFIX: &str = "/dhcpd";
const DNS_MONIKER_SUFFIX: &str = "/dns-resolver";
const NETWORK_REALM: &str = "/core/network";

struct FfxConnector<'a> {
    remote_control: fremotecontrol::RemoteControlProxy,
    realm: &'a str,
}

impl FfxConnector<'_> {
    async fn remotecontrol_connect<S: ProtocolMarker>(
        &self,
        moniker_suffix: &str,
    ) -> Result<S::Proxy, anyhow::Error> {
        let Self { remote_control, realm } = &self;
        let mut moniker = format!("{}{}", realm, moniker_suffix);
        // TODO: remove once all clients of this tool are passing monikers instead of selectors for
        // `realm`.
        if !moniker.starts_with("/") {
            moniker = moniker.replace("\\:", ":");
            moniker = format!("/{moniker}");
        }
        let (proxy, server_end) = fidl::endpoints::create_proxy::<S>()
            .with_context(|| format!("failed to create proxy to {}", S::DEBUG_NAME))?;
        remote_control
            .connect_capability(
                &moniker,
                S::DEBUG_NAME,
                server_end.into_channel(),
                fio::OpenFlags::RIGHT_READABLE,
            )
            .await?
            .map_err(|e| {
                anyhow::anyhow!("failed to connect to {} at {}: {:?}", S::DEBUG_NAME, moniker, e)
            })?;
        Ok(proxy)
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fdebug::InterfacesMarker> for FfxConnector<'_> {
    async fn connect(
        &self,
    ) -> Result<<fdebug::InterfacesMarker as ProtocolMarker>::Proxy, anyhow::Error> {
        self.remotecontrol_connect::<fdebug::InterfacesMarker>(NETSTACK_MONIKER_SUFFIX).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<froot::InterfacesMarker> for FfxConnector<'_> {
    async fn connect(
        &self,
    ) -> Result<<froot::InterfacesMarker as ProtocolMarker>::Proxy, anyhow::Error> {
        self.remotecontrol_connect::<froot::InterfacesMarker>(NETSTACK_MONIKER_SUFFIX).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fdhcp::Server_Marker> for FfxConnector<'_> {
    async fn connect(
        &self,
    ) -> Result<<fdhcp::Server_Marker as ProtocolMarker>::Proxy, anyhow::Error> {
        self.remotecontrol_connect::<fdhcp::Server_Marker>(DHCPD_MONIKER_SUFFIX).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<ffilter::FilterMarker> for FfxConnector<'_> {
    async fn connect(
        &self,
    ) -> Result<<ffilter::FilterMarker as ProtocolMarker>::Proxy, anyhow::Error> {
        self.remotecontrol_connect::<ffilter::FilterMarker>(NETSTACK_MONIKER_SUFFIX).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<finterfaces::StateMarker> for FfxConnector<'_> {
    async fn connect(
        &self,
    ) -> Result<<finterfaces::StateMarker as ProtocolMarker>::Proxy, anyhow::Error> {
        self.remotecontrol_connect::<finterfaces::StateMarker>(NETSTACK_MONIKER_SUFFIX).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fneighbor::ControllerMarker> for FfxConnector<'_> {
    async fn connect(
        &self,
    ) -> Result<<fneighbor::ControllerMarker as ProtocolMarker>::Proxy, anyhow::Error> {
        self.remotecontrol_connect::<fneighbor::ControllerMarker>(NETSTACK_MONIKER_SUFFIX).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fneighbor::ViewMarker> for FfxConnector<'_> {
    async fn connect(
        &self,
    ) -> Result<<fneighbor::ViewMarker as ProtocolMarker>::Proxy, anyhow::Error> {
        self.remotecontrol_connect::<fneighbor::ViewMarker>(NETSTACK_MONIKER_SUFFIX).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fstack::LogMarker> for FfxConnector<'_> {
    async fn connect(&self) -> Result<<fstack::LogMarker as ProtocolMarker>::Proxy, anyhow::Error> {
        self.remotecontrol_connect::<fstack::LogMarker>(NETSTACK_MONIKER_SUFFIX).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fstack::StackMarker> for FfxConnector<'_> {
    async fn connect(
        &self,
    ) -> Result<<fstack::StackMarker as ProtocolMarker>::Proxy, anyhow::Error> {
        self.remotecontrol_connect::<fstack::StackMarker>(NETSTACK_MONIKER_SUFFIX).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<froutes::StateV4Marker> for FfxConnector<'_> {
    async fn connect(
        &self,
    ) -> Result<<froutes::StateV4Marker as ProtocolMarker>::Proxy, anyhow::Error> {
        self.remotecontrol_connect::<froutes::StateV4Marker>(NETSTACK_MONIKER_SUFFIX).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<froutes::StateV6Marker> for FfxConnector<'_> {
    async fn connect(
        &self,
    ) -> Result<<froutes::StateV6Marker as ProtocolMarker>::Proxy, anyhow::Error> {
        self.remotecontrol_connect::<froutes::StateV6Marker>(NETSTACK_MONIKER_SUFFIX).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fname::LookupMarker> for FfxConnector<'_> {
    async fn connect(
        &self,
    ) -> Result<<fname::LookupMarker as ProtocolMarker>::Proxy, anyhow::Error> {
        self.remotecontrol_connect::<fname::LookupMarker>(DNS_MONIKER_SUFFIX).await
    }
}

const EXIT_FAILURE: i32 = 1;

// TODO(121214): Fix incorrect- or invalid-type writer declarations
#[ffx_plugin()]
pub async fn net(
    remote_control: fremotecontrol::RemoteControlProxy,
    cmd: ffx_net_args::Command,
    #[ffx(machine = Vec<dyn serde::Serialize>)] writer: ffx_writer::Writer,
) -> Result<(), anyhow::Error> {
    let ffx_net_args::Command { cmd, realm } = cmd;
    let realm = realm.as_deref().unwrap_or(NETWORK_REALM);
    net_cli::do_root(writer, net_cli::Command { cmd }, &FfxConnector { remote_control, realm })
        .await
        .map_err(|e| match net_cli::underlying_user_facing_error(&e) {
            Some(net_cli::UserFacingError { msg }) => {
                FfxError::Error(anyhow::Error::msg(msg), EXIT_FAILURE).into()
            }
            None => e,
        })
}
