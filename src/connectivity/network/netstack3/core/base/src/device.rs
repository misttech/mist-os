// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common traits abstracting the device layer.
//!
//! Devices are abstracted away throughout the netstack3 crates. This module
//! provides the base abstraction definitions.
//!
//! Abstracting devices provides:
//!
//! * A useful way to remove a lot of state and complexity from tests.
//! * Opaqueness to steer state access towards context traits.
//! * Type signature reduction since real device identifiers are parameterized
//!   by bindings types.
//! * Modularity.

use core::{fmt::Debug, hash::Hash};

/// An identifier for a device.
pub trait DeviceIdentifier: Clone + Debug + Eq + Hash + PartialEq + Send + Sync + 'static {
    /// Returns true if the device is a loopback device.
    fn is_loopback(&self) -> bool;
}

/// A strong device reference.
///
/// [`StrongDeviceIdentifier`] indicates that the referenced device is alive
/// while the instance exists.
pub trait StrongDeviceIdentifier: DeviceIdentifier {
    /// The weak version of this identifier.
    type Weak: WeakDeviceIdentifier<Strong = Self>;

    /// Returns a weak ID for this strong ID.
    fn downgrade(&self) -> Self::Weak;
}

/// A weak device reference.
///
/// This is the weak reference equivalent of [`StrongDeviceIdentifier`].
pub trait WeakDeviceIdentifier: DeviceIdentifier + PartialEq<Self::Strong> {
    /// The strong version of this identifier.
    type Strong: StrongDeviceIdentifier<Weak = Self>;

    /// Attempts to upgrade this weak ID to a strong ID.
    ///
    /// Returns `None` if the resource has been destroyed.
    fn upgrade(&self) -> Option<Self::Strong>;
}

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use alloc::sync::Arc;
    use core::sync::atomic::AtomicBool;

    use super::*;

    /// A fake weak device id.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
    pub struct FakeWeakDeviceId<D>(pub D);

    impl<D: PartialEq> PartialEq<D> for FakeWeakDeviceId<D> {
        fn eq(&self, other: &D) -> bool {
            let Self(this) = self;
            this == other
        }
    }

    impl<D: FakeStrongDeviceId> WeakDeviceIdentifier for FakeWeakDeviceId<D> {
        type Strong = D;

        fn upgrade(&self) -> Option<D> {
            let Self(inner) = self;
            inner.is_alive().then(|| inner.clone())
        }
    }

    impl<D: DeviceIdentifier> DeviceIdentifier for FakeWeakDeviceId<D> {
        fn is_loopback(&self) -> bool {
            let Self(inner) = self;
            inner.is_loopback()
        }
    }

    /// A fake device ID for use in testing.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
    pub struct FakeDeviceId;

    impl StrongDeviceIdentifier for FakeDeviceId {
        type Weak = FakeWeakDeviceId<Self>;

        fn downgrade(&self) -> Self::Weak {
            FakeWeakDeviceId(self.clone())
        }
    }

    impl DeviceIdentifier for FakeDeviceId {
        fn is_loopback(&self) -> bool {
            false
        }
    }

    impl FakeStrongDeviceId for FakeDeviceId {
        fn is_alive(&self) -> bool {
            true
        }
    }

    /// A fake device ID for use in testing.
    ///
    /// [`FakeReferencyDeviceId`] behaves like a referency device ID, each
    /// constructed instance represents a new device.
    #[derive(Clone, Debug, Default)]
    pub struct FakeReferencyDeviceId {
        removed: Arc<AtomicBool>,
    }

    impl core::hash::Hash for FakeReferencyDeviceId {
        fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
            let Self { removed } = self;
            core::ptr::hash(alloc::sync::Arc::as_ptr(removed), state)
        }
    }

    impl core::cmp::Eq for FakeReferencyDeviceId {}

    impl core::cmp::PartialEq for FakeReferencyDeviceId {
        fn eq(&self, Self { removed: other }: &Self) -> bool {
            let Self { removed } = self;
            alloc::sync::Arc::ptr_eq(removed, other)
        }
    }

    impl core::cmp::Ord for FakeReferencyDeviceId {
        fn cmp(&self, Self { removed: other }: &Self) -> core::cmp::Ordering {
            let Self { removed } = self;
            alloc::sync::Arc::as_ptr(removed).cmp(&alloc::sync::Arc::as_ptr(other))
        }
    }

    impl core::cmp::PartialOrd for FakeReferencyDeviceId {
        fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    impl FakeReferencyDeviceId {
        /// Marks this device as removed, all weak references will not be able
        /// to upgrade anymore.
        pub fn mark_removed(&self) {
            self.removed.store(true, core::sync::atomic::Ordering::Relaxed);
        }
    }

    impl StrongDeviceIdentifier for FakeReferencyDeviceId {
        type Weak = FakeWeakDeviceId<Self>;

        fn downgrade(&self) -> Self::Weak {
            FakeWeakDeviceId(self.clone())
        }
    }

    impl DeviceIdentifier for FakeReferencyDeviceId {
        fn is_loopback(&self) -> bool {
            false
        }
    }

    impl FakeStrongDeviceId for FakeReferencyDeviceId {
        fn is_alive(&self) -> bool {
            !self.removed.load(core::sync::atomic::Ordering::Relaxed)
        }
    }

    /// Marks a fake strong device id.
    pub trait FakeStrongDeviceId:
        StrongDeviceIdentifier<Weak = FakeWeakDeviceId<Self>> + 'static + Ord
    {
        /// Returns whether this ID is still alive.
        ///
        /// This is used by [`FakeWeakDeviceId`] to return `None` when trying to
        /// upgrade back a `FakeStrongDeviceId`.
        fn is_alive(&self) -> bool;
    }

    /// A device ID type that supports identifying more than one distinct
    /// device.
    #[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
    #[allow(missing_docs)]
    pub enum MultipleDevicesId {
        A,
        B,
        C,
    }

    impl MultipleDevicesId {
        /// Returns all variants.
        pub fn all() -> [Self; 3] {
            [Self::A, Self::B, Self::C]
        }
    }

    impl DeviceIdentifier for MultipleDevicesId {
        fn is_loopback(&self) -> bool {
            false
        }
    }

    impl StrongDeviceIdentifier for MultipleDevicesId {
        type Weak = FakeWeakDeviceId<Self>;

        fn downgrade(&self) -> Self::Weak {
            FakeWeakDeviceId(self.clone())
        }
    }

    impl FakeStrongDeviceId for MultipleDevicesId {
        fn is_alive(&self) -> bool {
            true
        }
    }
}
