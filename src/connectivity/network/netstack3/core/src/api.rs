// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

///! Defines the main API entry objects for the exposed API from core.
use lock_order::Unlocked;
use net_types::ip::Ip;

use crate::{
    context::{BindingsTypes, ContextProvider, CoreCtx, CtxPair},
    transport::udp::UdpApi,
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
}
