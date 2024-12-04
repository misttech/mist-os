// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::ot::WrongSize;
use crate::prelude_internal::*;
use core::pin::Pin;
use core::task::{Context, Poll};
use fuchsia_sync::Mutex;
use std::sync::Arc;
use std::task::Waker;

/// Iterator type for external routes.
#[allow(missing_debug_implementations)]
pub struct LocalExternalRouteIterator<'a, T: ?Sized> {
    ot_instance: &'a T,
    ot_iter: otNetworkDataIterator,
}

impl<T: ?Sized + BorderRouter> Iterator for LocalExternalRouteIterator<'_, T> {
    type Item = ExternalRouteConfig;
    fn next(&mut self) -> Option<Self::Item> {
        self.ot_instance.iter_next_local_external_route(&mut self.ot_iter)
    }
}

/// Iterator type for on-mesh prefixes.
#[allow(missing_debug_implementations)]
pub struct LocalOnMeshPrefixIterator<'a, T: ?Sized> {
    ot_instance: &'a T,
    ot_iter: otNetworkDataIterator,
}

impl<T: ?Sized + BorderRouter> Iterator for LocalOnMeshPrefixIterator<'_, T> {
    type Item = BorderRouterConfig;
    fn next(&mut self) -> Option<Self::Item> {
        self.ot_instance.iter_next_local_on_mesh_prefix(&mut self.ot_iter)
    }
}

/// Methods from the [OpenThread "Border Router" Module][1].
///
/// [1]: https://openthread.io/reference/group/api-border-router
pub trait BorderRouter {
    /// Functional equivalent of
    /// [`otsys::otBorderRouterAddRoute`](crate::otsys::otBorderRouterAddRoute).
    fn add_external_route(&self, route: &ExternalRouteConfig) -> Result;

    /// Functional equivalent of
    /// [`otsys::otBorderRouterAddOnMeshPrefix`](crate::otsys::otBorderRouterAddOnMeshPrefix).
    fn add_on_mesh_prefix(&self, route: &BorderRouterConfig) -> Result;

    /// Functional equivalent of
    /// [`otsys::otBorderRouterRemoveRoute`](crate::otsys::otBorderRouterRemoveRoute).
    fn remove_external_route(&self, prefix: &Ip6Prefix) -> Result;

    /// Functional equivalent of
    /// [`otsys::otBorderRouterRemoveOnMeshPrefix`](crate::otsys::otBorderRouterRemoveOnMeshPrefix).
    fn remove_on_mesh_prefix(&self, prefix: &Ip6Prefix) -> Result;

    /// Functional equivalent of
    /// [`otsys::otBorderRouterRegister`](crate::otsys::otBorderRouterRegister).
    fn border_router_register(&self) -> Result;

    /// Functional equivalent of
    /// [`otsys::otBorderRoutingInit`](crate::otsys::otBorderRoutingInit).
    fn border_routing_init(&self, infra_if: u32, infra_is_running: bool) -> Result;

    /// Functional equivalent of
    /// [`otsys::otBorderRoutingSetEnabled`](crate::otsys::otBorderRoutingSetEnabled).
    fn border_routing_set_enabled(&self, enabled: bool) -> Result;

    /// Functional equivalent of
    /// [`otsys::otBorderRoutingDhcp6PdSetEnabled`](crate::otsys::otBorderRoutingDhcp6PdSetEnabled).
    fn border_routing_dhcp6_pd_set_enabled(&self, enabled: bool);

    /// Functional equivalent of
    /// [`otsys::otBorderRoutingDhcp6PdGetState`](crate::otsys::otBorderRoutingDhcp6PdGetState).
    fn border_routing_dhcp6_pd_get_state(&self) -> BorderRoutingDhcp6PdState;

    /// Functional equivalent of
    /// [`otsys::otBorderRoutingDhcp6PdSetRequestCallback`](crate::otsys::otBorderRoutingDhcp6PdSetRequestCallback).
    fn border_routing_dhcp6_pd_set_request_fn<'a, F>(&'a self, f: Option<F>)
    where
        F: FnMut(BorderRoutingDhcp6PdState) + 'a;

    /// Get the DHCPv6 PD state change stream
    fn border_routing_dhcp6_pd_state_change_stream(&self)
        -> BorderRoutingDhcp6PdStateChangedStream;

    /// Functional equivalent of
    /// [`otsys::otBorderRoutingGetPdOmrPrefix`](crate::otsys::otBorderRoutingGetPdOmrPrefix).
    fn border_routing_get_pd_omr_prefix(&self) -> Result<ot::BorderRoutingPrefixTableEntry>;

    /// Functional equivalent of
    /// [`otsys::otBorderRoutingGetOmrPrefix`](crate::otsys::otBorderRoutingGetOmrPrefix).
    fn border_routing_get_omr_prefix(&self) -> Result<ot::Ip6Prefix>;

    /// Functional equivalent of
    /// [`otsys::otBorderRoutingGetOnLinkPrefix`](crate::otsys::otBorderRoutingGetOnLinkPrefix).
    fn border_routing_get_on_link_prefix(&self) -> Result<ot::Ip6Prefix>;

    /// Functional equivalent of
    /// [`otsys::otPlatBorderRoutingProcessIcmp6Ra`](crate::otsys::otPlatBorderRoutingProcessIcmp6Ra).
    fn border_routing_process_icmp6_ra(&self, message: &[u8]) -> Result<(), WrongSize>;

    /// Functional equivalent of
    /// [`otsys::otBorderRoutingGetPdProcessedRaInfo`](crate::otsys::otBorderRoutingGetPdProcessedRaInfo).
    fn border_routing_get_pd_processed_ra_info(&self) -> PdProcessedRaInfo;

    /// Functional equivalent of
    /// [`otsys::otBorderRouterGetNextRoute`](crate::otsys::otBorderRouterGetNextRoute).
    // TODO: Determine if the underlying implementation of
    //       this method has undefined behavior when network data
    //       is being mutated while iterating. If it is undefined,
    //       we may need to make it unsafe and provide a safe method
    //       that collects the results.
    fn iter_next_local_external_route(
        &self,
        ot_iter: &mut otNetworkDataIterator,
    ) -> Option<ExternalRouteConfig>;

    /// Functional equivalent of
    /// [`otsys::otBorderRouterGetNextOnMeshPrefix`](crate::otsys::otBorderRouterGetNextOnMeshPrefix).
    // TODO: Determine if the underlying implementation of
    //       this method has undefined behavior when network data
    //       is being mutated while iterating. If it is undefined,
    //       we may need to make it unsafe and provide a safe method
    //       that collects the results.
    fn iter_next_local_on_mesh_prefix(
        &self,
        ot_iter: &mut otNetworkDataIterator,
    ) -> Option<BorderRouterConfig>;

    /// Returns an iterator for iterating over external routes.
    fn iter_local_external_routes(&self) -> LocalExternalRouteIterator<'_, Self> {
        LocalExternalRouteIterator { ot_instance: self, ot_iter: OT_NETWORK_DATA_ITERATOR_INIT }
    }

    /// Returns an iterator for iterating over on-mesh prefixes
    fn iter_local_on_mesh_prefixes(&self) -> LocalOnMeshPrefixIterator<'_, Self> {
        LocalOnMeshPrefixIterator { ot_instance: self, ot_iter: OT_NETWORK_DATA_ITERATOR_INIT }
    }
}

impl<T: BorderRouter + Boxable> BorderRouter for ot::Box<T> {
    fn add_external_route(&self, route: &ExternalRouteConfig) -> Result {
        self.as_ref().add_external_route(route)
    }

    fn add_on_mesh_prefix(&self, route: &BorderRouterConfig) -> Result {
        self.as_ref().add_on_mesh_prefix(route)
    }

    fn remove_external_route(&self, prefix: &Ip6Prefix) -> Result {
        self.as_ref().remove_external_route(prefix)
    }

    fn remove_on_mesh_prefix(&self, prefix: &Ip6Prefix) -> Result {
        self.as_ref().remove_on_mesh_prefix(prefix)
    }

    fn border_router_register(&self) -> Result {
        self.as_ref().border_router_register()
    }

    fn border_routing_init(&self, infra_if: u32, infra_is_running: bool) -> Result {
        self.as_ref().border_routing_init(infra_if, infra_is_running)
    }

    fn border_routing_set_enabled(&self, enabled: bool) -> Result {
        self.as_ref().border_routing_set_enabled(enabled)
    }

    fn border_routing_dhcp6_pd_set_enabled(&self, enabled: bool) {
        self.as_ref().border_routing_dhcp6_pd_set_enabled(enabled)
    }

    fn border_routing_dhcp6_pd_get_state(&self) -> BorderRoutingDhcp6PdState {
        self.as_ref().border_routing_dhcp6_pd_get_state()
    }

    fn border_routing_dhcp6_pd_set_request_fn<'a, F>(&'a self, f: Option<F>)
    where
        F: FnMut(BorderRoutingDhcp6PdState) + 'a,
    {
        self.as_ref().border_routing_dhcp6_pd_set_request_fn(f)
    }

    fn border_routing_dhcp6_pd_state_change_stream(
        &self,
    ) -> BorderRoutingDhcp6PdStateChangedStream {
        self.as_ref().border_routing_dhcp6_pd_state_change_stream()
    }

    fn border_routing_get_pd_omr_prefix(&self) -> Result<ot::BorderRoutingPrefixTableEntry> {
        self.as_ref().border_routing_get_pd_omr_prefix()
    }

    fn border_routing_get_omr_prefix(&self) -> Result<ot::Ip6Prefix> {
        self.as_ref().border_routing_get_omr_prefix()
    }

    fn border_routing_get_on_link_prefix(&self) -> Result<ot::Ip6Prefix> {
        self.as_ref().border_routing_get_on_link_prefix()
    }

    fn border_routing_process_icmp6_ra(&self, message: &[u8]) -> Result<(), WrongSize> {
        self.as_ref().border_routing_process_icmp6_ra(message)
    }

    fn border_routing_get_pd_processed_ra_info(&self) -> PdProcessedRaInfo {
        self.as_ref().border_routing_get_pd_processed_ra_info()
    }

    fn iter_next_local_external_route(
        &self,
        ot_iter: &mut otNetworkDataIterator,
    ) -> Option<ExternalRouteConfig> {
        self.as_ref().iter_next_local_external_route(ot_iter)
    }

    fn iter_next_local_on_mesh_prefix(
        &self,
        ot_iter: &mut otNetworkDataIterator,
    ) -> Option<BorderRouterConfig> {
        self.as_ref().iter_next_local_on_mesh_prefix(ot_iter)
    }
}

impl BorderRouter for Instance {
    fn add_external_route(&self, route: &ExternalRouteConfig) -> Result {
        Error::from(unsafe { otBorderRouterAddRoute(self.as_ot_ptr(), route.as_ot_ptr()) }).into()
    }

    fn add_on_mesh_prefix(&self, route: &BorderRouterConfig) -> Result {
        Error::from(unsafe { otBorderRouterAddOnMeshPrefix(self.as_ot_ptr(), route.as_ot_ptr()) })
            .into()
    }

    fn remove_external_route(&self, prefix: &Ip6Prefix) -> Result {
        Error::from(unsafe { otBorderRouterRemoveRoute(self.as_ot_ptr(), prefix.as_ot_ptr()) })
            .into()
    }

    fn remove_on_mesh_prefix(&self, prefix: &Ip6Prefix) -> Result {
        Error::from(unsafe {
            otBorderRouterRemoveOnMeshPrefix(self.as_ot_ptr(), prefix.as_ot_ptr())
        })
        .into()
    }

    fn border_router_register(&self) -> Result {
        Error::from(unsafe { otBorderRouterRegister(self.as_ot_ptr()) }).into()
    }

    fn border_routing_init(&self, infra_if: u32, infra_is_running: bool) -> Result {
        Error::from(unsafe { otBorderRoutingInit(self.as_ot_ptr(), infra_if, infra_is_running) })
            .into()
    }

    fn border_routing_set_enabled(&self, enabled: bool) -> Result {
        Error::from(unsafe { otBorderRoutingSetEnabled(self.as_ot_ptr(), enabled) }).into()
    }

    fn border_routing_dhcp6_pd_set_enabled(&self, enabled: bool) {
        unsafe { otBorderRoutingDhcp6PdSetEnabled(self.as_ot_ptr(), enabled) }
    }

    fn border_routing_dhcp6_pd_get_state(&self) -> BorderRoutingDhcp6PdState {
        BorderRoutingDhcp6PdState::from_isize(unsafe {
            otBorderRoutingDhcp6PdGetState(self.as_ot_ptr())
        } as isize)
        .unwrap_or(BorderRoutingDhcp6PdState::Disabled)
    }

    fn border_routing_dhcp6_pd_set_request_fn<'a, F>(&'a self, f: Option<F>)
    where
        F: FnMut(BorderRoutingDhcp6PdState) + 'a,
    {
        unsafe extern "C" fn _ot_border_routing_dhcp6_pd_state_change_callback<
            'a,
            F: FnMut(BorderRoutingDhcp6PdState) + 'a,
        >(
            state: otBorderRoutingDhcp6PdState,
            context: *mut ::std::os::raw::c_void,
        ) {
            trace!("_ot_border_routing_dhcp6_pd_state_change_callback");

            // Convert `otBorderRoutingDhcp6PdState` to `BorderRoutingDhcp6PdState`
            let state = BorderRoutingDhcp6PdState::from(state);

            // Reconstitute a reference to our closure.
            let sender = &mut *(context as *mut F);

            sender(state)
        }

        let (fn_ptr, fn_box, cb): (_, _, otBorderRoutingRequestDhcp6PdCallback) = if let Some(f) = f
        {
            let mut x = Box::new(f);

            (
                x.as_mut() as *mut F as *mut ::std::os::raw::c_void,
                Some(x as Box<dyn FnMut(BorderRoutingDhcp6PdState) + 'a>),
                Some(_ot_border_routing_dhcp6_pd_state_change_callback::<F>),
            )
        } else {
            (std::ptr::null_mut() as *mut ::std::os::raw::c_void, None, None)
        };

        unsafe {
            otBorderRoutingDhcp6PdSetRequestCallback(self.as_ot_ptr(), cb, fn_ptr);

            // Make sure our object eventually gets cleaned up.
            // Here we must also transmute our closure to have a 'static lifetime.
            // We need to do this because the borrow checker cannot infer the
            // proper lifetime for the singleton instance backing, but
            // this is guaranteed by the API.
            self.borrow_backing().dhcp6pd_state_change_callback_fn.set(std::mem::transmute::<
                Option<Box<dyn FnMut(BorderRoutingDhcp6PdState) + 'a>>,
                Option<Box<dyn FnMut(BorderRoutingDhcp6PdState) + 'static>>,
            >(fn_box));
        }
    }

    fn border_routing_dhcp6_pd_state_change_stream(
        &self,
    ) -> BorderRoutingDhcp6PdStateChangedStream {
        let state = Arc::new(Mutex::new((None, futures::task::noop_waker())));

        let state_copy = state.clone();

        self.border_routing_dhcp6_pd_set_request_fn(Some(
            move |pd_state: BorderRoutingDhcp6PdState| {
                let mut borrowed = state_copy.lock();
                borrowed.0 = Some(pd_state);
                borrowed.1.wake_by_ref();
            },
        ));

        BorderRoutingDhcp6PdStateChangedStream(state)
    }

    fn border_routing_get_pd_omr_prefix(&self) -> Result<ot::BorderRoutingPrefixTableEntry> {
        let mut ret: BorderRoutingPrefixTableEntry = Default::default();
        Error::from(unsafe {
            otBorderRoutingGetPdOmrPrefix(self.as_ot_ptr(), ret.as_ot_mut_ptr())
        })
        .into_result()?;
        Ok(ret)
    }

    fn border_routing_get_omr_prefix(&self) -> Result<ot::Ip6Prefix> {
        let mut ret: Ip6Prefix = Default::default();
        Error::from(unsafe { otBorderRoutingGetOmrPrefix(self.as_ot_ptr(), ret.as_ot_mut_ptr()) })
            .into_result()?;
        Ok(ret)
    }

    fn border_routing_get_on_link_prefix(&self) -> Result<ot::Ip6Prefix> {
        let mut ret: Ip6Prefix = Default::default();
        Error::from(unsafe {
            otBorderRoutingGetOnLinkPrefix(self.as_ot_ptr(), ret.as_ot_mut_ptr())
        })
        .into_result()?;
        Ok(ret)
    }

    fn border_routing_process_icmp6_ra(&self, message: &[u8]) -> Result<(), WrongSize> {
        unsafe {
            otPlatBorderRoutingProcessIcmp6Ra(
                self.as_ot_ptr(),
                message.as_ptr(),
                message.len().try_into().map_err(|_| WrongSize)?,
            )
        }
        Ok(())
    }

    fn border_routing_get_pd_processed_ra_info(&self) -> PdProcessedRaInfo {
        let mut info = PdProcessedRaInfo::default();
        unsafe {
            otBorderRoutingGetPdProcessedRaInfo(self.as_ot_ptr(), info.as_ot_mut_ptr());
        }
        info
    }

    fn iter_next_local_external_route(
        &self,
        ot_iter: &mut otNetworkDataIterator,
    ) -> Option<ExternalRouteConfig> {
        unsafe {
            let mut ret = ExternalRouteConfig::default();
            match Error::from(otBorderRouterGetNextRoute(
                self.as_ot_ptr(),
                ot_iter as *mut otNetworkDataIterator,
                ret.as_ot_mut_ptr(),
            )) {
                Error::NotFound => None,
                Error::None => Some(ret),
                err => panic!("Unexpected error from otBorderRouterGetNextRoute: {err:?}"),
            }
        }
    }

    fn iter_next_local_on_mesh_prefix(
        &self,
        ot_iter: &mut otNetworkDataIterator,
    ) -> Option<BorderRouterConfig> {
        unsafe {
            let mut ret = BorderRouterConfig::default();
            match Error::from(otBorderRouterGetNextOnMeshPrefix(
                self.as_ot_ptr(),
                ot_iter as *mut otNetworkDataIterator,
                ret.as_ot_mut_ptr(),
            )) {
                Error::NotFound => None,
                Error::None => Some(ret),
                err => panic!("Unexpected error from otBorderRouterGetNextOnMeshPrefix: {err:?}"),
            }
        }
    }
}

/// Stream for getting state changed events.
#[derive(Debug, Clone)]
pub struct BorderRoutingDhcp6PdStateChangedStream(
    Arc<Mutex<(Option<BorderRoutingDhcp6PdState>, Waker)>>,
);

impl Stream for BorderRoutingDhcp6PdStateChangedStream {
    type Item = BorderRoutingDhcp6PdState;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut state = self.0.lock();

        state.1 = cx.waker().clone();

        if let Some(pd_state) = state.0.take() {
            Poll::Ready(Some(pd_state))
        } else {
            Poll::Pending
        }
    }
}
