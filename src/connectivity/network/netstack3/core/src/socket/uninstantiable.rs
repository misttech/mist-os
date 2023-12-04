// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::{convert::Infallible as Never, marker::PhantomData};
use explicit::UnreachableExt as _;
use net_types::SpecifiedAddr;
use packet::serialize::Serializer;

use crate::{
    device::{Device, DeviceIdContext, StrongId},
    ip::{
        socket::{IpSock, IpSockCreationError, IpSockSendError, IpSocketHandler, SendOptions},
        EitherDeviceId, HopLimits, IpExt, TransportIpContext,
    },
    socket::address::SocketIpAddr,
};

/// An uninstantiable type that implements [`BufferTransportIpContext`].
// TODO(https://fxbug.dev/135142): Remove this type when the buffer trait
// abstraction is removed.
pub(crate) struct UninstantiableBufferTransportIpContext<D> {
    never: Never,
    device_id: PhantomData<D>,
}

impl<D> AsRef<Never> for UninstantiableBufferTransportIpContext<D> {
    fn as_ref(&self) -> &Never {
        let Self { never, device_id: _ } = self;
        never
    }
}

impl<D: Device, ID: StrongId> DeviceIdContext<D> for UninstantiableBufferTransportIpContext<ID> {
    type DeviceId = ID;
    type WeakDeviceId = ID::Weak;
    fn downgrade_device_id(&self, _device_id: &Self::DeviceId) -> Self::WeakDeviceId {
        self.uninstantiable_unreachable()
    }
    fn upgrade_weak_device_id(
        &self,
        _weak_device_id: &Self::WeakDeviceId,
    ) -> Option<Self::DeviceId> {
        self.uninstantiable_unreachable()
    }
}

impl<I: IpExt, C, D: StrongId> IpSocketHandler<I, C> for UninstantiableBufferTransportIpContext<D> {
    fn new_ip_socket<O>(
        &mut self,
        _ctx: &mut C,
        _device: Option<EitherDeviceId<&Self::DeviceId, &Self::WeakDeviceId>>,
        _local_ip: Option<SocketIpAddr<I::Addr>>,
        _remote_ip: SocketIpAddr<I::Addr>,
        _proto: I::Proto,
        _options: O,
    ) -> Result<IpSock<I, Self::WeakDeviceId, O>, (IpSockCreationError, O)> {
        self.uninstantiable_unreachable()
    }

    fn send_ip_packet<S: Serializer, O: SendOptions<I>>(
        &mut self,
        _ctx: &mut C,
        _socket: &IpSock<I, Self::WeakDeviceId, O>,
        _body: S,
        _mtu: Option<u32>,
    ) -> Result<(), (S, IpSockSendError)> {
        self.uninstantiable_unreachable()
    }
}

impl<I: IpExt, C, D: StrongId> TransportIpContext<I, C>
    for UninstantiableBufferTransportIpContext<D>
{
    type DevicesWithAddrIter<'s> = core::iter::Empty<Self::DeviceId>;
    fn get_devices_with_assigned_addr(
        &mut self,
        _addr: SpecifiedAddr<I::Addr>,
    ) -> Self::DevicesWithAddrIter<'_> {
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
}
