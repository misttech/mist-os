// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use explicit::UnreachableExt as _;
use net_types::SpecifiedAddr;
use netstack3_base::socket::SocketIpAddr;
use netstack3_base::{AnyDevice, DeviceIdContext, EitherDeviceId, UninstantiableWrapper};
use netstack3_filter::Tuple;

use packet::{BufferMut, Serializer};

use crate::internal::base::{BaseTransportIpContext, HopLimits, IpExt, IpLayerIpExt};
use crate::internal::socket::{
    DeviceIpSocketHandler, IpSock, IpSockCreationError, IpSockSendError, IpSocketHandler, Mms,
    MmsError, SendOptions,
};

impl<I: IpExt, C, P: DeviceIdContext<AnyDevice>> BaseTransportIpContext<I, C>
    for UninstantiableWrapper<P>
{
    type DevicesWithAddrIter<'s> = UninstantiableWrapper<core::iter::Empty<P::DeviceId>>;
    fn with_devices_with_assigned_addr<O, F: FnOnce(Self::DevicesWithAddrIter<'_>) -> O>(
        &mut self,
        _addr: SpecifiedAddr<I::Addr>,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }
    fn get_default_hop_limits(&mut self, _device: Option<&Self::DeviceId>) -> HopLimits {
        self.uninstantiable_unreachable()
    }
    fn confirm_reachable_with_destination(
        &mut self,
        _ctx: &mut C,
        _dst: SpecifiedAddr<I::Addr>,
        _device: Option<&Self::DeviceId>,
    ) {
        self.uninstantiable_unreachable()
    }
    fn get_original_destination(&mut self, _tuple: &Tuple<I>) -> Option<(I::Addr, u16)> {
        self.uninstantiable_unreachable()
    }
}

impl<I: IpExt, C, P: DeviceIdContext<AnyDevice>> IpSocketHandler<I, C>
    for UninstantiableWrapper<P>
{
    fn new_ip_socket(
        &mut self,
        _ctx: &mut C,
        _device: Option<EitherDeviceId<&Self::DeviceId, &Self::WeakDeviceId>>,
        _local_ip: Option<SocketIpAddr<I::Addr>>,
        _remote_ip: SocketIpAddr<I::Addr>,
        _proto: I::Proto,
    ) -> Result<IpSock<I, Self::WeakDeviceId>, IpSockCreationError> {
        self.uninstantiable_unreachable()
    }
    fn send_ip_packet<S, O>(
        &mut self,
        _ctx: &mut C,
        _socket: &IpSock<I, Self::WeakDeviceId>,
        _body: S,
        _mtu: Option<u32>,
        _options: &O,
    ) -> Result<(), IpSockSendError>
    where
        S: Serializer,
        S::Buffer: BufferMut,
        O: SendOptions<I>,
    {
        self.uninstantiable_unreachable()
    }
}

impl<I: IpLayerIpExt, C, P: DeviceIpSocketHandler<I, C>> DeviceIpSocketHandler<I, C>
    for UninstantiableWrapper<P>
{
    fn get_mms(
        &mut self,
        _ctx: &mut C,
        _ip_sock: &IpSock<I, Self::WeakDeviceId>,
    ) -> Result<Mms, MmsError> {
        self.uninstantiable_unreachable()
    }
}
