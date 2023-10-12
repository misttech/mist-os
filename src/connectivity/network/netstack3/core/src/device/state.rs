// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! State maintained by the device layer.

use crate::{
    device::{socket::HeldDeviceSockets, DeviceIdDebugTag, DeviceLayerTypes, OriginTracker},
    ip::device::state::DualStackIpDeviceState,
    sync::RwLock,
};

/// Provides the specifications for device state held by [`IpLinkDeviceState`].
pub trait IpLinkDeviceStateSpec<C: DeviceLayerTypes>: 'static {
    /// The link state.
    type Link: Send + Sync;
    /// The external (bindings) state.
    type External: DeviceIdDebugTag + Send + Sync;
    /// Marker for loopback devices.
    const IS_LOOPBACK: bool;
    /// Marker used to print debug information for device identifiers.
    const DEBUG_TYPE: &'static str;
}

/// State for a link-device that is also an IP device.
///
/// `D` is the link-specific state.
pub(crate) struct IpLinkDeviceState<T: IpLinkDeviceStateSpec<C>, C: DeviceLayerTypes> {
    pub ip: DualStackIpDeviceState<C::Instant>,
    pub link: T::Link,
    pub(super) external_state: T::External,
    pub(super) origin: OriginTracker,
    pub(super) sockets: RwLock<HeldDeviceSockets<C>>,
}

impl<T: IpLinkDeviceStateSpec<C>, C: DeviceLayerTypes> IpLinkDeviceState<T, C> {
    /// Create a new `IpLinkDeviceState` with a link-specific state `link`.
    pub(super) fn new(link: T::Link, external_state: T::External, origin: OriginTracker) -> Self {
        Self {
            ip: DualStackIpDeviceState::default(),
            link,
            external_state,
            origin,
            sockets: RwLock::new(HeldDeviceSockets::default()),
        }
    }
}

impl<T: IpLinkDeviceStateSpec<C>, C: DeviceLayerTypes> AsRef<DualStackIpDeviceState<C::Instant>>
    for IpLinkDeviceState<T, C>
{
    fn as_ref(&self) -> &DualStackIpDeviceState<C::Instant> {
        &self.ip
    }
}
