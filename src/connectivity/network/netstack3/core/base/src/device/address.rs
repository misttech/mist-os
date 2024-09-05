// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Address definitions for devices.

use core::fmt::Display;

use net_types::ip::{GenericOverIp, Ip, IpAddress, Ipv4Addr, Ipv6Addr, Ipv6SourceAddr};
use net_types::{NonMappedAddr, SpecifiedAddr, UnicastAddr, Witness as _};

use crate::socket::SocketIpAddr;

/// An IP address that witnesses properties needed to be assigned to a device.
#[derive(Copy, Clone, Debug, Eq, GenericOverIp, Hash, PartialEq)]
#[generic_over_ip(A, IpAddress)]
pub struct IpDeviceAddr<A: IpAddress>(NonMappedAddr<SpecifiedAddr<A>>);

impl<A: IpAddress> Display for IpDeviceAddr<A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(addr) = self;
        write!(f, "{}", addr)
    }
}

impl<A: IpAddress> IpDeviceAddr<A> {
    /// Returns the inner address, dropping all witnesses.
    pub fn addr(self) -> A {
        let Self(addr) = self;
        **addr
    }

    /// Returns the inner address, including all witness types.
    pub fn into_inner(self) -> NonMappedAddr<SpecifiedAddr<A>> {
        let IpDeviceAddr(addr) = self;
        addr
    }

    /// Constructs an [`IpDeviceAddr`] if the address is compliant, else `None`.
    pub fn new(addr: A) -> Option<IpDeviceAddr<A>> {
        Some(IpDeviceAddr(NonMappedAddr::new(SpecifiedAddr::new(addr)?)?))
    }

    /// Constructs an [`IpDeviceAddr`] from the inner witness.
    pub fn new_from_witness(addr: NonMappedAddr<SpecifiedAddr<A>>) -> Self {
        Self(addr)
    }

    /// Attempts to constructs an [`IpDeviceAddr`] from a [`SocketIpAddr`].
    pub fn new_from_socket_ip_addr(addr: SocketIpAddr<A>) -> Option<Self> {
        Some(Self(addr.into_inner()))
    }
}

impl IpDeviceAddr<Ipv6Addr> {
    /// Constructs an [`IpDeviceAddr`] from the given [`Ipv6DeviceAddr`].
    pub fn new_from_ipv6_device_addr(addr: Ipv6DeviceAddr) -> Self {
        let addr: UnicastAddr<NonMappedAddr<_>> = addr.transpose();
        let addr: NonMappedAddr<SpecifiedAddr<_>> = addr.into_specified().transpose();
        IpDeviceAddr(addr)
    }

    /// Constructs an [`IpDeviceAddr`] from the given [`Ipv6SourceAddr`].
    pub fn new_from_ipv6_source(addr: Ipv6SourceAddr) -> Option<Self> {
        match addr {
            Ipv6SourceAddr::Unicast(addr) => Some(Self::new_from_ipv6_device_addr(addr)),
            Ipv6SourceAddr::Unspecified => None,
        }
    }
}

impl<A: IpAddress> From<IpDeviceAddr<A>> for SpecifiedAddr<A> {
    fn from(addr: IpDeviceAddr<A>) -> Self {
        *addr.into_inner()
    }
}

impl<A: IpAddress> AsRef<SpecifiedAddr<A>> for IpDeviceAddr<A> {
    fn as_ref(&self) -> &SpecifiedAddr<A> {
        let Self(addr) = self;
        addr.as_ref()
    }
}

impl<A: IpAddress> From<IpDeviceAddr<A>> for SocketIpAddr<A> {
    fn from(addr: IpDeviceAddr<A>) -> Self {
        SocketIpAddr::new_from_witness(addr.into_inner())
    }
}

#[derive(Debug)]
pub enum IpDeviceAddrError {
    NotNonMapped,
}

impl<A: IpAddress> TryFrom<SpecifiedAddr<A>> for IpDeviceAddr<A> {
    type Error = IpDeviceAddrError;
    fn try_from(addr: SpecifiedAddr<A>) -> Result<Self, Self::Error> {
        Ok(IpDeviceAddr::new_from_witness(
            NonMappedAddr::new(addr).ok_or(IpDeviceAddrError::NotNonMapped)?,
        ))
    }
}

/// An Ipv4 address that witnesses properties needed to be assigned to a device.
///
/// These witnesses are the same that are held by [`IpDeviceAddr`], however that
/// type cannot be used here directly because we need [`Ipv4DeviceAddr`] to
/// implement `Witness<Ipv4Addr>`. That implementation is not present for our
/// new type [`IpDeviceAddr`] which wraps the true witnesses from the net-types
/// crate.
pub type Ipv4DeviceAddr = NonMappedAddr<SpecifiedAddr<Ipv4Addr>>;

/// An IPv6 address that witnesses properties needed to be assigned to a device.
///
/// Like [`IpDeviceAddr`] but with stricter witnesses that are permitted for
/// IPv6 addresses.
pub type Ipv6DeviceAddr = NonMappedAddr<UnicastAddr<Ipv6Addr>>;
