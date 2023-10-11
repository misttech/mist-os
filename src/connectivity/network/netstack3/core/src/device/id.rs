// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common device identifier types.

use core::fmt::{self, Debug};
use core::hash::Hash;

use derivative::Derivative;

use crate::{
    device::{
        ethernet::{EthernetDeviceState, EthernetLinkDevice},
        loopback::{LoopbackDeviceId, LoopbackDeviceState, LoopbackWeakDeviceId},
        DeviceIdDebugTag, DeviceLayerStateTypes, DeviceLayerTypes, IpLinkDeviceState,
    },
    ip::device::nud::LinkResolutionContext,
    sync::{PrimaryRc, StrongRc, WeakRc},
    InstantBindingsTypes, NonSyncContext, SyncCtx,
};

/// An identifier for a device.
pub trait Id: Clone + Debug + Eq + Hash + PartialEq + Send + Sync + 'static {
    /// Returns true if the device is a loopback device.
    fn is_loopback(&self) -> bool;
}

/// A marker for a Strong device reference.
///
/// Types marked with [`StrongId`] indicates that the referenced device is alive
/// while the type exists.
pub trait StrongId: Id {
    /// The weak version of this identifier.
    type Weak: WeakId<Strong = Self>;
}

/// A marker for a Weak device reference.
///
/// This is the weak marker equivalent of [`StrongId`].
pub trait WeakId: Id + PartialEq<Self::Strong> {
    /// The strong version of this identifier.
    type Strong: StrongId<Weak = Self>;
}

/// A weak ID identifying a device.
///
/// This device ID makes no claim about the live-ness of the underlying device.
/// See [`DeviceId`] for a device ID that acts as a witness to the live-ness of
/// a device.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""))]
#[allow(missing_docs)]
pub enum WeakDeviceId<C: DeviceLayerTypes> {
    Ethernet(EthernetWeakDeviceId<C>),
    Loopback(LoopbackWeakDeviceId<C>),
}

impl<C: DeviceLayerTypes> PartialEq<DeviceId<C>> for WeakDeviceId<C> {
    fn eq(&self, other: &DeviceId<C>) -> bool {
        <DeviceId<C> as PartialEq<WeakDeviceId<C>>>::eq(other, self)
    }
}

impl<C: DeviceLayerTypes> From<EthernetWeakDeviceId<C>> for WeakDeviceId<C> {
    fn from(id: EthernetWeakDeviceId<C>) -> WeakDeviceId<C> {
        WeakDeviceId::Ethernet(id)
    }
}

impl<C: DeviceLayerTypes> From<LoopbackWeakDeviceId<C>> for WeakDeviceId<C> {
    fn from(id: LoopbackWeakDeviceId<C>) -> WeakDeviceId<C> {
        WeakDeviceId::Loopback(id)
    }
}

impl<C: DeviceLayerTypes> WeakDeviceId<C> {
    /// Attempts to upgrade the ID.
    pub fn upgrade(&self) -> Option<DeviceId<C>> {
        match self {
            WeakDeviceId::Ethernet(id) => id.upgrade().map(Into::into),
            WeakDeviceId::Loopback(id) => id.upgrade().map(Into::into),
        }
    }

    /// Creates a [`DebugReferences`] instance for this device.
    pub fn debug_references(&self) -> DebugReferences<C> {
        DebugReferences(match self {
            Self::Loopback(LoopbackWeakDeviceId(w)) => {
                DebugReferencesInner::Loopback(w.debug_references())
            }
            Self::Ethernet(EthernetWeakDeviceId(_id, w)) => {
                DebugReferencesInner::Ethernet(w.debug_references())
            }
        })
    }
}

enum DebugReferencesInner<C: DeviceLayerTypes> {
    Loopback(crate::sync::DebugReferences<LoopbackReferenceState<C>>),
    Ethernet(crate::sync::DebugReferences<EthernetReferenceState<C>>),
}

/// A type offering a [`Debug`] implementation that helps debug dangling device
/// references.
pub struct DebugReferences<C: DeviceLayerTypes>(DebugReferencesInner<C>);

impl<C: DeviceLayerTypes> Debug for DebugReferences<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(inner) = self;
        match inner {
            DebugReferencesInner::Loopback(d) => write!(f, "Loopback({d:?})"),
            DebugReferencesInner::Ethernet(d) => write!(f, "Ethernet({d:?})"),
        }
    }
}

impl<C: DeviceLayerTypes> Id for WeakDeviceId<C> {
    fn is_loopback(&self) -> bool {
        match self {
            WeakDeviceId::Loopback(LoopbackWeakDeviceId(_)) => true,
            WeakDeviceId::Ethernet(_) => false,
        }
    }
}

impl<C: DeviceLayerTypes> WeakId for WeakDeviceId<C> {
    type Strong = DeviceId<C>;
}

impl<C: DeviceLayerTypes> Debug for WeakDeviceId<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WeakDeviceId::Ethernet(id) => Debug::fmt(id, f),
            WeakDeviceId::Loopback(id) => Debug::fmt(id, f),
        }
    }
}

/// A strong ID identifying a device.
///
/// Holders may safely assume that the underlying device is "alive" in the sense
/// that the device is still recognized by the stack. That is, operations that
/// use this device ID will never fail as a result of "unrecognized device"-like
/// errors.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""))]
#[allow(missing_docs)]
pub enum DeviceId<C: DeviceLayerTypes> {
    Ethernet(EthernetDeviceId<C>),
    Loopback(LoopbackDeviceId<C>),
}

impl<C: DeviceLayerTypes> PartialEq<WeakDeviceId<C>> for DeviceId<C> {
    fn eq(&self, other: &WeakDeviceId<C>) -> bool {
        match (self, other) {
            (DeviceId::Ethernet(strong), WeakDeviceId::Ethernet(weak)) => strong == weak,
            (DeviceId::Loopback(strong), WeakDeviceId::Loopback(weak)) => strong == weak,
            (DeviceId::Loopback(_), WeakDeviceId::Ethernet(_))
            | (DeviceId::Ethernet(_), WeakDeviceId::Loopback(_)) => false,
        }
    }
}

impl<C: DeviceLayerTypes> PartialOrd for DeviceId<C> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<C: DeviceLayerTypes> Ord for DeviceId<C> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        match (self, other) {
            (DeviceId::Ethernet(me), DeviceId::Ethernet(other)) => me.cmp(other),
            (DeviceId::Loopback(me), DeviceId::Loopback(other)) => me.cmp(other),
            (DeviceId::Loopback(_), DeviceId::Ethernet(_)) => core::cmp::Ordering::Less,
            (DeviceId::Ethernet(_), DeviceId::Loopback(_)) => core::cmp::Ordering::Greater,
        }
    }
}

impl<C: DeviceLayerTypes> From<EthernetDeviceId<C>> for DeviceId<C> {
    fn from(id: EthernetDeviceId<C>) -> DeviceId<C> {
        DeviceId::Ethernet(id)
    }
}

impl<C: DeviceLayerTypes> From<LoopbackDeviceId<C>> for DeviceId<C> {
    fn from(id: LoopbackDeviceId<C>) -> DeviceId<C> {
        DeviceId::Loopback(id)
    }
}

impl<C: DeviceLayerTypes> DeviceId<C> {
    /// Downgrade to a [`WeakDeviceId`].
    pub fn downgrade(&self) -> WeakDeviceId<C> {
        match self {
            DeviceId::Ethernet(id) => id.downgrade().into(),
            DeviceId::Loopback(id) => id.downgrade().into(),
        }
    }
}

impl<C: DeviceLayerTypes> Id for DeviceId<C> {
    fn is_loopback(&self) -> bool {
        match self {
            DeviceId::Loopback(LoopbackDeviceId(_)) => true,
            DeviceId::Ethernet(_) => false,
        }
    }
}

impl<C: DeviceLayerTypes> StrongId for DeviceId<C> {
    type Weak = WeakDeviceId<C>;
}

impl<C: DeviceLayerTypes> Debug for DeviceId<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceId::Ethernet(id) => Debug::fmt(id, f),
            DeviceId::Loopback(id) => Debug::fmt(id, f),
        }
    }
}

/// A weak device ID identifying an ethernet device.
///
/// This device ID is like [`WeakDeviceId`] but specifically for ethernet
/// devices.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Hash(bound = ""))]
pub struct EthernetWeakDeviceId<C: DeviceLayerTypes>(usize, WeakRc<EthernetReferenceState<C>>);

impl<C: DeviceLayerTypes> PartialEq for EthernetWeakDeviceId<C> {
    fn eq(&self, EthernetWeakDeviceId(_, other_ptr): &EthernetWeakDeviceId<C>) -> bool {
        let EthernetWeakDeviceId(_, me_ptr) = self;
        WeakRc::ptr_eq(me_ptr, other_ptr)
    }
}

impl<C: DeviceLayerTypes> PartialEq<EthernetDeviceId<C>> for EthernetWeakDeviceId<C> {
    fn eq(&self, other: &EthernetDeviceId<C>) -> bool {
        <EthernetDeviceId<C> as PartialEq<EthernetWeakDeviceId<C>>>::eq(other, self)
    }
}

impl<C: DeviceLayerTypes> Eq for EthernetWeakDeviceId<C> {}

impl<C: DeviceLayerTypes> Debug for EthernetWeakDeviceId<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(id, _ptr) = self;
        write!(f, "WeakEthernet({id})")
    }
}

impl<C: DeviceLayerTypes> Id for EthernetWeakDeviceId<C>
where
    C::EthernetDeviceState: Send + Sync,
{
    fn is_loopback(&self) -> bool {
        false
    }
}

impl<C: DeviceLayerTypes> WeakId for EthernetWeakDeviceId<C>
where
    C::EthernetDeviceState: Send + Sync,
{
    type Strong = EthernetDeviceId<C>;
}

impl<C: DeviceLayerTypes> EthernetWeakDeviceId<C> {
    /// Attempts to upgrade the ID to an [`EthernetDeviceId`], failing if the
    /// device no longer exists.
    pub fn upgrade(&self) -> Option<EthernetDeviceId<C>> {
        let Self(_id, rc) = self;
        rc.upgrade().map(|rc| EthernetDeviceId(rc))
    }
}

pub(super) type EthernetReferenceState<C> = IpLinkDeviceState<
    C,
    <C as DeviceLayerStateTypes>::EthernetDeviceState,
    EthernetDeviceState<
        <C as InstantBindingsTypes>::Instant,
        <C as LinkResolutionContext<EthernetLinkDevice>>::Notifier,
    >,
>;
pub(super) type LoopbackReferenceState<C> =
    IpLinkDeviceState<C, <C as DeviceLayerStateTypes>::LoopbackDeviceState, LoopbackDeviceState>;

/// A strong device ID identifying an ethernet device.
///
/// This device ID is like [`DeviceId`] but specifically for ethernet devices.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Hash(bound = ""), Eq(bound = ""), PartialEq(bound = ""))]
pub struct EthernetDeviceId<C: DeviceLayerTypes>(pub(super) StrongRc<EthernetReferenceState<C>>);

impl<C: DeviceLayerTypes> PartialEq<EthernetWeakDeviceId<C>> for EthernetDeviceId<C> {
    fn eq(&self, EthernetWeakDeviceId(_id, other_ptr): &EthernetWeakDeviceId<C>) -> bool {
        let EthernetDeviceId(me_ptr) = self;
        StrongRc::weak_ptr_eq(me_ptr, other_ptr)
    }
}

impl<C: DeviceLayerTypes> PartialOrd for EthernetDeviceId<C> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<C: DeviceLayerTypes> Ord for EthernetDeviceId<C> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let Self(me) = self;
        let Self(other) = other;

        StrongRc::ptr_cmp(me, other)
    }
}

impl<C: DeviceLayerTypes> Debug for EthernetDeviceId<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(rc) = self;
        let id = rc.link.debug_id();
        write!(f, "Ethernet({id}{{")?;
        rc.external_state.id_debug_tag(f)?;
        write!(f, "}})")
    }
}

impl<C: DeviceLayerTypes> Id for EthernetDeviceId<C>
where
    C::EthernetDeviceState: Send + Sync,
{
    fn is_loopback(&self) -> bool {
        false
    }
}

impl<C: DeviceLayerTypes> StrongId for EthernetDeviceId<C>
where
    C::EthernetDeviceState: Send + Sync,
{
    type Weak = EthernetWeakDeviceId<C>;
}

impl<C: DeviceLayerTypes> EthernetDeviceId<C> {
    /// Returns a reference to the external state for the device.
    pub fn external_state(&self) -> &C::EthernetDeviceState {
        let Self(rc) = self;
        &rc.external_state
    }

    /// Downgrades the ID to an [`EthernetWeakDeviceId`].
    pub fn downgrade(&self) -> EthernetWeakDeviceId<C> {
        let Self(rc) = self;
        // When we downgrade an ethernet device reference we keep a copy of the
        // debug ID to help debugging.
        EthernetWeakDeviceId(rc.link.debug_id(), StrongRc::downgrade(rc))
    }
}

/// A helper trait to provide a common implementation for all device type
/// removals.
///
/// This trait exists to support [`remove_device`] which performs delicate
/// common tasks on all device removals.
pub(super) trait RemovableDeviceId<C: NonSyncContext>: Into<DeviceId<C>> + Clone {
    /// The external state for this device. Pulled from [`DeviceLayerTypes`]
    /// impl on `C`.
    type ExternalState: Send;
    /// The state held inside the [`PrimaryRc`] for this device.
    type ReferenceState;

    /// Removes the primary device reference from `sync_ctx`, returning its
    /// [`PrimaryRc`] and what must be the last [`StrongRc`] reference.
    fn remove(
        self,
        sync_ctx: &SyncCtx<C>,
    ) -> (PrimaryRc<Self::ReferenceState>, StrongRc<Self::ReferenceState>);

    /// Takes the external state from the unwrapped reference state in the
    /// device's [`PrimaryRc`].
    fn take_external_state(reference_state: Self::ReferenceState) -> Self::ExternalState;
}

impl<C: NonSyncContext> RemovableDeviceId<C> for EthernetDeviceId<C> {
    type ExternalState = C::EthernetDeviceState;
    type ReferenceState = EthernetReferenceState<C>;

    fn remove(
        self,
        sync_ctx: &SyncCtx<C>,
    ) -> (PrimaryRc<Self::ReferenceState>, StrongRc<Self::ReferenceState>) {
        let mut devices = sync_ctx.state.device.devices.write();
        tracing::debug!("removing Ethernet device with ID {self:?}");
        let EthernetDeviceId(rc) = self;
        let removed = devices
            .ethernet
            .remove(&rc)
            .unwrap_or_else(|| panic!("no such Ethernet device: {}", rc.link.debug_id()));

        (removed, rc)
    }

    fn take_external_state(reference_state: Self::ReferenceState) -> Self::ExternalState {
        reference_state.external_state
    }
}

impl<C: NonSyncContext> RemovableDeviceId<C> for LoopbackDeviceId<C> {
    type ExternalState = C::LoopbackDeviceState;
    type ReferenceState = LoopbackReferenceState<C>;

    fn remove(
        self,
        sync_ctx: &SyncCtx<C>,
    ) -> (PrimaryRc<Self::ReferenceState>, StrongRc<Self::ReferenceState>) {
        let mut devices = sync_ctx.state.device.devices.write();
        let LoopbackDeviceId(rc) = self;
        let removed = devices.loopback.take().expect("loopback device not installed");
        tracing::debug!("removing Loopback device");
        (removed, rc)
    }

    fn take_external_state(reference_state: Self::ReferenceState) -> Self::ExternalState {
        reference_state.external_state
    }
}
