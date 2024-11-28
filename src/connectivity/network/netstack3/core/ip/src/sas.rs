// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Provides common SAS (Source Address Selection) implementations.

use net_types::ip::{Ip, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr};
use net_types::SpecifiedAddr;
use netstack3_base::{AnyDevice, DeviceIdContext, IpDeviceAddr};

use crate::internal::device::state::{
    IpDeviceStateBindingsTypes, Ipv6AddressFlags, Ipv6AddressState,
};
use crate::internal::device::{IpAddressId, IpDeviceAddressContext as _, IpDeviceStateContext};
use crate::internal::socket::ipv6_source_address_selection::{self, SasCandidate};

/// A handler for Source Address Selection.
///
/// This trait helps implement source address selection for a variety of traits,
/// like [`crate::IpDeviceStateContext`].
///
/// A blanket implementation on IPv4 and IPv6 is provided for all types
/// implementing [`IpDeviceStateContext`].
pub trait IpSasHandler<I: Ip, BT>: DeviceIdContext<AnyDevice> {
    /// Returns the best local address on `device_id` for communicating with
    /// `remote`.
    fn get_local_addr_for_remote(
        &mut self,
        device_id: &Self::DeviceId,
        remote: Option<SpecifiedAddr<I::Addr>>,
    ) -> Option<IpDeviceAddr<I::Addr>>;
}

impl<CC, BT> IpSasHandler<Ipv4, BT> for CC
where
    CC: IpDeviceStateContext<Ipv4, BT>,
    BT: IpDeviceStateBindingsTypes,
{
    fn get_local_addr_for_remote(
        &mut self,
        device_id: &Self::DeviceId,
        _remote: Option<SpecifiedAddr<Ipv4Addr>>,
    ) -> Option<IpDeviceAddr<Ipv4Addr>> {
        self.with_address_ids(device_id, |mut addrs, _core_ctx| {
            addrs.next().as_ref().map(IpAddressId::addr)
        })
    }
}

impl<CC, BT> IpSasHandler<Ipv6, BT> for CC
where
    CC: IpDeviceStateContext<Ipv6, BT>,
    BT: IpDeviceStateBindingsTypes,
{
    fn get_local_addr_for_remote(
        &mut self,
        device_id: &Self::DeviceId,
        remote: Option<SpecifiedAddr<Ipv6Addr>>,
    ) -> Option<IpDeviceAddr<Ipv6Addr>> {
        self.with_address_ids(device_id, |addrs, core_ctx| {
            IpDeviceAddr::new_from_ipv6_source(
                ipv6_source_address_selection::select_ipv6_source_address(
                    remote,
                    device_id,
                    addrs.map(|addr_id| {
                        core_ctx.with_ip_address_state(
                            device_id,
                            &addr_id,
                            |Ipv6AddressState { flags: Ipv6AddressFlags { assigned }, config }| {
                                // Assume an address is deprecated if config is
                                // not available. That means the address is
                                // going away, so we should not prefer it.
                                const ASSUME_DEPRECATED: bool = true;
                                // Assume an address is not temporary if config
                                // is not available. That means the address is
                                // going away and we should remove any
                                // preference on it.
                                const ASSUME_TEMPORARY: bool = false;
                                let (deprecated, temporary) = config
                                    .map(|c| (c.is_deprecated(), c.is_temporary()))
                                    .unwrap_or((ASSUME_DEPRECATED, ASSUME_TEMPORARY));
                                SasCandidate {
                                    addr_sub: addr_id.addr_sub(),
                                    assigned: *assigned,
                                    temporary,
                                    deprecated,
                                    device: device_id.clone(),
                                }
                            },
                        )
                    }),
                ),
            )
        })
    }
}
