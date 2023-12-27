// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! State maintained by the device layer.

use alloc::sync::Arc;

use crate::{
    device::{socket::HeldDeviceSockets, DeviceLayerTypes, OriginTracker},
    ip::device::state::DualStackIpDeviceState,
    sync::RwLock,
    sync::WeakRc,
};

/// Provides the specifications for device state held by [`BaseDeviceId`] in
/// [`BaseDeviceState`].
pub trait DeviceStateSpec: Send + Sync + 'static {
    /// The link state.
    type Link<BT: DeviceLayerTypes>: Send + Sync;
    /// The external (bindings) state.
    type External<BT: DeviceLayerTypes>: Send + Sync;
    /// Marker for loopback devices.
    const IS_LOOPBACK: bool;
    /// Marker used to print debug information for device identifiers.
    const DEBUG_TYPE: &'static str;
}

/// Groups state kept by weak device references.
///
/// A weak device reference must be able to carry the bindings identifier
/// infallibly. The `WeakCookie` is kept inside [`BaseDeviceState`] in an `Arc`
/// to group all the information that is cloned out to support weak device
/// references.
pub(crate) struct WeakCookie<T: DeviceStateSpec, BT: DeviceLayerTypes> {
    pub(crate) bindings_id: BT::DeviceIdentifier,
    pub(crate) weak_ref: WeakRc<BaseDeviceState<T, BT>>,
}

pub(crate) struct BaseDeviceState<T: DeviceStateSpec, BT: DeviceLayerTypes> {
    pub(crate) ip: IpLinkDeviceState<T, BT>,
    pub(crate) external_state: T::External<BT>,
    pub(crate) weak_cookie: Arc<WeakCookie<T, BT>>,
}

/// A convenience wrapper around `IpLinkDeviceStateInner` that uses
/// `DeviceStateSpec` to extract the link state type and make type signatures
/// shorter.
pub(crate) type IpLinkDeviceState<T, BT> =
    IpLinkDeviceStateInner<<T as DeviceStateSpec>::Link<BT>, BT>;

/// State for a link-device that is also an IP device.
///
/// `D` is the link-specific state.
pub(crate) struct IpLinkDeviceStateInner<T, BT: DeviceLayerTypes> {
    pub ip: DualStackIpDeviceState<BT::Instant>,
    pub link: T,
    pub(super) origin: OriginTracker,
    pub(super) sockets: RwLock<HeldDeviceSockets<BT>>,
}

impl<T, BT: DeviceLayerTypes> IpLinkDeviceStateInner<T, BT> {
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

impl<T, BT: DeviceLayerTypes> AsRef<DualStackIpDeviceState<BT::Instant>>
    for IpLinkDeviceStateInner<T, BT>
{
    fn as_ref(&self) -> &DualStackIpDeviceState<BT::Instant> {
        &self.ip
    }
}
