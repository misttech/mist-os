// Copyright 2021 The Fuchsia Authors. All rights 1eserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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
