// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! State maintained by the device layer.

use crate::{device::OriginTracker, ip::device::state::DualStackIpDeviceState, Instant};

/// State for a link-device that is also an IP device.
///
/// `D` is the link-specific state.
pub(crate) struct IpLinkDeviceState<I: Instant, D> {
    pub ip: DualStackIpDeviceState<I>,
    pub link: D,
    pub(super) origin: OriginTracker,
}

impl<I: Instant, D> IpLinkDeviceState<I, D> {
    /// Create a new `IpLinkDeviceState` with a link-specific state `link`.
    pub(super) fn new(link: D, origin: OriginTracker) -> Self {
        Self { ip: DualStackIpDeviceState::default(), link, origin }
    }
}

impl<I: Instant, D> AsRef<DualStackIpDeviceState<I>> for IpLinkDeviceState<I, D> {
    fn as_ref(&self) -> &DualStackIpDeviceState<I> {
        let Self { ip, link: _, origin: _ } = self;
        ip
    }
}
