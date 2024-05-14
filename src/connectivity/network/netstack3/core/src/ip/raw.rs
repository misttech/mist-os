// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Facilities backing raw IP sockets.

use alloc::collections::HashMap;
use core::fmt::Debug;
use derivative::Derivative;
use net_types::ip::{Ip, IpVersionMarker};
use tracing::debug;

use crate::{
    context::ContextPair,
    ip::{
        base::IpLayerIpExt,
        raw::state::{RawIpSocketLockedState, RawIpSocketState},
    },
    sync::{PrimaryRc, StrongRc},
};

mod integration;
mod state;

/// Types provided by bindings used in the raw IP socket implementation.
pub trait RawIpSocketsBindingsTypes {
    /// The bindings state (opaque to core) associated with a socket.
    type RawIpSocketState<I: Ip>: Send + Sync + Debug;
}

/// The raw IP socket API.
pub struct RawIpSocketApi<I: Ip, C> {
    ctx: C,
    _ip_mark: IpVersionMarker<I>,
}

impl<I: Ip, C> RawIpSocketApi<I, C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self { ctx, _ip_mark: IpVersionMarker::new() }
    }
}

impl<I: IpLayerIpExt, C> RawIpSocketApi<I, C>
where
    C: ContextPair,
    C::BindingsContext: RawIpSocketsBindingsTypes,
    C::CoreContext: RawIpSocketMapContext<I, C::BindingsContext>,
{
    fn core_ctx(&mut self) -> &mut C::CoreContext {
        let Self { ctx, _ip_mark } = self;
        ctx.core_ctx()
    }

    /// Creates a raw IP socket for the given protocol.
    pub fn create(
        &mut self,
        protocol: I::Proto,
        external_state: <C::BindingsContext as RawIpSocketsBindingsTypes>::RawIpSocketState<I>,
    ) -> RawIpApiSocketId<I, C> {
        let socket =
            PrimaryRawIpSocketId(PrimaryRc::new(RawIpSocketState::new(protocol, external_state)));
        let strong = self.core_ctx().with_socket_map_mut(|socket_map| socket_map.insert(socket));
        debug!("created raw IP socket {strong:?}, on protocol {protocol}");
        strong
    }

    /// Removes the raw IP socket from the system, returning its external state.
    ///
    /// # Panics
    ///
    /// If the provided `id` is not the last instance for the socket, this
    /// method will panic.
    // TODO(https://fxbug.dev/42175797): Support deferred socket removal,
    // similar to UDP.
    pub fn remove(
        &mut self,
        id: RawIpApiSocketId<I, C>,
    ) -> <C::BindingsContext as RawIpSocketsBindingsTypes>::RawIpSocketState<I> {
        let primary = self.core_ctx().with_socket_map_mut(|socket_map| socket_map.remove(id));
        debug!("removed raw IP socket {primary:?}");
        let PrimaryRawIpSocketId(primary) = primary;
        PrimaryRc::unwrap(primary).into_external_state()
    }
}

/// The owner of socket state.
struct PrimaryRawIpSocketId<I: IpLayerIpExt, BT: RawIpSocketsBindingsTypes>(
    PrimaryRc<RawIpSocketState<I, BT>>,
);

impl<I: IpLayerIpExt, BT: RawIpSocketsBindingsTypes> Debug for PrimaryRawIpSocketId<I, BT> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_tuple("RawIpSocketId").field(&PrimaryRc::debug_id(rc)).finish()
    }
}

/// Reference to the state of a live socket.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Eq(bound = ""), Hash(bound = ""), PartialEq(bound = ""))]
pub struct RawIpSocketId<I: IpLayerIpExt, BT: RawIpSocketsBindingsTypes>(
    StrongRc<RawIpSocketState<I, BT>>,
);

impl<I: IpLayerIpExt, BT: RawIpSocketsBindingsTypes> Debug for RawIpSocketId<I, BT> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_tuple("RawIpSocketId").field(&StrongRc::debug_id(rc)).finish()
    }
}

/// An alias for [`RawIpSocketId`] in [`RawIpSocketApi`], for brevity.
type RawIpApiSocketId<I, C> = RawIpSocketId<I, <C as ContextPair>::BindingsContext>;

/// Provides access to the [`RawIpSocketLockedState`] for a raw IP socket.
///
/// Implementations must ensure a proper lock ordering is adhered to.
// TODO(https://fxbug.dev/42175797): Use this trait to access socket state.
#[allow(dead_code)]
trait RawIpSocketStateContext<I: IpLayerIpExt, BT: RawIpSocketsBindingsTypes> {
    fn with_locked_state<O, F: FnOnce(&RawIpSocketLockedState<I>) -> O>(
        &mut self,
        id: &RawIpSocketId<I, BT>,
        cb: F,
    ) -> O;
    fn with_locked_state_mut<O, F: FnOnce(&mut RawIpSocketLockedState<I>) -> O>(
        &mut self,
        id: &RawIpSocketId<I, BT>,
        cb: F,
    ) -> O;
}

/// The collection of all raw IP sockets installed in the system.
///
/// Implementations must ensure a proper lock ordering is adhered to.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct RawIpSocketMap<I: IpLayerIpExt, BT: RawIpSocketsBindingsTypes> {
    /// All sockets installed in the system.
    ///
    /// NB: use a `HashMap` keyed by strong IDs, rather than an `HashSet` keyed
    /// by primary IDs, because it would be impossible to build a lookup key for
    /// the hashset (there can only ever exist 1 primary ID, which is *in* the
    /// set).
    sockets: HashMap<RawIpSocketId<I, BT>, PrimaryRawIpSocketId<I, BT>>,
}

impl<I: IpLayerIpExt, BT: RawIpSocketsBindingsTypes> RawIpSocketMap<I, BT> {
    fn insert(&mut self, socket: PrimaryRawIpSocketId<I, BT>) -> RawIpSocketId<I, BT> {
        let PrimaryRawIpSocketId(primary) = &socket;
        let strong = RawIpSocketId(PrimaryRc::clone_strong(primary));
        // NB: The socket must be newly inserted because there can only ever
        // be a single primary ID for a socket.
        assert!(self.sockets.insert(strong.clone(), socket).is_none());
        strong
    }

    fn remove(&mut self, socket: RawIpSocketId<I, BT>) -> PrimaryRawIpSocketId<I, BT> {
        // NB: The socket must be present in the map, because the strong id is
        // witness to the liveness of the socket.
        self.sockets.remove(&socket).unwrap()
    }
}

/// A type that provides access to the `RawIpSocketMap` used by the system.
pub trait RawIpSocketMapContext<I: IpLayerIpExt, BT: RawIpSocketsBindingsTypes> {
    // TODO(https://fxbug.dev/42175797): Use this method during rx.
    #[allow(dead_code)]
    fn with_socket_map<O, F: FnOnce(&RawIpSocketMap<I, BT>) -> O>(&mut self, cb: F) -> O;
    fn with_socket_map_mut<O, F: FnOnce(&mut RawIpSocketMap<I, BT>) -> O>(&mut self, cb: F) -> O;
}

#[cfg(test)]
mod test {
    use super::*;

    use ip_test_macro::ip_test;
    use net_types::ip::{Ipv4, Ipv6};
    use packet_formats::ip::IpProto;

    use crate::context::{ContextProvider, CtxPair};

    #[derive(Default, Debug)]
    struct FakeExternalSocketState {}

    #[derive(Default)]
    struct FakeBindingsCtx {}
    #[derive(Default)]
    struct FakeCoreCtx<I: IpLayerIpExt> {
        socket_map: RawIpSocketMap<I, FakeBindingsCtx>,
    }

    impl RawIpSocketsBindingsTypes for FakeBindingsCtx {
        type RawIpSocketState<I: Ip> = FakeExternalSocketState;
    }

    impl<I: IpLayerIpExt> RawIpSocketMapContext<I, FakeBindingsCtx> for FakeCoreCtx<I> {
        fn with_socket_map<O, F: FnOnce(&RawIpSocketMap<I, FakeBindingsCtx>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            cb(&self.socket_map)
        }
        fn with_socket_map_mut<O, F: FnOnce(&mut RawIpSocketMap<I, FakeBindingsCtx>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            cb(&mut self.socket_map)
        }
    }

    impl ContextProvider for FakeBindingsCtx {
        type Context = FakeBindingsCtx;
        fn context(&mut self) -> &mut Self::Context {
            self
        }
    }

    impl<I: IpLayerIpExt> ContextProvider for FakeCoreCtx<I> {
        type Context = FakeCoreCtx<I>;
        fn context(&mut self) -> &mut Self::Context {
            self
        }
    }

    fn new_raw_ip_socket_api<I: IpLayerIpExt>(
    ) -> RawIpSocketApi<I, CtxPair<FakeCoreCtx<I>, FakeBindingsCtx>> {
        RawIpSocketApi::new(Default::default())
    }

    #[ip_test]
    fn create_and_remove<I: Ip + IpLayerIpExt>() {
        let mut api = new_raw_ip_socket_api::<I>();
        let sock = api.create(IpProto::Udp.into(), Default::default());
        let FakeExternalSocketState {} = api.remove(sock);
    }
}
