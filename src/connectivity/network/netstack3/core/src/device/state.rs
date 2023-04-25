// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! State maintained by the device layer.

use crate::{
    device::{socket::DeviceSockets, DeviceLayerTypes, OriginTracker},
    ip::device::state::DualStackIpDeviceState,
    sync::RwLock,
};

/// State for a link-device that is also an IP device.
///
/// `D` is the link-specific state.
pub(crate) struct IpLinkDeviceState<C: DeviceLayerTypes, S, D> {
    pub ip: DualStackIpDeviceState<C::Instant>,
    pub link: D,
    pub(super) external_state: S,
    pub(super) origin: OriginTracker,
    pub(super) sockets: RwLock<DeviceSockets>,
}

impl<C: DeviceLayerTypes, S, D> IpLinkDeviceState<C, S, D> {
    /// Create a new `IpLinkDeviceState` with a link-specific state `link`.
    pub(super) fn new(link: D, external_state: S, origin: OriginTracker) -> Self {
        Self {
            ip: DualStackIpDeviceState::default(),
            link,
            external_state,
            origin,
            sockets: RwLock::new(DeviceSockets::default()),
        }
    }
}

impl<C: DeviceLayerTypes, S, D> AsRef<DualStackIpDeviceState<C::Instant>>
    for IpLinkDeviceState<C, S, D>
{
    fn as_ref(&self) -> &DualStackIpDeviceState<C::Instant> {
        let Self { ip, link: _, external_state: _, origin: _, sockets: _ } = self;
        ip
    }
}
