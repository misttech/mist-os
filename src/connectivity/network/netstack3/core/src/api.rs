// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

///! Defines the main API entry objects for the exposed API from core.
use lock_order::Unlocked;
use net_types::ip::Ip;

use crate::{
    context::{ContextProvider, CoreCtx, CtxPair},
    device::socket::DeviceSocketApi,
    ip::{
        api::{RoutesAnyApi, RoutesApi},
        icmp::socket::IcmpEchoSocketApi,
    },
    transport::{tcp::socket::TcpApi, udp::UdpApi},
    BindingsTypes,
};

type CoreApiCtxPair<'a, BP> = CtxPair<CoreCtx<'a, <BP as ContextProvider>::Context, Unlocked>, BP>;

/// The single entry point for function calls into netstack3 core.
pub struct CoreApi<'a, BP>(CoreApiCtxPair<'a, BP>)
where
    BP: ContextProvider,
    BP::Context: BindingsTypes;

impl<'a, BP> CoreApi<'a, BP>
where
    BP: ContextProvider,
    BP::Context: BindingsTypes,
{
    pub(crate) fn new(ctx_pair: CoreApiCtxPair<'a, BP>) -> Self {
        Self(ctx_pair)
    }

    /// Gets access to the UDP API for IP version `I`.
    pub fn udp<I: Ip>(self) -> UdpApi<I, CoreApiCtxPair<'a, BP>> {
        let Self(ctx) = self;
        UdpApi::new(ctx)
    }

    /// Gets access to the ICMP socket API for IP version `I`.
    pub fn icmp_echo<I: Ip>(self) -> IcmpEchoSocketApi<I, CoreApiCtxPair<'a, BP>> {
        let Self(ctx) = self;
        IcmpEchoSocketApi::new(ctx)
    }

    /// Gets access to the TCP API for IP version `I`.
    pub fn tcp<I: Ip>(self) -> TcpApi<I, CoreApiCtxPair<'a, BP>> {
        let Self(ctx) = self;
        TcpApi::new(ctx)
    }

    /// Gets access to the device socket API.
    pub fn device_socket(self) -> DeviceSocketApi<CoreApiCtxPair<'a, BP>> {
        let Self(ctx) = self;
        DeviceSocketApi::new(ctx)
    }

    /// Gets access to the routes API for IP version `I`.
    pub fn routes<I: Ip>(self) -> RoutesApi<I, CoreApiCtxPair<'a, BP>> {
        let Self(ctx) = self;
        RoutesApi::new(ctx)
    }

    /// Gets access to the routes API for IP version `I`.
    pub fn routes_any(self) -> RoutesAnyApi<CoreApiCtxPair<'a, BP>> {
        let Self(ctx) = self;
        RoutesAnyApi::new(ctx)
    }
}

#[cfg(any(test, feature = "testutils"))]
impl<'a, BC> CoreApi<'a, &'a mut BC>
where
    BC: BindingsTypes,
{
    /// Creates a `CoreApi` from a `SyncCtx` and bindings context.
    // TODO(https://fxbug.dev/42083910): Remove this function once all the tests
    // are migrated to the new API structs. This is a handy transitional step
    // while we still have functions taking split contexts.
    pub fn with_contexts(core_ctx: &'a crate::SyncCtx<BC>, bindings_ctx: &'a mut BC) -> Self {
        core_ctx.state.api(bindings_ctx)
    }
}
