// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Device layer api.

use core::{convert::Infallible as Never, marker::PhantomData};

use net_types::ip::{Ipv4, Ipv6};
use tracing::debug;

use crate::{
    context::{ContextPair, ReferenceNotifiers},
    device::{
        state::{BaseDeviceState, DeviceStateSpec, IpLinkDeviceStateInner},
        AnyDevice, BaseDeviceId, BasePrimaryDeviceId, Device, DeviceCollectionContext, DeviceId,
        DeviceIdContext, DeviceLayerStateTypes, DeviceLayerTypes, OriginTrackerContext,
    },
    ip::{
        device::{
            IpDeviceBindingsContext, IpDeviceConfigurationContext, Ipv6DeviceConfigurationContext,
        },
        types::RawMetric,
    },
    sync::PrimaryRc,
};

/// The result of removing a device from core.
#[derive(Debug)]
pub enum RemoveDeviceResult<R, D> {
    /// The device was synchronously removed and no more references to it exist.
    Removed(R),
    /// The device was marked for destruction but there are still references to
    /// it in existence. The provided receiver can be polled on to observe
    /// device destruction completion.
    Deferred(D),
}

impl<R> RemoveDeviceResult<R, Never> {
    /// A helper function to unwrap a [`RemovedDeviceResult`] that can never be
    /// [`RemovedDeviceResult::Deferred`].
    pub fn into_removed(self) -> R {
        match self {
            Self::Removed(r) => r,
            Self::Deferred(never) => match never {},
        }
    }
}

/// An alias for [`RemoveDeviceResult`] that extracts the receiver type from the
/// bindings context.
pub type RemoveDeviceResultWithContext<S, BT> =
    RemoveDeviceResult<S, <BT as crate::ReferenceNotifiers>::ReferenceReceiver<S>>;

/// The device API.
pub struct DeviceApi<D, C>(C, PhantomData<D>);

impl<D, C> DeviceApi<D, C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx, PhantomData)
    }
}

impl<D, C> DeviceApi<D, C>
where
    D: Device + DeviceStateSpec,
    C: ContextPair,
    C::CoreContext: DeviceIdContext<D, DeviceId = BaseDeviceId<D, C::BindingsContext>>
        + OriginTrackerContext
        + DeviceCollectionContext<D, C::BindingsContext>,
    C::BindingsContext: DeviceLayerTypes + ReferenceNotifiers,
    // Required to call into IP layer for cleanup on removal:
    BaseDeviceId<D, C::BindingsContext>: Into<DeviceId<C::BindingsContext>>,
    C::CoreContext: IpDeviceConfigurationContext<Ipv4, C::BindingsContext>
        + Ipv6DeviceConfigurationContext<C::BindingsContext>
        + DeviceIdContext<AnyDevice, DeviceId = DeviceId<C::BindingsContext>>,
    C::BindingsContext: IpDeviceBindingsContext<Ipv4, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>
        + IpDeviceBindingsContext<Ipv6, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
{
    pub(crate) fn contexts(&mut self) -> (&mut C::CoreContext, &mut C::BindingsContext) {
        let Self(pair, PhantomData) = self;
        pair.contexts()
    }

    pub(crate) fn core_ctx(&mut self) -> &mut C::CoreContext {
        let Self(pair, PhantomData) = self;
        pair.core_ctx()
    }

    /// Adds a new device to the stack and returns its identifier.
    ///
    /// # Panics
    ///
    /// Panics if more than 1 loopback device is added to the stack.
    pub fn add_device(
        &mut self,
        bindings_id: <C::BindingsContext as DeviceLayerStateTypes>::DeviceIdentifier,
        properties: D::CreationProperties,
        metric: RawMetric,
        external_state: D::External<C::BindingsContext>,
    ) -> <C::CoreContext as DeviceIdContext<D>>::DeviceId {
        debug!("adding {} device with {:?} metric:{metric}", D::DEBUG_TYPE, properties);
        let core_ctx = self.core_ctx();
        let origin = core_ctx.origin_tracker();
        let primary = BasePrimaryDeviceId::new(
            IpLinkDeviceStateInner::new(D::new_link_state(properties), metric, origin),
            external_state,
            bindings_id,
        );
        let id = primary.clone_strong();
        core_ctx.insert(primary);
        id
    }

    /// Like [`DeviceApi::add_device`] but using default values for
    /// `bindings_id` and `external_state`.
    ///
    /// This is provided as a convenience method for tests with faked bindings
    /// contexts that have simple implementations for bindings state.
    #[cfg(any(test, feature = "testutils"))]
    pub(crate) fn add_device_with_default_state(
        &mut self,
        properties: D::CreationProperties,
        metric: RawMetric,
    ) -> <C::CoreContext as DeviceIdContext<D>>::DeviceId
    where
        <C::BindingsContext as DeviceLayerStateTypes>::DeviceIdentifier: Default,
        D::External<C::BindingsContext>: Default,
    {
        self.add_device(Default::default(), properties, metric, Default::default())
    }

    /// Removes `device` from the stack.
    ///
    /// If the return value is `RemoveDeviceResult::Removed` the device is
    /// immediately removed from the stack, otherwise
    /// `RemoveDeviceResult::Deferred` indicates that the device was marked for
    /// destruction but there are still references to it. It carries a
    /// `ReferenceReceiver` from the bindings context that can be awaited on
    /// until removal is complete.
    ///
    /// # Panics
    ///
    /// Panics if the device is not currently in the stack.
    pub fn remove_device(
        &mut self,
        device: BaseDeviceId<D, C::BindingsContext>,
    ) -> RemoveDeviceResultWithContext<D::External<C::BindingsContext>, C::BindingsContext> {
        // Start cleaning up the device by disabling IP state. This removes timers
        // for the device that would otherwise hold references to defunct device
        // state.
        let (core_ctx, bindings_ctx) = self.contexts();
        let debug_references = {
            let device = device.clone().into();
            crate::ip::device::clear_ipv4_device_state(core_ctx, bindings_ctx, &device);
            crate::ip::device::clear_ipv6_device_state(core_ctx, bindings_ctx, &device);
            device.downgrade().debug_references()
        };

        tracing::debug!("removing {device:?}");
        let primary = core_ctx.remove(&device).expect("tried to remove device not in stack");
        assert_eq!(device, primary);
        core::mem::drop(device);
        match PrimaryRc::unwrap_or_notify_with(primary.into_inner(), || {
            let (notifier, receiver) = C::BindingsContext::new_reference_notifier::<
                D::External<C::BindingsContext>,
                _,
            >(debug_references);
            let notifier =
                crate::sync::MapRcNotifier::new(notifier, |state: BaseDeviceState<_, _>| {
                    state.external_state
                });
            (notifier, receiver)
        }) {
            Ok(s) => RemoveDeviceResult::Removed(s.external_state),
            Err(receiver) => RemoveDeviceResult::Deferred(receiver),
        }
    }
}
