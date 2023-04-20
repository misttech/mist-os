// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! State maintained by the device layer.

use crate::{
    device::DeviceLayerTypes, device::OriginTracker, ip::device::state::DualStackIpDeviceState,
};

/// State for a link-device that is also an IP device.
///
/// `D` is the link-specific state.
pub(crate) struct IpLinkDeviceState<C: DeviceLayerTypes, S, D> {
    pub ip: DualStackIpDeviceState<C::Instant>,
    pub link: D,
    pub(super) external_state: S,
    pub(super) origin: OriginTracker,
}

impl<C: DeviceLayerTypes, S, D> IpLinkDeviceState<C, S, D> {
    /// Create a new `IpLinkDeviceState` with a link-specific state `link`.
    pub(super) fn new(link: D, external_state: S, origin: OriginTracker) -> Self {
        Self { ip: DualStackIpDeviceState::default(), link, external_state, origin }
    }
}

impl<C: DeviceLayerTypes, S, D> AsRef<DualStackIpDeviceState<C::Instant>>
    for IpLinkDeviceState<C, S, D>
{
    fn as_ref(&self) -> &DualStackIpDeviceState<C::Instant> {
        let Self { ip, link: _, external_state: _, origin: _ } = self;
        ip
    }
}
