// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Link device (L2) definitions.
//!
//! This module contains definitions of link-layer devices, otherwise known as
//! L2 devices.

use core::fmt::Debug;

use net_types::{ethernet::Mac, UnicastAddress};
use zerocopy::{AsBytes, FromBytes, NoCell, Unaligned};

use crate::device::Device;

/// The type of address used by a link device.
pub trait LinkAddress:
    'static + FromBytes + AsBytes + NoCell + Unaligned + Copy + Clone + Debug + Eq + Send
{
    /// The length of the address in bytes.
    const BYTES_LENGTH: usize;

    /// Returns the underlying bytes of a `LinkAddress`.
    fn bytes(&self) -> &[u8];

    /// Constructs a `LinkLayerAddress` from the provided bytes.
    ///
    /// # Panics
    ///
    /// `from_bytes` may panic if `bytes` is not **exactly** [`BYTES_LENGTH`]
    /// long.
    fn from_bytes(bytes: &[u8]) -> Self;
}

impl LinkAddress for Mac {
    const BYTES_LENGTH: usize = 6;

    fn bytes(&self) -> &[u8] {
        self.as_ref()
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        // Assert that contract is being held.
        debug_assert_eq!(bytes.len(), Self::BYTES_LENGTH);
        let mut b = [0; Self::BYTES_LENGTH];
        b.copy_from_slice(bytes);
        Self::new(b)
    }
}

/// A link address that can be unicast.
pub trait LinkUnicastAddress: LinkAddress + UnicastAddress {}
impl<L: LinkAddress + UnicastAddress> LinkUnicastAddress for L {}

/// A link device.
///
/// `LinkDevice` is used to identify a particular link device implementation. It
/// is only intended to exist at the type level, never instantiated at runtime.
pub trait LinkDevice: Device + Debug {
    /// The type of address used to address link devices of this type.
    type Address: LinkUnicastAddress;
}

/// Utilities for testing link devices.
#[cfg(test)]
pub(crate) mod testutil {

    use zerocopy::{AsBytes, FromBytes, FromZeros, NoCell, Unaligned};

    use super::*;
    use crate::{
        context::testutil::FakeCoreCtx,
        device::{
            testutil::{FakeStrongDeviceId, FakeWeakDeviceId},
            DeviceIdContext, Id, StrongId,
        },
    };

    /// A fake [`LinkDevice`].
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
    pub(crate) enum FakeLinkDevice {}

    const FAKE_LINK_ADDRESS_LEN: usize = 1;

    /// A fake [`LinkAddress`].
    ///
    /// The value 0xFF is the broadcast address.
    #[derive(
        FromZeros, FromBytes, AsBytes, NoCell, Unaligned, Copy, Clone, Debug, Hash, PartialEq, Eq,
    )]
    #[repr(transparent)]
    pub(crate) struct FakeLinkAddress(pub(crate) [u8; FAKE_LINK_ADDRESS_LEN]);

    impl UnicastAddress for FakeLinkAddress {
        fn is_unicast(&self) -> bool {
            let Self(bytes) = self;
            bytes != &[0xff]
        }
    }

    impl LinkAddress for FakeLinkAddress {
        const BYTES_LENGTH: usize = FAKE_LINK_ADDRESS_LEN;

        fn bytes(&self) -> &[u8] {
            &self.0[..]
        }

        fn from_bytes(bytes: &[u8]) -> FakeLinkAddress {
            FakeLinkAddress(bytes.try_into().unwrap())
        }
    }

    impl Device for FakeLinkDevice {}

    impl LinkDevice for FakeLinkDevice {
        type Address = FakeLinkAddress;
    }

    /// A fake ID identifying a [`FakeLinkDevice`].
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
    pub(crate) struct FakeLinkDeviceId;

    impl StrongId for FakeLinkDeviceId {
        type Weak = FakeWeakDeviceId<Self>;

        fn downgrade(&self) -> Self::Weak {
            FakeWeakDeviceId(*self)
        }
    }

    impl Id for FakeLinkDeviceId {
        fn is_loopback(&self) -> bool {
            false
        }
    }

    impl<S, M> DeviceIdContext<FakeLinkDevice> for FakeCoreCtx<S, M, FakeLinkDeviceId> {
        type DeviceId = FakeLinkDeviceId;
        type WeakDeviceId = FakeWeakDeviceId<FakeLinkDeviceId>;
    }

    impl FakeStrongDeviceId for FakeLinkDeviceId {
        fn is_alive(&self) -> bool {
            true
        }
    }
}
