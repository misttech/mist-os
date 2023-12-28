// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::borrow::Borrow;

use fidl_fuchsia_net_routes_admin as fnet_routes_admin;
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;
use fnet_routes_ext::{
    admin::{FidlRouteAdminIpExt, Responder as _, RouteSetRequest},
    FidlRouteIpExt,
};
use futures::{pin_mut, TryStream, TryStreamExt as _};
use net_types::ip::{GenericOverIp, Ip, IpAddress, IpVersion, Ipv4, Ipv6};
use netstack3_core::{device::DeviceId, routes::AddableEntry};

use crate::bindings::{
    routes,
    util::{TaskWaitGroupSpawner, TryFromFidlWithContext},
    BindingsCtx,
};

async fn serve_user_route_set<I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt>(
    ctx: crate::bindings::Ctx,
    stream: I::RouteSetRequestStream,
) {
    let route_set = UserRouteSet::new(ctx);

    serve_route_set::<I, UserRouteSet, _>(stream, &route_set).await;

    route_set.close().await;
}

pub(crate) async fn serve_route_set<
    I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt,
    R: RouteSet,
    B: Borrow<R>,
>(
    stream: I::RouteSetRequestStream,
    route_set: B,
) {
    let debug_name = match I::VERSION {
        IpVersion::V4 => "RouteSetV4",
        IpVersion::V6 => "RouteSetV6",
    };

    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct In<I: fnet_routes_ext::admin::FidlRouteAdminIpExt>(
        <I::RouteSetRequestStream as TryStream>::Ok,
    );

    stream
        .try_for_each(|request| {
            let route_set = route_set.borrow();
            async move {
                let request = net_types::map_ip_twice!(I, In(request), |In(request)| {
                    fnet_routes_ext::admin::RouteSetRequest::<I>::from(request)
                });

                route_set.handle_request(request).await.unwrap_or_else(|e| {
                    if !e.is_closed() {
                        tracing::error!("error handling {debug_name} request: {e:?}");
                    }
                });
                Ok(())
            }
        })
        .await
        .unwrap_or_else(|e| {
            if !e.is_closed() {
                tracing::error!("error serving {debug_name}: {e:?}");
            }
        });
}

pub(crate) async fn serve_provider_v4(
    stream: fnet_routes_admin::SetProviderV4RequestStream,
    spawner: TaskWaitGroupSpawner,
    ctx: &crate::bindings::Ctx,
) -> Result<(), fidl::Error> {
    pin_mut!(stream);

    while let Some(req) = stream.try_next().await? {
        let () = match req {
            fnet_routes_admin::SetProviderV4Request::NewRouteSet {
                route_set,
                control_handle: _,
            } => {
                let set_request_stream = route_set.into_stream()?;
                spawner.spawn(serve_user_route_set::<Ipv4>(ctx.clone(), set_request_stream));
            }
        };
    }

    Ok(())
}

pub(crate) async fn serve_provider_v6(
    stream: fnet_routes_admin::SetProviderV6RequestStream,
    spawner: TaskWaitGroupSpawner,
    ctx: &crate::bindings::Ctx,
) -> Result<(), fidl::Error> {
    pin_mut!(stream);

    while let Some(req) = stream.try_next().await? {
        let () = match req {
            fnet_routes_admin::SetProviderV6Request::NewRouteSet {
                route_set,
                control_handle: _,
            } => {
                let set_request_stream = route_set.into_stream()?;
                spawner.spawn(serve_user_route_set::<Ipv6>(ctx.clone(), set_request_stream));
            }
        };
    }

    Ok(())
}

#[derive(Debug)]
pub(crate) struct UserRouteSetId {
    _private_field_to_prevent_construction_outside_of_this_mod: (),
}

pub(crate) type WeakUserRouteSet = netstack3_core::sync::WeakRc<UserRouteSetId>;
pub(crate) type StrongUserRouteSet = netstack3_core::sync::StrongRc<UserRouteSetId>;

#[must_use = "UserRouteSets must explicitly have `.close()` called on them before dropping them"]
pub(crate) struct UserRouteSet {
    ctx: crate::bindings::Ctx,
    set: Option<netstack3_core::sync::PrimaryRc<UserRouteSetId>>,
}

impl Drop for UserRouteSet {
    fn drop(&mut self) {
        if self.set.is_some() {
            panic!("UserRouteSet must not be dropped without calling close()");
        }
    }
}

impl UserRouteSet {
    #[cfg_attr(feature = "instrumented", track_caller)]
    pub(crate) fn new(ctx: crate::bindings::Ctx) -> Self {
        let set = netstack3_core::sync::PrimaryRc::new(UserRouteSetId {
            _private_field_to_prevent_construction_outside_of_this_mod: (),
        });
        Self { ctx, set: Some(set) }
    }

    fn weak_set_id(&self) -> netstack3_core::sync::WeakRc<UserRouteSetId> {
        netstack3_core::sync::PrimaryRc::downgrade(
            self.set.as_ref().expect("close() can't have been called because it takes ownership"),
        )
    }

    pub(crate) async fn close(mut self) {
        fn consume_outcome(result: Result<routes::ChangeOutcome, routes::Error>) {
            match result {
                Ok(outcome) => match outcome {
                    routes::ChangeOutcome::Changed | routes::ChangeOutcome::NoChange => {
                        // We don't care what the outcome was as long as it succeeded.
                    }
                },
                Err(err) => match err {
                    routes::Error::ShuttingDown => panic!("routes change worker is shutting down"),
                    routes::Error::DeviceRemoved => {
                        unreachable!("closing a route set should not require upgrading a DeviceId")
                    }
                    routes::Error::SetRemoved => {
                        unreachable!(
                            "SetRemoved should not be observable while closing a route set, \
                            as `RouteSet::close()` takes ownership of `self` and thus can't be \
                            called twice on the same RouteSet"
                        )
                    }
                },
            }
        }

        consume_outcome(
            self.ctx
                .bindings_ctx()
                .apply_route_change::<Ipv4>(routes::Change::RemoveSet(self.weak_set_id()))
                .await,
        );
        consume_outcome(
            self.ctx
                .bindings_ctx()
                .apply_route_change::<Ipv6>(routes::Change::RemoveSet(self.weak_set_id()))
                .await,
        );

        let UserRouteSet { ctx: _, set } = &mut self;
        let UserRouteSetId { _private_field_to_prevent_construction_outside_of_this_mod: () } =
            netstack3_core::sync::PrimaryRc::unwrap(
                set.take().expect("close() can't be called twice"),
            );
    }
}

impl RouteSet for UserRouteSet {
    fn set(&self) -> routes::SetMembership<netstack3_core::sync::WeakRc<UserRouteSetId>> {
        routes::SetMembership::User(self.weak_set_id())
    }

    fn ctx(&self) -> &crate::bindings::Ctx {
        &self.ctx
    }
}

pub(crate) struct GlobalRouteSet {
    ctx: crate::bindings::Ctx,
}

impl GlobalRouteSet {
    #[cfg_attr(feature = "instrumented", track_caller)]
    pub(crate) fn new(ctx: crate::bindings::Ctx) -> Self {
        Self { ctx }
    }
}

impl RouteSet for GlobalRouteSet {
    fn set(
        &self,
    ) -> routes::SetMembership<netstack3_core::sync::WeakRc<routes::admin::UserRouteSetId>> {
        routes::SetMembership::Global
    }

    fn ctx(&self) -> &crate::bindings::Ctx {
        &self.ctx
    }
}

pub(crate) trait RouteSet: Send + Sync {
    fn set(&self) -> routes::SetMembership<netstack3_core::sync::WeakRc<UserRouteSetId>>;
    fn ctx(&self) -> &crate::bindings::Ctx;

    async fn handle_request<I: Ip + FidlRouteAdminIpExt + fnet_routes_ext::FidlRouteIpExt>(
        &self,
        request: RouteSetRequest<I>,
    ) -> Result<(), fidl::Error> {
        tracing::debug!("RouteSet::handle_request {request:?}");

        match request {
            RouteSetRequest::AddRoute { route, responder } => {
                let route = match route {
                    Ok(route) => route,
                    Err(e) => {
                        return responder.send(Err(e.into()));
                    }
                };

                let result = self.add_fidl_route(route).await;
                responder.send(result)
            }
            RouteSetRequest::RemoveRoute { route, responder } => {
                let route = match route {
                    Ok(route) => route,
                    Err(e) => {
                        return responder.send(Err(e.into()));
                    }
                };

                let result = self.remove_fidl_route(route).await;
                responder.send(result)
            }
            RouteSetRequest::AuthenticateForInterface { credential: _, responder } => {
                tracing::warn!(
                    "TODO(https://fxbug.dev/134307): RouteSetVX.AuthenticateForInterface \
                    is not implemented; assuming authenticated"
                );

                responder.send(Ok(()))
            }
        }
    }

    async fn apply_route_op<A: IpAddress>(
        &self,
        op: routes::RouteOp<A>,
    ) -> Result<routes::ChangeOutcome, routes::Error> {
        self.ctx()
            .bindings_ctx()
            .apply_route_change::<A::Version>(routes::Change::RouteOp(op, self.set()))
            .await
    }

    async fn add_fidl_route<I: Ip>(
        &self,
        route: fnet_routes_ext::Route<I>,
    ) -> Result<bool, fnet_routes_admin::RouteSetError> {
        let addable_entry = try_to_addable_entry::<I>(self.ctx().bindings_ctx(), route)?
            .map_device_id(|d| d.downgrade());

        let result = self.apply_route_op::<I::Addr>(routes::RouteOp::Add(addable_entry)).await;

        match result {
            Ok(outcome) => match outcome {
                routes::ChangeOutcome::NoChange => Ok(false),
                routes::ChangeOutcome::Changed => Ok(true),
            },
            Err(err) => match err {
                routes::Error::DeviceRemoved => Err(
                    fnet_routes_admin::RouteSetError::PreviouslyAuthenticatedInterfaceNoLongerExists,
                ),
                routes::Error::ShuttingDown => panic!("routes change worker is shutting down"),
                routes::Error::SetRemoved => unreachable!(
                    "SetRemoved should not be observable while holding a route set, \
                    as `RouteSet::close()` takes ownership of `self`"
                ),
            },
        }
    }

    async fn remove_fidl_route<I: Ip>(
        &self,
        route: fnet_routes_ext::Route<I>,
    ) -> Result<bool, fnet_routes_admin::RouteSetError> {
        let AddableEntry { subnet, device, gateway, metric } =
            try_to_addable_entry::<I>(self.ctx().bindings_ctx(), route)?
                .map_device_id(|d| d.downgrade());

        let result = self
            .apply_route_op::<I::Addr>(routes::RouteOp::RemoveMatching {
                subnet,
                device,
                gateway,
                metric: Some(metric),
            })
            .await;

        match result {
            Ok(outcome) => match outcome {
                routes::ChangeOutcome::NoChange => Ok(false),
                routes::ChangeOutcome::Changed => Ok(true),
            },
            Err(err) => match err {
                routes::Error::DeviceRemoved => Err(
                    fnet_routes_admin::RouteSetError::PreviouslyAuthenticatedInterfaceNoLongerExists,
                ),
                routes::Error::ShuttingDown => panic!("routes change worker is shutting down"),
                routes::Error::SetRemoved => unreachable!(
                    "SetRemoved should not be observable while holding a route set, \
                    as `RouteSet::close()` takes ownership of `self`"
                ),
            },
        }
    }
}

fn try_to_addable_entry<I: Ip>(
    bindings_ctx: &crate::bindings::BindingsCtx,
    route: fnet_routes_ext::Route<I>,
) -> Result<AddableEntry<I::Addr, DeviceId<BindingsCtx>>, fnet_routes_admin::RouteSetError> {
    AddableEntry::try_from_fidl_with_ctx(bindings_ctx, route).map_err(|err| match err {
        crate::bindings::util::AddableEntryFromRoutesExtError::DeviceNotFound => {
            fnet_routes_admin::RouteSetError::PreviouslyAuthenticatedInterfaceNoLongerExists
        }
        crate::bindings::util::AddableEntryFromRoutesExtError::UnknownAction => {
            fnet_routes_admin::RouteSetError::UnsupportedAction
        }
    })
}
