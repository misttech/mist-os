// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Provides workers and data structures for netstack-wide settings.

use futures::TryStreamExt as _;
use log::error;
use netstack3_core::types::SynchronizedWriterRcu;
use {
    fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin,
    fidl_fuchsia_net_settings as fnet_settings,
};

use crate::bindings::interface_config::{
    DeviceNeighborConfig, InterfaceConfig, InterfaceConfigDefaults,
};
use crate::bindings::util::{ErrorLogExt, ResultExt as _};

pub(crate) async fn serve_control(
    mut rs: fnet_settings::ControlRequestStream,
) -> Result<(), fidl::Error> {
    while let Some(req) = rs.try_next().await? {
        match req {
            fnet_settings::ControlRequest::UpdateInterfaceDefaults { payload, responder } => {
                let r = update_interface_defaults(payload);
                r.log_if_err("settings::UpdateInterfaceDefaults");
                responder.send(r.borrowed_ok())?;
            }
            fnet_settings::ControlRequest::UpdateTcp { payload, responder } => {
                let r = update_tcp(payload);
                r.log_if_err("settings::UpdateTcp");
                responder.send(r.borrowed_ok())?;
            }
            fnet_settings::ControlRequest::UpdateUdp { payload, responder } => {
                let r = update_udp(payload);
                r.log_if_err("settings::UpdateUdp");
                responder.send(r.borrowed_ok())?;
            }
            fnet_settings::ControlRequest::UpdateIcmp { payload, responder } => {
                let r = update_icmp(payload);
                r.log_if_err("settings::UpdateIcmp");
                responder.send(r.borrowed_ok())?;
            }
            fnet_settings::ControlRequest::UpdateIp { payload, responder } => {
                let r = update_ip(payload);
                r.log_if_err("settings::UpdateIp");
                responder.send(r.borrowed_ok())?;
            }
            fnet_settings::ControlRequest::UpdateDevice { payload, responder } => {
                let r = update_device(payload);
                r.log_if_err("settings::UpdateDevice");
                responder.send(r.borrowed_ok())?;
            }
        }
    }
    Ok(())
}

impl ErrorLogExt for fnet_settings::UpdateError {
    fn log_level(&self) -> log::Level {
        log::Level::Warn
    }
}

fn update_interface_defaults(
    _interfaces: fnet_interfaces_admin::Configuration,
) -> Result<fnet_interfaces_admin::Configuration, fnet_settings::UpdateError> {
    error!("TODO(https://fxbug.dev/425936078): UpdateInterfaceDefaults not supported");
    Ok(Default::default())
}

fn update_tcp(tcp: fnet_settings::Tcp) -> Result<fnet_settings::Tcp, fnet_settings::UpdateError> {
    let fnet_settings::Tcp { buffer_sizes, __source_breaking } = tcp;
    let prev = fnet_settings::Tcp::default();
    if let Some(_buffer_sizes) = buffer_sizes {
        error!("TODO(https://fxbug.dev/425935644): Implement TCP buffer sizes setting");
    }
    Ok(prev)
}

fn update_udp(udp: fnet_settings::Udp) -> Result<fnet_settings::Udp, fnet_settings::UpdateError> {
    let fnet_settings::Udp { buffer_sizes, __source_breaking } = udp;
    let prev = fnet_settings::Udp::default();
    if let Some(_buffer_sizes) = buffer_sizes {
        error!("TODO(https://fxbug.dev/425935644): Implement UDP buffer sizes setting");
    }
    Ok(prev)
}

fn update_icmp(
    icmp: fnet_settings::Icmp,
) -> Result<fnet_settings::Icmp, fnet_settings::UpdateError> {
    let fnet_settings::Icmp { echo_buffer_sizes, icmpv4, icmpv6, __source_breaking } = icmp;
    let prev = fnet_settings::Icmp::default();
    if let Some(_buffer_sizes) = echo_buffer_sizes {
        error!("TODO(https://fxbug.dev/425935644): Implement ICMP echo buffer sizes setting");
    }
    if let Some(fnet_settings::Icmpv4 { __source_breaking }) = icmpv4 {}
    if let Some(fnet_settings::Icmpv6 { __source_breaking }) = icmpv6 {}
    Ok(prev)
}

fn update_ip(ip: fnet_settings::Ip) -> Result<fnet_settings::Ip, fnet_settings::UpdateError> {
    let fnet_settings::Ip { raw_buffer_sizes, ipv4, ipv6, __source_breaking } = ip;
    let prev = fnet_settings::Ip::default();
    if let Some(_buffer_sizes) = raw_buffer_sizes {
        error!("TODO(https://fxbug.dev/425935644): Implement raw IP buffer sizes setting");
    }
    if let Some(fnet_settings::Ipv4 { __source_breaking }) = ipv4 {}
    if let Some(fnet_settings::Ipv6 { __source_breaking }) = ipv6 {}
    Ok(prev)
}

fn update_device(
    device: fnet_settings::Device,
) -> Result<fnet_settings::Device, fnet_settings::UpdateError> {
    let fnet_settings::Device { packet_buffer_sizes, __source_breaking } = device;
    let prev = fnet_settings::Device::default();
    if let Some(_buffer_sizes) = packet_buffer_sizes {
        error!("TODO(https://fxbug.dev/425935644): Implement packet buffer sizes setting");
    }
    Ok(prev)
}

pub(crate) async fn serve_state(
    mut rs: fnet_settings::StateRequestStream,
) -> Result<(), fidl::Error> {
    while let Some(req) = rs.try_next().await? {
        match req {
            fnet_settings::StateRequest::GetInterfaceDefaults { responder } => {
                responder.send(&get_interface_defaults())?;
            }
            fnet_settings::StateRequest::GetTcp { responder } => {
                responder.send(&get_tcp())?;
            }
            fnet_settings::StateRequest::GetUdp { responder } => {
                responder.send(&get_udp())?;
            }
            fnet_settings::StateRequest::GetIcmp { responder } => {
                responder.send(&get_icmp())?;
            }
            fnet_settings::StateRequest::GetIp { responder } => {
                responder.send(&get_ip())?;
            }
            fnet_settings::StateRequest::GetDevice { responder } => {
                responder.send(&get_device())?;
            }
        }
    }
    Ok(())
}

fn get_interface_defaults() -> fnet_interfaces_admin::Configuration {
    error!("TODO(https://fxbug.dev/425936078): implement GetInterfaceDefaults");
    Default::default()
}

fn get_tcp() -> fnet_settings::Tcp {
    error!("TODO(https://fxbug.dev/425935644): implement GetTcp buffer sizes");
    let buffer_sizes = Default::default();
    fnet_settings::Tcp {
        buffer_sizes: Some(buffer_sizes),
        __source_breaking: fidl::marker::SourceBreaking,
    }
}

fn get_udp() -> fnet_settings::Udp {
    error!("TODO(https://fxbug.dev/425935644): implement GetUdp buffer sizes");
    let buffer_sizes = Default::default();
    fnet_settings::Udp {
        buffer_sizes: Some(buffer_sizes),
        __source_breaking: fidl::marker::SourceBreaking,
    }
}

fn get_icmp() -> fnet_settings::Icmp {
    error!("TODO(https://fxbug.dev/425935644): implement GetIcmp buffer sizes");
    let echo_buffer_sizes = Default::default();
    let icmpv4 = fnet_settings::Icmpv4 { __source_breaking: fidl::marker::SourceBreaking };
    let icmpv6 = fnet_settings::Icmpv6 { __source_breaking: fidl::marker::SourceBreaking };
    fnet_settings::Icmp {
        echo_buffer_sizes: Some(echo_buffer_sizes),
        icmpv4: Some(icmpv4),
        icmpv6: Some(icmpv6),
        __source_breaking: fidl::marker::SourceBreaking,
    }
}

fn get_ip() -> fnet_settings::Ip {
    error!("TODO(https://fxbug.dev/425935644): implement GetIp buffer sizes");
    let raw_buffer_sizes = Default::default();
    let ipv4 = fnet_settings::Ipv4 { __source_breaking: fidl::marker::SourceBreaking };
    let ipv6 = fnet_settings::Ipv6 { __source_breaking: fidl::marker::SourceBreaking };
    fnet_settings::Ip {
        raw_buffer_sizes: Some(raw_buffer_sizes),
        ipv4: Some(ipv4),
        ipv6: Some(ipv6),
        __source_breaking: fidl::marker::SourceBreaking,
    }
}

fn get_device() -> fnet_settings::Device {
    error!("TODO(https://fxbug.dev/425935644): implement GetDevice buffer sizes");
    let packet_buffer_sizes = Default::default();
    fnet_settings::Device {
        packet_buffer_sizes: Some(packet_buffer_sizes),
        __source_breaking: fidl::marker::SourceBreaking,
    }
}

pub(crate) struct Settings {
    pub(crate) interface_defaults: SynchronizedWriterRcu<InterfaceConfig<DeviceNeighborConfig>>,
}

impl Settings {
    pub(crate) fn new(interface: &InterfaceConfigDefaults) -> Self {
        Self { interface_defaults: SynchronizedWriterRcu::new(InterfaceConfig::new(interface)) }
    }
}
