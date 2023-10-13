// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! State maintained by the device layer.

use crate::{
    device::{socket::HeldDeviceSockets, DeviceIdDebugTag, DeviceLayerTypes, OriginTracker},
    ip::device::state::DualStackIpDeviceState,
    sync::RwLock,
};

/// Provides the specifications for device state held by [`BaseDeviceId`] in
/// [`BaseDeviceState`].
pub trait DeviceStateSpec: 'static {
    /// The link state.
    type Link<C: DeviceLayerTypes>: Send + Sync;
    /// The external (bindings) state.
    type External<C: DeviceLayerTypes>: DeviceIdDebugTag + Send + Sync;
    /// Marker for loopback devices.
    const IS_LOOPBACK: bool;
    /// Marker used to print debug information for device identifiers.
    const DEBUG_TYPE: &'static str;
}

pub(crate) struct BaseDeviceState<T: DeviceStateSpec, C: DeviceLayerTypes> {
    pub(crate) ip: IpLinkDeviceState<T, C>,
    pub(crate) external_state: T::External<C>,
}

/// A convenience wrapper around `IpLinkDeviceStateInner` that uses
/// `DeviceStateSpec` to extract the link state type and make type signatures
/// shorter.
pub(crate) type IpLinkDeviceState<T, C> =
    IpLinkDeviceStateInner<<T as DeviceStateSpec>::Link<C>, C>;

/// State for a link-device that is also an IP device.
///
/// `D` is the link-specific state.
pub(crate) struct IpLinkDeviceStateInner<T, C: DeviceLayerTypes> {
    pub ip: DualStackIpDeviceState<C::Instant>,
    pub link: T,
    pub(super) origin: OriginTracker,
    pub(super) sockets: RwLock<HeldDeviceSockets<C>>,
}

impl<T, C: DeviceLayerTypes> IpLinkDeviceStateInner<T, C> {
    /// Create a new `IpLinkDeviceState` with a link-specific state `link`.
    pub(super) fn new(link: T, origin: OriginTracker) -> Self {
        Self {
            ip: DualStackIpDeviceState::default(),
            link,
            origin,
            sockets: RwLock::new(HeldDeviceSockets::default()),
        }
    }
}

impl<T, C: DeviceLayerTypes> AsRef<DualStackIpDeviceState<C::Instant>>
    for IpLinkDeviceStateInner<T, C>
{
    fn as_ref(&self) -> &DualStackIpDeviceState<C::Instant> {
        &self.ip
    }
}
