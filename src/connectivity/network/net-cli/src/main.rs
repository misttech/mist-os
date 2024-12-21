// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use component_debug::dirs::{connect_to_instance_protocol_at_dir_root, OpenDirType};
use fidl::endpoints::ProtocolMarker;
use fuchsia_component::client::connect_to_protocol_at_path;
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::io::Write;
use {
    fidl_fuchsia_net_debug as fdebug, fidl_fuchsia_net_dhcp as fdhcp,
    fidl_fuchsia_net_filter as ffilter, fidl_fuchsia_net_filter_deprecated as ffilter_deprecated,
    fidl_fuchsia_net_interfaces as finterfaces, fidl_fuchsia_net_name as fname,
    fidl_fuchsia_net_neighbor as fneighbor, fidl_fuchsia_net_root as froot,
    fidl_fuchsia_net_routes as froutes, fidl_fuchsia_net_stack as fstack,
    fidl_fuchsia_net_stackmigrationdeprecated as fnet_migration, fidl_fuchsia_sys2 as fsys,
    fuchsia_async as fasync,
};

const LOG_LEVEL: LevelFilter = LevelFilter::Info;

struct SimpleLogger;

impl Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= LOG_LEVEL
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            match record.level() {
                Level::Error | Level::Warn => {
                    let mut stderr = std::io::stderr();
                    let _ = writeln!(&mut stderr, "{}", record.args());
                }
                _ => {
                    let mut stdout = std::io::stdout();
                    let _ = writeln!(&mut stdout, "{}", record.args());
                }
            }
        }
    }

    fn flush(&self) {
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
}

fn logger_init() {
    static LOGGER: SimpleLogger = SimpleLogger;
    log::set_logger(&LOGGER).map(|_| log::set_max_level(LOG_LEVEL)).unwrap();
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
const MIGRATION_CONTROLLER_MONIKER: &str = "./core/network/netstack-migration";

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
impl net_cli::ServiceConnector<froot::FilterMarker> for Connector {
    async fn connect(&self) -> Result<<froot::FilterMarker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<froot::FilterMarker>(NETSTACK_MONIKER).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fdhcp::Server_Marker> for Connector {
    async fn connect(&self) -> Result<<fdhcp::Server_Marker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<fdhcp::Server_Marker>(DHCPD_MONIKER).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<ffilter_deprecated::FilterMarker> for Connector {
    async fn connect(
        &self,
    ) -> Result<<ffilter_deprecated::FilterMarker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<ffilter_deprecated::FilterMarker>(NETSTACK_MONIKER).await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<ffilter::StateMarker> for Connector {
    async fn connect(&self) -> Result<<ffilter::StateMarker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<ffilter::StateMarker>(NETSTACK_MONIKER).await
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

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fnet_migration::ControlMarker> for Connector {
    async fn connect(
        &self,
    ) -> Result<<fnet_migration::ControlMarker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<fnet_migration::ControlMarker>(
            MIGRATION_CONTROLLER_MONIKER,
        )
        .await
    }
}

#[async_trait::async_trait]
impl net_cli::ServiceConnector<fnet_migration::StateMarker> for Connector {
    async fn connect(
        &self,
    ) -> Result<<fnet_migration::StateMarker as ProtocolMarker>::Proxy, Error> {
        self.connect_to_exposed_protocol::<fnet_migration::StateMarker>(
            MIGRATION_CONTROLLER_MONIKER,
        )
        .await
    }
}

#[fasync::run_singlethreaded]
async fn main() -> Result<(), Error> {
    logger_init();
    let command: net_cli::Command = argh::from_env();
    let connector = Connector::new()?;
    net_cli::do_root(ffx_writer::MachineWriter::new(None), command, &connector).await
}
