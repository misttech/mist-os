// Copyright 2021 The Fuchsia Authors. All rights 1eserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::{anyhow, bail, Result};
use manual_targets::watcher::{ManualTargetEvent, ManualTargetState};
use std::fmt;
use std::fmt::Display;
use usb_fastboot_discovery::FastbootEvent;
// TODO(colnnelson): Long term it would be nice to have this be pulled into the mDNS library
// so that it can speak our language. Or even have the mdns library not export FIDL structs
// but rather some other well-defined type
use fidl_fuchsia_developer_ffx as ffx;

#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum FastbootConnectionState {
    Usb,
    Tcp(Vec<TargetAddr>),
    Udp(Vec<TargetAddr>),
}

impl Display for FastbootConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let res = match self {
            Self::Usb => format!("Usb"),
            Self::Tcp(addr) => format!("Tcp({:?})", addr),
            Self::Udp(addr) => format!("Udp({:?})", addr),
        };
        write!(f, "{}", res)
    }
}

#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct FastbootTargetState {
    pub serial_number: String,
    pub connection_state: FastbootConnectionState,
}

impl Display for FastbootTargetState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.serial_number, self.connection_state)
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum TargetState {
    Unknown,
    Product(Vec<TargetAddr>),
    Fastboot(FastbootTargetState),
    Zedboot,
}

impl Display for TargetState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let res = match self {
            TargetState::Unknown => "Unknown".to_string(),
            TargetState::Product(addr) => format!("Product({:?})", addr),
            TargetState::Fastboot(state) => format!("Fastboot({:?})", state),
            TargetState::Zedboot => "Zedboot".to_string(),
        };
        write!(f, "{}", res)
    }
}

#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct TargetHandle {
    pub node_name: Option<String>,
    pub state: TargetState,
}

impl Display for TargetHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = self.node_name.clone().unwrap_or_else(|| "".to_string());
        write!(f, "Node: \"{}\" in state: {}", name, self.state)
    }
}

/// Target discovery events. See `wait_for_devices`.
#[derive(Debug, PartialEq, Eq, Hash)]
pub enum TargetEvent {
    /// Indicates a Target has been discovered.
    Added(TargetHandle),
    /// Indicates a Target has been lost.
    Removed(TargetHandle),
}

pub(crate) fn target_event_from_mdns_event(
    event: ffx::MdnsEventType,
) -> Option<Result<TargetEvent>> {
    match event {
        ffx::MdnsEventType::SocketBound(_) => {
            // Unsupported
            None
        }
        e @ _ => {
            let converted = TargetEvent::try_from(e);
            match converted {
                Ok(m) => Some(Ok(m)),
                Err(_) => None,
            }
        }
    }
}

impl TryFrom<ffx::MdnsEventType> for TargetEvent {
    type Error = anyhow::Error;

    fn try_from(event: ffx::MdnsEventType) -> Result<Self, Self::Error> {
        match event {
            ffx::MdnsEventType::TargetFound(info) => {
                let handle: TargetHandle = info.try_into()?;
                Ok(TargetEvent::Added(handle))
            }
            ffx::MdnsEventType::TargetRediscovered(info) => {
                let handle: TargetHandle = info.try_into()?;
                Ok(TargetEvent::Added(handle))
            }
            ffx::MdnsEventType::TargetExpired(info) => {
                let handle: TargetHandle = info.try_into()?;
                Ok(TargetEvent::Removed(handle))
            }
            ffx::MdnsEventType::SocketBound(_) => {
                anyhow::bail!("SocketBound events are not supported")
            }
        }
    }
}

impl TryFrom<emulator_instance::EmulatorTargetAction> for TargetEvent {
    type Error = anyhow::Error;

    fn try_from(event: emulator_instance::EmulatorTargetAction) -> Result<Self, Self::Error> {
        match event {
            emulator_instance::EmulatorTargetAction::Add(info) => {
                let handle: TargetHandle = info.try_into()?;
                Ok(TargetEvent::Added(handle))
            }
            emulator_instance::EmulatorTargetAction::Remove(info) => {
                let handle: TargetHandle = info.try_into()?;
                Ok(TargetEvent::Removed(handle))
            }
        }
    }
}

impl TryFrom<ffx::TargetInfo> for TargetHandle {
    type Error = anyhow::Error;

    fn try_from(info: ffx::TargetInfo) -> Result<Self, Self::Error> {
        let addresses = info.addresses.ok_or_else(|| anyhow!("Addresses are populated"))?;
        // Get the TargetAddrs
        let mut addrs: Vec<_> = addresses.into_iter().map(TargetAddr::from).collect();
        // Sorting them this way put ipv6 above ipv4
        addrs.sort_by(|a, b| b.cmp(a));

        if addrs.is_empty() {
            bail!("There must be at least one target address")
        }

        let state = match info.fastboot_interface {
            None => TargetState::Product(addrs),
            Some(iface) => {
                let serial_number = info.serial_number.unwrap_or_else(|| "".to_string());
                let connection_state = match iface {
                    ffx::FastbootInterface::Usb => FastbootConnectionState::Usb,
                    ffx::FastbootInterface::Udp => FastbootConnectionState::Udp(addrs),
                    ffx::FastbootInterface::Tcp => FastbootConnectionState::Tcp(addrs),
                };
                TargetState::Fastboot(FastbootTargetState { serial_number, connection_state })
            }
        };
        Ok(TargetHandle { node_name: info.nodename, state })
    }
}

impl From<FastbootEvent> for TargetEvent {
    fn from(fastboot_event: FastbootEvent) -> Self {
        match fastboot_event {
            FastbootEvent::Discovered(serial) => {
                let handle = TargetHandle {
                    node_name: Some("".to_string()),
                    state: TargetState::Fastboot(FastbootTargetState {
                        serial_number: serial,
                        connection_state: FastbootConnectionState::Usb,
                    }),
                };
                TargetEvent::Added(handle)
            }
            FastbootEvent::Lost(serial) => {
                let handle = TargetHandle {
                    node_name: Some("".to_string()),
                    state: TargetState::Fastboot(FastbootTargetState {
                        serial_number: serial,
                        connection_state: FastbootConnectionState::Usb,
                    }),
                };
                TargetEvent::Removed(handle)
            }
        }
    }
}

impl From<ManualTargetEvent> for TargetEvent {
    fn from(manual_target_event: ManualTargetEvent) -> Self {
        match manual_target_event {
            ManualTargetEvent::Discovered(manual_target, manual_state) => {
                let state = match manual_state {
                    ManualTargetState::Disconnected => TargetState::Unknown,
                    ManualTargetState::Product => {
                        TargetState::Product(vec![manual_target.addr().into()])
                    }
                    ManualTargetState::Fastboot => TargetState::Fastboot(FastbootTargetState {
                        serial_number: "".to_string(),
                        connection_state: FastbootConnectionState::Tcp(vec![manual_target
                            .addr()
                            .into()]),
                    }),
                };

                let handle =
                    TargetHandle { node_name: Some(manual_target.addr().to_string()), state };
                TargetEvent::Added(handle)
            }
            ManualTargetEvent::Lost(manual_target) => {
                let handle = TargetHandle {
                    node_name: Some(manual_target.addr().to_string()),
                    state: TargetState::Unknown,
                };
                TargetEvent::Removed(handle)
            }
        }
    }
}

impl From<fastboot_file_discovery::FastbootEvent> for TargetEvent {
    fn from(fastboot_event: fastboot_file_discovery::FastbootEvent) -> Self {
        match fastboot_event {
            fastboot_file_discovery::FastbootEvent::Discovered(device) => {
                let address: TargetAddr = device.socket_addr().into();
                let connection_state = match device.mode() {
                    fastboot_file_discovery::FastbootMode::UDP => {
                        FastbootConnectionState::Udp(vec![address])
                    }
                    fastboot_file_discovery::FastbootMode::TCP => {
                        FastbootConnectionState::Tcp(vec![address])
                    }
                };

                let handle = TargetHandle {
                    node_name: None,
                    state: TargetState::Fastboot(FastbootTargetState {
                        serial_number: "".to_string(),
                        connection_state,
                    }),
                };
                TargetEvent::Added(handle)
            }
            fastboot_file_discovery::FastbootEvent::Lost(device) => {
                let address: TargetAddr = device.socket_addr().into();
                let connection_state = match device.mode() {
                    fastboot_file_discovery::FastbootMode::UDP => {
                        FastbootConnectionState::Udp(vec![address])
                    }
                    fastboot_file_discovery::FastbootMode::TCP => {
                        FastbootConnectionState::Tcp(vec![address])
                    }
                };
                let handle = TargetHandle {
                    node_name: Some("".to_string()),
                    state: TargetState::Fastboot(FastbootTargetState {
                        serial_number: "".to_string(),
                        connection_state,
                    }),
                };
                TargetEvent::Removed(handle)
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use addr::TargetAddr;
    use manual_targets::watcher::ManualTarget;
    use pretty_assertions::assert_eq;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
    use std::str::FromStr;

    #[test]
    fn test_from_fastbootevent_for_targetevent() -> Result<()> {
        {
            let f = FastbootEvent::Lost("1234".to_string());
            let t = TargetEvent::from(f);
            assert_eq!(
                t,
                TargetEvent::Removed(TargetHandle {
                    node_name: Some("".to_string()),
                    state: TargetState::Fastboot(FastbootTargetState {
                        serial_number: "1234".to_string(),
                        connection_state: FastbootConnectionState::Usb,
                    }),
                })
            );
        }

        {
            let f = FastbootEvent::Discovered("1234".to_string());
            let t = TargetEvent::from(f);
            assert_eq!(
                t,
                TargetEvent::Added(TargetHandle {
                    node_name: Some("".to_string()),
                    state: TargetState::Fastboot(FastbootTargetState {
                        serial_number: "1234".to_string(),
                        connection_state: FastbootConnectionState::Usb,
                    }),
                })
            );
        }
        Ok(())
    }

    #[test]
    fn test_try_from_targetinfo_for_targethandle() -> Result<()> {
        {
            let info: ffx::TargetInfo = Default::default();
            assert!(TargetHandle::try_from(info).is_err());
        }
        {
            let info = ffx::TargetInfo { nodename: Some("foo".to_string()), ..Default::default() };
            assert!(TargetHandle::try_from(info).is_err());
        }
        {
            let info = ffx::TargetInfo {
                nodename: Some("foo".to_string()),
                addresses: Some(vec![]),
                ..Default::default()
            };
            assert!(TargetHandle::try_from(info).is_err());
        }
        {
            let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
            let addr = TargetAddr::from(socket);
            let addr_info: ffx::TargetAddrInfo = addr.into();
            let info = ffx::TargetInfo {
                nodename: Some("foo".to_string()),
                addresses: Some(vec![addr_info]),
                ..Default::default()
            };
            assert_eq!(
                TargetHandle::try_from(info)?,
                TargetHandle {
                    node_name: Some("foo".to_string()),
                    state: TargetState::Product(vec![addr])
                }
            );
        }
        {
            let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
            let addr = TargetAddr::from(socket);
            let addr_info: ffx::TargetAddrInfo = addr.into();
            let info = ffx::TargetInfo {
                nodename: Some("foo".to_string()),
                addresses: Some(vec![addr_info]),
                fastboot_interface: Some(ffx::FastbootInterface::Udp),
                ..Default::default()
            };
            assert_eq!(
                TargetHandle::try_from(info)?,
                TargetHandle {
                    node_name: Some("foo".to_string()),
                    state: TargetState::Fastboot(FastbootTargetState {
                        serial_number: "".to_string(),
                        connection_state: FastbootConnectionState::Udp(vec![addr])
                    })
                }
            );
        }
        {
            let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
            let addr = TargetAddr::from(socket);
            let addr_info: ffx::TargetAddrInfo = addr.into();
            let info = ffx::TargetInfo {
                nodename: Some("foo".to_string()),
                addresses: Some(vec![addr_info]),
                fastboot_interface: Some(ffx::FastbootInterface::Tcp),
                ..Default::default()
            };
            assert_eq!(
                TargetHandle::try_from(info)?,
                TargetHandle {
                    node_name: Some("foo".to_string()),
                    state: TargetState::Fastboot(FastbootTargetState {
                        serial_number: "".to_string(),
                        connection_state: FastbootConnectionState::Tcp(vec![addr])
                    })
                }
            );
        }
        {
            let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
            let addr = TargetAddr::from(socket);
            let addr_info: ffx::TargetAddrInfo = addr.into();
            let info = ffx::TargetInfo {
                nodename: Some("foo".to_string()),
                addresses: Some(vec![addr_info]),
                fastboot_interface: Some(ffx::FastbootInterface::Usb),
                ..Default::default()
            };
            assert_eq!(
                TargetHandle::try_from(info)?,
                TargetHandle {
                    node_name: Some("foo".to_string()),
                    state: TargetState::Fastboot(FastbootTargetState {
                        serial_number: "".to_string(),
                        connection_state: FastbootConnectionState::Usb
                    })
                }
            );
        }
        Ok(())
    }

    #[test]
    fn test_from_mdnseventtype_for_targetevent() -> Result<()> {
        {
            //SocketBound is not supported
            let mdns_event = ffx::MdnsEventType::SocketBound(Default::default());
            assert!(TargetEvent::try_from(mdns_event).is_err());
        }
        {
            let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
            let addr = TargetAddr::from(socket);
            let addr_info: ffx::TargetAddrInfo = addr.into();
            let info = ffx::TargetInfo {
                nodename: Some("foo".to_string()),
                addresses: Some(vec![addr_info]),
                ..Default::default()
            };
            let mdns_event = ffx::MdnsEventType::TargetFound(info);
            assert_eq!(
                TargetEvent::try_from(mdns_event)?,
                TargetEvent::Added(TargetHandle {
                    node_name: Some("foo".to_string()),
                    state: TargetState::Product(vec![addr])
                })
            );
        }
        {
            let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
            let addr = TargetAddr::from(socket);
            let addr_info: ffx::TargetAddrInfo = addr.into();
            let info = ffx::TargetInfo {
                nodename: Some("foo".to_string()),
                addresses: Some(vec![addr_info]),
                ..Default::default()
            };
            let mdns_event = ffx::MdnsEventType::TargetRediscovered(info);
            assert_eq!(
                TargetEvent::try_from(mdns_event)?,
                TargetEvent::Added(TargetHandle {
                    node_name: Some("foo".to_string()),
                    state: TargetState::Product(vec![addr])
                })
            );
        }
        {
            let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
            let addr = TargetAddr::from(socket);
            let addr_info: ffx::TargetAddrInfo = addr.into();
            let info = ffx::TargetInfo {
                nodename: Some("foo".to_string()),
                addresses: Some(vec![addr_info]),
                ..Default::default()
            };
            let mdns_event = ffx::MdnsEventType::TargetExpired(info);
            assert_eq!(
                TargetEvent::try_from(mdns_event)?,
                TargetEvent::Removed(TargetHandle {
                    node_name: Some("foo".to_string()),
                    state: TargetState::Product(vec![addr])
                })
            );
        }
        Ok(())
    }

    #[test]
    fn test_from_emulatoreventtype_for_targetevent() -> Result<()> {
        let addr = TargetAddr::from_str("127.0.0.1:8080")?;
        {
            let addr_info: ffx::TargetAddrInfo = addr.into();
            let info = ffx::TargetInfo {
                nodename: Some("foo".to_string()),
                addresses: Some(vec![addr_info]),
                ..Default::default()
            };
            let emulator_event = emulator_instance::EmulatorTargetAction::Add(info);
            assert_eq!(
                TargetEvent::try_from(emulator_event)?,
                TargetEvent::Added(TargetHandle {
                    node_name: Some("foo".to_string()),
                    state: TargetState::Product(vec![addr])
                })
            );
        }
        {
            let addr_info: ffx::TargetAddrInfo = addr.into();
            let info = ffx::TargetInfo {
                nodename: Some("foo".to_string()),
                addresses: Some(vec![addr_info]),
                ..Default::default()
            };
            let emulator_event = emulator_instance::EmulatorTargetAction::Remove(info);
            assert_eq!(
                TargetEvent::try_from(emulator_event)?,
                TargetEvent::Removed(TargetHandle {
                    node_name: Some("foo".to_string()),
                    state: TargetState::Product(vec![addr])
                })
            );
        }
        Ok(())
    }

    #[test]
    fn test_from_manual_target_event_for_target_event() -> Result<()> {
        {
            let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
            let lifetime = None;
            let manual_target_event = ManualTargetEvent::Discovered(
                ManualTarget::new(addr, lifetime),
                ManualTargetState::Product,
            );
            assert_eq!(
                TargetEvent::from(manual_target_event),
                TargetEvent::Added(TargetHandle {
                    node_name: Some("127.0.0.1:8080".to_string()),
                    state: TargetState::Product(vec![addr.into()]),
                })
            );
        }
        {
            let addr = SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1),
                8023,
                0,
                0,
            ));
            let lifetime = None;
            let manual_target_event = ManualTargetEvent::Discovered(
                ManualTarget::new(addr, lifetime),
                ManualTargetState::Product,
            );
            assert_eq!(
                TargetEvent::from(manual_target_event),
                TargetEvent::Added(TargetHandle {
                    node_name: Some("[::1]:8023".to_string()),
                    state: TargetState::Product(vec![addr.into()]),
                })
            );
        }
        {
            let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
            let lifetime = None;
            let manual_target_event = ManualTargetEvent::Discovered(
                ManualTarget::new(addr, lifetime),
                ManualTargetState::Fastboot,
            );
            assert_eq!(
                TargetEvent::from(manual_target_event),
                TargetEvent::Added(TargetHandle {
                    node_name: Some("127.0.0.1:8080".to_string()),
                    state: TargetState::Fastboot(FastbootTargetState {
                        serial_number: "".to_string(),
                        connection_state: FastbootConnectionState::Tcp(vec![addr.into()])
                    }),
                })
            );
        }
        {
            let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
            let lifetime = None;
            let manual_target_event = ManualTargetEvent::Lost(ManualTarget::new(addr, lifetime));
            assert_eq!(
                TargetEvent::from(manual_target_event),
                TargetEvent::Removed(TargetHandle {
                    node_name: Some("127.0.0.1:8080".to_string()),
                    state: TargetState::Unknown,
                })
            );
        }
        Ok(())
    }
}
