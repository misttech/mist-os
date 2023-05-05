// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod devices;
mod dhcpv4;
mod dhcpv6;
mod dns;
mod errors;
mod interface;
mod virtualization;

use ::dhcpv4::protocol::FromFidlExt as _;
use std::{
    boxed::Box,
    collections::{hash_map::Entry, HashMap, HashSet},
    fs, io,
    num::NonZeroU64,
    path,
    pin::Pin,
    str::FromStr,
};

use fidl::endpoints::{RequestStream as _, Responder as _};
use fidl_fuchsia_io as fio;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_dhcp as fnet_dhcp;
use fidl_fuchsia_net_dhcp_ext as fnet_dhcp_ext;
use fidl_fuchsia_net_dhcpv6 as fnet_dhcpv6;
use fidl_fuchsia_net_ext::{self as fnet_ext, DisplayExt as _, IpExt as _};
use fidl_fuchsia_net_filter as fnet_filter;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_interfaces_ext::{self as fnet_interfaces_ext, Update as _};
use fidl_fuchsia_net_name as fnet_name;
use fidl_fuchsia_net_stack as fnet_stack;
use fidl_fuchsia_net_virtualization as fnet_virtualization;
use fuchsia_async::DurationExt as _;
use fuchsia_component::client::{clone_namespace_svc, new_protocol_connector_in_dir};
use fuchsia_component::server::{ServiceFs, ServiceFsDir};
use fuchsia_fs::directory as fvfs_watcher;
use fuchsia_zircon::{self as zx, DurationNum as _};

use anyhow::{anyhow, bail, Context as _};
use async_trait::async_trait;
use async_utils::stream::{TryFlattenUnorderedExt as _, WithTag as _};
use dns_server_watcher::{DnsServers, DnsServersUpdateSource, DEFAULT_DNS_PORT};
use fuchsia_fs::OpenFlags;
use futures::{stream::BoxStream, StreamExt as _, TryFutureExt as _, TryStreamExt as _};
use net_declare::{fidl_ip_v4, net::prefix_length_v4};
use net_types::ip::{IpAddress as _, Ipv4, PrefixLength};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, trace, warn};

use self::devices::DeviceInfo;
use self::errors::{accept_error, ContextExt as _};

/// Interface metrics.
///
/// Interface metrics are used to sort the route table. An interface with a
/// lower metric is favored over one with a higher metric.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct Metric(u32);

impl Default for Metric {
    // A default value of 600 is chosen for Metric: this provides plenty of space for routing
    // policy on devices with many interfaces (physical or logical) while remaining in the same
    // magnitude as our current default ethernet metric.
    fn default() -> Self {
        Self(600)
    }
}

impl std::fmt::Display for Metric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Metric(u) = self;
        write!(f, "{}", u)
    }
}

impl From<Metric> for u32 {
    fn from(Metric(u): Metric) -> u32 {
        u
    }
}

/// File that stores persistent interface configurations.
const PERSISTED_INTERFACE_CONFIG_FILEPATH: &str = "/data/net_interfaces.cfg.json";

/// A node that represents the directory it is in.
///
/// `/dir` and `/dir/.` point to the same directory.
const THIS_DIRECTORY: &str = ".";

/// The prefix length for the address assigned to a WLAN AP interface.
const WLAN_AP_PREFIX_LEN: PrefixLength<Ipv4> = prefix_length_v4!(29);

/// The address for the network the WLAN AP interface is a part of.
const WLAN_AP_NETWORK_ADDR: fnet::Ipv4Address = fidl_ip_v4!("192.168.255.248");

/// The lease time for a DHCP lease.
///
/// 1 day in seconds.
const WLAN_AP_DHCP_LEASE_TIME_SECONDS: u32 = 24 * 60 * 60;

/// The maximum number of times to attempt to add a device.
const MAX_ADD_DEVICE_ATTEMPTS: u8 = 3;

/// A map of DNS server watcher streams that yields `DnsServerWatcherEvent` as DNS
/// server updates become available.
///
/// DNS server watcher streams may be added or removed at runtime as the watchers
/// are started or stopped.
type DnsServerWatchers<'a> = async_utils::stream::StreamMap<
    DnsServersUpdateSource,
    BoxStream<'a, (DnsServersUpdateSource, Result<Vec<fnet_name::DnsServer_>, fidl::Error>)>,
>;

/// Defines log levels.
#[derive(Debug, Copy, Clone)]
pub struct LogLevel(diagnostics_log::Severity);

impl Default for LogLevel {
    fn default() -> Self {
        Self(diagnostics_log::Severity::Info)
    }
}

impl FromStr for LogLevel {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, anyhow::Error> {
        match s.to_uppercase().as_str() {
            "TRACE" => Ok(Self(diagnostics_log::Severity::Trace)),
            "DEBUG" => Ok(Self(diagnostics_log::Severity::Debug)),
            "INFO" => Ok(Self(diagnostics_log::Severity::Info)),
            "WARN" => Ok(Self(diagnostics_log::Severity::Warn)),
            "ERROR" => Ok(Self(diagnostics_log::Severity::Error)),
            "FATAL" => Ok(Self(diagnostics_log::Severity::Fatal)),
            _ => Err(anyhow::anyhow!("unrecognized log level = {}", s)),
        }
    }
}

/// Network Configuration tool.
///
/// Configures network components in response to events.
#[derive(argh::FromArgs, Debug)]
struct Opt {
    /// minimum severity for logs
    #[argh(option, default = "Default::default()")]
    min_severity: LogLevel,

    /// config file to use
    #[argh(option, default = "\"/config/data/default.json\".to_string()")]
    config_data: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnsConfig {
    pub servers: Vec<std::net::IpAddr>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterConfig {
    pub rules: Vec<String>,
    pub nat_rules: Vec<String>,
    pub rdr_rules: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Hash, Copy, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
enum InterfaceType {
    Ethernet,
    Wlan,
}

#[cfg_attr(test, derive(PartialEq))]
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceMetrics {
    #[serde(default)]
    pub wlan_metric: Metric,
    #[serde(default)]
    pub eth_metric: Metric,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum DeviceClass {
    Virtual,
    Ethernet,
    Wlan,
    Ppp,
    Bridge,
    WlanAp,
}

impl From<fidl_fuchsia_hardware_network::DeviceClass> for DeviceClass {
    fn from(device_class: fidl_fuchsia_hardware_network::DeviceClass) -> Self {
        match device_class {
            fidl_fuchsia_hardware_network::DeviceClass::Virtual => DeviceClass::Virtual,
            fidl_fuchsia_hardware_network::DeviceClass::Ethernet => DeviceClass::Ethernet,
            fidl_fuchsia_hardware_network::DeviceClass::Wlan => DeviceClass::Wlan,
            fidl_fuchsia_hardware_network::DeviceClass::Ppp => DeviceClass::Ppp,
            fidl_fuchsia_hardware_network::DeviceClass::Bridge => DeviceClass::Bridge,
            fidl_fuchsia_hardware_network::DeviceClass::WlanAp => DeviceClass::WlanAp,
        }
    }
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(transparent)]
struct AllowedDeviceClasses(HashSet<DeviceClass>);

impl Default for AllowedDeviceClasses {
    fn default() -> Self {
        // When new variants are added, this exhaustive match will cause a compilation failure as a
        // reminder to add the new variant to the default array.
        match DeviceClass::Virtual {
            DeviceClass::Virtual
            | DeviceClass::Ethernet
            | DeviceClass::Wlan
            | DeviceClass::Ppp
            | DeviceClass::Bridge
            | DeviceClass::WlanAp => {}
        }
        Self(HashSet::from([
            DeviceClass::Virtual,
            DeviceClass::Ethernet,
            DeviceClass::Wlan,
            DeviceClass::Ppp,
            DeviceClass::Bridge,
            DeviceClass::WlanAp,
        ]))
    }
}

// TODO(https://github.com/serde-rs/serde/issues/368): use an inline literal for the default value
// rather than defining a one-off function.
fn dhcpv6_enabled_default() -> bool {
    true
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
struct ForwardedDeviceClasses {
    #[serde(default)]
    pub ipv4: HashSet<DeviceClass>,
    #[serde(default)]
    pub ipv6: HashSet<DeviceClass>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    pub dns_config: DnsConfig,
    pub filter_config: FilterConfig,
    pub filter_enabled_interface_types: HashSet<InterfaceType>,
    #[serde(default)]
    pub interface_metrics: InterfaceMetrics,
    #[serde(default)]
    pub allowed_upstream_device_classes: AllowedDeviceClasses,
    #[serde(default)]
    pub allowed_bridge_upstream_device_classes: AllowedDeviceClasses,
    // TODO(https://fxbug.dev/92096): default to false.
    #[serde(default = "dhcpv6_enabled_default")]
    pub enable_dhcpv6: bool,
    #[serde(default)]
    pub forwarded_device_classes: ForwardedDeviceClasses,
}

impl Config {
    pub fn load<P: AsRef<path::Path>>(path: P) -> Result<Self, anyhow::Error> {
        let path = path.as_ref();
        let file = fs::File::open(path)
            .with_context(|| format!("could not open the config file {}", path.display()))?;
        let config = serde_json::from_reader(io::BufReader::new(file))
            .with_context(|| format!("could not deserialize the config file {}", path.display()))?;
        Ok(config)
    }
}

#[derive(Clone, Debug)]
struct InterfaceConfig {
    name: String,
    metric: u32,
}

// We use Compare-And-Swap (CAS) protocol to update filter rules. $get_rules returns the current
// generation number. $update_rules will send it with new rules to make sure we are updating the
// intended generation. If the generation number doesn't match, $update_rules will return a
// GenerationMismatch error, then we have to restart from $get_rules.

const FILTER_CAS_RETRY_MAX: i32 = 3;
const FILTER_CAS_RETRY_INTERVAL_MILLIS: i64 = 500;

macro_rules! cas_filter_rules {
    ($filter:expr, $get_rules:ident, $update_rules:ident, $rules:expr, $error_type:ident) => {
        for retry in 0..FILTER_CAS_RETRY_MAX {
            let (_rules, generation) = $filter
                .$get_rules()
                .await
                .unwrap_or_else(|err| exit_with_fidl_error(err))
                .map_err(|e| anyhow!("{} failed: {:?}", stringify!($get_rules), e))?;

            match $filter
                .$update_rules(&$rules, generation)
                .await
                .unwrap_or_else(|err| exit_with_fidl_error(err))
            {
                Ok(()) => {
                    break;
                }
                Err(fnet_filter::$error_type::GenerationMismatch)
                    if retry < FILTER_CAS_RETRY_MAX - 1 =>
                {
                    fuchsia_async::Timer::new(
                        FILTER_CAS_RETRY_INTERVAL_MILLIS.millis().after_now(),
                    )
                    .await;
                }
                Err(e) => {
                    bail!("{} failed: {:?}", stringify!($update_rules), e);
                }
            }
        }
    };
}

// This is a placeholder macro while some update operations are not supported.
macro_rules! no_update_filter_rules {
    ($filter:expr, $get_rules:ident, $update_rules:ident, $rules:expr, $error_type:ident) => {
        let (_rules, generation) = $filter
            .$get_rules()
            .await
            .unwrap_or_else(|err| exit_with_fidl_error(err))
            .map_err(|e| anyhow!("{} failed: {:?}", stringify!($get_rules), e))?;

        match $filter
            .$update_rules(&$rules, generation)
            .await
            .unwrap_or_else(|err| exit_with_fidl_error(err))
        {
            Ok(()) => {}
            Err(fnet_filter::$error_type::NotSupported) => {
                error!("{} not supported", stringify!($update_rules));
            }
        }
    };
}

fn should_enable_filter(
    filter_enabled_interface_types: &HashSet<InterfaceType>,
    info: &DeviceInfo,
) -> bool {
    filter_enabled_interface_types.contains(&info.interface_type())
}

#[derive(Debug)]
struct InterfaceState {
    // Hold on to control to enforce interface ownership, even if unused.
    control: fidl_fuchsia_net_interfaces_ext::admin::Control,
    device_class: DeviceClass,
    config: InterfaceConfigState,
}

/// State for an interface.
#[derive(Debug)]
enum InterfaceConfigState {
    Host(HostInterfaceState),
    WlanAp(WlanApInterfaceState),
}

#[derive(Debug)]
struct HostInterfaceState {
    dhcpv4_client: Option<dhcpv4::ClientState>,
    dhcpv6_client_state: Option<dhcpv6::ClientState>,
    // The PD configuration to use for the DHCPv6 client on this interface.
    dhcpv6_pd_config: Option<fnet_dhcpv6::PrefixDelegationConfig>,
}

#[derive(Debug)]
struct WlanApInterfaceState {}

impl InterfaceState {
    fn new_host(
        control: fidl_fuchsia_net_interfaces_ext::admin::Control,
        device_class: DeviceClass,
        dhcpv6_pd_config: Option<fnet_dhcpv6::PrefixDelegationConfig>,
    ) -> Self {
        Self {
            control,
            config: InterfaceConfigState::Host(HostInterfaceState {
                dhcpv4_client: None,
                dhcpv6_client_state: None,
                dhcpv6_pd_config,
            }),
            device_class,
        }
    }

    fn new_wlan_ap(
        control: fidl_fuchsia_net_interfaces_ext::admin::Control,
        device_class: DeviceClass,
    ) -> Self {
        Self {
            control,
            device_class,
            config: InterfaceConfigState::WlanAp(WlanApInterfaceState {}),
        }
    }

    fn is_wlan_ap(&self) -> bool {
        let Self { config, control: _, device_class: _ } = self;
        match config {
            InterfaceConfigState::Host(_) => false,
            InterfaceConfigState::WlanAp(_) => true,
        }
    }

    /// Handles the interface being discovered.
    async fn on_discovery(
        &mut self,
        properties: &fnet_interfaces_ext::Properties,
        dhcpv4_client_provider: Option<&fnet_dhcp::ClientProviderProxy>,
        dhcpv6_client_provider: Option<&fnet_dhcpv6::ClientProviderProxy>,
        watchers: &mut DnsServerWatchers<'_>,
        dhcpv4_configuration_streams: &mut dhcpv4::ConfigurationStreamMap,
        dhcpv6_prefixes_streams: &mut dhcpv6::PrefixesStreamMap,
    ) -> Result<(), errors::Error> {
        let Self { config, control: _, device_class: _ } = self;
        let fnet_interfaces_ext::Properties { online, .. } = properties;
        match config {
            InterfaceConfigState::Host(HostInterfaceState {
                dhcpv4_client,
                dhcpv6_client_state,
                dhcpv6_pd_config,
            }) => {
                if !online {
                    return Ok(());
                }

                NetCfg::handle_dhcpv4_client_start(
                    properties.id,
                    &properties.name,
                    dhcpv4_client,
                    dhcpv4_client_provider,
                    dhcpv4_configuration_streams,
                );

                if let Some(dhcpv6_client_provider) = dhcpv6_client_provider {
                    let sockaddr = start_dhcpv6_client(
                        properties,
                        dhcpv6_client_provider,
                        dhcpv6_pd_config.clone(),
                        watchers,
                        dhcpv6_prefixes_streams,
                    )?;
                    *dhcpv6_client_state = sockaddr.map(dhcpv6::ClientState::new);
                }
            }
            InterfaceConfigState::WlanAp(WlanApInterfaceState {}) => {}
        }

        Ok(())
    }
}

/// Network Configuration state.
pub struct NetCfg<'a> {
    stack: fnet_stack::StackProxy,
    lookup_admin: fnet_name::LookupAdminProxy,
    filter: fnet_filter::FilterProxy,
    interface_state: fnet_interfaces::StateProxy,
    installer: fidl_fuchsia_net_interfaces_admin::InstallerProxy,
    dhcp_server: Option<fnet_dhcp::Server_Proxy>,
    dhcpv4_client_provider: Option<fnet_dhcp::ClientProviderProxy>,
    dhcpv6_client_provider: Option<fnet_dhcpv6::ClientProviderProxy>,

    persisted_interface_config: interface::FileBackedConfig<'a>,

    filter_enabled_interface_types: HashSet<InterfaceType>,

    // TODO(https://fxbug.dev/67407): These hashmaps are all indexed by
    // interface ID and store per-interface state, and should be merged.
    interface_states: HashMap<NonZeroU64, InterfaceState>,
    interface_properties: HashMap<NonZeroU64, fnet_interfaces_ext::Properties>,
    interface_metrics: InterfaceMetrics,

    dns_servers: DnsServers,

    forwarded_device_classes: ForwardedDeviceClasses,

    allowed_upstream_device_classes: &'a HashSet<DeviceClass>,

    dhcpv4_configuration_streams: dhcpv4::ConfigurationStreamMap,

    dhcpv6_prefix_provider_handler: Option<dhcpv6::PrefixProviderHandler>,
    dhcpv6_prefixes_streams: dhcpv6::PrefixesStreamMap,
}

/// Returns a [`fnet_name::DnsServer_`] with a static source from a [`std::net::IpAddr`].
fn static_source_from_ip(f: std::net::IpAddr) -> fnet_name::DnsServer_ {
    let socket_addr = match fnet_ext::IpAddress(f).into() {
        fnet::IpAddress::Ipv4(addr) => fnet::SocketAddress::Ipv4(fnet::Ipv4SocketAddress {
            address: addr,
            port: DEFAULT_DNS_PORT,
        }),
        fnet::IpAddress::Ipv6(addr) => fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
            address: addr,
            port: DEFAULT_DNS_PORT,
            zone_index: 0,
        }),
    };

    fnet_name::DnsServer_ {
        address: Some(socket_addr),
        source: Some(fnet_name::DnsServerSource::StaticSource(
            fnet_name::StaticDnsServerSource::default(),
        )),
        ..Default::default()
    }
}

/// Connect to a service, returning an error if the service does not exist in
/// the service directory.
async fn svc_connect<S: fidl::endpoints::DiscoverableProtocolMarker>(
    svc_dir: &fio::DirectoryProxy,
) -> Result<S::Proxy, anyhow::Error> {
    optional_svc_connect::<S>(svc_dir).await?.ok_or(anyhow::anyhow!("service does not exist"))
}

/// Attempt to connect to a service, returning `None` if the service does not
/// exist in the service directory.
async fn optional_svc_connect<S: fidl::endpoints::DiscoverableProtocolMarker>(
    svc_dir: &fio::DirectoryProxy,
) -> Result<Option<S::Proxy>, anyhow::Error> {
    let req = new_protocol_connector_in_dir::<S>(&svc_dir);
    if !req.exists().await.context("error checking for service existence")? {
        Ok(None)
    } else {
        req.connect().context("error connecting to service").map(Some)
    }
}

/// Start a DHCPv6 client if there is a unicast link-local IPv6 address in `addresses` to use as
/// the address.
fn start_dhcpv6_client(
    fnet_interfaces_ext::Properties {
        id,
        online,
        name,
        addresses,
        device_class: _,
        has_default_ipv4_route: _,
        has_default_ipv6_route: _,
    }: &fnet_interfaces_ext::Properties,
    dhcpv6_client_provider: &fnet_dhcpv6::ClientProviderProxy,
    pd_config: Option<fnet_dhcpv6::PrefixDelegationConfig>,
    watchers: &mut DnsServerWatchers<'_>,
    dhcpv6_prefixes_streams: &mut dhcpv6::PrefixesStreamMap,
) -> Result<Option<fnet::Ipv6SocketAddress>, errors::Error> {
    if !online {
        return Ok(None);
    }

    let sockaddr = if let Some(sockaddr) = addresses.iter().find_map(
        |&fnet_interfaces_ext::Address {
             addr: fnet::Subnet { addr, prefix_len: _ },
             valid_until: _,
         }| match addr {
            fnet::IpAddress::Ipv6(address) => {
                if address.is_unicast_link_local() {
                    Some(fnet::Ipv6SocketAddress {
                        address,
                        port: fnet_dhcpv6::DEFAULT_CLIENT_PORT,
                        zone_index: id.get(),
                    })
                } else {
                    None
                }
            }
            fnet::IpAddress::Ipv4(_) => None,
        },
    ) {
        sockaddr
    } else {
        return Ok(None);
    };

    if matches!(pd_config, Some(fnet_dhcpv6::PrefixDelegationConfig::Prefix(_))) {
        // We debug-log the `PrefixDelegationConfig` below. This is okay for
        // now because we do not use the prefix variant, but if we did we need
        // to support pretty-printing prefixes as it is considered PII and only
        // pretty-printed prefixes/addresses are properly redacted.
        todo!("http://fxbug.dev/117848: Support pretty-printing configured prefix");
    }

    let source = DnsServersUpdateSource::Dhcpv6 { interface_id: id.get() };
    assert!(
        !watchers.contains_key(&source) && !dhcpv6_prefixes_streams.contains_key(id),
        "interface with id={} already has a DHCPv6 client",
        id
    );

    let (dns_servers_stream, prefixes_stream) = dhcpv6::start_client(
        dhcpv6_client_provider,
        *id,
        sockaddr,
        pd_config.clone(),
    )
        .with_context(|| {
            format!(
                "failed to start DHCPv6 client on interface {} (id={}) w/ sockaddr {} and PD config {:?}",
                name,
                id,
                sockaddr.display_ext(),
                pd_config,
            )
        })?;
    if let Some(o) = watchers.insert(source, dns_servers_stream.tagged(source).boxed()) {
        let _: Pin<Box<BoxStream<'_, _>>> = o;
        unreachable!("DNS server watchers must not contain key {:?}", source);
    }
    if let Some(o) = dhcpv6_prefixes_streams.insert(*id, prefixes_stream.tagged(*id)) {
        let _: Pin<Box<dhcpv6::InterfaceIdTaggedPrefixesStream>> = o;
        unreachable!("DHCPv6 prefixes streams must not contain key {:?}", *id);
    }

    info!(
        "started DHCPv6 client on host interface {} (id={}) w/ sockaddr {} and PD config {:?}",
        name,
        id,
        sockaddr.display_ext(),
        pd_config,
    );

    Ok(Some(sockaddr))
}

impl<'a> NetCfg<'a> {
    async fn new(
        filter_enabled_interface_types: HashSet<InterfaceType>,
        interface_metrics: InterfaceMetrics,
        enable_dhcpv6: bool,
        forwarded_device_classes: ForwardedDeviceClasses,
        allowed_upstream_device_classes: &'a HashSet<DeviceClass>,
    ) -> Result<NetCfg<'a>, anyhow::Error> {
        let svc_dir = clone_namespace_svc().context("error cloning svc directory handle")?;
        let stack = svc_connect::<fnet_stack::StackMarker>(&svc_dir)
            .await
            .context("could not connect to stack")?;
        let lookup_admin = svc_connect::<fnet_name::LookupAdminMarker>(&svc_dir)
            .await
            .context("could not connect to lookup admin")?;
        let filter = svc_connect::<fnet_filter::FilterMarker>(&svc_dir)
            .await
            .context("could not connect to filter")?;
        let interface_state = svc_connect::<fnet_interfaces::StateMarker>(&svc_dir)
            .await
            .context("could not connect to interfaces state")?;
        let dhcp_server = optional_svc_connect::<fnet_dhcp::Server_Marker>(&svc_dir)
            .await
            .context("could not connect to DHCP Server")?;
        let dhcpv4_client_provider = {
            let provider = optional_svc_connect::<fnet_dhcp::ClientProviderMarker>(&svc_dir)
                .await
                .context("could not connect to DHCPv4 client provider")?;
            match provider {
                Some(provider) => dhcpv4::probe_for_presence(&provider).await.then_some(provider),
                None => None,
            }
        };
        let dhcpv6_client_provider = if enable_dhcpv6 {
            let dhcpv6_client_provider = svc_connect::<fnet_dhcpv6::ClientProviderMarker>(&svc_dir)
                .await
                .context("could not connect to DHCPv6 client provider")?;
            Some(dhcpv6_client_provider)
        } else {
            None
        };
        let installer = svc_connect::<fnet_interfaces_admin::InstallerMarker>(&svc_dir)
            .await
            .context("could not connect to installer")?;
        let persisted_interface_config =
            interface::FileBackedConfig::load(&PERSISTED_INTERFACE_CONFIG_FILEPATH)
                .context("error loading persistent interface configurations")?;

        Ok(NetCfg {
            stack,
            lookup_admin,
            filter,
            interface_state,
            dhcp_server,
            installer,
            dhcpv4_client_provider,
            dhcpv6_client_provider,
            persisted_interface_config,
            filter_enabled_interface_types,
            interface_properties: Default::default(),
            interface_states: Default::default(),
            interface_metrics,
            dns_servers: Default::default(),
            forwarded_device_classes,
            dhcpv4_configuration_streams: dhcpv4::ConfigurationStreamMap::empty(),
            dhcpv6_prefix_provider_handler: None,
            dhcpv6_prefixes_streams: dhcpv6::PrefixesStreamMap::empty(),
            allowed_upstream_device_classes,
        })
    }

    /// Updates the network filter configurations.
    async fn update_filters(&mut self, config: FilterConfig) -> Result<(), anyhow::Error> {
        let FilterConfig { rules, nat_rules, rdr_rules } = config;

        if !rules.is_empty() {
            let rules = netfilter::parser::parse_str_to_rules(&rules.join(""))
                .context("error parsing filter rules")?;
            cas_filter_rules!(self.filter, get_rules, update_rules, rules, FilterUpdateRulesError);
        }

        if !nat_rules.is_empty() {
            let nat_rules = netfilter::parser::parse_str_to_nat_rules(&nat_rules.join(""))
                .context("error parsing NAT rules")?;
            cas_filter_rules!(
                self.filter,
                get_nat_rules,
                update_nat_rules,
                nat_rules,
                FilterUpdateNatRulesError
            );
        }

        if !rdr_rules.is_empty() {
            let rdr_rules = netfilter::parser::parse_str_to_rdr_rules(&rdr_rules.join(""))
                .context("error parsing RDR rules")?;
            // TODO(https://fxbug.dev/68279): Change this to cas_filter_rules once update is supported.
            no_update_filter_rules!(
                self.filter,
                get_rdr_rules,
                update_rdr_rules,
                rdr_rules,
                FilterUpdateRdrRulesError
            );
        }

        Ok(())
    }

    /// Updates the DNS servers used by the DNS resolver.
    async fn update_dns_servers(
        &mut self,
        source: DnsServersUpdateSource,
        servers: Vec<fnet_name::DnsServer_>,
    ) {
        dns::update_servers(&self.lookup_admin, &mut self.dns_servers, source, servers).await
    }

    /// Handles the completion of the DNS server watcher associated with `source`.
    ///
    /// Clears the servers for `source` and removes the watcher from `dns_watchers`.
    async fn handle_dns_server_watcher_done(
        &mut self,
        source: DnsServersUpdateSource,
        dns_watchers: &mut DnsServerWatchers<'_>,
    ) -> Result<(), anyhow::Error> {
        match source {
            DnsServersUpdateSource::Default => {
                panic!("should not have a DNS server watcher for the default source");
            }
            DnsServersUpdateSource::Dhcpv4 { interface_id } => {
                unreachable!(
                    "DHCPv4 configurations are not obtained through DNS server watcher; \
                     interface_id={}",
                    interface_id,
                )
            }
            DnsServersUpdateSource::Netstack => Ok(()),
            DnsServersUpdateSource::Dhcpv6 { interface_id } => {
                let interface_id = interface_id.try_into().expect("should be nonzero");
                let InterfaceState { config, control: _, device_class: _ } = self
                    .interface_states
                    .get_mut(&interface_id)
                    .unwrap_or_else(|| panic!("no interface state found for id={}", interface_id));

                match config {
                    InterfaceConfigState::Host(HostInterfaceState {
                        dhcpv4_client: _,
                        dhcpv6_client_state,
                        dhcpv6_pd_config: _,
                    }) => {
                        let _: dhcpv6::ClientState =
                            dhcpv6_client_state.take().unwrap_or_else(|| {
                                panic!(
                                    "DHCPv6 was not being performed on host interface with id={}",
                                    interface_id
                                )
                            });

                        // If the DNS server watcher is done, that means the server-end
                        // of the channel is closed meaning DHCPv6 has been stopped.
                        // Perform the cleanup for our end of the DHCPv6 client
                        // (which will update DNS servers) and send an prefix update to any
                        // blocked watchers of fuchsia.net.dhcpv6/PrefixControl.WatchPrefix.
                        dhcpv6::stop_client(
                            &self.lookup_admin,
                            &mut self.dns_servers,
                            interface_id,
                            dns_watchers,
                            &mut self.dhcpv6_prefixes_streams,
                        )
                        .await;

                        dhcpv6::maybe_send_watch_prefix_response(
                            &self.interface_states,
                            &self.allowed_upstream_device_classes,
                            self.dhcpv6_prefix_provider_handler.as_mut(),
                        )
                    }
                    InterfaceConfigState::WlanAp(WlanApInterfaceState {}) => {
                        panic!(
                            "should not have a DNS watcher for a WLAN AP interface with id={}",
                            interface_id
                        );
                    }
                }
            }
        }
    }

    /// Run the network configuration eventloop.
    ///
    /// The device directory will be monitored for device events and the netstack will be
    /// configured with a new interface on new device discovery.
    async fn run(
        &mut self,
        mut virtualization_handler: impl virtualization::Handler,
    ) -> Result<(), anyhow::Error> {
        let netdev_stream =
            self.create_device_stream().await.context("create netdevice stream")?.fuse();
        futures::pin_mut!(netdev_stream);

        let if_watcher_event_stream =
            fnet_interfaces_ext::event_stream_from_state(&self.interface_state)
                .context("error creating interface watcher event stream")?
                .fuse();
        futures::pin_mut!(if_watcher_event_stream);

        let (dns_server_watcher, dns_server_watcher_req) =
            fidl::endpoints::create_proxy::<fnet_name::DnsServerWatcherMarker>()
                .context("error creating dns server watcher")?;
        let () = self
            .stack
            .get_dns_server_watcher(dns_server_watcher_req)
            .context("get dns server watcher")?;
        let netstack_dns_server_stream = dns_server_watcher::new_dns_server_stream(
            DnsServersUpdateSource::Netstack,
            dns_server_watcher,
        )
        .boxed();

        let dns_watchers = DnsServerWatchers::empty();
        // `Fuse` (the return of `fuse`) guarantees that once the underlying stream is
        // exhausted, future attempts to poll the stream will return `None`. This would
        // be undesirable if we needed to support a scenario where all streams are
        // exhausted before adding a new stream to the `StreamMap`. However,
        // `netstack_dns_server_stream` is not expected to end so we can fuse the
        // `StreamMap` without issue.
        let mut dns_watchers = dns_watchers.fuse();
        assert!(
            dns_watchers
                .get_mut()
                .insert(DnsServersUpdateSource::Netstack, netstack_dns_server_stream)
                .is_none(),
            "dns watchers should be empty"
        );

        enum RequestStream {
            Virtualization(fnet_virtualization::ControlRequestStream),
            Dhcpv6PrefixProvider(fnet_dhcpv6::PrefixProviderRequestStream),
        }

        // Serve fuchsia.net.virtualization/Control.
        let mut fs = ServiceFs::new_local();
        let _: &mut ServiceFsDir<'_, _> =
            fs.dir("svc").add_fidl_service(RequestStream::Virtualization);
        let _: &mut ServiceFsDir<'_, _> =
            fs.dir("svc").add_fidl_service(RequestStream::Dhcpv6PrefixProvider);
        let _: &mut ServiceFs<_> =
            fs.take_and_serve_directory_handle().context("take and serve directory handle")?;
        let mut fs = fs.fuse();

        let mut dhcpv6_prefix_provider_requests = futures::stream::SelectAll::new();

        // Maintain a queue of virtualization events to be dispatched to the virtualization handler.
        let mut virtualization_events = futures::stream::SelectAll::new();

        // Lifecycle handle takes no args, must be set to zero.
        // See zircon/processargs.h.
        const LIFECYCLE_HANDLE_ARG: u16 = 0;
        let lifecycle = fuchsia_runtime::take_startup_handle(fuchsia_runtime::HandleInfo::new(
            fuchsia_runtime::HandleType::Lifecycle,
            LIFECYCLE_HANDLE_ARG,
        ))
        .ok_or_else(|| anyhow::anyhow!("lifecycle handle not present"))?;
        let lifecycle = fuchsia_async::Channel::from_channel(lifecycle.into())
            .context("lifecycle async channel")?;
        let mut lifecycle = fidl_fuchsia_process_lifecycle::LifecycleRequestStream::from_channel(
            fidl::AsyncChannel::from(lifecycle),
        );

        debug!("starting eventloop...");

        enum Event {
            NetworkDeviceResult(Result<Option<devices::NetworkDeviceInstance>, anyhow::Error>),
            InterfaceWatcherResult(Result<Option<fidl_fuchsia_net_interfaces::Event>, fidl::Error>),
            DnsWatcherResult(
                Option<(
                    dns_server_watcher::DnsServersUpdateSource,
                    Result<Vec<fnet_name::DnsServer_>, fidl::Error>,
                )>,
            ),
            RequestStream(Option<RequestStream>),
            Dhcpv4Configuration(
                Option<(NonZeroU64, Result<fnet_dhcp_ext::Configuration, fnet_dhcp_ext::Error>)>,
            ),
            Dhcpv6PrefixProviderRequest(Result<fnet_dhcpv6::PrefixProviderRequest, fidl::Error>),
            Dhcpv6PrefixControlRequest(
                Option<Result<Option<fnet_dhcpv6::PrefixControlRequest>, fidl::Error>>,
            ),
            Dhcpv6Prefixes(Option<(NonZeroU64, Result<Vec<fnet_dhcpv6::Prefix>, fidl::Error>)>),
            VirtualizationEvent(virtualization::Event),
            LifecycleRequest(
                Result<Option<fidl_fuchsia_process_lifecycle::LifecycleRequest>, fidl::Error>,
            ),
        }
        loop {
            let mut dhcpv6_prefix_control_fut = futures::future::OptionFuture::from(
                self.dhcpv6_prefix_provider_handler
                    .as_mut()
                    .map(dhcpv6::PrefixProviderHandler::try_next_prefix_control_request),
            );
            let event = futures::select! {
                netdev_res = netdev_stream.try_next() => {
                    Event::NetworkDeviceResult(netdev_res)
                }
                if_watcher_res = if_watcher_event_stream.try_next() => {
                    Event::InterfaceWatcherResult(if_watcher_res)
                }
                dns_watchers_res = dns_watchers.next() => {
                    Event::DnsWatcherResult(dns_watchers_res)
                }
                req_stream = fs.next() => {
                    Event::RequestStream(req_stream)
                }
                dhcpv4_configuration = self.dhcpv4_configuration_streams.next() => {
                    Event::Dhcpv4Configuration(dhcpv4_configuration)
                }
                dhcpv6_prefix_req = dhcpv6_prefix_provider_requests.select_next_some() => {
                    Event::Dhcpv6PrefixProviderRequest(dhcpv6_prefix_req)
                }
                dhcpv6_prefix_control_req = dhcpv6_prefix_control_fut => {
                    Event::Dhcpv6PrefixControlRequest(dhcpv6_prefix_control_req)
                }
                dhcpv6_prefixes = self.dhcpv6_prefixes_streams.next() => {
                    Event::Dhcpv6Prefixes(dhcpv6_prefixes)
                }
                virt_event = virtualization_events.select_next_some() => {
                    Event::VirtualizationEvent(virt_event)
                }
                req = lifecycle.try_next() => {
                    Event::LifecycleRequest(req)
                }
                complete => return Err(anyhow::anyhow!("eventloop ended unexpectedly")),
            };
            match event {
                Event::NetworkDeviceResult(netdev_res) => {
                    let instance =
                        netdev_res.context("error retrieving netdev instance")?.ok_or_else(
                            || anyhow::anyhow!("netdev instance watcher stream ended unexpectedly"),
                        )?;
                    self.handle_device_instance(instance).await.context("handle netdev instance")?
                }
                Event::InterfaceWatcherResult(if_watcher_res) => {
                    let event = if_watcher_res
                        .unwrap_or_else(|err| exit_with_fidl_error(err))
                        .expect("watcher stream never returns None");
                    trace!("got interfaces watcher event = {:?}", event);

                    self.handle_interface_watcher_event(
                        event,
                        dns_watchers.get_mut(),
                        &mut virtualization_handler,
                    )
                    .await
                    .context("handle interface watcher event")?
                }
                Event::DnsWatcherResult(dns_watchers_res) => {
                    let (source, res) = dns_watchers_res
                        .ok_or(anyhow::anyhow!("dns watchers stream should never be exhausted"))?;
                    let servers = match res {
                        Ok(s) => s,
                        Err(e) => {
                            // TODO(fxbug.dev/57484): Restart the DNS server watcher.
                            warn!(
                                "non-fatal error getting next event from DNS server watcher stream
                                with source = {:?}: {:?}",
                                source, e
                            );
                            let () = self
                                .handle_dns_server_watcher_done(source, dns_watchers.get_mut())
                                .await
                                .with_context(|| {
                                    format!(
                                        "error handling completion of DNS server watcher for \
                                        {:?}",
                                        source
                                    )
                                })?;
                            continue;
                        }
                    };

                    self.update_dns_servers(source, servers).await
                }
                Event::RequestStream(req_stream) => {
                    match req_stream.context("ServiceFs ended unexpectedly")? {
                        RequestStream::Virtualization(req_stream) => virtualization_handler
                            .handle_event(
                                virtualization::Event::ControlRequestStream(req_stream),
                                &mut virtualization_events,
                            )
                            .await
                            .context("handle virtualization event")
                            .or_else(errors::Error::accept_non_fatal)?,
                        RequestStream::Dhcpv6PrefixProvider(req_stream) => {
                            dhcpv6_prefix_provider_requests.push(req_stream);
                        }
                    }
                }
                Event::Dhcpv4Configuration(config) => {
                    let (interface_id, config) =
                        config.expect("DHCPv4 configuration stream is never exhausted");
                    self.handle_dhcpv4_configuration(interface_id, config).await
                }
                Event::Dhcpv6PrefixProviderRequest(res) => {
                    match res {
                        Ok(fnet_dhcpv6::PrefixProviderRequest::AcquirePrefix {
                            config,
                            prefix,
                            control_handle: _,
                        }) => {
                            self.handle_dhcpv6_acquire_prefix(
                                config,
                                prefix,
                                dns_watchers.get_mut(),
                            )
                            .await
                            .or_else(errors::Error::accept_non_fatal)?;
                        }
                        Err(e) => {
                            error!("fuchsia.net.dhcpv6/PrefixProvider request error: {:?}", e)
                        }
                    };
                }
                Event::Dhcpv6PrefixControlRequest(req) => {
                    let res = req.context(
                        "PrefixControl OptionFuture will only be selected if it is not None",
                    )?;
                    match res {
                        Err(e) => {
                            error!(
                                "fuchsia.net.dhcpv6/PrefixControl request stream error: {:?}",
                                e
                            );
                            self.on_dhcpv6_prefix_control_close(dns_watchers.get_mut()).await;
                        }
                        Ok(None) => {
                            info!("fuchsia.net.dhcpv6/PrefixControl closed by client");
                            self.on_dhcpv6_prefix_control_close(dns_watchers.get_mut()).await;
                        }
                        Ok(Some(fnet_dhcpv6::PrefixControlRequest::WatchPrefix { responder })) => {
                            self.handle_watch_prefix(responder, dns_watchers.get_mut())
                                .await
                                .context("handle PrefixControl.WatchPrefix")
                                .unwrap_or_else(accept_error);
                        }
                    }
                }
                Event::Dhcpv6Prefixes(prefixes) => {
                    let (interface_id, res) = prefixes
                        .context("DHCPv6 watch prefixes stream map can never be exhausted")?;
                    self.handle_dhcpv6_prefixes(interface_id, res, dns_watchers.get_mut())
                        .await
                        .unwrap_or_else(accept_error);
                }
                Event::VirtualizationEvent(event) => virtualization_handler
                    .handle_event(event, &mut virtualization_events)
                    .await
                    .context("handle virtualization event")
                    .or_else(errors::Error::accept_non_fatal)?,
                Event::LifecycleRequest(req) => {
                    let req = req.context("lifecycle request")?.ok_or_else(|| {
                        anyhow::anyhow!("LifecycleRequestStream ended unexpectedly")
                    })?;
                    match req {
                        fidl_fuchsia_process_lifecycle::LifecycleRequest::Stop {
                            control_handle,
                        } => {
                            info!("received shutdown request");
                            // Shutdown request is acknowledged by the lifecycle
                            // channel shutting down. Intentionally leak the
                            // channel so it'll only be closed on process
                            // termination, allowing clean process termination
                            // to always be observed.

                            // Must drop the control_handle to unwrap the
                            // lifecycle channel.
                            std::mem::drop(control_handle);
                            let (inner, _terminated): (_, bool) = lifecycle.into_inner();
                            let inner = std::sync::Arc::try_unwrap(inner).map_err(
                                |_: std::sync::Arc<_>| {
                                    anyhow::anyhow!("failed to retrieve lifecycle channel")
                                },
                            )?;
                            let inner: zx::Channel = inner.into_channel().into_zx_channel();
                            std::mem::forget(inner);

                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    async fn handle_dhcpv4_client_stop(
        id: NonZeroU64,
        name: &str,
        dhcpv4_client: &mut Option<dhcpv4::ClientState>,
        configuration_streams: &mut dhcpv4::ConfigurationStreamMap,
        dns_servers: &mut DnsServers,
        control: &fnet_interfaces_ext::admin::Control,
        stack: &fnet_stack::StackProxy,
        lookup_admin: &fnet_name::LookupAdminProxy,
    ) {
        if let Some(fut) = dhcpv4_client.take().map(|c| {
            dhcpv4::stop_client(
                id,
                name,
                c,
                configuration_streams,
                dns_servers,
                control,
                stack,
                lookup_admin,
            )
        }) {
            fut.await
        }
    }

    fn handle_dhcpv4_client_start(
        id: NonZeroU64,
        name: &str,
        dhcpv4_client: &mut Option<dhcpv4::ClientState>,
        dhcpv4_client_provider: Option<&fnet_dhcp::ClientProviderProxy>,
        configuration_streams: &mut dhcpv4::ConfigurationStreamMap,
    ) {
        *dhcpv4_client = dhcpv4_client_provider
            .map(|p| dhcpv4::start_client(id, name, p, configuration_streams));
    }

    async fn handle_dhcpv4_client_update(
        id: NonZeroU64,
        name: &str,
        online: bool,
        dhcpv4_client: &mut Option<dhcpv4::ClientState>,
        dhcpv4_client_provider: Option<&fnet_dhcp::ClientProviderProxy>,
        configuration_streams: &mut dhcpv4::ConfigurationStreamMap,
        dns_servers: &mut DnsServers,
        control: &fnet_interfaces_ext::admin::Control,
        stack: &fnet_stack::StackProxy,
        lookup_admin: &fnet_name::LookupAdminProxy,
    ) {
        if online {
            Self::handle_dhcpv4_client_start(
                id,
                name,
                dhcpv4_client,
                dhcpv4_client_provider,
                configuration_streams,
            )
        } else {
            Self::handle_dhcpv4_client_stop(
                id,
                name,
                dhcpv4_client,
                configuration_streams,
                dns_servers,
                control,
                stack,
                lookup_admin,
            )
            .await
        }
    }

    /// Handles an interface watcher event (existing, added, changed, or removed).
    async fn handle_interface_watcher_event(
        &mut self,
        event: fnet_interfaces::Event,
        watchers: &mut DnsServerWatchers<'_>,
        virtualization_handler: &mut impl virtualization::Handler,
    ) -> Result<(), anyhow::Error> {
        let Self {
            stack,
            interface_properties,
            dns_servers,
            interface_states,
            lookup_admin,
            dhcp_server,
            dhcpv4_client_provider,
            dhcpv6_client_provider,
            dhcpv4_configuration_streams,
            dhcpv6_prefixes_streams,
            allowed_upstream_device_classes,
            dhcpv6_prefix_provider_handler,
            ..
        } = self;
        let update_result = interface_properties
            .update(event)
            .context("failed to update interface properties with watcher event")?;
        Self::handle_interface_update_result(
            &update_result,
            watchers,
            dns_servers,
            interface_states,
            dhcpv4_configuration_streams,
            dhcpv6_prefixes_streams,
            stack,
            lookup_admin,
            dhcp_server,
            dhcpv4_client_provider,
            dhcpv6_client_provider,
        )
        .await
        .context("handle interface update")
        .or_else(errors::Error::accept_non_fatal)?;

        // The interface watcher event may have disabled DHCPv6 on the interface
        // so respond accordingly.
        dhcpv6::maybe_send_watch_prefix_response(
            interface_states,
            allowed_upstream_device_classes,
            dhcpv6_prefix_provider_handler.as_mut(),
        )
        .context("maybe send PrefixControl.WatchPrefix response")?;

        virtualization_handler
            .handle_interface_update_result(&update_result)
            .await
            .context("handle interface update for virtualization")
            .or_else(errors::Error::accept_non_fatal)
    }

    // This method takes mutable references to several fields of `NetCfg` separately as parameters,
    // rather than `&mut self` directly, because `update_result` already holds a reference into
    // `self.interface_properties`.
    async fn handle_interface_update_result(
        update_result: &fnet_interfaces_ext::UpdateResult<'_>,
        watchers: &mut DnsServerWatchers<'_>,
        dns_servers: &mut DnsServers,
        interface_states: &mut HashMap<NonZeroU64, InterfaceState>,
        dhcpv4_configuration_streams: &mut dhcpv4::ConfigurationStreamMap,
        dhcpv6_prefixes_streams: &mut dhcpv6::PrefixesStreamMap,
        stack: &fnet_stack::StackProxy,
        lookup_admin: &fnet_name::LookupAdminProxy,
        dhcp_server: &Option<fnet_dhcp::Server_Proxy>,
        dhcpv4_client_provider: &Option<fnet_dhcp::ClientProviderProxy>,
        dhcpv6_client_provider: &Option<fnet_dhcpv6::ClientProviderProxy>,
    ) -> Result<(), errors::Error> {
        match update_result {
            fnet_interfaces_ext::UpdateResult::Added(properties) => {
                match interface_states.get_mut(&properties.id) {
                    Some(state) => state
                        .on_discovery(
                            properties,
                            dhcpv4_client_provider.as_ref(),
                            dhcpv6_client_provider.as_ref(),
                            watchers,
                            dhcpv4_configuration_streams,
                            dhcpv6_prefixes_streams,
                        )
                        .await
                        .context("failed to handle interface added event"),
                    // An interface netcfg won't be configuring was added, do nothing.
                    None => Ok(()),
                }
            }
            fnet_interfaces_ext::UpdateResult::Existing(properties) => {
                match interface_states.get_mut(&properties.id) {
                    Some(state) => state
                        .on_discovery(
                            properties,
                            dhcpv4_client_provider.as_ref(),
                            dhcpv6_client_provider.as_ref(),
                            watchers,
                            dhcpv4_configuration_streams,
                            dhcpv6_prefixes_streams,
                        )
                        .await
                        .context("failed to handle existing interface event"),
                    // An interface netcfg won't be configuring was discovered, do nothing.
                    None => Ok(()),
                }
            }
            fnet_interfaces_ext::UpdateResult::Changed {
                previous: fnet_interfaces::Properties { online: previous_online, .. },
                current: current_properties,
            } => {
                let &fnet_interfaces_ext::Properties {
                    id, ref name, online, ref addresses, ..
                } = current_properties;
                match interface_states.get_mut(&id) {
                    // An interface netcfg is not configuring was changed, do nothing.
                    None => return Ok(()),
                    Some(InterfaceState {
                        config:
                            InterfaceConfigState::Host(HostInterfaceState {
                                dhcpv4_client,
                                dhcpv6_client_state,
                                dhcpv6_pd_config,
                            }),
                        control,
                        device_class: _,
                    }) => {
                        if previous_online.is_some() {
                            Self::handle_dhcpv4_client_update(
                                *id,
                                name,
                                *online,
                                dhcpv4_client,
                                dhcpv4_client_provider.as_ref(),
                                dhcpv4_configuration_streams,
                                dns_servers,
                                control,
                                stack,
                                lookup_admin,
                            )
                            .await;
                        }

                        let dhcpv6_client_provider =
                            if let Some(dhcpv6_client_provider) = dhcpv6_client_provider {
                                dhcpv6_client_provider
                            } else {
                                return Ok(());
                            };

                        // Stop DHCPv6 client if interface went down.
                        if !online {
                            let dhcpv6::ClientState { sockaddr, prefixes: _ } =
                                match dhcpv6_client_state.take() {
                                    Some(s) => s,
                                    None => return Ok(()),
                                };

                            info!(
                                "host interface {} (id={}) went down \
                                so stopping DHCPv6 client w/ sockaddr = {}",
                                name,
                                id,
                                sockaddr.display_ext(),
                            );

                            return Ok(dhcpv6::stop_client(
                                &lookup_admin,
                                dns_servers,
                                *id,
                                watchers,
                                dhcpv6_prefixes_streams,
                            )
                            .await);
                        }

                        // Stop the DHCPv6 client if its address can no longer be found on the
                        // interface.
                        if let Some(dhcpv6::ClientState { sockaddr, prefixes: _ }) =
                            dhcpv6_client_state
                        {
                            let &mut fnet::Ipv6SocketAddress { address, port: _, zone_index: _ } =
                                sockaddr;
                            if !addresses.iter().any(
                                |&fnet_interfaces_ext::Address {
                                     addr: fnet::Subnet { addr, prefix_len: _ },
                                     valid_until: _,
                                 }| {
                                    addr == fnet::IpAddress::Ipv6(address)
                                },
                            ) {
                                let sockaddr = *sockaddr;
                                *dhcpv6_client_state = None;

                                info!(
                                    "stopping DHCPv6 client on host interface {} (id={}) \
                                    w/ removed sockaddr = {}",
                                    name,
                                    id,
                                    sockaddr.display_ext(),
                                );

                                let () = dhcpv6::stop_client(
                                    &lookup_admin,
                                    dns_servers,
                                    *id,
                                    watchers,
                                    dhcpv6_prefixes_streams,
                                )
                                .await;
                            }
                        }

                        // Start a DHCPv6 client if there isn't one.
                        if dhcpv6_client_state.is_none() {
                            let sockaddr = start_dhcpv6_client(
                                current_properties,
                                &dhcpv6_client_provider,
                                dhcpv6_pd_config.clone(),
                                watchers,
                                dhcpv6_prefixes_streams,
                            )?;
                            *dhcpv6_client_state = sockaddr.map(dhcpv6::ClientState::new);
                        }
                        Ok(())
                    }
                    Some(InterfaceState {
                        config: InterfaceConfigState::WlanAp(WlanApInterfaceState {}),
                        control: _,
                        device_class: _,
                    }) => {
                        // TODO(fxbug.dev/55879): Stop the DHCP server when the address it is
                        // listening on is removed.
                        let dhcp_server = if let Some(dhcp_server) = dhcp_server {
                            dhcp_server
                        } else {
                            return Ok(());
                        };

                        if previous_online
                            .map_or(true, |previous_online| previous_online == *online)
                        {
                            return Ok(());
                        }

                        if *online {
                            info!(
                                "WLAN AP interface {} (id={}) came up so starting DHCP server",
                                name, id
                            );
                            dhcpv4::start_server(&dhcp_server)
                                .await
                                .context("error starting DHCP server")
                        } else {
                            info!(
                                "WLAN AP interface {} (id={}) went down so stopping DHCP server",
                                name, id
                            );
                            dhcpv4::stop_server(&dhcp_server)
                                .await
                                .context("error stopping DHCP server")
                        }
                    }
                }
            }
            fnet_interfaces_ext::UpdateResult::Removed(fnet_interfaces_ext::Properties {
                id,
                name,
                ..
            }) => {
                match interface_states.remove(&id) {
                    // An interface netcfg was not responsible for configuring was removed, do
                    // nothing.
                    None => Ok(()),
                    Some(InterfaceState { config, control, device_class: _ }) => {
                        match config {
                            InterfaceConfigState::Host(HostInterfaceState {
                                mut dhcpv4_client,
                                mut dhcpv6_client_state,
                                dhcpv6_pd_config: _,
                            }) => {
                                Self::handle_dhcpv4_client_stop(
                                    *id,
                                    name,
                                    &mut dhcpv4_client,
                                    dhcpv4_configuration_streams,
                                    dns_servers,
                                    &control,
                                    stack,
                                    lookup_admin,
                                )
                                    .await;


                                let dhcpv6::ClientState {
                                    sockaddr,
                                    prefixes: _,
                                } = match dhcpv6_client_state.take() {
                                    Some(s) => s,
                                    None => return Ok(()),
                                };

                                info!(
                                    "host interface {} (id={}) removed \
                                    so stopping DHCPv6 client w/ sockaddr = {}",
                                    name,
                                    id,
                                    sockaddr.display_ext()
                                );

                                Ok(dhcpv6::stop_client(
                                    &lookup_admin,
                                    dns_servers,
                                    *id,
                                    watchers,
                                    dhcpv6_prefixes_streams,
                                )
                                .await)
                            }
                            InterfaceConfigState::WlanAp(WlanApInterfaceState {}) => {
                                if let Some(dhcp_server) = dhcp_server {
                                    // The DHCP server should only run on the WLAN AP interface, so stop it
                                    // since the AP interface is removed.
                                    info!(
                                        "WLAN AP interface {} (id={}) is removed, stopping DHCP server",
                                        name, id
                                    );
                                    dhcpv4::stop_server(&dhcp_server)
                                        .await
                                        .context("error stopping DHCP server")
                                } else {
                                    Ok(())
                                }
                            }
                        }
                        .context("failed to handle interface removed event")
                    }
                }
            }
            fnet_interfaces_ext::UpdateResult::NoChange => Ok(()),
        }
    }

    /// Handle an event from `D`'s device directory.
    async fn create_device_stream(
        &self,
    ) -> Result<
        impl futures::Stream<Item = Result<devices::NetworkDeviceInstance, anyhow::Error>>,
        anyhow::Error,
    > {
        let installer = self.installer.clone();
        let directory = fuchsia_fs::directory::open_in_namespace(
            devices::NetworkDeviceInstance::PATH,
            OpenFlags::RIGHT_READABLE,
        )
        .with_context(|| format!("error opening netdevice directory"))?;
        let stream_of_streams = fvfs_watcher::Watcher::new(&directory)
            .await
            .with_context(|| {
                format!("creating watcher for {}", devices::NetworkDeviceInstance::PATH)
            })?
            .err_into()
            .try_filter_map(move |fvfs_watcher::WatchMessage { event, filename }| {
                let installer = installer.clone();
                async move {
                    trace!("got {:?} event for {}", event, filename.display());

                    if filename == path::PathBuf::from(THIS_DIRECTORY) {
                        debug!("skipping device w/ filename = {}", filename.display());
                        return Ok(None);
                    }

                    match event {
                        fvfs_watcher::WatchEvent::ADD_FILE | fvfs_watcher::WatchEvent::EXISTING => {
                            let filepath = path::Path::new(devices::NetworkDeviceInstance::PATH)
                                .join(filename);
                            info!("found new network device at {:?}", filepath);
                            match devices::NetworkDeviceInstance::get_instance_stream(
                                &installer, &filepath,
                            )
                            .await
                            .context("create instance stream")
                            {
                                Ok(stream) => Ok(Some(
                                    stream
                                        .filter_map(move |r| {
                                            futures::future::ready(match r {
                                                Ok(instance) => Some(Ok(instance)),
                                                Err(errors::Error::NonFatal(nonfatal)) => {
                                                    error!(
                                        "non-fatal error operating device stream for {:?}: {:?}",
                                        filepath,
                                        nonfatal
                                    );
                                                    None
                                                }
                                                Err(errors::Error::Fatal(fatal)) => {
                                                    Some(Err(fatal))
                                                }
                                            })
                                        })
                                        // Need to box the stream to combine it
                                        // with flatten_unordered because it's
                                        // not Unpin.
                                        .boxed(),
                                )),
                                Err(errors::Error::NonFatal(nonfatal)) => {
                                    error!(
                                        "non-fatal error fetching device stream for {:?}: {:?}",
                                        filepath, nonfatal
                                    );
                                    Ok(None)
                                }
                                Err(errors::Error::Fatal(fatal)) => Err(fatal),
                            }
                        }
                        fvfs_watcher::WatchEvent::IDLE | fvfs_watcher::WatchEvent::REMOVE_FILE => {
                            Ok(None)
                        }
                        event => Err(anyhow::anyhow!(
                            "unrecognized event {:?} for device filename {}",
                            event,
                            filename.display()
                        )),
                    }
                }
            })
            .fuse()
            .try_flatten_unordered();
        Ok(stream_of_streams)
    }

    async fn handle_device_instance(
        &mut self,
        instance: devices::NetworkDeviceInstance,
    ) -> Result<(), anyhow::Error> {
        let mut i = 0;
        loop {
            // TODO(fxbug.dev/56559): The same interface may flap so instead of using a
            // temporary name, try to determine if the interface flapped and wait
            // for the teardown to complete.
            match self.add_new_device(&instance, i == 0 /* stable_name */).await {
                Ok(()) => {
                    break Ok(());
                }
                Err(devices::AddDeviceError::AlreadyExists) => {
                    i += 1;
                    if i == MAX_ADD_DEVICE_ATTEMPTS {
                        error!(
                            "failed to add {:?} after {} attempts due to already exists error",
                            instance, MAX_ADD_DEVICE_ATTEMPTS,
                        );

                        break Ok(());
                    }

                    warn!(
                        "got already exists error on attempt {} of adding {:?}, trying again...",
                        i, instance,
                    );
                }
                Err(devices::AddDeviceError::Other(errors::Error::NonFatal(e))) => {
                    error!("non-fatal error adding device {:?}: {:?}", instance, e);

                    break Ok(());
                }
                Err(devices::AddDeviceError::Other(errors::Error::Fatal(e))) => {
                    break Err(e.context(format!("error adding new device {:?}", instance)));
                }
            }
        }
    }

    /// Add a device at `filepath` to the netstack.
    async fn add_new_device(
        &mut self,
        device_instance: &devices::NetworkDeviceInstance,
        stable_name: bool,
    ) -> Result<(), devices::AddDeviceError> {
        let info =
            device_instance.get_device_info().await.context("error getting device info and MAC")?;

        let DeviceInfo { mac, device_class: _, topological_path } = &info;

        let mac = mac.ok_or_else(|| {
            warn!("devices without mac address not supported yet");
            devices::AddDeviceError::Other(errors::Error::NonFatal(anyhow::anyhow!(
                "device without mac not supported"
            )))
        })?;

        let interface_type = info.interface_type();
        let metric = match interface_type {
            InterfaceType::Wlan => self.interface_metrics.wlan_metric,
            InterfaceType::Ethernet => self.interface_metrics.eth_metric,
        }
        .into();
        let interface_name = if stable_name {
            match self.persisted_interface_config.generate_stable_name(
                topological_path.into(),
                mac,
                interface_type,
            ) {
                Ok(name) => name.to_string(),
                Err(interface::NameGenerationError::FileUpdateError { name, err }) => {
                    warn!(
                        "failed to update interface \
                            (topo path = {}, mac = {}, interface_type = {:?}) \
                            with new name = {}: {}",
                        topological_path, mac, interface_type, name, err
                    );
                    name.to_string()
                }
                Err(interface::NameGenerationError::GenerationError(e)) => {
                    return Err(devices::AddDeviceError::Other(errors::Error::Fatal(
                        e.context("error getting stable name"),
                    )))
                }
            }
        } else {
            self.persisted_interface_config.generate_temporary_name(interface_type)
        };

        info!("adding {:?} to stack with name = {}", device_instance, interface_name);

        let (interface_id, control) = device_instance
            .add_to_stack(self, InterfaceConfig { name: interface_name.clone(), metric })
            .await
            .context("error adding to stack")?;

        self.configure_eth_interface(
            interface_id.try_into().expect("interface ID should be nonzero"),
            control,
            interface_name,
            &info,
        )
        .await
        .context("error configuring ethernet interface")
        .map_err(devices::AddDeviceError::Other)
    }

    /// Configure an ethernet interface.
    ///
    /// If the device is a WLAN AP, it will be configured as a WLAN AP (see
    /// `configure_wlan_ap_and_dhcp_server` for more details). Otherwise, it
    /// will be configured as a host (see `configure_host` for more details).
    async fn configure_eth_interface(
        &mut self,
        interface_id: NonZeroU64,
        control: fidl_fuchsia_net_interfaces_ext::admin::Control,
        interface_name: String,
        info: &DeviceInfo,
    ) -> Result<(), errors::Error> {
        let class: DeviceClass = info.device_class.into();
        let ForwardedDeviceClasses { ipv4, ipv6 } = &self.forwarded_device_classes;
        let ipv4_forwarding = ipv4.contains(&class);
        let ipv6_forwarding = ipv6.contains(&class);
        let config: fnet_interfaces_admin::Configuration = control
            .set_configuration(fnet_interfaces_admin::Configuration {
                ipv6: Some(fnet_interfaces_admin::Ipv6Configuration {
                    forwarding: Some(ipv6_forwarding),
                    multicast_forwarding: Some(ipv6_forwarding),
                    ..Default::default()
                }),
                ipv4: Some(fnet_interfaces_admin::Ipv4Configuration {
                    forwarding: Some(ipv4_forwarding),
                    multicast_forwarding: Some(ipv4_forwarding),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .map_err(map_control_error("setting configuration"))
            .and_then(|res| {
                res.map_err(|e: fnet_interfaces_admin::ControlSetConfigurationError| {
                    errors::Error::Fatal(anyhow::anyhow!("{:?}", e))
                })
            })?;
        info!("installed configuration with result {:?}", config);

        if info.is_wlan_ap() {
            if let Some(id) = self.interface_states.iter().find_map(|(id, state)| {
                if state.is_wlan_ap() {
                    Some(id)
                } else {
                    None
                }
            }) {
                return Err(errors::Error::NonFatal(anyhow::anyhow!(
                    "multiple WLAN AP interfaces are not supported, \
                        have WLAN AP interface with id = {}",
                    id
                )));
            }
            let InterfaceState { control, config: _, device_class: _ } = match self
                .interface_states
                .entry(interface_id)
            {
                Entry::Occupied(entry) => {
                    panic!(
                        "multiple interfaces with the same ID = {}; \
                                attempting to add state for a WLAN AP, existing state = {:?}",
                        entry.key(),
                        entry.get(),
                    );
                }
                Entry::Vacant(entry) => entry.insert(InterfaceState::new_wlan_ap(control, class)),
            };

            info!("discovered WLAN AP (interface ID={})", interface_id);

            if let Some(dhcp_server) = &self.dhcp_server {
                info!("configuring DHCP server for WLAN AP (interface ID={})", interface_id);
                let () = Self::configure_wlan_ap_and_dhcp_server(
                    interface_id,
                    dhcp_server,
                    control,
                    &self.stack,
                    interface_name,
                )
                .await
                .context("error configuring wlan ap and dhcp server")?;
            } else {
                warn!(
                    "cannot configure DHCP server for WLAN AP (interface ID={}) \
                        since DHCP server service is not available",
                    interface_id
                );
            }
        } else {
            let InterfaceState { control, config: _, device_class: _ } = match self
                .interface_states
                .entry(interface_id)
            {
                Entry::Occupied(entry) => {
                    panic!(
                        "multiple interfaces with the same ID = {}; \
                            attempting to add state for a host, existing state = {:?}",
                        entry.key(),
                        entry.get()
                    );
                }
                Entry::Vacant(entry) => {
                    let dhcpv6_pd_config = if !self.allowed_upstream_device_classes.contains(&class)
                    {
                        None
                    } else {
                        self.dhcpv6_prefix_provider_handler.as_ref().map(
                            |dhcpv6::PrefixProviderHandler {
                                 preferred_prefix_len,
                                 prefix_control_request_stream: _,
                                 watch_prefix_responder: _,
                                 interface_config: _,
                                 current_prefix: _,
                             }| {
                                preferred_prefix_len.map_or(
                                    fnet_dhcpv6::PrefixDelegationConfig::Empty(fnet_dhcpv6::Empty),
                                    |preferred_prefix_len| {
                                        fnet_dhcpv6::PrefixDelegationConfig::PrefixLength(
                                            preferred_prefix_len,
                                        )
                                    },
                                )
                            },
                        )
                    };
                    entry.insert(InterfaceState::new_host(control, class, dhcpv6_pd_config))
                }
            };

            info!("discovered host interface with id={}, configuring interface", interface_id);

            let () = Self::configure_host(
                &self.filter_enabled_interface_types,
                &self.filter,
                &self.stack,
                interface_id,
                info,
                self.dhcpv4_client_provider.is_none(),
            )
            .await
            .context("error configuring host")?;

            let _did_enable: bool = control
                .enable()
                .await
                .map_err(map_control_error("error sending enable request"))
                .and_then(|res| {
                    // ControlEnableError is an empty *flexible* enum, so we can't match on it, but
                    // the operation is infallible at the time of writing.
                    res.map_err(|e: fidl_fuchsia_net_interfaces_admin::ControlEnableError| {
                        errors::Error::Fatal(anyhow::anyhow!("enable interface: {:?}", e))
                    })
                })?;
        }

        Ok(())
    }

    /// Configure host interface.
    async fn configure_host(
        filter_enabled_interface_types: &HashSet<InterfaceType>,
        filter: &fnet_filter::FilterProxy,
        stack: &fnet_stack::StackProxy,
        interface_id: NonZeroU64,
        info: &DeviceInfo,
        start_in_stack_dhcpv4: bool,
    ) -> Result<(), errors::Error> {
        if should_enable_filter(filter_enabled_interface_types, info) {
            info!("enable filter for nic {}", interface_id);
            let () = filter
                .enable_interface(interface_id.get())
                .await
                .unwrap_or_else(|err| exit_with_fidl_error(err))
                .map_err(|e| {
                    anyhow::anyhow!(
                        "failed to enable filter on nic {} with error = {:?}",
                        interface_id,
                        e
                    )
                })
                .map_err(errors::Error::NonFatal)?;
        } else {
            info!("disable filter for nic {}", interface_id);
            let () = filter
                .disable_interface(interface_id.get())
                .await
                .unwrap_or_else(|err| exit_with_fidl_error(err))
                .map_err(|e| {
                    anyhow::anyhow!(
                        "failed to disable filter on nic {} with error = {:?}",
                        interface_id,
                        e
                    )
                })
                .map_err(errors::Error::NonFatal)?;
        };

        // Enable DHCP.
        if start_in_stack_dhcpv4 {
            let () = stack
                .set_dhcp_client_enabled(interface_id.get(), true)
                .await
                .unwrap_or_else(|err| exit_with_fidl_error(err))
                .map_err(|e| anyhow!("failed to start dhcp client: {:?}", e))
                .map_err(errors::Error::NonFatal)?;
        }

        Ok(())
    }

    /// Configure the WLAN AP and the DHCP server to serve requests on its network.
    ///
    /// Note, this method will not start the DHCP server, it will only be configured
    /// with the parameters so it is ready to be started when an interface UP event
    /// is received for the WLAN AP.
    async fn configure_wlan_ap_and_dhcp_server(
        interface_id: NonZeroU64,
        dhcp_server: &fnet_dhcp::Server_Proxy,
        control: &fidl_fuchsia_net_interfaces_ext::admin::Control,
        stack: &fidl_fuchsia_net_stack::StackProxy,
        name: String,
    ) -> Result<(), errors::Error> {
        let (address_state_provider, server_end) = fidl::endpoints::create_proxy::<
            fidl_fuchsia_net_interfaces_admin::AddressStateProviderMarker,
        >()
        .context("address state provider: failed to create fidl endpoints")
        .map_err(errors::Error::Fatal)?;

        // Calculate and set the interface address based on the network address.
        // The interface address should be the first available address.
        let network_addr_as_u32 = u32::from_be_bytes(WLAN_AP_NETWORK_ADDR.addr);
        let interface_addr_as_u32 = network_addr_as_u32 + 1;
        let ipv4 = fnet::Ipv4Address { addr: interface_addr_as_u32.to_be_bytes() };
        let addr = fidl_fuchsia_net::Subnet {
            addr: fnet::IpAddress::Ipv4(ipv4.clone()),
            prefix_len: WLAN_AP_PREFIX_LEN.get(),
        };

        let () = control
            .add_address(
                &mut addr.clone(),
                fidl_fuchsia_net_interfaces_admin::AddressParameters::default(),
                server_end,
            )
            .map_err(map_control_error("error sending add address request"))?;

        // Allow the address to outlive this scope. At the time of writing its lifetime is
        // identical to the interface's lifetime and no updates to its properties are made. We may
        // wish to retain the handle in the future to allow external address removal (e.g. by a
        // user) to be observed so that an error can be emitted (as such removal would break a
        // critical user journey).
        let () = address_state_provider
            .detach()
            .map_err(Into::into)
            .map_err(map_address_state_provider_error("error sending detach request"))?;

        // Enable the interface to allow DAD to proceed.
        let _did_enable: bool = control
            .enable()
            .await
            .map_err(map_control_error("error sending enable request"))
            .and_then(|res| {
                // ControlEnableError is an empty *flexible* enum, so we can't match on it, but the
                // operation is infallible at the time of writing.
                res.map_err(|e: fidl_fuchsia_net_interfaces_admin::ControlEnableError| {
                    errors::Error::Fatal(anyhow::anyhow!("enable interface: {:?}", e))
                })
            })?;

        let state_stream =
            fidl_fuchsia_net_interfaces_ext::admin::assignment_state_stream(address_state_provider);
        futures::pin_mut!(state_stream);
        let () = fidl_fuchsia_net_interfaces_ext::admin::wait_assignment_state(
            &mut state_stream,
            fidl_fuchsia_net_interfaces_admin::AddressAssignmentState::Assigned,
        )
        .await
        .map_err(map_address_state_provider_error("failed to add interface address for WLAN AP"))?;

        let subnet = fidl_fuchsia_net_ext::apply_subnet_mask(addr);
        let () = stack
            .add_forwarding_entry(&mut fidl_fuchsia_net_stack::ForwardingEntry {
                subnet,
                device_id: interface_id.get(),
                next_hop: None,
                metric: fnet_stack::UNSPECIFIED_METRIC,
            })
            .await
            .unwrap_or_else(|err| exit_with_fidl_error(err))
            .map_err(|e| {
                let severity = match e {
                    fidl_fuchsia_net_stack::Error::InvalidArgs => {
                        // Do not consider this a fatal error because the interface could have been
                        // removed after it was added, but before we reached this point.
                        //
                        // NB: this error is returned by Netstack2 when the interface doesn't
                        // exist. 🤷
                        errors::Error::NonFatal
                    }
                    fidl_fuchsia_net_stack::Error::Internal
                    | fidl_fuchsia_net_stack::Error::NotSupported
                    | fidl_fuchsia_net_stack::Error::BadState
                    | fidl_fuchsia_net_stack::Error::TimeOut
                    | fidl_fuchsia_net_stack::Error::NotFound
                    | fidl_fuchsia_net_stack::Error::AlreadyExists
                    | fidl_fuchsia_net_stack::Error::Io => errors::Error::Fatal,
                };
                severity(anyhow::anyhow!("adding route: {:?}", e))
            })?;

        // First we clear any leases that the server knows about since the server
        // will be used on a new interface. If leases exist, configuring the DHCP
        // server parameters may fail (AddressPool).
        debug!("clearing DHCP leases");
        let () = dhcp_server
            .clear_leases()
            .await
            .context("error sending clear DHCP leases request")
            .map_err(errors::Error::NonFatal)?
            .map_err(zx::Status::from_raw)
            .context("error clearing DHCP leases request")
            .map_err(errors::Error::NonFatal)?;

        // Configure the DHCP server.
        let v = vec![ipv4];
        debug!("setting DHCP IpAddrs parameter to {:?}", v);
        let () = dhcp_server
            .set_parameter(&mut fnet_dhcp::Parameter::IpAddrs(v))
            .await
            .context("error sending set DHCP IpAddrs parameter request")
            .map_err(errors::Error::NonFatal)?
            .map_err(zx::Status::from_raw)
            .context("error setting DHCP IpAddrs parameter")
            .map_err(errors::Error::NonFatal)?;

        let v = vec![name];
        debug!("setting DHCP BoundDeviceNames parameter to {:?}", v);
        let () = dhcp_server
            .set_parameter(&mut fnet_dhcp::Parameter::BoundDeviceNames(v))
            .await
            .context("error sending set DHCP BoundDeviceName parameter request")
            .map_err(errors::Error::NonFatal)?
            .map_err(zx::Status::from_raw)
            .context("error setting DHCP BoundDeviceNames parameter")
            .map_err(errors::Error::NonFatal)?;

        let v = fnet_dhcp::LeaseLength {
            default: Some(WLAN_AP_DHCP_LEASE_TIME_SECONDS),
            max: Some(WLAN_AP_DHCP_LEASE_TIME_SECONDS),
            ..Default::default()
        };
        debug!("setting DHCP LeaseLength parameter to {:?}", v);
        let () = dhcp_server
            .set_parameter(&mut fnet_dhcp::Parameter::Lease(v))
            .await
            .context("error sending set DHCP LeaseLength parameter request")
            .map_err(errors::Error::NonFatal)?
            .map_err(zx::Status::from_raw)
            .context("error setting DHCP LeaseLength parameter")
            .map_err(errors::Error::NonFatal)?;

        let host_mask = ::dhcpv4::configuration::SubnetMask::new(WLAN_AP_PREFIX_LEN);
        let broadcast_addr =
            host_mask.broadcast_of(&std::net::Ipv4Addr::from_fidl(WLAN_AP_NETWORK_ADDR));
        let broadcast_addr_as_u32: u32 = broadcast_addr.into();
        // The start address of the DHCP pool should be the first address after the WLAN AP
        // interface address.
        let dhcp_pool_start =
            fnet::Ipv4Address { addr: (interface_addr_as_u32 + 1).to_be_bytes() }.into();
        // The last address of the DHCP pool should be the last available address
        // in the interface's network. This is the address immediately before the
        // network's broadcast address.
        let dhcp_pool_end = fnet::Ipv4Address { addr: (broadcast_addr_as_u32 - 1).to_be_bytes() };

        let v = fnet_dhcp::AddressPool {
            prefix_length: Some(WLAN_AP_PREFIX_LEN.get()),
            range_start: Some(dhcp_pool_start),
            range_stop: Some(dhcp_pool_end),
            ..Default::default()
        };
        debug!("setting DHCP AddressPool parameter to {:?}", v);
        dhcp_server
            .set_parameter(&mut fnet_dhcp::Parameter::AddressPool(v))
            .await
            .context("error sending set DHCP AddressPool parameter request")
            .map_err(errors::Error::NonFatal)?
            .map_err(zx::Status::from_raw)
            .context("error setting DHCP AddressPool parameter")
            .map_err(errors::Error::NonFatal)
    }

    async fn handle_dhcpv6_acquire_prefix(
        &mut self,
        fnet_dhcpv6::AcquirePrefixConfig {
            interface_id,
            preferred_prefix_len,
            ..
        }: fnet_dhcpv6::AcquirePrefixConfig,
        prefix: fidl::endpoints::ServerEnd<fnet_dhcpv6::PrefixControlMarker>,
        dns_watchers: &mut DnsServerWatchers<'_>,
    ) -> Result<(), errors::Error> {
        let (prefix_control_request_stream, control_handle) = prefix
            .into_stream_and_control_handle()
            .context("fuchsia.net.dhcpv6/PrefixControl server end to stream and control handle")
            .map_err(errors::Error::NonFatal)?;

        let dhcpv6_client_provider = if let Some(s) = self.dhcpv6_client_provider.as_ref() {
            s
        } else {
            warn!(
                "Attempted to acquire prefix when DHCPv6 is not supported; interface_id={:?}, preferred_prefix_len={:?}",
                interface_id, preferred_prefix_len,
            );

            return control_handle
                .send_on_exit(fnet_dhcpv6::PrefixControlExitReason::NotSupported)
                .context("failed to send NotSupported terminal event")
                .map_err(errors::Error::NonFatal);
        };

        let interface_config = if let Some(interface_id) = interface_id {
            match self
                .interface_states
                .get(&interface_id.try_into().expect("interface ID should be nonzero"))
            {
                // It is invalid to acquire a prefix over an interface that netcfg doesn't own,
                // or a WLAN AP interface.
                None
                | Some(InterfaceState {
                    config: InterfaceConfigState::WlanAp(_),
                    control: _,
                    device_class: _,
                }) => {
                    return control_handle
                        .send_on_exit(fnet_dhcpv6::PrefixControlExitReason::InvalidInterface)
                        .context("failed to send InvalidInterface terminal event")
                        .map_err(errors::Error::NonFatal);
                }
                Some(InterfaceState {
                    config:
                        InterfaceConfigState::Host(HostInterfaceState {
                            dhcpv4_client: _,
                            dhcpv6_client_state: _,
                            dhcpv6_pd_config: _,
                        }),
                    control: _,
                    device_class: _,
                }) => dhcpv6::AcquirePrefixInterfaceConfig::Id(interface_id),
            }
        } else {
            dhcpv6::AcquirePrefixInterfaceConfig::Upstreams
        };
        let pd_config = if let Some(preferred_prefix_len) = preferred_prefix_len {
            if preferred_prefix_len > net_types::ip::Ipv6Addr::BYTES * 8 {
                return control_handle
                    .send_on_exit(fnet_dhcpv6::PrefixControlExitReason::InvalidPrefixLength)
                    .context("failed to send InvalidPrefixLength terminal event")
                    .map_err(errors::Error::NonFatal);
            }
            fnet_dhcpv6::PrefixDelegationConfig::PrefixLength(preferred_prefix_len)
        } else {
            fnet_dhcpv6::PrefixDelegationConfig::Empty(fnet_dhcpv6::Empty)
        };

        if self.dhcpv6_prefix_provider_handler.is_some() {
            return control_handle
                .send_on_exit(fnet_dhcpv6::PrefixControlExitReason::AlreadyAcquiring)
                .context("failed to send AlreadyAcquiring terminal event")
                .map_err(errors::Error::NonFatal);
        }

        let interface_state_iter = match interface_config {
            dhcpv6::AcquirePrefixInterfaceConfig::Id(want_id) => {
                let want_id = want_id.try_into().expect("interface ID should be nonzero");
                either::Either::Left(std::iter::once((
                    want_id,
                    self.interface_states
                        .get_mut(&want_id)
                        .unwrap_or_else(|| panic!("interface {} state not present", want_id)),
                )))
            }
            dhcpv6::AcquirePrefixInterfaceConfig::Upstreams => either::Either::Right(
                self.interface_states.iter_mut().filter_map(|(id, if_state)| {
                    self.allowed_upstream_device_classes
                        .contains(&if_state.device_class)
                        .then_some((*id, if_state))
                }),
            ),
        };
        // Look for all eligible interfaces and start/restart DHCPv6 client as needed.
        for (id, InterfaceState { config, control: _, device_class: _ }) in interface_state_iter {
            let HostInterfaceState { dhcpv4_client: _, dhcpv6_client_state, dhcpv6_pd_config } =
                match config {
                    InterfaceConfigState::Host(state) => state,
                    InterfaceConfigState::WlanAp(WlanApInterfaceState {}) => {
                        continue;
                    }
                };

            // Save the config so that future DHCPv6 clients are started with PD.
            *dhcpv6_pd_config = Some(pd_config.clone());

            let properties = if let Some(properties) = self.interface_properties.get(&id) {
                properties
            } else {
                // There is a delay between when netcfg installs and when said interface's
                // properties are received via the interface watcher and ends up in
                // `interface_properties`, so if the properties are not yet known, simply
                // continue. The DHCPv6 client on such an interface will be started with PD
                // configured per the usual process when handling interface watcher events.
                continue;
            };

            // TODO(https://fxbug.dev/117651): Reload configuration in-place rather than
            // restarting the DHCPv6 client with different configuration.
            // Stop DHCPv6 client if it's running.
            if let Some::<dhcpv6::ClientState>(_) = dhcpv6_client_state.take() {
                dhcpv6::stop_client(
                    &self.lookup_admin,
                    &mut self.dns_servers,
                    id,
                    dns_watchers,
                    &mut self.dhcpv6_prefixes_streams,
                )
                .await;
            }

            // Restart DHCPv6 client and configure it to perform PD.
            let sockaddr = match start_dhcpv6_client(
                properties,
                &dhcpv6_client_provider,
                Some(pd_config.clone()),
                dns_watchers,
                &mut self.dhcpv6_prefixes_streams,
            )
            .context("starting DHCPv6 client with PD")
            {
                Ok(dhcpv6_client_addr) => dhcpv6_client_addr,
                Err(errors::Error::NonFatal(e)) => {
                    warn!("error restarting DHCPv6 client to perform PD: {:?}", e);
                    None
                }
                Err(errors::Error::Fatal(e)) => {
                    panic!("error restarting DHCPv6 client to perform PD: {:?}", e);
                }
            };
            *dhcpv6_client_state = sockaddr.map(dhcpv6::ClientState::new);
        }
        self.dhcpv6_prefix_provider_handler = Some(dhcpv6::PrefixProviderHandler {
            prefix_control_request_stream,
            watch_prefix_responder: None,
            current_prefix: None,
            interface_config,
            preferred_prefix_len,
        });
        Ok(())
    }

    async fn handle_watch_prefix(
        &mut self,
        responder: fnet_dhcpv6::PrefixControlWatchPrefixResponder,
        dns_watchers: &mut DnsServerWatchers<'_>,
    ) -> Result<(), anyhow::Error> {
        let dhcpv6::PrefixProviderHandler {
            watch_prefix_responder,
            interface_config: _,
            prefix_control_request_stream: _,
            preferred_prefix_len: _,
            current_prefix: _,
        } = self
            .dhcpv6_prefix_provider_handler
            .as_mut()
            .expect("DHCPv6 prefix provider handler must be present to handle WatchPrefix");
        if let Some(responder) = watch_prefix_responder.take() {
            match responder
                .control_handle()
                .send_on_exit(fnet_dhcpv6::PrefixControlExitReason::DoubleWatch)
            {
                Err(e) => {
                    warn!(
                        "failed to send DoubleWatch terminal event on PrefixControl channel: {:?}",
                        e
                    );
                }
                Ok(()) => {}
            }
            self.on_dhcpv6_prefix_control_close(dns_watchers).await;
            Ok(())
        } else {
            *watch_prefix_responder = Some(responder);

            dhcpv6::maybe_send_watch_prefix_response(
                &self.interface_states,
                &self.allowed_upstream_device_classes,
                self.dhcpv6_prefix_provider_handler.as_mut(),
            )
        }
    }

    async fn on_dhcpv6_prefix_control_close(&mut self, dns_watchers: &mut DnsServerWatchers<'_>) {
        let _: dhcpv6::PrefixProviderHandler = self
            .dhcpv6_prefix_provider_handler
            .take()
            .expect("DHCPv6 prefix provider handler must be present");
        let dhcpv6_client_provider =
            self.dhcpv6_client_provider.as_ref().expect("DHCPv6 client provider must be present");
        for (id, InterfaceState { config, control: _, device_class: _ }) in
            self.interface_states.iter_mut()
        {
            let dhcpv6_client_state = match config {
                InterfaceConfigState::WlanAp(WlanApInterfaceState {}) => {
                    continue;
                }
                InterfaceConfigState::Host(HostInterfaceState {
                    dhcpv4_client: _,
                    dhcpv6_client_state,
                    dhcpv6_pd_config,
                }) => {
                    if dhcpv6_pd_config.take().is_none() {
                        continue;
                    }
                    match dhcpv6_client_state.take() {
                        Some(_) => dhcpv6_client_state,
                        None => continue,
                    }
                }
            };

            // TODO(https://fxbug.dev/117651): Reload configuration in-place rather than
            // restarting the DHCPv6 client with different configuration.
            // Stop DHCPv6 client if it's running.
            dhcpv6::stop_client(
                &self.lookup_admin,
                &mut self.dns_servers,
                *id,
                dns_watchers,
                &mut self.dhcpv6_prefixes_streams,
            )
            .await;

            let properties = self.interface_properties.get(id).unwrap_or_else(|| {
                panic!("interface {} has DHCPv6 client but properties unknown", id)
            });

            // Restart DHCPv6 client without PD.
            let sockaddr = match start_dhcpv6_client(
                properties,
                &dhcpv6_client_provider,
                None,
                dns_watchers,
                &mut self.dhcpv6_prefixes_streams,
            )
            .context("starting DHCPv6 client with PD")
            {
                Ok(dhcpv6_client_addr) => dhcpv6_client_addr,
                Err(errors::Error::NonFatal(e)) => {
                    warn!("restarting DHCPv6 client to stop PD: {:?}", e);
                    None
                }
                Err(e @ errors::Error::Fatal(_)) => {
                    panic!("restarting DHCPv6 client to stop PD: {:?}", e);
                }
            };
            *dhcpv6_client_state = sockaddr.map(dhcpv6::ClientState::new);
        }
    }

    async fn handle_dhcpv4_configuration(
        &mut self,
        interface_id: NonZeroU64,
        res: Result<fnet_dhcp_ext::Configuration, fnet_dhcp_ext::Error>,
    ) {
        let configuration = match res {
            Ok(o) => o,
            Err(e) => {
                warn!(
                    "DHCPv4 client on interface={} error while watching for configuration: {:?}",
                    interface_id, e,
                );

                return;
            }
        };

        let (dhcpv4_client, control) = {
            let InterfaceState { config, control, device_class: _ } = self
                .interface_states
                .get_mut(&interface_id)
                .unwrap_or_else(|| panic!("interface {} not found", interface_id));

            match config {
                InterfaceConfigState::Host(HostInterfaceState {
                    dhcpv4_client,
                    dhcpv6_client_state: _,
                    dhcpv6_pd_config: _,
                }) => (
                    dhcpv4_client.as_mut().unwrap_or_else(|| {
                        panic!("interface {} does not have a running DHCPv4 client", interface_id)
                    }),
                    control,
                ),
                InterfaceConfigState::WlanAp(wlan_ap_state) => {
                    panic!(
                        "interface {} expected to be host but is WLAN AP with state {:?}",
                        interface_id, wlan_ap_state
                    );
                }
            }
        };

        dhcpv4::update_configuration(
            interface_id,
            dhcpv4_client,
            configuration,
            &mut self.dns_servers,
            control,
            &self.stack,
            &self.lookup_admin,
        )
        .await
    }

    async fn handle_dhcpv6_prefixes(
        &mut self,
        interface_id: NonZeroU64,
        res: Result<Vec<fnet_dhcpv6::Prefix>, fidl::Error>,
        dns_watchers: &mut DnsServerWatchers<'_>,
    ) -> Result<(), anyhow::Error> {
        let new_prefixes = match res
            .context("DHCPv6 prefixes stream FIDL error")
            .and_then(|prefixes| dhcpv6::from_fidl_prefixes(&prefixes))
        {
            Err(e) => {
                warn!(
                    "DHCPv6 client on interface={} error on watch_prefixes: {:?}",
                    interface_id, e
                );
                dhcpv6::stop_client(
                    &self.lookup_admin,
                    &mut self.dns_servers,
                    interface_id,
                    dns_watchers,
                    &mut self.dhcpv6_prefixes_streams,
                )
                .await;
                HashMap::new()
            }
            Ok(prefixes_map) => prefixes_map,
        };
        let InterfaceState { config, control: _, device_class: _ } = self
            .interface_states
            .get_mut(&interface_id)
            .unwrap_or_else(|| panic!("interface {} not found", interface_id));
        let dhcpv6::ClientState { prefixes, sockaddr: _ } = match config {
            InterfaceConfigState::Host(HostInterfaceState {
                dhcpv4_client: _,
                dhcpv6_client_state,
                dhcpv6_pd_config: _,
            }) => dhcpv6_client_state.as_mut().unwrap_or_else(|| {
                panic!("interface {} does not have a running DHCPv6 client", interface_id)
            }),
            InterfaceConfigState::WlanAp(wlan_ap_state) => {
                panic!(
                    "interface {} expected to be host but is WLAN AP with state {:?}",
                    interface_id, wlan_ap_state
                );
            }
        };
        *prefixes = new_prefixes;

        dhcpv6::maybe_send_watch_prefix_response(
            &self.interface_states,
            &self.allowed_upstream_device_classes,
            self.dhcpv6_prefix_provider_handler.as_mut(),
        )
    }
}

pub async fn run<M: Mode>() -> Result<(), anyhow::Error> {
    let opt: Opt = argh::from_env();
    let Opt { min_severity: LogLevel(min_severity), config_data } = &opt;

    // Use the diagnostics_log library directly rather than e.g. the #[fuchsia::main] macro on
    // the main function, so that we can specify the logging severity level at runtime based on a
    // command line argument.
    diagnostics_log::initialize(
        diagnostics_log::PublishOptions::default().minimum_severity(*min_severity),
    )?;

    info!("starting");
    debug!("starting with options = {:?}", opt);

    let Config {
        dns_config: DnsConfig { servers },
        filter_config,
        filter_enabled_interface_types,
        interface_metrics,
        allowed_upstream_device_classes: AllowedDeviceClasses(allowed_upstream_device_classes),
        allowed_bridge_upstream_device_classes:
            AllowedDeviceClasses(allowed_bridge_upstream_device_classes),
        enable_dhcpv6,
        forwarded_device_classes,
    } = Config::load(config_data)?;

    let mut netcfg = NetCfg::new(
        filter_enabled_interface_types,
        interface_metrics,
        enable_dhcpv6,
        forwarded_device_classes,
        &allowed_upstream_device_classes,
    )
    .await
    .context("error creating new netcfg instance")?;

    let () =
        netcfg.update_filters(filter_config).await.context("update filters based on config")?;

    let servers = servers.into_iter().map(static_source_from_ip).collect();
    debug!("updating default servers to {:?}", servers);
    let () = netcfg.update_dns_servers(DnsServersUpdateSource::Default, servers).await;

    M::run(netcfg, allowed_bridge_upstream_device_classes)
        .map_err(|e| {
            let err_str = format!("fatal error running main: {:?}", e);
            error!("{}", err_str);
            anyhow!(err_str)
        })
        .await
}

/// Allows callers of `netcfg::run` to configure at compile time which features
/// should be enabled.
///
/// This trait may be expanded to support combinations of features that can be
/// assembled together for specific netcfg builds.
#[async_trait(?Send)]
pub trait Mode {
    async fn run<'a>(
        netcfg: NetCfg<'a>,
        allowed_bridge_upstream_device_classes: HashSet<DeviceClass>,
    ) -> Result<(), anyhow::Error>;
}

/// In this configuration, netcfg acts as the policy manager for netstack,
/// watching for device events and configuring the netstack with new interfaces
/// as needed on new device discovery. It does not implement any FIDL protocols.
pub enum BasicMode {}

#[async_trait(?Send)]
impl Mode for BasicMode {
    async fn run<'a>(
        mut netcfg: NetCfg<'a>,
        _allowed_bridge_upstream_device_classes: HashSet<DeviceClass>,
    ) -> Result<(), anyhow::Error> {
        netcfg.run(virtualization::Stub).await.context("event loop")
    }
}

/// In this configuration, netcfg implements the base functionality included in
/// `BasicMode`, and also serves the `fuchsia.net.virtualization/Control`
/// protocol, allowing clients to create virtual networks.
pub enum VirtualizationEnabled {}

#[async_trait(?Send)]
impl Mode for VirtualizationEnabled {
    async fn run<'a>(
        mut netcfg: NetCfg<'a>,
        allowed_bridge_upstream_device_classes: HashSet<DeviceClass>,
    ) -> Result<(), anyhow::Error> {
        let handler = virtualization::Virtualization::new(
            netcfg.allowed_upstream_device_classes,
            allowed_bridge_upstream_device_classes,
            virtualization::BridgeHandlerImpl::new(netcfg.stack.clone()),
            netcfg.installer.clone(),
        );
        netcfg.run(handler).await.context("event loop")
    }
}

fn map_control_error(
    ctx: &'static str,
) -> impl FnOnce(
    fnet_interfaces_ext::admin::TerminalError<fnet_interfaces_admin::InterfaceRemovedReason>,
) -> errors::Error {
    move |e| {
        let severity = match &e {
            fidl_fuchsia_net_interfaces_ext::admin::TerminalError::Fidl(e) => {
                if e.is_closed() {
                    // Control handle can close when interface is
                    // removed; not a fatal error.
                    errors::Error::NonFatal
                } else {
                    errors::Error::Fatal
                }
            }
            fidl_fuchsia_net_interfaces_ext::admin::TerminalError::Terminal(e) => match e {
                fidl_fuchsia_net_interfaces_admin::InterfaceRemovedReason::DuplicateName
                | fidl_fuchsia_net_interfaces_admin::InterfaceRemovedReason::PortAlreadyBound
                | fidl_fuchsia_net_interfaces_admin::InterfaceRemovedReason::BadPort => {
                    errors::Error::Fatal
                }
                fidl_fuchsia_net_interfaces_admin::InterfaceRemovedReason::PortClosed
                | fidl_fuchsia_net_interfaces_admin::InterfaceRemovedReason::User
                | _ => errors::Error::NonFatal,
            },
        };
        severity(anyhow::Error::new(e).context(ctx))
    }
}

fn map_address_state_provider_error(
    ctx: &'static str,
) -> impl FnOnce(fnet_interfaces_ext::admin::AddressStateProviderError) -> errors::Error {
    move |e| {
        let severity = match &e {
            fnet_interfaces_ext::admin::AddressStateProviderError::Fidl(e) => {
                if e.is_closed() {
                    // TODO(https://fxbug.dev/89290): Reconsider whether this
                    // should be a fatal error, as it can be caused by a
                    // netstack bug.
                    errors::Error::NonFatal
                } else {
                    errors::Error::Fatal
                }
            }
            fnet_interfaces_ext::admin::AddressStateProviderError::ChannelClosed => {
                // TODO(https://fxbug.dev/89290): Reconsider whether this should
                // be a fatal error, as it can be caused by a netstack bug.
                errors::Error::NonFatal
            }
            fnet_interfaces_ext::admin::AddressStateProviderError::AddressRemoved(e) => match e {
                fidl_fuchsia_net_interfaces_admin::AddressRemovalReason::Invalid => {
                    errors::Error::Fatal
                }
                fidl_fuchsia_net_interfaces_admin::AddressRemovalReason::AlreadyAssigned
                | fidl_fuchsia_net_interfaces_admin::AddressRemovalReason::DadFailed
                | fidl_fuchsia_net_interfaces_admin::AddressRemovalReason::InterfaceRemoved
                | fidl_fuchsia_net_interfaces_admin::AddressRemovalReason::UserRemoved => {
                    errors::Error::NonFatal
                }
            },
        };
        severity(anyhow::Error::new(e).context(ctx))
    }
}

/// If we can't reach netstack via fidl, log an error and exit.
//
// TODO(fxbug.dev/119295): add a test that works as intended.
fn exit_with_fidl_error(cause: fidl::Error) -> ! {
    error!(%cause, "exiting due to fidl error");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use fidl_fuchsia_net_ext::FromExt as _;

    use assert_matches::assert_matches;
    use futures::future::{self, FutureExt as _};
    use futures::stream::{FusedStream as _, TryStreamExt as _};
    use net_declare::{
        fidl_ip, fidl_ip_v4_with_prefix, fidl_ip_v6, fidl_ip_v6_with_prefix, fidl_subnet,
    };
    use test_case::test_case;

    use super::*;

    impl Config {
        pub fn load_str(s: &str) -> Result<Self, anyhow::Error> {
            let config = serde_json::from_str(s)
                .with_context(|| format!("could not deserialize the config data {}", s))?;
            Ok(config)
        }
    }

    struct ServerEnds {
        stack: fnet_stack::StackRequestStream,
        lookup_admin: fnet_name::LookupAdminRequestStream,
        dhcpv4_client_provider: fnet_dhcp::ClientProviderRequestStream,
        dhcpv6_client_provider: fnet_dhcpv6::ClientProviderRequestStream,
    }

    impl Into<anyhow::Error> for errors::Error {
        fn into(self) -> anyhow::Error {
            match self {
                errors::Error::NonFatal(e) => e,
                errors::Error::Fatal(e) => e,
            }
        }
    }

    impl Into<fidl_fuchsia_hardware_network::DeviceClass> for DeviceClass {
        fn into(self) -> fidl_fuchsia_hardware_network::DeviceClass {
            match self {
                Self::Virtual => fidl_fuchsia_hardware_network::DeviceClass::Virtual,
                Self::Ethernet => fidl_fuchsia_hardware_network::DeviceClass::Ethernet,
                Self::Wlan => fidl_fuchsia_hardware_network::DeviceClass::Wlan,
                Self::Ppp => fidl_fuchsia_hardware_network::DeviceClass::Ppp,
                Self::Bridge => fidl_fuchsia_hardware_network::DeviceClass::Bridge,
                Self::WlanAp => fidl_fuchsia_hardware_network::DeviceClass::WlanAp,
            }
        }
    }

    lazy_static::lazy_static! {
        static ref DEFAULT_ALLOWED_UPSTREAM_DEVICE_CLASSES: HashSet<DeviceClass> = HashSet::new();
    }

    fn test_netcfg(
        with_dhcpv4_client_provider: bool,
    ) -> Result<(NetCfg<'static>, ServerEnds), anyhow::Error> {
        let (stack, stack_server) = fidl::endpoints::create_proxy::<fnet_stack::StackMarker>()
            .context("error creating stack endpoints")?;
        let (lookup_admin, lookup_admin_server) =
            fidl::endpoints::create_proxy::<fnet_name::LookupAdminMarker>()
                .context("error creating lookup_admin endpoints")?;
        let (filter, _filter_server) = fidl::endpoints::create_proxy::<fnet_filter::FilterMarker>()
            .context("create filter endpoints")?;
        let (interface_state, _interface_state_server) =
            fidl::endpoints::create_proxy::<fnet_interfaces::StateMarker>()
                .context("create interface state endpoints")?;
        let (dhcp_server, _dhcp_server_server) =
            fidl::endpoints::create_proxy::<fnet_dhcp::Server_Marker>()
                .context("error creating dhcp_server endpoints")?;
        let (dhcpv4_client_provider, dhcpv4_client_provider_server) =
            fidl::endpoints::create_proxy::<fnet_dhcp::ClientProviderMarker>()
                .context("error creating dhcpv4_client_provider endpoints")?;
        let (dhcpv6_client_provider, dhcpv6_client_provider_server) =
            fidl::endpoints::create_proxy::<fnet_dhcpv6::ClientProviderMarker>()
                .context("error creating dhcpv6_client_provider endpoints")?;
        let (installer, _installer_server) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_net_interfaces_admin::InstallerMarker>()
                .context("error creating installer endpoints")?;
        let persisted_interface_config =
            interface::FileBackedConfig::load(&PERSISTED_INTERFACE_CONFIG_FILEPATH)
                .context("error loading persistent interface configurations")?;

        Ok((
            NetCfg {
                stack,
                lookup_admin,
                filter,
                interface_state,
                installer,
                dhcp_server: Some(dhcp_server),
                dhcpv4_client_provider: with_dhcpv4_client_provider
                    .then_some(dhcpv4_client_provider),
                dhcpv6_client_provider: Some(dhcpv6_client_provider),
                persisted_interface_config,
                filter_enabled_interface_types: Default::default(),
                interface_properties: Default::default(),
                interface_states: Default::default(),
                interface_metrics: Default::default(),
                dns_servers: Default::default(),
                forwarded_device_classes: Default::default(),
                dhcpv6_prefix_provider_handler: Default::default(),
                allowed_upstream_device_classes: &DEFAULT_ALLOWED_UPSTREAM_DEVICE_CLASSES,
                dhcpv4_configuration_streams: dhcpv4::ConfigurationStreamMap::empty(),
                dhcpv6_prefixes_streams: dhcpv6::PrefixesStreamMap::empty(),
            },
            ServerEnds {
                stack: stack_server
                    .into_stream()
                    .context("error converting stack server to stream")?,
                lookup_admin: lookup_admin_server
                    .into_stream()
                    .context("error converting lookup_admin server to stream")?,
                dhcpv4_client_provider: dhcpv4_client_provider_server
                    .into_stream()
                    .context("error converting dhcpv4_client_provider server to stream")?,
                dhcpv6_client_provider: dhcpv6_client_provider_server
                    .into_stream()
                    .context("error converting dhcpv6_client_provider server to stream")?,
            },
        ))
    }

    const INTERFACE_ID: NonZeroU64 = nonzero_ext::nonzero!(1u64);
    const DHCPV6_DNS_SOURCE: DnsServersUpdateSource =
        DnsServersUpdateSource::Dhcpv6 { interface_id: INTERFACE_ID.get() };
    const LINK_LOCAL_SOCKADDR1: fnet::Ipv6SocketAddress = fnet::Ipv6SocketAddress {
        address: fidl_ip_v6!("fe80::1"),
        port: fnet_dhcpv6::DEFAULT_CLIENT_PORT,
        zone_index: INTERFACE_ID.get(),
    };
    const LINK_LOCAL_SOCKADDR2: fnet::Ipv6SocketAddress = fnet::Ipv6SocketAddress {
        address: fidl_ip_v6!("fe80::2"),
        port: fnet_dhcpv6::DEFAULT_CLIENT_PORT,
        zone_index: INTERFACE_ID.get(),
    };
    const GLOBAL_ADDR: fnet::Subnet = fnet::Subnet { addr: fidl_ip!("2000::1"), prefix_len: 64 };
    const DNS_SERVER1: fnet::SocketAddress = fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
        address: fidl_ip_v6!("2001::1"),
        port: fnet_dhcpv6::DEFAULT_CLIENT_PORT,
        zone_index: 0,
    });
    const DNS_SERVER2: fnet::SocketAddress = fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
        address: fidl_ip_v6!("2001::2"),
        port: fnet_dhcpv6::DEFAULT_CLIENT_PORT,
        zone_index: 0,
    });

    fn ipv6addrs(a: Option<fnet::Ipv6SocketAddress>) -> Vec<fnet_interfaces::Address> {
        // The DHCPv6 client will only use a link-local address but we include a global address
        // and expect it to not be used.
        std::iter::once(fnet_interfaces::Address {
            addr: Some(GLOBAL_ADDR),
            valid_until: Some(fuchsia_zircon::Time::INFINITE.into_nanos()),
            ..Default::default()
        })
        .chain(a.map(|fnet::Ipv6SocketAddress { address, port: _, zone_index: _ }| {
            fnet_interfaces::Address {
                addr: Some(fnet::Subnet { addr: fnet::IpAddress::Ipv6(address), prefix_len: 64 }),
                valid_until: Some(fuchsia_zircon::Time::INFINITE.into_nanos()),
                ..Default::default()
            }
        }))
        .collect()
    }

    /// Handle receiving a netstack interface changed event.
    async fn handle_interface_changed_event(
        netcfg: &mut NetCfg<'_>,
        dns_watchers: &mut DnsServerWatchers<'_>,
        online: Option<bool>,
        addresses: Option<Vec<fnet_interfaces::Address>>,
    ) -> Result<(), anyhow::Error> {
        let event = fnet_interfaces::Event::Changed(fnet_interfaces::Properties {
            id: Some(INTERFACE_ID.get()),
            name: None,
            device_class: None,
            online,
            addresses,
            has_default_ipv4_route: None,
            has_default_ipv6_route: None,
            ..Default::default()
        });
        netcfg.handle_interface_watcher_event(event, dns_watchers, &mut virtualization::Stub).await
    }

    /// Make sure that a new DHCPv6 client was requested, and verify its parameters.
    async fn check_new_dhcpv6_client(
        server: &mut fnet_dhcpv6::ClientProviderRequestStream,
        id: NonZeroU64,
        sockaddr: fnet::Ipv6SocketAddress,
        prefix_delegation_config: Option<fnet_dhcpv6::PrefixDelegationConfig>,
        dns_watchers: &mut DnsServerWatchers<'_>,
    ) -> Result<fnet_dhcpv6::ClientRequestStream, anyhow::Error> {
        let evt =
            server.try_next().await.context("error getting next dhcpv6 client provider event")?;
        let mut client_server =
            match evt.ok_or(anyhow::anyhow!("expected dhcpv6 client provider request"))? {
                fnet_dhcpv6::ClientProviderRequest::NewClient {
                    params,
                    request,
                    control_handle: _,
                } => {
                    assert_eq!(
                        params,
                        fnet_dhcpv6::NewClientParams {
                            interface_id: Some(id.get()),
                            address: Some(sockaddr),
                            config: Some(fnet_dhcpv6::ClientConfig {
                                information_config: Some(fnet_dhcpv6::InformationConfig {
                                    dns_servers: Some(true),
                                    ..Default::default()
                                }),
                                prefix_delegation_config,
                                ..Default::default()
                            }),
                            ..Default::default()
                        }
                    );

                    request.into_stream().context("error converting client server end to stream")?
                }
            };
        assert!(dns_watchers.contains_key(&DHCPV6_DNS_SOURCE), "should have a watcher");
        // NetCfg always keeps the server hydrated with a pending hanging-get.
        expect_watch_prefixes(&mut client_server).await;
        Ok(client_server)
    }

    fn expect_watch_dhcpv4_configuration(
        stream: &mut fnet_dhcp::ClientRequestStream,
    ) -> fnet_dhcp::ClientWatchConfigurationResponder {
        assert_matches::assert_matches!(
            stream.try_next().now_or_never(),
            Some(Ok(Some(fnet_dhcp::ClientRequest::WatchConfiguration { responder }))) => {
                responder
            },
            "expect a watch_configuration call immediately"
        )
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stopping_dhcpv6_with_down_lookup_admin() {
        let (
            mut netcfg,
            ServerEnds {
                stack: _,
                lookup_admin,
                dhcpv4_client_provider: _,
                mut dhcpv6_client_provider,
            },
        ) = test_netcfg(false /* with_dhcpv4_client_provider */)
            .expect("error creating test netcfg");
        let mut dns_watchers = DnsServerWatchers::empty();

        // Mock a new interface being discovered by NetCfg (we only need to make NetCfg aware of a
        // NIC with ID `INTERFACE_ID` to test DHCPv6).
        let (control, _control_server_end) =
            fidl_fuchsia_net_interfaces_ext::admin::Control::create_endpoints()
                .expect("create endpoints");
        let device_class = fidl_fuchsia_hardware_network::DeviceClass::Virtual;
        assert_matches::assert_matches!(
            netcfg
                .interface_states
                .insert(INTERFACE_ID, InterfaceState::new_host(control, device_class.into(), None)),
            None
        );

        // Should start the DHCPv6 client when we get an interface changed event that shows the
        // interface as up with an link-local address.
        let () = netcfg
            .handle_interface_watcher_event(
                fnet_interfaces::Event::Added(fnet_interfaces::Properties {
                    id: Some(INTERFACE_ID.get()),
                    name: Some("testif01".to_string()),
                    device_class: Some(fnet_interfaces::DeviceClass::Device(device_class)),
                    online: Some(true),
                    addresses: Some(ipv6addrs(Some(LINK_LOCAL_SOCKADDR1))),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                &mut dns_watchers,
                &mut virtualization::Stub,
            )
            .await
            .expect("error handling interface added event with interface up and sockaddr1");
        let _: fnet_dhcpv6::ClientRequestStream = check_new_dhcpv6_client(
            &mut dhcpv6_client_provider,
            INTERFACE_ID,
            LINK_LOCAL_SOCKADDR1,
            None,
            &mut dns_watchers,
        )
        .await
        .expect("error checking for new client with sockaddr1");

        // Drop the server-end of the lookup admin to simulate a down lookup admin service.
        let () = std::mem::drop(lookup_admin);

        // Not having any more link local IPv6 addresses should terminate the client.
        let () = handle_interface_changed_event(
            &mut netcfg,
            &mut dns_watchers,
            None,
            Some(ipv6addrs(None)),
        )
        .await
        .expect("error handling interface changed event with sockaddr1 removed");

        // Another update without any link-local IPv6 addresses should do nothing
        // since the DHCPv6 client was already stopped.
        let () =
            handle_interface_changed_event(&mut netcfg, &mut dns_watchers, None, Some(Vec::new()))
                .await
                .expect("error handling interface changed event with sockaddr1 removed");

        // Update interface with a link-local address to create a new DHCPv6 client.
        let () = handle_interface_changed_event(
            &mut netcfg,
            &mut dns_watchers,
            None,
            Some(ipv6addrs(Some(LINK_LOCAL_SOCKADDR1))),
        )
        .await
        .expect("error handling interface changed event with sockaddr1 removed");
        let _: fnet_dhcpv6::ClientRequestStream = check_new_dhcpv6_client(
            &mut dhcpv6_client_provider,
            INTERFACE_ID,
            LINK_LOCAL_SOCKADDR1,
            None,
            &mut dns_watchers,
        )
        .await
        .expect("error checking for new client with sockaddr1");

        // Update offline status to down to stop DHCPv6 client.
        let () = handle_interface_changed_event(&mut netcfg, &mut dns_watchers, Some(false), None)
            .await
            .expect("error handling interface changed event with sockaddr1 removed");

        // Update interface with new addresses but leave offline status as down.
        handle_interface_changed_event(&mut netcfg, &mut dns_watchers, None, Some(ipv6addrs(None)))
            .await
            .expect("error handling interface changed event with sockaddr1 removed")
    }

    async fn expect_watch_prefixes(client_server: &mut fnet_dhcpv6::ClientRequestStream) {
        assert_matches::assert_matches!(
            client_server.try_next().now_or_never(),
            Some(Ok(Some(fnet_dhcpv6::ClientRequest::WatchPrefixes { responder }))) => {
                responder.drop_without_shutdown();
            },
            "expect a watch_prefixes call immediately"
        )
    }

    async fn handle_update(
        netcfg: &mut NetCfg<'_>,
        online: Option<bool>,
        addresses: Option<Vec<fnet_interfaces::Address>>,
        dns_watchers: &mut DnsServerWatchers<'_>,
    ) {
        netcfg
            .handle_interface_watcher_event(
                fnet_interfaces::Event::Changed(fnet_interfaces::Properties {
                    id: Some(INTERFACE_ID.get()),
                    online,
                    addresses,
                    ..Default::default()
                }),
                dns_watchers,
                &mut virtualization::Stub,
            )
            .await
            .expect("error handling interface change event with interface online")
    }
    const DHCP_ADDRESS: fnet::Ipv4AddressWithPrefix = fidl_ip_v4_with_prefix!("192.0.2.254/24");

    fn dhcp_address_parameters() -> fnet_interfaces_admin::AddressParameters {
        fnet_interfaces_admin::AddressParameters {
            initial_properties: Some(fnet_interfaces_admin::AddressProperties {
                preferred_lifetime_info: None,
                valid_lifetime_end: Some(zx::Time::INFINITE.into_nanos()),
                ..Default::default()
            }),
            temporary: Some(true),
            add_subnet_route: Some(false),
            ..Default::default()
        }
    }

    #[test_case(true, true; "added online and removed interface")]
    #[test_case(false, true; "added offline and removed interface")]
    #[test_case(true, false; "added online and disabled interface")]
    #[test_case(false, false; "added offline and disabled interface")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_dhcpv4(added_online: bool, remove_interface: bool) {
        let (
            mut netcfg,
            ServerEnds {
                stack,
                mut lookup_admin,
                mut dhcpv4_client_provider,
                dhcpv6_client_provider: _,
            },
        ) = test_netcfg(true /* with_dhcpv4_client_provider */)
            .expect("error creating test netcfg");
        let mut dns_watchers = DnsServerWatchers::empty();

        // Mock a new interface being discovered by NetCfg (we only need to make NetCfg aware of a
        // NIC with ID `INTERFACE_ID` to test DHCPv4).
        let (control, control_server_end) =
            fidl_fuchsia_net_interfaces_ext::admin::Control::create_endpoints()
                .expect("create endpoints");
        let device_class = fidl_fuchsia_hardware_network::DeviceClass::Virtual;
        assert_matches::assert_matches!(
            netcfg
                .interface_states
                .insert(INTERFACE_ID, InterfaceState::new_host(control, device_class.into(), None)),
            None
        );
        let mut control = control_server_end.into_stream().expect("control request stream");

        // Make sure the DHCPv4 client is created on interface up.
        let (mut client_stream, responder) = {
            netcfg
                .handle_interface_watcher_event(
                    fnet_interfaces::Event::Added(fnet_interfaces::Properties {
                        id: Some(INTERFACE_ID.get()),
                        name: Some("testif01".to_string()),
                        device_class: Some(fnet_interfaces::DeviceClass::Device(
                            fidl_fuchsia_hardware_network::DeviceClass::Virtual,
                        )),
                        online: Some(added_online),
                        addresses: Some(Vec::new()),
                        has_default_ipv4_route: Some(false),
                        has_default_ipv6_route: Some(false),
                        ..Default::default()
                    }),
                    &mut dns_watchers,
                    &mut virtualization::Stub,
                )
                .await
                .expect("error handling interface added event");

            if !added_online {
                assert_matches::assert_matches!(
                    dhcpv4_client_provider.try_next().now_or_never(),
                    None
                );
                handle_update(
                    &mut netcfg,
                    Some(true), /* online */
                    None,       /* addresses */
                    &mut dns_watchers,
                )
                .await;
            }

            let mut client_req_stream = match dhcpv4_client_provider
                .try_next()
                .await
                .expect("get next dhcpv4 client provider event")
                .expect("dhcpv4 client provider request")
            {
                fnet_dhcp::ClientProviderRequest::NewClient {
                    interface_id,
                    params,
                    request,
                    control_handle: _,
                } => {
                    assert_eq!(interface_id, INTERFACE_ID.get());
                    assert_eq!(params, dhcpv4::new_client_params());
                    request.into_stream().expect("error converting client server end to stream")
                }
                fnet_dhcp::ClientProviderRequest::CheckPresence { responder: _ } => {
                    unreachable!("only called at startup")
                }
            };

            let responder = expect_watch_dhcpv4_configuration(&mut client_req_stream);
            (client_req_stream, responder)
        };

        let check_fwd_entry =
            |fnet_stack::ForwardingEntry { subnet, device_id, next_hop, metric },
             routers: &mut HashSet<_>| {
                assert_eq!(subnet, fnet_dhcp_ext::DEFAULT_SUBNET);
                assert_eq!(device_id, INTERFACE_ID.get());
                assert_eq!(metric, 0);
                assert!(routers.insert(*next_hop.expect("specified next hop")))
            };

        let (expected_routers, stack) = {
            let dns_servers = vec![fidl_ip_v4!("192.0.2.1"), fidl_ip_v4!("192.0.2.2")];
            let routers = vec![fidl_ip_v4!("192.0.2.3"), fidl_ip_v4!("192.0.2.4")];

            let (_asp_client, asp_server) = fidl::endpoints::create_proxy().expect("create proxy");

            responder
                .send(fnet_dhcp::ClientWatchConfigurationResponse {
                    address: Some(fnet_dhcp::Address {
                        address: Some(DHCP_ADDRESS),
                        address_parameters: Some(dhcp_address_parameters()),
                        address_state_provider: Some(asp_server),
                        ..Default::default()
                    }),
                    dns_servers: Some(dns_servers.clone()),
                    routers: Some(routers.clone()),
                    ..Default::default()
                })
                .expect("send configuration update");

            let (got_interface_id, got_response) = netcfg
                .dhcpv4_configuration_streams
                .next()
                .await
                .expect("DHCPv4 configuration streams should never be exhausted");
            assert_eq!(got_interface_id, INTERFACE_ID);

            let dns_servers = dns_servers
                .into_iter()
                .map(|address| {
                    fnet::SocketAddress::Ipv4(fnet::Ipv4SocketAddress {
                        address,
                        port: DEFAULT_DNS_PORT,
                    })
                })
                .collect::<Vec<_>>();

            let expect_add_default_routers = futures::stream::repeat(()).take(routers.len()).fold(
                (HashSet::new(), stack),
                |(mut routers, mut stack), ()| async move {
                    let (entry, responder) = assert_matches!(
                        stack.next().await.expect("stack request stream should not be exhausted"),
                        Ok(fnet_stack::StackRequest::AddForwardingEntry { entry, responder }) => {
                            (entry, responder)
                        }
                    );
                    check_fwd_entry(entry, &mut routers);
                    responder.send(&mut Ok(())).expect("send add fwd entry response");
                    (routers, stack)
                },
            );

            let expect_add_address_called = async {
                assert_matches!(
                    control.next().await.expect("control request stream should not be exhausted"),
                    Ok(fnet_interfaces_admin::ControlRequest::AddAddress {
                        address,
                        parameters,
                        address_state_provider: _,
                        control_handle: _,
                    })  => {
                        assert_eq!(address, fnet::Subnet::from_ext(DHCP_ADDRESS));
                        assert_eq!(parameters, dhcp_address_parameters());
                    }
                );
            };

            let ((), (), (added_routers, stack), ()) = future::join4(
                netcfg.handle_dhcpv4_configuration(got_interface_id, got_response),
                run_lookup_admin_once(&mut lookup_admin, &dns_servers),
                expect_add_default_routers,
                expect_add_address_called,
            )
            .await;
            assert_eq!(netcfg.dns_servers.consolidated(), dns_servers);
            let expected_routers =
                routers.iter().cloned().map(fnet::IpAddress::Ipv4).collect::<HashSet<_>>();
            assert_eq!(added_routers, expected_routers);
            (expected_routers, stack)
        };

        // Netcfg always keeps the server hydrated with a WatchConfiguration
        // request.
        let _responder: fnet_dhcp::ClientWatchConfigurationResponder =
            expect_watch_dhcpv4_configuration(&mut client_stream);

        let expect_delete_default_routers = futures::stream::repeat(())
            .take(expected_routers.len())
            .fold((HashSet::new(), stack), |(mut routers, mut stack), ()| async move {
                let (entry, responder) = assert_matches!(
                    stack.next().await.expect("stack request stream should not be exhausted"),
                    Ok(fnet_stack::StackRequest::DelForwardingEntry { entry, responder }) => {
                        (entry, responder)
                    }
                );
                check_fwd_entry(entry, &mut routers);
                responder.send(&mut Ok(())).expect("send del fwd entry response");
                (routers, stack)
            });

        // Make sure the DHCPv4 client is shutdown on interface disable/removal.
        let ((), (), (), (deleted_routers, mut stack)) = future::join4(
            async {
                if remove_interface {
                    netcfg
                        .handle_interface_watcher_event(
                            fnet_interfaces::Event::Removed(INTERFACE_ID.get()),
                            &mut dns_watchers,
                            &mut virtualization::Stub,
                        )
                        .await
                        .expect("error handling interface removed event")
                } else {
                    handle_update(
                        &mut netcfg,
                        Some(false), /* online */
                        None,        /* addresses */
                        &mut dns_watchers,
                    )
                    .await
                }
            },
            async {
                match client_stream
                    .try_next()
                    .await
                    .expect("wait for next shutdown request")
                    .expect("netcfg should send shutdown request before closing client")
                {
                    req @ fnet_dhcp::ClientRequest::WatchConfiguration { responder: _ } => {
                        panic!("unexpected request = {:?}", req);
                    }
                    fnet_dhcp::ClientRequest::Shutdown { control_handle } => control_handle
                        .send_on_exit(fnet_dhcp::ClientExitReason::GracefulShutdown)
                        .expect("send client exit reason"),
                }
            },
            run_lookup_admin_once(&mut lookup_admin, &Vec::new()),
            expect_delete_default_routers,
        )
        .await;
        assert_eq!(netcfg.dns_servers.consolidated(), []);
        assert_eq!(deleted_routers, expected_routers);

        // No more requests sent by NetCfg.
        assert_matches!(lookup_admin.next().now_or_never(), None);
        assert_matches!(stack.next().now_or_never(), None);
        let control_next = control.next().now_or_never();
        if remove_interface {
            assert_matches!(control_next, Some(None));
        } else {
            assert_matches!(control_next, None);
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_dhcpv4_ignores_address_change() {
        let (
            mut netcfg,
            ServerEnds {
                stack: _,
                lookup_admin: _,
                mut dhcpv4_client_provider,
                dhcpv6_client_provider: _,
            },
        ) = test_netcfg(true /* with_dhcpv4_client_provider */)
            .expect("error creating test netcfg");
        let mut dns_watchers = DnsServerWatchers::empty();

        // Mock a new interface being discovered by NetCfg (we only need to make NetCfg aware of a
        // NIC with ID `INTERFACE_ID` to test DHCPv4).
        let (control, control_server_end) =
            fidl_fuchsia_net_interfaces_ext::admin::Control::create_endpoints()
                .expect("create endpoints");
        let device_class = fidl_fuchsia_hardware_network::DeviceClass::Virtual;
        assert_matches::assert_matches!(
            netcfg
                .interface_states
                .insert(INTERFACE_ID, InterfaceState::new_host(control, device_class.into(), None)),
            None
        );
        let _control = control_server_end.into_stream().expect("control request stream");

        // Make sure the DHCPv4 client is created on interface up.
        netcfg
            .handle_interface_watcher_event(
                fnet_interfaces::Event::Added(fnet_interfaces::Properties {
                    id: Some(INTERFACE_ID.get()),
                    name: Some("testif01".to_string()),
                    device_class: Some(fnet_interfaces::DeviceClass::Device(
                        fidl_fuchsia_hardware_network::DeviceClass::Virtual,
                    )),
                    online: Some(true),
                    addresses: Some(Vec::new()),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                &mut dns_watchers,
                &mut virtualization::Stub,
            )
            .await
            .expect("error handling interface added event");

        let mut client_req_stream = match dhcpv4_client_provider
            .try_next()
            .await
            .expect("get next dhcpv4 client provider event")
            .expect("dhcpv4 client provider request")
        {
            fnet_dhcp::ClientProviderRequest::NewClient {
                interface_id,
                params,
                request,
                control_handle: _,
            } => {
                assert_eq!(interface_id, INTERFACE_ID.get());
                assert_eq!(params, dhcpv4::new_client_params());
                request.into_stream().expect("error converting client server end to stream")
            }
            fnet_dhcp::ClientProviderRequest::CheckPresence { responder: _ } => {
                unreachable!("only called at startup")
            }
        };

        let responder = expect_watch_dhcpv4_configuration(&mut client_req_stream);
        let (_asp_client, asp_server) = fidl::endpoints::create_proxy().expect("create proxy");
        responder
            .send(fnet_dhcp::ClientWatchConfigurationResponse {
                address: Some(fnet_dhcp::Address {
                    address: Some(DHCP_ADDRESS),
                    address_parameters: Some(dhcp_address_parameters()),
                    address_state_provider: Some(asp_server),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .expect("send configuration update");

        // A DHCP client should only be started or stopped when the interface is
        // enabled or disabled. If we change the addresses seen on the
        // interface, we shouldn't see requests to create a new DHCP client, and
        // the existing one should remain alive (netcfg shouldn't have sent a
        // shutdown request for it).
        for addresses in [
            vec![fidl_subnet!("192.2.2.2/28")],
            vec![],
            vec![fidl_subnet!("192.2.2.2/28"), fidl_subnet!("fe80::1234/64")],
        ] {
            handle_update(
                &mut netcfg,
                None,
                Some(
                    addresses
                        .into_iter()
                        .map(|address| fnet_interfaces::Address {
                            addr: Some(address),
                            valid_until: Some(fuchsia_zircon::sys::ZX_TIME_INFINITE),
                            ..Default::default()
                        })
                        .collect(),
                ),
                &mut dns_watchers,
            )
            .await;
        }
        assert_matches!(dhcpv4_client_provider.next().now_or_never(), None);
        assert_matches!(client_req_stream.next().now_or_never(), None);
    }

    /// Waits for a `SetDnsServers` request with the specified servers.
    async fn run_lookup_admin_once(
        server: &mut fnet_name::LookupAdminRequestStream,
        expected_servers: &Vec<fnet::SocketAddress>,
    ) {
        let req = server
            .try_next()
            .await
            .expect("get next lookup admin request")
            .expect("lookup admin request stream should not be exhausted");
        let (servers, responder) = assert_matches!(
            req,
            fnet_name::LookupAdminRequest::SetDnsServers { servers, responder } => {
                (servers, responder)
            }
        );

        assert_eq!(expected_servers, &servers);
        responder.send(&mut Ok(())).expect("send set dns servers response");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_dhcpv6() {
        let (mut netcfg, mut servers) = test_netcfg(false /* with_dhcpv4_client_provider */)
            .expect("error creating test netcfg");
        let mut dns_watchers = DnsServerWatchers::empty();

        // Mock a fake DNS update from the netstack.
        let netstack_servers = vec![DNS_SERVER1];
        let ((), ()) = future::join(
            netcfg.update_dns_servers(
                DnsServersUpdateSource::Netstack,
                vec![fnet_name::DnsServer_ {
                    address: Some(DNS_SERVER1),
                    source: Some(fnet_name::DnsServerSource::StaticSource(
                        fnet_name::StaticDnsServerSource::default(),
                    )),
                    ..Default::default()
                }],
            ),
            run_lookup_admin_once(&mut servers.lookup_admin, &netstack_servers),
        )
        .await;

        // Mock a new interface being discovered by NetCfg (we only need to make NetCfg aware of a
        // NIC with ID `INTERFACE_ID` to test DHCPv6).
        let (control, _control_server_end) =
            fidl_fuchsia_net_interfaces_ext::admin::Control::create_endpoints()
                .expect("create endpoints");
        let device_class = fidl_fuchsia_hardware_network::DeviceClass::Virtual;
        assert_matches::assert_matches!(
            netcfg
                .interface_states
                .insert(INTERFACE_ID, InterfaceState::new_host(control, device_class.into(), None)),
            None
        );

        // Should start the DHCPv6 client when we get an interface changed event that shows the
        // interface as up with an link-local address.
        let () = netcfg
            .handle_interface_watcher_event(
                fnet_interfaces::Event::Added(fnet_interfaces::Properties {
                    id: Some(INTERFACE_ID.get()),
                    name: Some("testif01".to_string()),
                    device_class: Some(fnet_interfaces::DeviceClass::Device(device_class)),
                    online: Some(true),
                    addresses: Some(ipv6addrs(Some(LINK_LOCAL_SOCKADDR1))),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                &mut dns_watchers,
                &mut virtualization::Stub,
            )
            .await
            .expect("error handling interface added event with interface up and sockaddr1");
        let mut client_server = check_new_dhcpv6_client(
            &mut servers.dhcpv6_client_provider,
            INTERFACE_ID,
            LINK_LOCAL_SOCKADDR1,
            None,
            &mut dns_watchers,
        )
        .await
        .expect("error checking for new client with sockaddr1");

        // Mock a fake DNS update from the DHCPv6 client.
        let ((), ()) = future::join(
            netcfg.update_dns_servers(
                DHCPV6_DNS_SOURCE,
                vec![fnet_name::DnsServer_ {
                    address: Some(DNS_SERVER2),
                    source: Some(fnet_name::DnsServerSource::Dhcpv6(
                        fnet_name::Dhcpv6DnsServerSource {
                            source_interface: Some(INTERFACE_ID.get()),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }],
            ),
            run_lookup_admin_once(&mut servers.lookup_admin, &vec![DNS_SERVER2, DNS_SERVER1]),
        )
        .await;

        // Not having any more link local IPv6 addresses should terminate the client.
        let ((), ()) = future::join(
            handle_interface_changed_event(
                &mut netcfg,
                &mut dns_watchers,
                None,
                Some(ipv6addrs(None)),
            )
            .map(|r| r.expect("error handling interface changed event with sockaddr1 removed")),
            run_lookup_admin_once(&mut servers.lookup_admin, &netstack_servers),
        )
        .await;
        assert!(!dns_watchers.contains_key(&DHCPV6_DNS_SOURCE), "should not have a watcher");
        assert_matches::assert_matches!(client_server.try_next().await, Ok(None));

        // Should start a new DHCPv6 client when we get an interface changed event that shows the
        // interface as up with an link-local address.
        let () = handle_interface_changed_event(
            &mut netcfg,
            &mut dns_watchers,
            None,
            Some(ipv6addrs(Some(LINK_LOCAL_SOCKADDR2))),
        )
        .await
        .expect("error handling netstack event with sockaddr2 added");
        let mut client_server = check_new_dhcpv6_client(
            &mut servers.dhcpv6_client_provider,
            INTERFACE_ID,
            LINK_LOCAL_SOCKADDR2,
            None,
            &mut dns_watchers,
        )
        .await
        .expect("error checking for new client with sockaddr2");

        // Interface being down should terminate the client.
        let ((), ()) = future::join(
            handle_interface_changed_event(
                &mut netcfg,
                &mut dns_watchers,
                Some(false), /* down */
                None,
            )
            .map(|r| r.expect("error handling interface changed event with interface down")),
            run_lookup_admin_once(&mut servers.lookup_admin, &netstack_servers),
        )
        .await;
        assert!(!dns_watchers.contains_key(&DHCPV6_DNS_SOURCE), "should not have a watcher");
        assert_matches::assert_matches!(client_server.try_next().await, Ok(None));

        // Should start a new DHCPv6 client when we get an interface changed event that shows the
        // interface as up with an link-local address.
        let () = handle_interface_changed_event(
            &mut netcfg,
            &mut dns_watchers,
            Some(true), /* up */
            None,
        )
        .await
        .expect("error handling interface up event");
        let mut client_server = check_new_dhcpv6_client(
            &mut servers.dhcpv6_client_provider,
            INTERFACE_ID,
            LINK_LOCAL_SOCKADDR2,
            None,
            &mut dns_watchers,
        )
        .await
        .expect("error checking for new client with sockaddr2 after interface up again");

        // Should start a new DHCPv6 client when we get an interface changed event that shows the
        // interface as up with a new link-local address.
        let ((), ()) = future::join(
            handle_interface_changed_event(
                &mut netcfg,
                &mut dns_watchers,
                None,
                Some(ipv6addrs(Some(LINK_LOCAL_SOCKADDR1))),
            )
            .map(|r| {
                r.expect("error handling interface change event with sockaddr1 replacing sockaddr2")
            }),
            run_lookup_admin_once(&mut servers.lookup_admin, &netstack_servers),
        )
        .await;
        assert_matches::assert_matches!(client_server.try_next().await, Ok(None));
        let _client_server: fnet_dhcpv6::ClientRequestStream = check_new_dhcpv6_client(
            &mut servers.dhcpv6_client_provider,
            INTERFACE_ID,
            LINK_LOCAL_SOCKADDR1,
            None,
            &mut dns_watchers,
        )
        .await
        .expect("error checking for new client with sockaddr1 after address change");

        // Complete the DNS server watcher then start a new one.
        let ((), ()) = future::join(
            netcfg
                .handle_dns_server_watcher_done(DHCPV6_DNS_SOURCE, &mut dns_watchers)
                .map(|r| r.expect("error handling completion of dns server watcher")),
            run_lookup_admin_once(&mut servers.lookup_admin, &netstack_servers),
        )
        .await;
        assert!(!dns_watchers.contains_key(&DHCPV6_DNS_SOURCE), "should not have a watcher");
        let () = handle_interface_changed_event(
            &mut netcfg,
            &mut dns_watchers,
            None,
            Some(ipv6addrs(Some(LINK_LOCAL_SOCKADDR2))),
        )
        .await
        .expect("error handling interface change event with sockaddr2 replacing sockaddr1");
        let mut client_server = check_new_dhcpv6_client(
            &mut servers.dhcpv6_client_provider,
            INTERFACE_ID,
            LINK_LOCAL_SOCKADDR2,
            None,
            &mut dns_watchers,
        )
        .await
        .expect("error checking for new client with sockaddr2 after completing dns watcher");

        // An event that indicates the interface is removed should stop the client.
        let ((), ()) = future::join(
            netcfg
                .handle_interface_watcher_event(
                    fnet_interfaces::Event::Removed(INTERFACE_ID.get()),
                    &mut dns_watchers,
                    &mut virtualization::Stub,
                )
                .map(|r| r.expect("error handling interface removed event")),
            run_lookup_admin_once(&mut servers.lookup_admin, &netstack_servers),
        )
        .await;
        assert!(!dns_watchers.contains_key(&DHCPV6_DNS_SOURCE), "should not have a watcher");
        assert_matches::assert_matches!(client_server.try_next().await, Ok(None));
        assert!(!netcfg.interface_states.contains_key(&INTERFACE_ID));
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    enum InterfaceKind {
        Unowned,
        NonHost,
        Host { upstream: bool },
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct InterfaceConfig {
        id: NonZeroU64,
        kind: InterfaceKind,
    }

    const UPSTREAM_INTERFACE_CONFIG: InterfaceConfig = InterfaceConfig {
        id: nonzero_ext::nonzero!(1u64),
        kind: InterfaceKind::Host { upstream: true },
    };

    const ALLOWED_UPSTREAM_DEVICE_CLASS: DeviceClass = DeviceClass::Ethernet;
    const DISALLOWED_UPSTREAM_DEVICE_CLASS: DeviceClass = DeviceClass::Virtual;

    fn dhcpv6_sockaddr(interface_id: NonZeroU64) -> fnet::Ipv6SocketAddress {
        let mut address = fidl_ip_v6!("fe80::");
        let interface_id: u8 =
            u64::from(interface_id).try_into().expect("interface ID should fit into u8");
        *address.addr.last_mut().expect("IPv6 address is empty") = interface_id;
        fnet::Ipv6SocketAddress {
            address: address,
            port: fnet_dhcpv6::DEFAULT_CLIENT_PORT,
            zone_index: interface_id.into(),
        }
    }

    #[test_case(
        Some(UPSTREAM_INTERFACE_CONFIG.id),
        None; "specific_interface_no_preferred_prefix_len")]
    #[test_case(
        None,
        None; "all_upstreams_no_preferred_prefix_len")]
    #[test_case(
        Some(UPSTREAM_INTERFACE_CONFIG.id),
        Some(2); "specific_interface_preferred_prefix_len")]
    #[test_case(
        None,
        Some(1); "all_upstreams_preferred_prefix_len")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_dhcpv6_acquire_prefix(
        interface_id: Option<NonZeroU64>,
        preferred_prefix_len: Option<u8>,
    ) {
        const INTERFACE_CONFIGS: [InterfaceConfig; 5] = [
            UPSTREAM_INTERFACE_CONFIG,
            InterfaceConfig {
                id: nonzero_ext::nonzero!(2u64),
                kind: InterfaceKind::Host { upstream: true },
            },
            InterfaceConfig {
                id: nonzero_ext::nonzero!(3u64),
                kind: InterfaceKind::Host { upstream: false },
            },
            InterfaceConfig { id: nonzero_ext::nonzero!(4u64), kind: InterfaceKind::Unowned },
            InterfaceConfig { id: nonzero_ext::nonzero!(5u64), kind: InterfaceKind::NonHost },
        ];

        let (
            mut netcfg,
            ServerEnds {
                stack: _,
                lookup_admin: lookup_admin_request_stream,
                dhcpv4_client_provider: _,
                dhcpv6_client_provider: mut dhcpv6_client_provider_request_stream,
            },
        ) = test_netcfg(false /* with_dhcpv4_client_provider */)
            .expect("error creating test netcfg");
        let allowed_upstream_device_classes = HashSet::from([ALLOWED_UPSTREAM_DEVICE_CLASS]);
        netcfg.allowed_upstream_device_classes = &allowed_upstream_device_classes;
        let mut dns_watchers = DnsServerWatchers::empty();

        struct TestInterfaceState {
            _control_server_end:
                Option<fidl::endpoints::ServerEnd<fnet_interfaces_admin::ControlMarker>>,
            dhcpv6_client_request_stream: Option<fnet_dhcpv6::ClientRequestStream>,
            kind: InterfaceKind,
        }
        let mut interface_states = HashMap::new();
        for InterfaceConfig { id, kind } in INTERFACE_CONFIGS.into_iter() {
            // Mock new interfaces being discovered by NetCfg as needed.
            let (device_class, control_server_end) = match kind {
                InterfaceKind::Unowned => (DISALLOWED_UPSTREAM_DEVICE_CLASS, None),
                InterfaceKind::NonHost => {
                    let (control, control_server_end) =
                        fidl_fuchsia_net_interfaces_ext::admin::Control::create_endpoints()
                            .expect("create endpoints");
                    let device_class = DeviceClass::WlanAp;
                    assert_matches::assert_matches!(
                        netcfg.interface_states.insert(
                            id.try_into().expect("interface ID should be nonzero"),
                            InterfaceState::new_wlan_ap(control, device_class)
                        ),
                        None
                    );
                    (device_class, Some(control_server_end))
                }
                InterfaceKind::Host { upstream } => {
                    let (control, control_server_end) =
                        fidl_fuchsia_net_interfaces_ext::admin::Control::create_endpoints()
                            .expect("create endpoints");
                    let device_class = if upstream {
                        ALLOWED_UPSTREAM_DEVICE_CLASS
                    } else {
                        DISALLOWED_UPSTREAM_DEVICE_CLASS
                    };
                    assert_matches::assert_matches!(
                        netcfg.interface_states.insert(
                            id.try_into().expect("interface ID should be nonzero"),
                            InterfaceState::new_host(control, device_class, None)
                        ),
                        None
                    );
                    (device_class, Some(control_server_end))
                }
            };

            let sockaddr = dhcpv6_sockaddr(id);

            // Fake an interface added event.
            let () = netcfg
                .handle_interface_watcher_event(
                    fnet_interfaces::Event::Added(fnet_interfaces::Properties {
                        id: Some(id.get()),
                        name: Some(format!("testif{}", id)),
                        device_class: Some(fnet_interfaces::DeviceClass::Device(
                            device_class.into(),
                        )),
                        online: Some(true),
                        addresses: Some(ipv6addrs(Some(sockaddr))),
                        has_default_ipv4_route: Some(false),
                        has_default_ipv6_route: Some(false),
                        ..Default::default()
                    }),
                    &mut dns_watchers,
                    &mut virtualization::Stub,
                )
                .await
                .unwrap_or_else(|e| panic!("error handling interface added event for {} with interface up and link-local addr: {}", id, e));

            // Expect DHCPv6 client to have started on host interfaces.
            let dhcpv6_client_request_stream = match kind {
                InterfaceKind::Unowned | InterfaceKind::NonHost => None,
                InterfaceKind::Host { upstream: _ } => {
                    let request_stream = check_new_dhcpv6_client(
                        &mut dhcpv6_client_provider_request_stream,
                        id,
                        sockaddr,
                        None,
                        &mut dns_watchers,
                    )
                    .await
                    .unwrap_or_else(|e| {
                        panic!("error checking for new DHCPv6 client on interface {}: {}", id, e)
                    });
                    Some(request_stream)
                }
            };
            assert!(interface_states
                .insert(
                    id,
                    TestInterfaceState {
                        _control_server_end: control_server_end,
                        dhcpv6_client_request_stream,
                        kind,
                    }
                )
                .is_none());
        }

        async fn assert_dhcpv6_clients_stopped(
            interface_states: &mut HashMap<NonZeroU64, TestInterfaceState>,
            interface_id: Option<NonZeroU64>,
        ) -> HashSet<NonZeroU64> {
            futures::stream::iter(
                interface_states.iter_mut(),
            ).filter_map(|(
                id,
                TestInterfaceState { _control_server_end, dhcpv6_client_request_stream, kind },
            )| async move {
                // Expect DHCPv6 to be restarted iff the interface matches the
                // interface used to acquire prefix on, or is an upstream
                // capable host if no interface was specified to acquire
                // prefixes from.
                let expect_restart = interface_id.map_or_else(
                    || *kind == InterfaceKind::Host { upstream: true },
                    |want_id| want_id == *id,
                );
                if expect_restart {
                    let res = dhcpv6_client_request_stream
                        .take()
                        .unwrap_or_else(|| panic!("interface {} DHCPv6 client provider request stream missing when expecting restart", id))
                        .try_next().await;
                    assert_matches!(res, Ok(None));
                    Some(*id)
                } else {
                    if let Some(req_stream) = dhcpv6_client_request_stream.as_mut() {
                        // We do not expect DHCPv6 to restart and want to make
                        // sure that the stream has not ended. We can't `.await`
                        // because that will result in us blocking forever on
                        // the next event (since the DHCPv6 client did not close
                        // so the stream is still open and blocked).
                        assert_matches!(req_stream.try_next().now_or_never(), None, "interface_id={:?}, kind={:?}, id={:?}", interface_id, kind, id);
                        assert!(!req_stream.is_terminated());
                    }
                    None
                }
            })
            .collect().await
        }

        async fn assert_dhcpv6_clients_started(
            count: usize,
            dhcpv6_client_provider_request_stream: &mut fnet_dhcpv6::ClientProviderRequestStream,
            interface_states: &mut HashMap<NonZeroU64, TestInterfaceState>,
            want_pd_config: Option<fnet_dhcpv6::PrefixDelegationConfig>,
        ) -> HashSet<NonZeroU64> {
            dhcpv6_client_provider_request_stream
                .map(|res| res.expect("DHCPv6 ClientProvider request stream error"))
                .take(count)
                .then(
                    |fnet_dhcpv6::ClientProviderRequest::NewClient {
                         params:
                             fnet_dhcpv6::NewClientParams { interface_id, address: _, config, .. },
                         request,
                         control_handle: _,
                     }| async move {
                        let mut new_stream =
                            request.into_stream().expect("fuchsia.net.dhcpv6/Client into_stream");
                        // NetCfg always keeps the server hydrated with a pending hanging-get.
                        expect_watch_prefixes(&mut new_stream).await;
                        (interface_id, config, new_stream)
                    },
                )
                .map(|(interface_id, config, new_stream)| {
                    let interface_id = interface_id
                        .expect("interface ID missing in new DHCPv6 client request")
                        .try_into()
                        .expect("interface ID should be nonzero");
                    let TestInterfaceState {
                        _control_server_end,
                        dhcpv6_client_request_stream,
                        kind: _,
                    } = interface_states.get_mut(&interface_id).unwrap_or_else(|| {
                        panic!("interface {} must be present in map", interface_id)
                    });
                    assert!(
                        std::mem::replace(dhcpv6_client_request_stream, Some(new_stream)).is_none()
                    );

                    assert_eq!(
                        config,
                        Some(fnet_dhcpv6::ClientConfig {
                            information_config: Some(fnet_dhcpv6::InformationConfig {
                                dns_servers: Some(true),
                                ..Default::default()
                            }),
                            prefix_delegation_config: want_pd_config.clone(),
                            ..Default::default()
                        })
                    );
                    interface_id
                })
                .collect()
                .await
        }

        async fn handle_watch_prefix_with_fake(
            netcfg: &mut NetCfg<'_>,
            dns_watchers: &mut DnsServerWatchers<'_>,
            interface_id: NonZeroU64,
            fake_prefixes: Vec<fnet_dhcpv6::Prefix>,
            // Used to test handling PrefixControl.WatchPrefix & Client.WatchPrefixes
            // events in different orders.
            handle_dhcpv6_prefixes_before_watch_prefix: bool,
        ) {
            let responder = netcfg
                .dhcpv6_prefix_provider_handler
                .as_mut()
                .map(dhcpv6::PrefixProviderHandler::try_next_prefix_control_request)
                .expect("DHCPv6 prefix provider handler must be present")
                .await
                .expect("prefix provider request")
                .expect("PrefixProvider request stream exhausted")
                .into_watch_prefix()
                .expect("request not WatchPrefix");

            async fn handle_dhcpv6_prefixes(
                netcfg: &mut NetCfg<'_>,
                dns_watchers: &mut DnsServerWatchers<'_>,
                interface_id: NonZeroU64,
                fake_prefixes: Vec<fnet_dhcpv6::Prefix>,
            ) {
                netcfg
                    .handle_dhcpv6_prefixes(interface_id, Ok(fake_prefixes), dns_watchers)
                    .await
                    .expect("handle DHCPv6 prefixes")
            }

            async fn handle_watch_prefix(
                netcfg: &mut NetCfg<'_>,
                dns_watchers: &mut DnsServerWatchers<'_>,
                responder: fnet_dhcpv6::PrefixControlWatchPrefixResponder,
            ) {
                netcfg
                    .handle_watch_prefix(responder, dns_watchers)
                    .await
                    .expect("handle watch prefix")
            }

            if handle_dhcpv6_prefixes_before_watch_prefix {
                handle_dhcpv6_prefixes(netcfg, dns_watchers, interface_id, fake_prefixes).await;
                handle_watch_prefix(netcfg, dns_watchers, responder).await;
            } else {
                handle_watch_prefix(netcfg, dns_watchers, responder).await;
                handle_dhcpv6_prefixes(netcfg, dns_watchers, interface_id, fake_prefixes).await;
            }
        }

        // Making an AcquirePrefix call should trigger restarting DHCPv6 w/ PD.
        let (prefix_control, server_end) =
            fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
                .expect("create fuchsia.net.dhcpv6/PrefixControl endpoints");
        let mut lookup_admin_fut = lookup_admin_request_stream
            .try_for_each(|req| {
                let (_, responder): (Vec<fnet::SocketAddress>, _) =
                    req.into_set_dns_servers().expect("request must be SetDnsServers");
                responder.send(&mut Ok(())).expect("send SetDnsServers response");
                futures::future::ok(())
            })
            .fuse();

        {
            let acquire_prefix_fut = netcfg.handle_dhcpv6_acquire_prefix(
                fnet_dhcpv6::AcquirePrefixConfig {
                    interface_id: interface_id.map(NonZeroU64::get),
                    preferred_prefix_len,
                    ..Default::default()
                },
                server_end,
                &mut dns_watchers,
            );
            futures::select! {
                res = acquire_prefix_fut.fuse() => {
                    res.expect("acquire DHCPv6 prefix")
                }
                res = lookup_admin_fut => {
                    panic!("fuchsia.net.name/LookupAdmin request stream exhausted unexpectedly: {:?}", res)
                },
            };
            // Expect DHCPv6 client to have been restarted on the appropriate
            // interfaces with PD configured.
            let stopped = assert_dhcpv6_clients_stopped(&mut interface_states, interface_id).await;
            let started = assert_dhcpv6_clients_started(
                stopped.len(),
                dhcpv6_client_provider_request_stream.by_ref(),
                &mut interface_states,
                Some(preferred_prefix_len.map_or(
                    fnet_dhcpv6::PrefixDelegationConfig::Empty(fnet_dhcpv6::Empty),
                    fnet_dhcpv6::PrefixDelegationConfig::PrefixLength,
                )),
            )
            .await;
            assert_eq!(started, stopped);

            // Yield a prefix to the PrefixControl.
            let prefix = fnet_dhcpv6::Prefix {
                prefix: fidl_ip_v6_with_prefix!("abcd::/64"),
                lifetimes: fnet_dhcpv6::Lifetimes { valid_until: 123, preferred_until: 456 },
            };
            let any_eligible_interface = *started.iter().next().expect(
                "must have configured DHCPv6 client to perform PD on at least one interface",
            );
            let (watch_prefix_res, ()) = futures::future::join(
                prefix_control.watch_prefix(),
                handle_watch_prefix_with_fake(
                    &mut netcfg,
                    &mut dns_watchers,
                    any_eligible_interface,
                    vec![prefix.clone()],
                    true, /* handle_dhcpv6_prefixes_before_watch_prefix */
                ),
            )
            .await;
            assert_matches!(watch_prefix_res, Ok(fnet_dhcpv6::PrefixEvent::Assigned(got_prefix)) => {
                assert_eq!(got_prefix, prefix);
            });

            // Yield a different prefix from what was yielded before.
            let renewed_prefix = fnet_dhcpv6::Prefix {
                lifetimes: fnet_dhcpv6::Lifetimes {
                    valid_until: 123_123,
                    preferred_until: 456_456,
                },
                ..prefix
            };
            let (watch_prefix_res, ()) = futures::future::join(
                prefix_control.watch_prefix(),
                handle_watch_prefix_with_fake(
                    &mut netcfg,
                    &mut dns_watchers,
                    any_eligible_interface,
                    vec![renewed_prefix.clone()],
                    false, /* handle_dhcpv6_prefixes_before_watch_prefix */
                ),
            )
            .await;
            assert_matches!(watch_prefix_res, Ok(fnet_dhcpv6::PrefixEvent::Assigned(
                got_prefix,
            )) => {
                assert_eq!(got_prefix, renewed_prefix);
            });
            let (watch_prefix_res, ()) = futures::future::join(
                prefix_control.watch_prefix(),
                handle_watch_prefix_with_fake(
                    &mut netcfg,
                    &mut dns_watchers,
                    any_eligible_interface,
                    vec![],
                    true, /* handle_dhcpv6_prefixes_before_watch_prefix */
                ),
            )
            .await;
            assert_matches!(
                watch_prefix_res,
                Ok(fnet_dhcpv6::PrefixEvent::Unassigned(fnet_dhcpv6::Empty))
            );
        }

        // Closing the PrefixControl should trigger clients running DHCPv6-PD to
        // be restarted.
        {
            futures::select! {
                () = netcfg.on_dhcpv6_prefix_control_close(&mut dns_watchers).fuse() => {},
                res = lookup_admin_fut => {
                    panic!(
                        "fuchsia.net.name/LookupAdmin request stream exhausted unexpectedly: {:?}",
                        res,
                    )
                },
            }
            let stopped = assert_dhcpv6_clients_stopped(&mut interface_states, interface_id).await;
            let started = assert_dhcpv6_clients_started(
                stopped.len(),
                dhcpv6_client_provider_request_stream.by_ref(),
                &mut interface_states,
                None,
            )
            .await;
            assert_eq!(started, stopped);
        }
    }

    // Tests that DHCPv6 clients are configured to perform PD on eligible
    // upstream-providing interfaces while a `PrefixControl` channel is open.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_dhcpv6_pd_on_added_upstream() {
        let (
            mut netcfg,
            ServerEnds {
                stack: _,
                lookup_admin: _,
                dhcpv4_client_provider: _,
                dhcpv6_client_provider: mut dhcpv6_client_provider_request_stream,
            },
        ) = test_netcfg(false /* with_dhcpv4_client_provider */)
            .expect("error creating test netcfg");
        let allowed_upstream_device_classes = HashSet::from([ALLOWED_UPSTREAM_DEVICE_CLASS]);
        netcfg.allowed_upstream_device_classes = &allowed_upstream_device_classes;
        let mut dns_watchers = DnsServerWatchers::empty();

        let (_prefix_control, server_end) =
            fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
                .expect("create fuchsia.net.dhcpv6/PrefixControl endpoints");
        netcfg
            .handle_dhcpv6_acquire_prefix(
                fnet_dhcpv6::AcquirePrefixConfig::default(),
                server_end,
                &mut dns_watchers,
            )
            .await
            .expect("handle DHCPv6 acquire prefix");

        for (id, upstream) in
            [(nonzero_ext::nonzero!(1u64), true), (nonzero_ext::nonzero!(2u64), false)]
        {
            // Mock interface being discovered by NetCfg.
            let (control, _control_server_end) =
                fidl_fuchsia_net_interfaces_ext::admin::Control::create_endpoints()
                    .expect("create endpoints");
            let device_class = if upstream {
                ALLOWED_UPSTREAM_DEVICE_CLASS
            } else {
                DISALLOWED_UPSTREAM_DEVICE_CLASS
            };
            assert_matches::assert_matches!(
                netcfg.interface_states.insert(
                    id,
                    InterfaceState::new_host(
                        control,
                        device_class,
                        upstream.then_some(fnet_dhcpv6::PrefixDelegationConfig::Empty(
                            fnet_dhcpv6::Empty
                        ))
                    )
                ),
                None
            );

            let sockaddr = dhcpv6_sockaddr(id);

            // Fake an interface added event.
            let () = netcfg
                .handle_interface_watcher_event(
                    fnet_interfaces::Event::Added(fnet_interfaces::Properties {
                        id: Some(id.get()),
                        name: Some(format!("testif{}", id)),
                        device_class: Some(fnet_interfaces::DeviceClass::Device(
                            device_class.into(),
                        )),
                        online: Some(true),
                        addresses: Some(ipv6addrs(Some(sockaddr))),
                        has_default_ipv4_route: Some(false),
                        has_default_ipv6_route: Some(false),
                        ..Default::default()
                    }),
                    &mut dns_watchers,
                    &mut virtualization::Stub,
                )
                .await
                .unwrap_or_else(|e| panic!("error handling interface added event for {} with interface up and link-local addr: {:?}", id, e));

            // Expect DHCPv6 client to have started with PD configuration.
            let _: fnet_dhcpv6::ClientRequestStream = check_new_dhcpv6_client(
                &mut dhcpv6_client_provider_request_stream,
                id,
                sockaddr,
                upstream.then_some(fnet_dhcpv6::PrefixDelegationConfig::Empty(fnet_dhcpv6::Empty)),
                &mut dns_watchers,
            )
            .await
            .unwrap_or_else(|e| {
                panic!("error checking for new DHCPv6 client on interface {}: {:?}", id, e)
            });
        }
    }

    #[test]
    fn test_config() {
        let config_str = r#"
{
  "dns_config": {
    "servers": ["8.8.8.8"]
  },
  "filter_config": {
    "rules": [],
    "nat_rules": [],
    "rdr_rules": []
  },
  "filter_enabled_interface_types": ["wlan"],
  "interface_metrics": {
    "wlan_metric": 100,
    "eth_metric": 10
  },
  "allowed_upstream_device_classes": ["ethernet", "wlan"],
  "allowed_bridge_upstream_device_classes": ["ethernet"],
  "enable_dhcpv6": true,
  "forwarded_device_classes": { "ipv4": [ "ethernet" ], "ipv6": [ "wlan" ] }
}
"#;

        let Config {
            dns_config: DnsConfig { servers },
            filter_config,
            filter_enabled_interface_types,
            interface_metrics,
            allowed_upstream_device_classes,
            allowed_bridge_upstream_device_classes,
            enable_dhcpv6,
            forwarded_device_classes,
        } = Config::load_str(config_str).unwrap();

        assert_eq!(vec!["8.8.8.8".parse::<std::net::IpAddr>().unwrap()], servers);
        let FilterConfig { rules, nat_rules, rdr_rules } = filter_config;
        assert_eq!(Vec::<String>::new(), rules);
        assert_eq!(Vec::<String>::new(), nat_rules);
        assert_eq!(Vec::<String>::new(), rdr_rules);

        assert_eq!(HashSet::from([InterfaceType::Wlan]), filter_enabled_interface_types);

        let expected_metrics =
            InterfaceMetrics { wlan_metric: Metric(100), eth_metric: Metric(10) };
        assert_eq!(interface_metrics, expected_metrics);

        assert_eq!(
            AllowedDeviceClasses(HashSet::from([DeviceClass::Ethernet, DeviceClass::Wlan])),
            allowed_upstream_device_classes
        );
        assert_eq!(
            AllowedDeviceClasses(HashSet::from([DeviceClass::Ethernet])),
            allowed_bridge_upstream_device_classes
        );

        assert_eq!(enable_dhcpv6, true);

        let expected_classes = ForwardedDeviceClasses {
            ipv4: HashSet::from([DeviceClass::Ethernet]),
            ipv6: HashSet::from([DeviceClass::Wlan]),
        };
        assert_eq!(forwarded_device_classes, expected_classes);
    }

    #[test]
    fn test_config_defaults() {
        let config_str = r#"
{
  "dns_config": { "servers": [] },
  "filter_config": {
    "rules": [],
    "nat_rules": [],
    "rdr_rules": []
  },
  "filter_enabled_interface_types": []
}
"#;

        let Config {
            dns_config: _,
            filter_config: _,
            filter_enabled_interface_types: _,
            allowed_upstream_device_classes,
            allowed_bridge_upstream_device_classes,
            interface_metrics,
            enable_dhcpv6,
            forwarded_device_classes: _,
        } = Config::load_str(config_str).unwrap();

        assert_eq!(allowed_upstream_device_classes, Default::default());
        assert_eq!(allowed_bridge_upstream_device_classes, Default::default());
        assert_eq!(interface_metrics, Default::default());
        assert_eq!(enable_dhcpv6, true);
    }

    #[test_case(
        "eth_metric", Default::default(), Metric(1), Metric(1);
        "wlan assumes default metric when unspecified")]
    #[test_case("wlan_metric", Metric(1), Default::default(), Metric(1);
        "eth assumes default metric when unspecified")]
    fn test_config_metric_individual_defaults(
        metric_name: &'static str,
        wlan_metric: Metric,
        eth_metric: Metric,
        expect_metric: Metric,
    ) {
        let config_str = format!(
            r#"
{{
  "dns_config": {{ "servers": [] }},
  "filter_config": {{
    "rules": [],
    "nat_rules": [],
    "rdr_rules": []
  }},
  "filter_enabled_interface_types": [],
  "interface_metrics": {{ "{}": {} }}
}}
"#,
            metric_name, expect_metric
        );

        let Config {
            dns_config: _,
            filter_config: _,
            filter_enabled_interface_types: _,
            allowed_upstream_device_classes: _,
            allowed_bridge_upstream_device_classes: _,
            enable_dhcpv6: _,
            interface_metrics,
            forwarded_device_classes: _,
        } = Config::load_str(&config_str).unwrap();

        let expected_metrics = InterfaceMetrics { wlan_metric, eth_metric };
        assert_eq!(interface_metrics, expected_metrics);
    }

    #[test]
    fn test_config_denies_unknown_fields() {
        let config_str = r#"{
            "filter_enabled_interface_types": ["wlan"],
            "foobar": "baz"
        }"#;

        let err = Config::load_str(config_str).expect_err("config shouldn't accept unknown fields");
        let err = err.downcast::<serde_json::Error>().expect("downcast error");
        assert_eq!(err.classify(), serde_json::error::Category::Data);
        assert_eq!(err.line(), 3);
        // Ensure the error is complaining about unknown field.
        assert!(format!("{:?}", err).contains("foobar"));
    }

    #[test]
    fn test_config_denies_unknown_fields_nested() {
        let bad_configs = vec![
            r#"
{
  "dns_config": { "speling": [] },
  "filter_config": {
    "rules": [],
    "nat_rules": [],
    "rdr_rules": []
  },
  "filter_enabled_interface_types": []
}
"#,
            r#"
{
  "dns_config": { "servers": [] },
  "filter_config": {
    "speling": [],
    "nat_rules": [],
    "rdr_rules": []
  },
  "filter_enabled_interface_types": []
}
"#,
            r#"
{
  "dns_config": { "servers": [] },
  "filter_config": {
    "rules": [],
    "nat_rules": [],
    "rdr_rules": []
  },
  "filter_enabled_interface_types": ["speling"]
}
"#,
            r#"
{
  "dns_config": { "servers": [] },
  "filter_config": {
    "rules": [],
    "nat_rules": [],
    "rdr_rules": []
  },
  "interface_metrics": {
    "eth_metric": 1,
    "wlan_metric": 2,
    "speling": 3,
  },
  "filter_enabled_interface_types": []
}
"#,
            r#"
{
  "dns_config": { "servers": [] },
  "filter_config": {
    "rules": [],
    "nat_rules": [],
    "rdr_rules": []
  },
  "filter_enabled_interface_types": ["speling"]
}
"#,
            r#"
{
  "dns_config": { "servers": [] },
  "filter_config": {
    "rules": [],
    "nat_rules": [],
    "rdr_rules": []
  },
  "filter_enabled_interface_types": [],
  "allowed_upstream_device_classes": ["speling"]
}
"#,
            r#"
{
  "dns_config": { "servers": [] },
  "filter_config": {
    "rules": [],
    "nat_rules": [],
    "rdr_rules": []
  },
  "filter_enabled_interface_types": [],
  "allowed_bridge_upstream_device_classes": ["speling"]
}
"#,
            r#"
{
  "dns_config": { "servers": [] },
  "filter_config": {
    "rules": [],
    "nat_rules": [],
    "rdr_rules": []
  },
  "filter_enabled_interface_types": [],
  "allowed_upstream_device_classes": [],
  "forwarded_device_classes": { "ipv4": [], "ipv6": [], "speling": [] }
}
"#,
        ];

        for config_str in bad_configs {
            let err =
                Config::load_str(config_str).expect_err("config shouldn't accept unknown fields");
            let err = err.downcast::<serde_json::Error>().expect("downcast error");
            assert_eq!(err.classify(), serde_json::error::Category::Data);
            // Ensure the error is complaining about unknown field.
            assert!(format!("{:?}", err).contains("speling"));
        }
    }

    #[test]
    fn test_should_enable_filter() {
        let types_empty: HashSet<InterfaceType> = [].iter().cloned().collect();
        let types_ethernet: HashSet<InterfaceType> =
            [InterfaceType::Ethernet].iter().cloned().collect();
        let types_wlan: HashSet<InterfaceType> = [InterfaceType::Wlan].iter().cloned().collect();

        let make_info =
            |device_class| DeviceInfo { device_class, mac: None, topological_path: "".to_string() };

        let wlan_info = make_info(fidl_fuchsia_hardware_network::DeviceClass::Wlan);
        let wlan_ap_info = make_info(fidl_fuchsia_hardware_network::DeviceClass::WlanAp);
        let ethernet_info = make_info(fidl_fuchsia_hardware_network::DeviceClass::Ethernet);

        assert_eq!(should_enable_filter(&types_empty, &wlan_info), false);
        assert_eq!(should_enable_filter(&types_empty, &wlan_ap_info), false);
        assert_eq!(should_enable_filter(&types_empty, &ethernet_info), false);

        assert_eq!(should_enable_filter(&types_ethernet, &wlan_info), false);
        assert_eq!(should_enable_filter(&types_ethernet, &wlan_ap_info), false);
        assert_eq!(should_enable_filter(&types_ethernet, &ethernet_info), true);

        assert_eq!(should_enable_filter(&types_wlan, &wlan_info), true);
        assert_eq!(should_enable_filter(&types_wlan, &wlan_ap_info), true);
        assert_eq!(should_enable_filter(&types_wlan, &ethernet_info), false);
    }
}
