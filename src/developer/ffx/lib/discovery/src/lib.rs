// Copyright 2021 The Fuchsia Authors. All rights 1eserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::{anyhow, bail, Result};
use bitflags::bitflags;
use emulator_instance::EmulatorInstances;
use fuchsia_async::Task;
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::Stream;
use manual_targets::watcher::{
    recommended_watcher as manual_recommended_watcher, ManualTargetEvent, ManualTargetState,
    ManualTargetWatcher,
};
use mdns_discovery::{recommended_watcher, MdnsWatcher};
use std::fmt;
use std::path::PathBuf;
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
#[derive(Debug, PartialEq, Eq, Hash)]
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
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct FastbootTargetState {
    pub serial_number: String,
    pub connection_state: FastbootConnectionState,
}

impl Display for FastbootTargetState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.serial_number, self.connection_state)
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
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
#[derive(Debug, PartialEq, Eq, Hash)]
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
#[derive(Debug, PartialEq, Eq, Hash)]
pub enum TargetEvent {
    /// Indicates a Target has been discovered.
    Added(TargetHandle),
    /// Indicates a Target has been lost.
    Removed(TargetHandle),
}

struct EmulatorWatcher {
    // Task for the drain loop
    drain_task: Option<Task<()>>,
}

impl EmulatorWatcher {
    async fn new(
        instance_root: PathBuf,
        sender: UnboundedSender<Result<TargetEvent>>,
    ) -> Result<Self> {
        let emu_instances = EmulatorInstances::new(instance_root.clone());
        let existing = emulator_instance::get_all_targets(&emu_instances)?;
        for i in existing {
            let handle = i.try_into();
            if let Ok(h) = handle {
                let _ = sender.unbounded_send(Ok(TargetEvent::Added(h)));
            }
        }
        let mut res = Self { drain_task: None };

        // Emulator (and therefore notify thread) lifetime should last as long as the task,
        // because it is moved into the loop
        let mut watcher = emulator_instance::start_emulator_watching(instance_root.clone()).await?;
        let task = Task::local(async move {
            loop {
                if let Some(act) = watcher.emulator_target_detected().await {
                    let event = act.try_into();
                    if let Ok(e) = event {
                        let _ = sender.unbounded_send(Ok(e));
                    }
                }
            }
        });
        res.drain_task.replace(task);
        Ok(res)
    }
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

    /// Watches for Emulator events
    emulator_watcher: Option<EmulatorWatcher>,

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
        const EMULATOR = 1 << 3;
    }
}
pub async fn wait_for_devices<F>(
    filter: F,
    emulator_instance_root: Option<PathBuf>,
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

    let emulator_watcher = if sources.contains(DiscoverySources::EMULATOR) {
        let emulator_sender = sender.clone();
        if let Some(instance_root) = emulator_instance_root {
            Some(EmulatorWatcher::new(instance_root, emulator_sender).await?)
        } else {
            None
        }
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
        emulator_watcher,
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
        let addresses = info.addresses.ok_or(anyhow!("Addresses are populated"))?;
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
                let serial_number = info.serial_number.unwrap_or("".to_string());
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
                        serial_number: serial.clone(),
                        connection_state: FastbootConnectionState::Usb,
                    }),
                };
                TargetEvent::Added(handle)
            }
            FastbootEvent::Lost(serial) => {
                let handle = TargetHandle {
                    node_name: Some("".to_string()),
                    state: TargetState::Fastboot(FastbootTargetState {
                        serial_number: serial.clone(),
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
    use std::fs::File;
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

    fn true_target_filter(_handle: &TargetHandle) -> bool {
        true
    }

    fn zedboot_target_filter(handle: &TargetHandle) -> bool {
        match handle.state {
            TargetState::Zedboot => true,
            _ => false,
        }
    }

    #[fuchsia::test]
    async fn test_target_stream() -> Result<()> {
        let (sender, queue) = unbounded();

        let mut stream = TargetStream {
            filter: Some(Box::new(true_target_filter)),
            mdns_watcher: None,
            fastboot_usb_watcher: None,
            manual_targets_watcher: None,
            emulator_watcher: None,
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

    #[fuchsia::test]
    async fn test_target_stream_ignores_added() -> Result<()> {
        let (sender, queue) = unbounded();

        let mut stream = TargetStream {
            filter: Some(Box::new(true_target_filter)),
            mdns_watcher: None,
            fastboot_usb_watcher: None,
            manual_targets_watcher: None,
            emulator_watcher: None,
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

    #[fuchsia::test]
    async fn test_target_stream_ignores_removed() -> Result<()> {
        let (sender, queue) = unbounded();

        let mut stream = TargetStream {
            filter: Some(Box::new(true_target_filter)),
            mdns_watcher: None,
            fastboot_usb_watcher: None,
            manual_targets_watcher: None,
            emulator_watcher: None,
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

    #[fuchsia::test]
    async fn test_target_stream_filtered() -> Result<()> {
        let (sender, queue) = unbounded();

        let mut stream = TargetStream {
            filter: Some(Box::new(zedboot_target_filter)),
            mdns_watcher: None,
            fastboot_usb_watcher: None,
            manual_targets_watcher: None,
            emulator_watcher: None,
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
            state: TargetState::Product(vec![addr]),
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

    fn build_instance_file(dir: &PathBuf, name: &str) -> Result<File> {
        use std::io::Write;
        let new_instance_dir = dir.join(String::from(name));
        std::fs::create_dir_all(&new_instance_dir)?;
        let new_instance_engine_file = new_instance_dir.join("engine.json");
        use emulator_instance::EmulatorInstanceInfo;
        // Build the expected config JSON contents
        let mut instance_data = emulator_instance::EmulatorInstanceData::new_with_state(
            name,
            emulator_instance::EngineState::Running,
        );
        instance_data.set_pid(std::process::id());
        let config = instance_data.get_emulator_configuration_mut();
        config.host.networking = emulator_instance::NetworkingMode::User;
        config.host.port_map.insert(
            String::from("ssh"),
            emulator_instance::PortMapping { guest: 22, host: Some(3322) },
        );
        let config_str = serde_json::to_string(&instance_data)?;
        let mut config_file = File::create(&new_instance_engine_file)?;
        config_file.write_all(config_str.as_bytes())?;
        config_file.flush()?;
        Ok(config_file)
    }

    // This test has a race condition, which I (slgrady) have spent hours trying to find.
    // Apparently the notify crate in emulator_instance is for some reason not producing
    // the Create event for the new emulator file. The event is created in a separate
    // thread, so it's not an async issue. The watcher is not being dropped at that point.
    // The file is in fact created and placed on the filesystem; the watcher thread
    // is running at the point we are waiting for the event.  Giving up, since I believe
    // the race-condition only comes up in the artificial environment of a test case.
    // Normally, emulators are extended events, not just a fast creation of a single file.
    #[ignore]
    #[fuchsia::test]
    async fn test_target_stream_produces_emulator() -> Result<()> {
        use tempfile::tempdir;

        let _env = ffx_config::test_init().await.expect("Failed to initialize test env");

        // Create the emulator instance dir
        let temp = tempdir().expect("cannot get tempdir");
        let instance_dir = temp.path().to_path_buf();
        let emu_instances = emulator_instance::EmulatorInstances::new(instance_dir.clone());

        // Add a new emulator
        let config_file = build_instance_file(&instance_dir, "emu-data-instance")?;

        // Before waiting on devices, let's make sure we're actually getting the
        // emulator. (This shouldn't be necessary, but I've seen this test flake
        // by timing out, so this is a validity check.)
        let existing = emulator_instance::get_all_targets(&emu_instances)?;
        assert_eq!(existing.len(), 1);

        // Start watching the directory
        let mut stream = wait_for_devices(
            true_target_filter,
            Some(instance_dir.clone()),
            true,
            false,
            DiscoverySources::EMULATOR,
        )
        .await?;

        // Assert that the existing emulator is discovered
        let next =
            stream.next().await.expect("No event was waiting after watching for existing emulator");
        let next = next.expect("Getting emulator event failed");
        assert_eq!(
            next,
            // The node_name and the state both have to match the contents of the emu_config above.
            TargetEvent::Added(TargetHandle {
                // Name must correspond to "runtime:name" value in config
                node_name: Some("emu-data-instance".to_string()),
                // Addr must correspond to "host:port_map:sh:host" value in config
                state: TargetState::Product(vec![TargetAddr::from_str("127.0.0.1:3322")?]),
            })
        );

        // Add a new (different) emulator
        let config_file2 = build_instance_file(&instance_dir, "emu-data-instance2")?;
        std::thread::sleep(std::time::Duration::from_secs(1));
        // let existing = emulator_instance::get_all_targets().await?;
        // assert_eq!(existing.len(), 2);

        // Assert that the newly-created emulator is discovered
        let next =
            stream.next().await.expect("No event was waiting after watching for new emulator");
        let next = next.expect("Getting emulator event failed");
        assert_eq!(
            next,
            // The node_name and the state both have to match the contents of the emu_config above.
            TargetEvent::Added(TargetHandle {
                // Name must correspond to "runtime:name" value in config
                node_name: Some("emu-data-instance2".to_string()),
                // Addr must correspond to "host:port_map:sh:host" value in config
                state: TargetState::Product(vec![TargetAddr::from_str("127.0.0.1:3322")?]),
            })
        );

        drop(config_file);
        drop(config_file2);
        std::fs::remove_dir_all(&instance_dir)?;
        // TODO(325325761) -- re-enable when emulator Remove events are generated
        // correctly.
        // let next = stream
        //     .next()
        //     .await
        //     .unwrap();
        // let next = next.expect("Getting emulator event failed");
        // assert_eq!(
        //     next,
        //     // The node_name and the state both have to match the contents of the emu_config above.
        //     TargetEvent::Removed(TargetHandle {
        //         // Name must correspond to "runtime:name" value in config
        //         node_name: Some("fuchsia-emulator".to_string()),
        //         // Addr must correspond to "host:port_map:sh:host" value in config
        //         state: TargetState::Product(TargetAddr::from_str("127.0.0.1:33881")?),
        //     })
        // );

        Ok(())
    }
}
