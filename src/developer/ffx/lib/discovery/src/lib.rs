// Copyright 2021 The Fuchsia Authors. All rights 1eserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::{anyhow, Result};
use bitflags::bitflags;
use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use futures::Stream;
use manual_targets::watcher::{
    recommended_watcher as manual_recommended_watcher, ManualTargetEvent, ManualTargetState,
    ManualTargetWatcher,
};
use mdns_discovery::{recommended_watcher, MdnsWatcher};
use std::fmt;
use std::task::{ready, Context, Poll};
use std::{fmt::Display, pin::Pin};
use usb_fastboot_discovery::{
    recommended_watcher as fastboot_watcher, FastbootEvent, FastbootUsbWatcher,
};
// TODO(colnnelson): Long term it would be nice to have this be pulled into the mDNS library
// so that it can speak our language. Or even have the mdns library not export FIDL structs
// but rather some other well-defined type
use fidl_fuchsia_developer_ffx as ffx;

#[allow(dead_code)]
#[derive(Debug, PartialEq)]
pub enum FastbootConnectionState {
    Usb(String),
    Tcp(TargetAddr),
    Udp(TargetAddr),
}

impl Display for FastbootConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let res = match self {
            Self::Usb(s) => format!("Usb({})", s),
            Self::Tcp(addr) => format!("Tcp({})", addr),
            Self::Udp(addr) => format!("Udp({})", addr),
        };
        write!(f, "{}", res)
    }
}

#[allow(dead_code)]
#[derive(Debug, PartialEq)]
pub struct FastbootTargetState {
    pub serial_number: String,
    pub connection_state: FastbootConnectionState,
}

impl Display for FastbootTargetState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.serial_number, self.connection_state)
    }
}

#[derive(Debug, PartialEq)]
pub enum TargetState {
    Unknown,
    Product(TargetAddr),
    Fastboot(FastbootTargetState),
    Zedboot,
}

impl Display for TargetState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let res = match self {
            TargetState::Unknown => "Unknown".to_string(),
            TargetState::Product(addr) => format!("Product({})", addr),
            TargetState::Fastboot(state) => format!("Fastboot({})", state),
            TargetState::Zedboot => "Zedboot".to_string(),
        };
        write!(f, "{}", res)
    }
}

#[allow(dead_code)]
#[derive(Debug, PartialEq)]
pub struct TargetHandle {
    pub node_name: Option<String>,
    pub state: TargetState,
}

impl Display for TargetHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = self.node_name.clone().unwrap_or("".to_string());
        write!(f, "Node: \"{}\" in state: {}", name, self.state)
    }
}

/// Target discovery events. See `wait_for_devices`.
#[derive(Debug, PartialEq)]
pub enum TargetEvent {
    /// Indicates a Target has been discovered.
    Added(TargetHandle),
    /// Indicates a Target has been lost.
    Removed(TargetHandle),
}

#[allow(dead_code)]
/// A stream of new devices as they appear on the bus. See [`wait_for_devices`].
pub struct TargetStream {
    filter: Option<Box<dyn TargetFilter>>,

    /// Watches mdns events
    mdns_watcher: Option<MdnsWatcher>,

    /// Watches for FastbootUsb events
    fastboot_usb_watcher: Option<FastbootUsbWatcher>,

    /// Watches for ManualTarget events
    manual_targets_watcher: Option<ManualTargetWatcher>,

    /// This is where results from the various watchers are published.
    queue: UnboundedReceiver<Result<TargetEvent>>,

    /// Whether we want to get Added events.
    notify_added: bool,

    /// Whether we want to get Removed events.
    notify_removed: bool,
}

pub trait TargetFilter: Send + 'static {
    fn filter_target(&mut self, handle: &TargetHandle) -> bool;
}

impl<F> TargetFilter for F
where
    F: FnMut(&TargetHandle) -> bool + Send + 'static,
{
    fn filter_target(&mut self, handle: &TargetHandle) -> bool {
        self(handle)
    }
}

bitflags! {
    pub struct DiscoverySources: u8 {
        const MDNS = 1 << 0;
        const USB = 1 << 1;
        const MANUAL = 1 << 2;
        // TODO(b/319923485): Emulator
    }
}

pub fn wait_for_devices<F>(
    filter: F,
    notify_added: bool,
    notify_removed: bool,
    sources: DiscoverySources,
) -> Result<TargetStream>
where
    F: TargetFilter,
{
    let (sender, queue) = unbounded();

    // MDNS Watcher
    let mdns_watcher = if sources.contains(DiscoverySources::MDNS) {
        let mdns_sender = sender.clone();
        Some(recommended_watcher(move |res: Result<ffx::MdnsEventType>| {
            // Translate the result to a TargetEvent
            let event = match res {
                Ok(r) => target_event_from_mdns_event(r),
                Err(e) => Some(Err(anyhow!(e))),
            };
            if let Some(event) = event {
                let _ = mdns_sender.unbounded_send(event);
            }
        })?)
    } else {
        None
    };

    // USB Fastboot watcher
    let fastboot_usb_watcher = if sources.contains(DiscoverySources::USB) {
        let fastboot_sender = sender.clone();
        Some(fastboot_watcher(move |res: Result<FastbootEvent>| {
            // Translate the result to a TargetEvent
            tracing::trace!("discovery watcher got fastboot event: {:#?}", res);
            let event = match res {
                Ok(r) => {
                    let event: TargetEvent = r.into();
                    Ok(event)
                }
                Err(e) => Err(anyhow!(e)),
            };
            let _ = fastboot_sender.unbounded_send(event);
        })?)
    } else {
        None
    };

    // Manual Targets watcher
    let manual_targets_watcher = if sources.contains(DiscoverySources::MANUAL) {
        let manual_targets_sender = sender.clone();
        Some(manual_recommended_watcher(move |res: Result<ManualTargetEvent>| {
            // Translate the result to a TargetEvent
            tracing::trace!("discovery watcher got manual target event: {:#?}", res);
            let event = match res {
                Ok(r) => {
                    let event: TargetEvent = r.into();
                    Ok(event)
                }
                Err(e) => Err(anyhow!(e)),
            };
            let _ = manual_targets_sender.unbounded_send(event);
        })?)
    } else {
        None
    };

    Ok(TargetStream {
        filter: Some(Box::new(filter)),
        queue,
        notify_added,
        notify_removed,
        mdns_watcher,
        fastboot_usb_watcher,
        manual_targets_watcher,
    })
}

fn target_event_from_mdns_event(event: ffx::MdnsEventType) -> Option<Result<TargetEvent>> {
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

impl TryFrom<ffx::TargetInfo> for TargetHandle {
    type Error = anyhow::Error;

    fn try_from(info: ffx::TargetInfo) -> Result<Self, Self::Error> {
        let addresses = info.addresses.ok_or(anyhow!("Addresses are populated"))?;
        let addr = addresses.first().ok_or(anyhow!("There must be at least one address"))?;

        let state = match info.fastboot_interface {
            None => TargetState::Product(addr.into()),
            Some(iface) => {
                let serial_number = info.serial_number.unwrap_or("".to_string());
                let connection_state = match iface {
                    ffx::FastbootInterface::Usb => {
                        FastbootConnectionState::Usb(serial_number.clone())
                    }
                    ffx::FastbootInterface::Udp => FastbootConnectionState::Udp(addr.into()),
                    ffx::FastbootInterface::Tcp => FastbootConnectionState::Tcp(addr.into()),
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
                        serial_number: serial.clone(),
                        connection_state: FastbootConnectionState::Usb(serial),
                    }),
                };
                TargetEvent::Added(handle)
            }
            FastbootEvent::Lost(serial) => {
                let handle = TargetHandle {
                    node_name: Some("".to_string()),
                    state: TargetState::Fastboot(FastbootTargetState {
                        serial_number: serial.clone(),
                        connection_state: FastbootConnectionState::Usb(serial),
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
                    ManualTargetState::Product => TargetState::Product(manual_target.addr().into()),
                    ManualTargetState::Fastboot => TargetState::Fastboot(FastbootTargetState {
                        serial_number: "".to_string(),
                        connection_state: FastbootConnectionState::Tcp(manual_target.addr().into()),
                    }),
                };

                let handle = TargetHandle {
                    node_name: Some(manual_target.addr().to_string()),
                    state: state,
                };
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

impl Stream for TargetStream {
    type Item = Result<TargetEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(event) = ready!(Pin::new(&mut self.queue).poll_next(cx)) else {
            return Poll::Ready(None);
        };

        let event = event?;

        if let Some(ref mut filter) = self.filter {
            // TODO(colnnelson): This destructure feels odd. Can this be done
            // better or differently?
            let handle = match event {
                TargetEvent::Added(ref handle) => handle,
                TargetEvent::Removed(ref handle) => handle,
            };

            if !filter.filter_target(handle) {
                tracing::trace!(
                    "Skipping event for target handle: {} as it did not match our filter",
                    handle
                );
                // Schedule the future for this to be woken up again.
                cx.waker().clone().wake();
                return Poll::Pending;
            }
        }

        if matches!(event, TargetEvent::Added(_)) && self.notify_added
            || matches!(event, TargetEvent::Removed(_)) && self.notify_removed
        {
            return Poll::Ready(Some(Ok(event)));
        }

        // Schedule the future for this to be woken up again.
        cx.waker().clone().wake();
        return Poll::Pending;
    }
}

// TODO(colnnelson): Should add a builder struct to allow for building of
// custom TargetStreams

#[cfg(test)]
mod test {
    use super::*;
    use futures::StreamExt;
    use manual_targets::watcher::ManualTarget;
    use pretty_assertions::assert_eq;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};

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
                        connection_state: FastbootConnectionState::Usb("1234".to_string()),
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
                        connection_state: FastbootConnectionState::Usb("1234".to_string()),
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
                    state: TargetState::Product(addr)
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
                        connection_state: FastbootConnectionState::Udp(addr)
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
                        connection_state: FastbootConnectionState::Tcp(addr)
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
                        connection_state: FastbootConnectionState::Usb("".to_string())
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
                    state: TargetState::Product(addr)
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
                    state: TargetState::Product(addr)
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
                    state: TargetState::Product(addr)
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
                    state: TargetState::Product(addr.into()),
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
                    state: TargetState::Product(addr.into()),
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
                        connection_state: FastbootConnectionState::Tcp(addr.into())
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

    fn true_target_filter(_handle: &TargetHandle) -> bool {
        true
    }

    fn zedboot_target_filter(handle: &TargetHandle) -> bool {
        match handle.state {
            TargetState::Zedboot => true,
            _ => false,
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_stream() -> Result<()> {
        let (sender, queue) = unbounded();

        let mut stream = TargetStream {
            filter: Some(Box::new(true_target_filter)),
            mdns_watcher: None,
            fastboot_usb_watcher: None,
            manual_targets_watcher: None,
            queue,
            notify_added: true,
            notify_removed: true,
        };

        // Send a few events
        sender.unbounded_send(Ok(TargetEvent::Added(TargetHandle {
            node_name: Some("Vin".to_string()),
            state: TargetState::Zedboot,
        })))?;

        sender.unbounded_send(Ok(TargetEvent::Removed(TargetHandle {
            node_name: Some("Vin".to_string()),
            state: TargetState::Zedboot,
        })))?;

        assert_eq!(
            stream.next().await.unwrap().ok().unwrap(),
            TargetEvent::Added(TargetHandle {
                node_name: Some("Vin".to_string()),
                state: TargetState::Zedboot
            })
        );

        assert_eq!(
            stream.next().await.unwrap().ok().unwrap(),
            TargetEvent::Removed(TargetHandle {
                node_name: Some("Vin".to_string()),
                state: TargetState::Zedboot
            })
        );

        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_stream_ignores_added() -> Result<()> {
        let (sender, queue) = unbounded();

        let mut stream = TargetStream {
            filter: Some(Box::new(true_target_filter)),
            mdns_watcher: None,
            fastboot_usb_watcher: None,
            manual_targets_watcher: None,
            queue,
            notify_added: false,
            notify_removed: true,
        };

        // Send a few events
        sender.unbounded_send(Ok(TargetEvent::Added(TargetHandle {
            node_name: Some("Vin".to_string()),
            state: TargetState::Zedboot,
        })))?;

        sender.unbounded_send(Ok(TargetEvent::Removed(TargetHandle {
            node_name: Some("Vin".to_string()),
            state: TargetState::Zedboot,
        })))?;

        assert_eq!(
            stream.next().await.unwrap().ok().unwrap(),
            TargetEvent::Removed(TargetHandle {
                node_name: Some("Vin".to_string()),
                state: TargetState::Zedboot
            })
        );

        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_stream_ignores_removed() -> Result<()> {
        let (sender, queue) = unbounded();

        let mut stream = TargetStream {
            filter: Some(Box::new(true_target_filter)),
            mdns_watcher: None,
            fastboot_usb_watcher: None,
            manual_targets_watcher: None,
            queue,
            notify_added: true,
            notify_removed: false,
        };

        // Send a few events
        sender.unbounded_send(Ok(TargetEvent::Removed(TargetHandle {
            node_name: Some("Vin".to_string()),
            state: TargetState::Zedboot,
        })))?;

        sender.unbounded_send(Ok(TargetEvent::Added(TargetHandle {
            node_name: Some("Vin".to_string()),
            state: TargetState::Zedboot,
        })))?;

        assert_eq!(
            stream.next().await.unwrap().ok().unwrap(),
            TargetEvent::Added(TargetHandle {
                node_name: Some("Vin".to_string()),
                state: TargetState::Zedboot
            })
        );

        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_stream_filtered() -> Result<()> {
        let (sender, queue) = unbounded();

        let mut stream = TargetStream {
            filter: Some(Box::new(zedboot_target_filter)),
            mdns_watcher: None,
            fastboot_usb_watcher: None,
            manual_targets_watcher: None,
            queue,
            notify_added: true,
            notify_removed: true,
        };

        // Send a few events
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let addr = TargetAddr::from(socket);
        // This should not come into the queue since the target is not in zedboot
        sender.unbounded_send(Ok(TargetEvent::Added(TargetHandle {
            node_name: Some("Kelsier".to_string()),
            state: TargetState::Product(addr),
        })))?;

        sender.unbounded_send(Ok(TargetEvent::Added(TargetHandle {
            node_name: Some("Vin".to_string()),
            state: TargetState::Zedboot,
        })))?;

        assert_eq!(
            stream.next().await.unwrap().ok().unwrap(),
            TargetEvent::Added(TargetHandle {
                node_name: Some("Vin".to_string()),
                state: TargetState::Zedboot
            })
        );

        Ok(())
    }
}
