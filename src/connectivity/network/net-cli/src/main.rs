// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use component_debug::dirs::{connect_to_instance_protocol_at_dir_root, OpenDirType};
use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_net_debug as fdebug;
use fidl_fuchsia_net_dhcp as fdhcp;
use fidl_fuchsia_net_filter as ffilter;
use fidl_fuchsia_net_interfaces as finterfaces;
use fidl_fuchsia_net_name as fname;
use fidl_fuchsia_net_neighbor as fneighbor;
use fidl_fuchsia_net_root as froot;
use fidl_fuchsia_net_routes as froutes;
use fidl_fuchsia_net_stack as fstack;
use fidl_fuchsia_sys2 as fsys;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol_at_path;
use tracing::{Level, Subscriber};
use tracing_subscriber::{
    fmt::{
        format::{self, FormatEvent, FormatFields},
        FmtContext,
    },
    prelude::*,
    registry::LookupSpan,
};

const LOG_LEVEL: Level = Level::INFO;

struct SimpleFormatter;

impl<S, N> FormatEvent<S, N> for SimpleFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

fn logger_init() {
    tracing_subscriber::fmt()
        .event_format(SimpleFormatter)
        .with_writer(
            std::io::stderr
                .with_max_level(Level::WARN)
                .or_else(std::io::stdout.with_max_level(LOG_LEVEL)),
        )
        .init()
}

struct Connector {
    realm_query: fsys::RealmQueryProxy,
}

impl Connector {
    pub fn new() -> Result<Self, Error> {
        let realm_query = connect_to_protocol_at_path::<fsys::RealmQueryMarker>(REALM_QUERY_PATH)?;
        Ok(Self { realm_query })
    }

    async fn connect_to_exposed_protocol<P: fidl::endpoints::DiscoverableProtocolMarker>(
        &self,
        moniker: &str,
    ) -> Result<P::Proxy, Error> {
        let moniker = moniker.try_into()?;
        let proxy = connect_to_instance_protocol_at_dir_root::<P>(
            &moniker,
            OpenDirType::Exposed,
            &self.realm_query,
        )
        .await?;
        Ok(proxy)
    }
}

const REALM_QUERY_PATH: &str = "/svc/fuchsia.sys2.RealmQuery.root";
const NETSTACK_MONIKER: &str = "./core/network/netstack";
const DHCPD_MONIKER: &str = "./core/network/dhcpd";
const DNS_RESOLVER_MONIKER: &str = "./core/network/dns-resolver";

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fdebug::InterfacesMarker> for Connector {
    async fn connect(&self) -> Result<<fdebug::InterfacesMarker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<fdebug::InterfacesMarker>(NETSTACK_MONIKER).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<froot::InterfacesMarker> for Connector {
    async fn connect(&self) -> Result<<froot::InterfacesMarker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<froot::InterfacesMarker>(NETSTACK_MONIKER).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fdhcp::Server_Marker> for Connector {
    async fn connect(&self) -> Result<<fdhcp::Server_Marker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<fdhcp::Server_Marker>(DHCPD_MONIKER).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<ffilter::FilterMarker> for Connector {
    async fn connect(&self) -> Result<<ffilter::FilterMarker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<ffilter::FilterMarker>(NETSTACK_MONIKER).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<finterfaces::StateMarker> for Connector {
    async fn connect(&self) -> Result<<finterfaces::StateMarker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<finterfaces::StateMarker>(NETSTACK_MONIKER).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fneighbor::ControllerMarker> for Connector {
    async fn connect(
        &self,
    ) -> Result<<fneighbor::ControllerMarker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<fneighbor::ControllerMarker>(NETSTACK_MONIKER).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fneighbor::ViewMarker> for Connector {
    async fn connect(&self) -> Result<<fneighbor::ViewMarker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<fneighbor::ViewMarker>(NETSTACK_MONIKER).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fstack::LogMarker> for Connector {
    async fn connect(&self) -> Result<<fstack::LogMarker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<fstack::LogMarker>(NETSTACK_MONIKER).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fstack::StackMarker> for Connector {
    async fn connect(&self) -> Result<<fstack::StackMarker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<fstack::StackMarker>(NETSTACK_MONIKER).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<froutes::StateV4Marker> for Connector {
    async fn connect(&self) -> Result<<froutes::StateV4Marker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<froutes::StateV4Marker>(NETSTACK_MONIKER).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<froutes::StateV6Marker> for Connector {
    async fn connect(&self) -> Result<<froutes::StateV6Marker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<froutes::StateV6Marker>(NETSTACK_MONIKER).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fname::LookupMarker> for Connector {
    async fn connect(&self) -> Result<<fname::LookupMarker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<fname::LookupMarker>(DNS_RESOLVER_MONIKER).await
    }
}

#[fasync::run_singlethreaded]
async fn main() -> Result<(), Error> {
    logger_init();
    let command: net_cli::Command = argh::from_env();
    let connector = Connector::new()?;
    net_cli::do_root(ffx_writer::Writer::new(None), command, &connector).await
}
