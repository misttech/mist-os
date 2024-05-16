// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{borrow::BorrowMut, collections::HashSet, pin::pin};

use fidl::endpoints::{ControlHandle as _, ProtocolMarker as _};
use fidl_fuchsia_net_interfaces_admin::ProofOfInterfaceAuthorization;
use fidl_fuchsia_net_routes_admin as fnet_routes_admin;
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;
use fnet_routes_ext::{
    admin::{FidlRouteAdminIpExt, Responder as _, RouteSetRequest, RouteTableRequest},
    FidlRouteIpExt,
};
use fuchsia_zircon::{self as zx, AsHandleRef, HandleBased as _};
use futures::{
    channel::{mpsc, oneshot},
    TryStream, TryStreamExt as _,
};
use net_types::ip::{GenericOverIp, Ip, IpVersion, Ipv4, Ipv6};
use netstack3_core::{device::DeviceId, routes::AddableEntry};

use crate::bindings::{
    devices::StaticCommonInfo,
    routes::{self, witness::TableId},
    util::{TaskWaitGroupSpawner, TryFromFidlWithContext},
    BindingsCtx, Ctx, DeviceIdExt,
};

use super::{RouteWorkItem, WeakDeviceId};

async fn serve_user_route_set<I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt>(
    stream: I::RouteSetRequestStream,
    mut route_set: UserRouteSet<I>,
) {
    serve_route_set::<I, UserRouteSet<I>, _>(stream, &mut route_set).await;

    route_set.close().await;
}

pub(crate) async fn serve_route_set<
    I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt,
    R: RouteSet<I>,
    B: BorrowMut<R>,
>(
    stream: I::RouteSetRequestStream,
    mut route_set: B,
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
        .try_fold(route_set.borrow_mut(), |route_set, request| async {
            let request = net_types::map_ip_twice!(I, In(request), |In(request)| {
                fnet_routes_ext::admin::RouteSetRequest::<I>::from(request)
            });

            route_set.handle_request(request).await.unwrap_or_else(|e| {
                if !e.is_closed() {
                    tracing::error!("error handling {debug_name} request: {e:?}");
                }
            });
            Ok(route_set)
        })
        .await
        .map(|_| ())
        .unwrap_or_else(|e| {
            if !e.is_closed() {
                tracing::error!("error serving {debug_name}: {e:?}");
            }
        });
}

pub(crate) async fn serve_route_table_provider_v4(
    stream: fnet_routes_admin::RouteTableProviderV4RequestStream,
    spawner: TaskWaitGroupSpawner,
    ctx: &Ctx,
) -> Result<(), fidl::Error> {
    let mut stream = pin!(stream);

    while let Some(req) = stream.try_next().await? {
        match req {
            fnet_routes_admin::RouteTableProviderV4Request::NewRouteTable {
                provider,
                options: fnet_routes_admin::RouteTableOptionsV4 { name, __source_breaking: _ },
                control_handle,
            } => {
                let route_table = match ctx.bindings_ctx().routes.add_table::<Ipv4>().await {
                    Ok((table_id, route_work_sink)) => {
                        UserRouteTable::new(ctx.clone(), name, table_id, route_work_sink)
                    }
                    Err(routes::TableError::TableIdOverflows) => {
                        control_handle.shutdown_with_epitaph(zx::Status::NO_SPACE);
                        break;
                    }
                    Err(routes::TableError::ShuttingDown) => {
                        control_handle.shutdown_with_epitaph(zx::Status::BAD_STATE);
                        break;
                    }
                };
                let stream = provider.into_stream()?;
                spawner.spawn(serve_route_table::<Ipv4, UserRouteTable<Ipv4>, _>(
                    stream,
                    spawner.clone(),
                    route_table,
                ));
            }
        }
    }
    Ok(())
}

pub(crate) async fn serve_route_table_provider_v6(
    stream: fnet_routes_admin::RouteTableProviderV6RequestStream,
    spawner: TaskWaitGroupSpawner,
    ctx: &Ctx,
) -> Result<(), fidl::Error> {
    let mut stream = pin!(stream);

    while let Some(req) = stream.try_next().await? {
        match req {
            fnet_routes_admin::RouteTableProviderV6Request::NewRouteTable {
                provider,
                options: fnet_routes_admin::RouteTableOptionsV6 { name, __source_breaking: _ },
                control_handle,
            } => {
                let route_table = match ctx.bindings_ctx().routes.add_table::<Ipv6>().await {
                    Ok((table_id, route_work_sink)) => {
                        UserRouteTable::new(ctx.clone(), name, table_id, route_work_sink)
                    }
                    Err(routes::TableError::TableIdOverflows) => {
                        control_handle.shutdown_with_epitaph(zx::Status::NO_SPACE);
                        break;
                    }
                    Err(routes::TableError::ShuttingDown) => {
                        control_handle.shutdown_with_epitaph(zx::Status::BAD_STATE);
                        break;
                    }
                };
                let stream = provider.into_stream()?;
                spawner.spawn(serve_route_table::<Ipv6, UserRouteTable<Ipv6>, _>(
                    stream,
                    spawner.clone(),
                    route_table,
                ));
            }
        }
    }
    Ok(())
}

pub(crate) async fn serve_route_table<
    I: Ip + FidlRouteAdminIpExt + FidlRouteIpExt,
    R: RouteTable<I>,
    B: BorrowMut<R>,
>(
    stream: <I as FidlRouteAdminIpExt>::RouteTableRequestStream,
    spawner: TaskWaitGroupSpawner,
    mut route_table: B,
) where
    <I::RouteSetRequestStream as TryStream>::Ok: Send,
{
    let debug_name = I::RouteTableMarker::DEBUG_NAME;

    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct In<I: fnet_routes_ext::admin::FidlRouteAdminIpExt>(
        <I::RouteTableRequestStream as TryStream>::Ok,
    );

    stream
        .try_fold(route_table.borrow_mut(), |route_table, request| async {
            let request = net_types::map_ip_twice!(I, In(request), |In(request)| {
                fnet_routes_ext::admin::RouteTableRequest::<I>::from(request)
            });

            route_table.handle_request(request, spawner.clone()).unwrap_or_else(|e| {
                if !e.is_closed() {
                    tracing::error!("error handling {debug_name} request: {e:?}");
                }
            });
            Ok(route_table)
        })
        .await
        .map(|_| ())
        .unwrap_or_else(|e| {
            if !e.is_closed() {
                tracing::error!("error serving {debug_name}: {e:?}");
            }
        })
}

/// The common trait for operations on route table.
///
/// Allows abstracting differences between netstack and user owned tables.
pub(crate) trait RouteTable<I: FidlRouteAdminIpExt + FidlRouteIpExt>: Send + Sync {
    /// Gets the table ID.
    fn id(&self) -> TableId<I>;
    /// Gets the token for authorization.
    fn token(&self) -> zx::Event;
    /// Removes this route table from the system.
    fn remove(&mut self) -> Result<(), fnet_routes_admin::BaseRouteTableRemoveError>;
    /// Returns a new [`UserRouteSet`] to modify this route table.
    fn new_user_route_set(&self) -> UserRouteSet<I>;

    /// Handles an incoming FIDL request.
    fn handle_request(
        &mut self,
        request: RouteTableRequest<I>,
        spawner: TaskWaitGroupSpawner,
    ) -> Result<(), fidl::Error>
    where
        <I::RouteSetRequestStream as TryStream>::Ok: Send,
    {
        match request {
            RouteTableRequest::NewRouteSet { route_set, control_handle: _ } => {
                let set_request_stream = route_set.into_stream()?;
                spawner.spawn(serve_user_route_set::<I>(
                    set_request_stream,
                    self.new_user_route_set(),
                ));
            }
            RouteTableRequest::GetTableId { responder } => {
                responder.send(self.id().into())?;
            }
            RouteTableRequest::Detach { control_handle: _ } => {}
            RouteTableRequest::Remove { responder } => {
                responder.send(self.remove())?;
            }
            RouteTableRequest::GetAuthorizationForRouteTable { responder } => {
                responder.send(fnet_routes_admin::GrantForRouteTableAuthorization {
                    table_id: self.id().into(),
                    token: self.token(),
                })?;
            }
        };
        Ok(())
    }
}

pub(crate) struct MainRouteTable {
    ctx: Ctx,
    token: zx::Event,
}

impl MainRouteTable {
    pub(crate) fn new(ctx: Ctx) -> Self {
        let token = zx::Event::create();
        Self { ctx, token }
    }
}

impl<I: FidlRouteAdminIpExt + FidlRouteIpExt> RouteTable<I> for MainRouteTable {
    fn id(&self) -> TableId<I> {
        routes::main_table_id::<I>()
    }
    fn token(&self) -> zx::Event {
        self.token
            .duplicate_handle(zx::Rights::TRANSFER | zx::Rights::DUPLICATE)
            .expect("failed to duplicate")
    }
    fn remove(&mut self) -> Result<(), fnet_routes_admin::BaseRouteTableRemoveError> {
        Err(fnet_routes_admin::BaseRouteTableRemoveError::InvalidOpOnMainTable)
    }
    fn new_user_route_set(&self) -> UserRouteSet<I> {
        UserRouteSet::from_main_table(self.ctx.clone())
    }
}

pub(crate) struct UserRouteTable<I: Ip> {
    ctx: Ctx,
    // TODO(https://fxbug.dev/337868190): Use this method in the observational
    // API.
    _name: Option<String>,
    table_id: TableId<I>,
    token: zx::Event,
    route_work_sink: mpsc::UnboundedSender<RouteWorkItem<I::Addr>>,
}

impl<I: Ip> UserRouteTable<I> {
    fn new(
        ctx: Ctx,
        name: Option<String>,
        table_id: TableId<I>,
        route_work_sink: mpsc::UnboundedSender<RouteWorkItem<I::Addr>>,
    ) -> Self {
        let token = zx::Event::create();
        Self { ctx, _name: name, table_id, token, route_work_sink }
    }
}

impl<I: FidlRouteAdminIpExt + FidlRouteIpExt> RouteTable<I> for UserRouteTable<I> {
    fn id(&self) -> TableId<I> {
        self.table_id
    }
    fn token(&self) -> zx::Event {
        self.token
            .duplicate_handle(zx::Rights::TRANSFER | zx::Rights::DUPLICATE)
            .expect("failed to duplicate")
    }
    fn remove(&mut self) -> Result<(), fnet_routes_admin::BaseRouteTableRemoveError> {
        todo!("https://fxbug.dev/337065118: Support table removal")
    }
    fn new_user_route_set(&self) -> UserRouteSet<I> {
        UserRouteSet::new(self.ctx.clone(), self.table_id, self.route_work_sink.clone())
    }
}

#[derive(Debug)]
pub(crate) struct UserRouteSetId<I: Ip> {
    table_id: TableId<I>,
}

pub(crate) type WeakUserRouteSet<I> = netstack3_core::sync::WeakRc<UserRouteSetId<I>>;
pub(crate) type StrongUserRouteSet<I> = netstack3_core::sync::StrongRc<UserRouteSetId<I>>;

impl<I: Ip> UserRouteSetId<I> {
    pub(super) fn table(&self) -> TableId<I> {
        self.table_id
    }
}

#[must_use = "UserRouteSets must explicitly have `.close()` called on them before dropping them"]
pub(crate) struct UserRouteSet<I: Ip> {
    ctx: Ctx,
    set: Option<netstack3_core::sync::PrimaryRc<UserRouteSetId<I>>>,
    route_work_sink: mpsc::UnboundedSender<RouteWorkItem<I::Addr>>,
    authorization_set: HashSet<WeakDeviceId>,
}

impl<I: Ip> Drop for UserRouteSet<I> {
    fn drop(&mut self) {
        if self.set.is_some() {
            panic!("UserRouteSet must not be dropped without calling close()");
        }
    }
}

impl<I: FidlRouteAdminIpExt> UserRouteSet<I> {
    #[cfg_attr(feature = "instrumented", track_caller)]
    pub(crate) fn new(
        ctx: Ctx,
        table: TableId<I>,
        route_work_sink: mpsc::UnboundedSender<RouteWorkItem<I::Addr>>,
    ) -> Self {
        let set = netstack3_core::sync::PrimaryRc::new(UserRouteSetId { table_id: table });
        Self { ctx, set: Some(set), authorization_set: HashSet::new(), route_work_sink }
    }

    pub(crate) fn from_main_table(ctx: Ctx) -> Self {
        let route_work_sink = ctx.bindings_ctx().routes.main_table_route_work_sink::<I>().clone();
        Self::new(ctx, routes::main_table_id::<I>(), route_work_sink)
    }

    fn weak_set_id(&self) -> netstack3_core::sync::WeakRc<UserRouteSetId<I>> {
        netstack3_core::sync::PrimaryRc::downgrade(
            self.set.as_ref().expect("close() can't have been called because it takes ownership"),
        )
    }

    pub(crate) async fn close(mut self) {
        fn consume_outcome(result: Result<routes::ChangeOutcome, routes::ChangeError>) {
            match result {
                Ok(outcome) => match outcome {
                    routes::ChangeOutcome::Changed | routes::ChangeOutcome::NoChange => {
                        // We don't care what the outcome was as long as it succeeded.
                    }
                },
                Err(err) => match err {
                    routes::ChangeError::TableRemoved => {
                        tracing::warn!("the table backing this route set has been removed");
                    }
                    routes::ChangeError::DeviceRemoved => {
                        unreachable!("closing a route set should not require upgrading a DeviceId")
                    }
                    routes::ChangeError::SetRemoved => {
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
            self.apply_route_change(routes::Change::RemoveSet(self.weak_set_id())).await,
        );

        let UserRouteSet { ctx: _, set, authorization_set: _, route_work_sink: _ } = &mut self;
        let UserRouteSetId { table_id: _ } = netstack3_core::sync::PrimaryRc::unwrap(
            set.take().expect("close() can't be called twice"),
        );
    }
}

impl<I: Ip + FidlRouteAdminIpExt> RouteSet<I> for UserRouteSet<I> {
    fn set(&self) -> routes::SetMembership<netstack3_core::sync::WeakRc<UserRouteSetId<I>>> {
        routes::SetMembership::User(self.weak_set_id())
    }

    fn ctx(&self) -> &Ctx {
        &self.ctx
    }

    fn authorization_set(&self) -> &HashSet<WeakDeviceId> {
        &self.authorization_set
    }

    fn authorization_set_mut(&mut self) -> &mut HashSet<WeakDeviceId> {
        &mut self.authorization_set
    }

    fn route_work_sink(&self) -> &mpsc::UnboundedSender<RouteWorkItem<<I>::Addr>> {
        &self.route_work_sink
    }
}

pub(crate) struct GlobalRouteSet {
    ctx: Ctx,
    authorization_set: HashSet<WeakDeviceId>,
}

impl GlobalRouteSet {
    #[cfg_attr(feature = "instrumented", track_caller)]
    pub(crate) fn new(ctx: Ctx) -> Self {
        Self { ctx, authorization_set: HashSet::new() }
    }
}

impl<I: FidlRouteAdminIpExt> RouteSet<I> for GlobalRouteSet {
    fn set(
        &self,
    ) -> routes::SetMembership<netstack3_core::sync::WeakRc<routes::admin::UserRouteSetId<I>>> {
        routes::SetMembership::Global
    }

    fn ctx(&self) -> &Ctx {
        &self.ctx
    }

    fn authorization_set(&self) -> &HashSet<WeakDeviceId> {
        &self.authorization_set
    }

    fn authorization_set_mut(&mut self) -> &mut HashSet<WeakDeviceId> {
        &mut self.authorization_set
    }

    fn route_work_sink(&self) -> &mpsc::UnboundedSender<RouteWorkItem<<I>::Addr>> {
        // TODO(https://fxbug.dev/339567592): GlobalRouteSet should be aware of
        // the route table as well.
        &self.ctx.bindings_ctx().routes.main_table_route_work_sink::<I>()
    }
}

pub(crate) trait RouteSet<I: FidlRouteAdminIpExt>: Send + Sync {
    fn set(&self) -> routes::SetMembership<netstack3_core::sync::WeakRc<UserRouteSetId<I>>>;
    fn ctx(&self) -> &Ctx;
    fn authorization_set(&self) -> &HashSet<WeakDeviceId>;
    fn authorization_set_mut(&mut self) -> &mut HashSet<WeakDeviceId>;
    fn route_work_sink(&self) -> &mpsc::UnboundedSender<RouteWorkItem<I::Addr>>;

    async fn handle_request(&mut self, request: RouteSetRequest<I>) -> Result<(), fidl::Error> {
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
            RouteSetRequest::AuthenticateForInterface { credential, responder } => {
                responder.send(self.authenticate_for_interface(credential))
            }
        }
    }

    async fn apply_route_op(
        &self,
        op: routes::RouteOp<I::Addr>,
    ) -> Result<routes::ChangeOutcome, routes::ChangeError> {
        self.apply_route_change(routes::Change::RouteOp(op, self.set())).await
    }

    async fn apply_route_change(
        &self,
        change: routes::Change<I::Addr>,
    ) -> Result<routes::ChangeOutcome, routes::ChangeError> {
        let sender = self.route_work_sink();
        let (responder, receiver) = oneshot::channel();
        let work_item = RouteWorkItem { change, responder: Some(responder) };
        match sender.unbounded_send(work_item) {
            Ok(()) => receiver.await.expect("responder should not be dropped"),
            Err(e) => {
                let _: mpsc::TrySendError<_> = e;
                Err(routes::ChangeError::TableRemoved)
            }
        }
    }

    async fn add_fidl_route(
        &self,
        route: fnet_routes_ext::Route<I>,
    ) -> Result<bool, fnet_routes_admin::RouteSetError> {
        let addable_entry = try_to_addable_entry::<I>(self.ctx().bindings_ctx(), route)?
            .map_device_id(|d| d.downgrade());

        if !self.authorization_set().contains(&addable_entry.device) {
            return Err(fnet_routes_admin::RouteSetError::Unauthenticated);
        }

        let result = self.apply_route_op(routes::RouteOp::Add(addable_entry)).await;

        match result {
            Ok(outcome) => match outcome {
                routes::ChangeOutcome::NoChange => Ok(false),
                routes::ChangeOutcome::Changed => Ok(true),
            },
            Err(err) => match err {
                routes::ChangeError::DeviceRemoved => Err(
                    fnet_routes_admin::RouteSetError::PreviouslyAuthenticatedInterfaceNoLongerExists,
                ),
                routes::ChangeError::TableRemoved => panic!("the route table has been removed"),
                routes::ChangeError::SetRemoved => unreachable!(
                    "SetRemoved should not be observable while holding a route set, \
                    as `RouteSet::close()` takes ownership of `self`"
                ),
            },
        }
    }

    async fn remove_fidl_route(
        &self,
        route: fnet_routes_ext::Route<I>,
    ) -> Result<bool, fnet_routes_admin::RouteSetError> {
        let AddableEntry { subnet, device, gateway, metric } =
            try_to_addable_entry::<I>(self.ctx().bindings_ctx(), route)?
                .map_device_id(|d| d.downgrade());

        if !self.authorization_set().contains(&device) {
            return Err(fnet_routes_admin::RouteSetError::Unauthenticated);
        }

        let result = self
            .apply_route_op(routes::RouteOp::RemoveMatching {
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
                routes::ChangeError::DeviceRemoved => Err(
                    fnet_routes_admin::RouteSetError::PreviouslyAuthenticatedInterfaceNoLongerExists,
                ),
                routes::ChangeError::TableRemoved => panic!("the route table has been removed"),
                routes::ChangeError::SetRemoved => unreachable!(
                    "SetRemoved should not be observable while holding a route set, \
                    as `RouteSet::close()` takes ownership of `self`"
                ),
            },
        }
    }

    fn authenticate_for_interface(
        &mut self,
        client_credential: ProofOfInterfaceAuthorization,
    ) -> Result<(), fnet_routes_admin::AuthenticateForInterfaceError> {
        let bindings_id = client_credential
            .interface_id
            .try_into()
            .map_err(|_| fnet_routes_admin::AuthenticateForInterfaceError::InvalidAuthentication)?;

        let core_id =
            self.ctx().bindings_ctx().devices.get_core_id(bindings_id).ok_or_else(|| {
                tracing::warn!("authentication interface {bindings_id} does not exist");
                fnet_routes_admin::AuthenticateForInterfaceError::InvalidAuthentication
            })?;

        let external_state = core_id.external_state();
        let StaticCommonInfo { authorization_token: netstack_token, tx_notifier: _ } =
            external_state.static_common_info();

        let netstack_koid = netstack_token
            .basic_info()
            .expect("failed to get basic info for netstack-owned token")
            .koid;

        let client_koid = client_credential
            .token
            .basic_info()
            .map_err(|e| {
                tracing::error!("failed to get basic info for client-provided token: {}", e);
                fnet_routes_admin::AuthenticateForInterfaceError::InvalidAuthentication
            })?
            .koid;

        if netstack_koid == client_koid {
            let authorization_set = self.authorization_set_mut();

            // Prune any devices that no longer exist.  Since we store
            // weak references, we only need to check whether any given
            // reference can be upgraded.
            authorization_set.retain(|k| k.upgrade().is_some());

            // Insert after pruning the map to avoid a needless call to upgrade.
            let _ = authorization_set.insert(core_id.downgrade());

            Ok(())
        } else {
            Err(fnet_routes_admin::AuthenticateForInterfaceError::InvalidAuthentication)
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
