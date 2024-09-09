// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FIDL Worker for the `fuchsia.net.multicast.admin` API.

use std::num::NonZeroU64;

use derivative::Derivative;
use fidl::endpoints::{DiscoverableProtocolMarker as _, RequestStream};
use fidl_fuchsia_net_multicast_admin::{self as fnet_multicast_admin, TableControllerCloseReason};
use fidl_fuchsia_net_multicast_ext::{
    AddRouteError, DelRouteError, FidlMulticastAdminIpExt, FidlResponder as _,
    Route as FidlExtRoute, TableControllerRequest, TerminalEventControlHandle,
    UnicastSourceAndMulticastDestination,
};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures::channel::oneshot;
use futures::future::OptionFuture;
use futures::StreamExt as _;
use log::{error, info, warn};
use net_types::ip::{GenericOverIp, Ip, Ipv4, Ipv6};
use netstack3_core::device::DeviceId;
use netstack3_core::ip::{
    ForwardMulticastRouteError, MulticastForwardingDisabledError, MulticastRoute,
    MulticastRouteKey, MulticastRouteTarget,
};
use netstack3_core::IpExt;

use crate::bindings::util::{
    DeviceNotFoundError, IntoFidl, ResultExt as _, TryFromFidl, TryFromFidlWithContext,
    TryIntoCore as _, TryIntoCoreWithContext,
};
use crate::bindings::{BindingsCtx, ConversionContext, Ctx};

/// An event associated with the `fuchsia.net.multicast.admin` FIDL API.
enum MulticastAdminEvent<I: IpExt + FidlMulticastAdminIpExt> {
    /// A new table controller client is connecting.
    NewClient { request_stream: I::TableControllerRequestStream },
    /// A device is being removed, and references to it need to be purged from
    /// the multicast route table.
    RemoveDevice {
        device: netstack3_core::device::WeakDeviceId<BindingsCtx>,
        completer: oneshot::Sender<()>,
    },
}

/// An event sink that dispatches events to the [`MulticastAdminWorker`].
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub(crate) struct MulticastAdminEventSink<I: IpExt + FidlMulticastAdminIpExt> {
    sender: UnboundedSender<MulticastAdminEvent<I>>,
}

impl<I: IpExt + FidlMulticastAdminIpExt> MulticastAdminEventSink<I> {
    /// Installs a new table controller client.
    ///
    /// # Panics
    ///
    /// Panics if the corresponding [`MulticastAdminWorker`] has been dropped.
    pub(crate) fn serve_multicast_admin_client(
        &self,
        request_stream: I::TableControllerRequestStream,
    ) {
        self.sender
            .unbounded_send(MulticastAdminEvent::NewClient { request_stream })
            .expect("MulticastAdmiNWorker should never close before the sink");
    }

    /// Remove the devices from the IPv4/IPv6 multicast route tables.
    ///
    /// Generates the appropriate events to send to the [`MulticastAdminWorker`]
    /// and waits for their completion.
    ///
    /// # Panics
    ///
    /// Panics if the corresponding [`MulticastAdminWorker`] has been dropped.
    async fn remove_multicast_routes_on_device(
        &self,
        device: &netstack3_core::device::WeakDeviceId<BindingsCtx>,
    ) {
        let (completer, waiter) = oneshot::channel();
        self.sender
            .unbounded_send(MulticastAdminEvent::RemoveDevice { device: device.clone(), completer })
            .expect("MulticastAdminWorker should never close before the sink");
        waiter.await.expect("completer should not be dropped");
    }

    /// Close the sink, allowing the [`MulticastAdminWorker`] to finish.
    fn close(&self) {
        self.sender.close_channel();
    }
}

/// The worker to handle events received from the [`MulticastAdminEventSink`].
pub(crate) struct MulticastAdminWorker<I: IpExt + FidlMulticastAdminIpExt> {
    receiver: UnboundedReceiver<MulticastAdminEvent<I>>,
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
impl<I: IpExt + FidlMulticastAdminIpExt> MulticastAdminWorker<I> {
    /// Runs the [`MulticastAdminWorker`].
    ///
    /// Terminates when the [`MulticastAdminEventSink`] is dropped.
    async fn run(&mut self, mut ctx: Ctx) {
        info!("starting {} MulticastAdminWorker.", I::NAME);
        let mut client: Option<MulticastAdminClient<I>> = None;
        loop {
            let mut client_fut =
                OptionFuture::from(client.as_mut().map(|c| c.request_stream.by_ref().next()));

            // NB: allows us to pull code out of the select statement
            // and keep the formatter happy.
            enum Work<E, R> {
                Event(E),
                Request(R),
            }
            let next_work = futures::select!(
                event = self.receiver.next() => Work::Event(event),
                req = client_fut => Work::Request(
                    req.expect("OptionFuture is only selected when non-empty")
                ),
            );

            struct MaybeClose {
                // True if this worker should be closed as a result of the work.
                close_worker: bool,
                // `Some` if a currently connected client should be closed.
                // Note, this does not imply the existence of an active client.
                close_client: Option<ClientCloseReason>,
            }
            let MaybeClose { close_worker, close_client } = match next_work {
                // The sink was closed: close the client (if we have one) and
                // terminate the worker.
                Work::Event(None) => MaybeClose {
                    close_worker: true,
                    close_client: Some(ClientCloseReason::WorkerExit),
                },
                Work::Event(Some(event)) => {
                    handle_event(&mut ctx, event, &mut client);
                    MaybeClose { close_worker: false, close_client: None }
                }
                Work::Request(None) => MaybeClose {
                    close_worker: false,
                    close_client: Some(ClientCloseReason::ClientHungUp),
                },
                Work::Request(Some(Err(e))) => MaybeClose {
                    close_worker: false,
                    close_client: Some(ClientCloseReason::FidlError(e)),
                },
                Work::Request(Some(Ok(r))) => {
                    let c = client.as_mut().expect("`Work::Request` proves that client is `Some`");
                    match handle_request(&mut ctx, c, r.into()) {
                        Ok(()) => MaybeClose { close_worker: false, close_client: None },
                        Err(e) => MaybeClose {
                            close_worker: false,
                            close_client: Some(ClientCloseReason::TerminatedByServer(e)),
                        },
                    }
                }
            };

            if let Some(client_close_reason) = close_client {
                if let Some(client) = client.take() {
                    client.close(client_close_reason);
                    assert!(
                        ctx.api().multicast_forwarding::<I>().disable(),
                        "multicast forwarding should be newly disabled"
                    );
                }
            }

            if close_worker {
                break;
            }
        }
        info!("{} MulticastAdminWorker exited.", I::NAME);
    }
}

/// Creates a pair of worker + sink for [`MulticastAdminEvent`].
///
/// [`MulticastAdminWorker::run`] will run indefinitely, until the sink is
/// dropped.
///
/// Attempting to use the sink after the worker has been dropped may panic.
fn new_worker_and_sink<I: IpExt + FidlMulticastAdminIpExt>(
) -> (MulticastAdminWorker<I>, MulticastAdminEventSink<I>) {
    let (sender, receiver) = futures::channel::mpsc::unbounded();
    (MulticastAdminWorker { receiver }, MulticastAdminEventSink { sender })
}

/// An IPv4 and IPv6 [`MulticastAdminEventSink`].
pub(crate) struct MulticastAdminEventSinks {
    v4_sink: MulticastAdminEventSink<Ipv4>,
    v6_sink: MulticastAdminEventSink<Ipv6>,
}

impl MulticastAdminEventSinks {
    /// Returns a reference to the [`MulticastAdminEventSink`] for `I`.
    pub(crate) fn sink<I: IpExt + FidlMulticastAdminIpExt>(&self) -> &MulticastAdminEventSink<I> {
        I::map_ip((), |()| &self.v4_sink, |()| &self.v6_sink)
    }

    /// Like [`MulticastAdminEventSink::remove_multicast_routes_on_device`].
    pub(crate) async fn remove_multicast_routes_on_device(
        &self,
        device: &netstack3_core::device::WeakDeviceId<BindingsCtx>,
    ) {
        futures::join!(
            self.v4_sink.remove_multicast_routes_on_device(device),
            self.v6_sink.remove_multicast_routes_on_device(device),
        );
    }

    /// Like [`MulticastAdminEventSink::close`].
    pub(crate) fn close(&self) {
        self.v4_sink.close();
        self.v6_sink.close();
    }
}

/// An IPv4 and IPv6 [`MulticastAdminWorker`].
pub(crate) struct MulticastAdminWorkers {
    v4_worker: MulticastAdminWorker<Ipv4>,
    v6_worker: MulticastAdminWorker<Ipv6>,
}

impl MulticastAdminWorkers {
    /// Runs both inner [`MulticastAdminWorker`], blocking on their termination.
    pub(crate) async fn run(&mut self, ctx: Ctx) {
        futures::join!(self.v4_worker.run(ctx.clone()), self.v6_worker.run(ctx));
    }
}

/// Creates a paired [`MulticastAdminWorkers`] and [`MulticastAdminEventSinks`].
pub(crate) fn new_workers_and_sinks() -> (MulticastAdminWorkers, MulticastAdminEventSinks) {
    let (v4_worker, v4_sink) = new_worker_and_sink::<Ipv4>();
    let (v6_worker, v6_sink) = new_worker_and_sink::<Ipv6>();
    (MulticastAdminWorkers { v4_worker, v6_worker }, MulticastAdminEventSinks { v4_sink, v6_sink })
}

/// An active connection to the multicast admin table controller FIDL protocol.
struct MulticastAdminClient<I: FidlMulticastAdminIpExt> {
    /// Stream of incoming requests.
    request_stream: futures::stream::Fuse<I::TableControllerRequestStream>,
    /// The control_handle for the connection.
    control_handle:
        <I::TableControllerRequestStream as fidl::endpoints::RequestStream>::ControlHandle,
    /// The state of the watcher associated with this client.
    watcher: MulticastRoutingEventsWatcher<I>,
}

impl<I: FidlMulticastAdminIpExt> MulticastAdminClient<I> {
    fn new(request_stream: I::TableControllerRequestStream) -> Self {
        let control_handle = request_stream.control_handle();
        MulticastAdminClient {
            request_stream: request_stream.fuse(),
            control_handle,
            watcher: Default::default(),
        }
    }

    /// Close the client connection, sending a terminal event, if appropriate.
    fn close(self, reason: ClientCloseReason) {
        let fidl_result = match reason {
            ClientCloseReason::TerminatedByServer(e) => {
                warn!("closed {}: {:?}", I::TableControllerMarker::PROTOCOL_NAME, e);
                self.control_handle.send_terminal_event(e)
            }
            ClientCloseReason::WorkerExit => {
                warn!(
                    "hanging up on {} client because the MulticastAdminWorker is exiting.",
                    I::TableControllerMarker::PROTOCOL_NAME
                );
                Ok(())
            }
            ClientCloseReason::FidlError(e) => Err(e),
            ClientCloseReason::ClientHungUp => Ok(()),
        };
        fidl_result.unwrap_or_else(|e| {
            if !e.is_closed() {
                error!("error serving {}: {:?}", I::TableControllerMarker::PROTOCOL_NAME, e);
            }
        })
    }
}

/// State associated with the hanging-get watcher for multicast routing events.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
struct MulticastRoutingEventsWatcher<I: FidlMulticastAdminIpExt> {
    parked_watch_request: Option<I::WatchRoutingEventsResponder>,
}

/// The reason a [`MulticastAdminClient`] closed.
#[derive(Debug)]
pub(crate) enum ClientCloseReason {
    TerminatedByServer(fnet_multicast_admin::TableControllerCloseReason),
    WorkerExit,
    ClientHungUp,
    FidlError(fidl::Error),
}

/// Handler for [`MulticastAdminEvent`].
///
/// The given `client` may be updated from `None` to `Some`, in the case a new
/// client is connecting.
#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn handle_event<I: IpExt + FidlMulticastAdminIpExt>(
    ctx: &mut Ctx,
    event: MulticastAdminEvent<I>,
    client: &mut Option<MulticastAdminClient<I>>,
) {
    match event {
        MulticastAdminEvent::RemoveDevice { device, completer } => {
            ctx.api().multicast_forwarding::<I>().remove_references_to_device(&device);
            completer.send(()).expect("completer should be newly signaled");
        }
        MulticastAdminEvent::NewClient { request_stream } => {
            let new_client = MulticastAdminClient::new(request_stream);
            // If we already have a client, reject the incoming request.
            if client.is_some() {
                new_client.close(ClientCloseReason::TerminatedByServer(
                    fnet_multicast_admin::TableControllerCloseReason::AlreadyInUse,
                ));
                return;
            } else {
                *client = Some(new_client);
                assert!(
                    ctx.api().multicast_forwarding::<I>().enable(),
                    "multicast forwarding should be newly enabled"
                );
            }
        }
    }
}

/// Handler for [`TableControllerRequest`].
#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn handle_request<I: IpExt + FidlMulticastAdminIpExt>(
    ctx: &mut Ctx,
    client: &mut MulticastAdminClient<I>,
    request: TableControllerRequest<I>,
) -> Result<(), TableControllerCloseReason> {
    match request {
        TableControllerRequest::AddRoute { addresses, route, responder } => {
            info!("adding multicast route: addresses={addresses:?}, route={route:?}");
            let result = handle_add_route(ctx, addresses, route);
            if let Err(e) = &result {
                warn!("failed to add multicast route: {e:?}")
            }
            responder.try_send(result).unwrap_or_log("failed to respond");
            Ok(())
        }
        TableControllerRequest::DelRoute { addresses, responder } => {
            info!("removing multicast route: addresses={addresses:?}");
            let result = handle_del_route(ctx, addresses);
            if let Err(e) = &result {
                warn!("failed to remove multicast route: {e:?}")
            }
            responder.try_send(result).unwrap_or_log("failed to respond");
            Ok(())
        }
        TableControllerRequest::GetRouteStats { addresses, responder } => {
            // TODO(https://fxbug.dev/323052525): Support getting multicast route stats.
            warn!("not getting routes stats; unimplemented: addresses={addresses:?}");
            let stats = fnet_multicast_admin::RouteStats::default();
            responder.try_send(Ok(&stats)).unwrap_or_log("failed to respond");
            Ok(())
        }
        TableControllerRequest::WatchRoutingEvents { responder } => {
            // TODO(https://fxbug.dev/323052525): Support watching multicast routing
            // events.
            warn!("not publishing multicast routing events; unimplemented");
            match client.watcher.parked_watch_request.replace(responder) {
                None => Ok(()),
                Some(_) => Err(TableControllerCloseReason::HangingGetError),
            }
        }
    }
}

/// Add a multicast route to the Netstack.
#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn handle_add_route<I: IpExt + FidlMulticastAdminIpExt>(
    ctx: &mut Ctx,
    addresses: UnicastSourceAndMulticastDestination<I>,
    route: fnet_multicast_admin::Route,
) -> Result<(), AddRouteError> {
    let key = addresses.try_into_core().map_err(IntoFidl::<AddRouteError>::into_fidl)?;
    let route = FidlExtRoute::try_from(route)?;
    let route = route.try_into_core_with_ctx(ctx.bindings_ctx())?;
    match ctx.api().multicast_forwarding().add_multicast_route(key, route) {
        Ok(None) => {}
        Ok(Some(prev_route)) => info!("overwrote previous multicast route: {prev_route:?}"),
        Err(MulticastForwardingDisabledError {}) => {
            unreachable!("the existence of a `MulticastAdminClient` proves the api is enabled")
        }
    }
    Ok(())
}

/// Remove a multicast route from the Netstack.
#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn handle_del_route<I: IpExt + FidlMulticastAdminIpExt>(
    ctx: &mut Ctx,
    addresses: UnicastSourceAndMulticastDestination<I>,
) -> Result<(), DelRouteError> {
    let key = addresses.try_into_core().map_err(IntoFidl::<DelRouteError>::into_fidl)?;
    match ctx.api().multicast_forwarding().remove_multicast_route(&key) {
        Ok(None) => Err(DelRouteError::NotFound),
        Ok(Some(_route)) => Ok(()),
        Err(MulticastForwardingDisabledError {}) => {
            unreachable!("the existance of a `MulticastAdminClient` proves the api is enabled")
        }
    }
}

/// Error with the provided [`UnicastSourceAndMulticastDestination`].
#[derive(Debug, PartialEq)]
pub struct MulticastRouteAddressError;

impl IntoFidl<AddRouteError> for MulticastRouteAddressError {
    fn into_fidl(self) -> AddRouteError {
        let MulticastRouteAddressError {} = self;
        AddRouteError::InvalidAddress
    }
}

impl IntoFidl<DelRouteError> for MulticastRouteAddressError {
    fn into_fidl(self) -> DelRouteError {
        let MulticastRouteAddressError {} = self;
        DelRouteError::InvalidAddress
    }
}

impl<I: IpExt + FidlMulticastAdminIpExt> TryFromFidl<UnicastSourceAndMulticastDestination<I>>
    for MulticastRouteKey<I>
{
    type Error = MulticastRouteAddressError;

    fn try_from_fidl(addrs: UnicastSourceAndMulticastDestination<I>) -> Result<Self, Self::Error> {
        let UnicastSourceAndMulticastDestination { unicast_source, multicast_destination } = addrs;
        MulticastRouteKey::new(unicast_source, multicast_destination)
            .ok_or(MulticastRouteAddressError)
    }
}

impl TryFromFidlWithContext<FidlExtRoute> for MulticastRoute<DeviceId<BindingsCtx>> {
    type Error = AddRouteError;

    fn try_from_fidl_with_ctx<C: ConversionContext>(
        ctx: &C,
        route: FidlExtRoute,
    ) -> Result<Self, Self::Error> {
        let FidlExtRoute { expected_input_interface, action } = route;
        let input_interface: DeviceId<BindingsCtx> =
            TryFromFidlWithContext::try_from_fidl_with_ctx(
                ctx,
                NonZeroU64::new(expected_input_interface)
                    .ok_or(AddRouteError::InterfaceNotFound)?,
            )
            .map_err(|DeviceNotFoundError {}| AddRouteError::InterfaceNotFound)?;
        match action {
            fnet_multicast_admin::Action::OutgoingInterfaces(interfaces) => {
                let targets = interfaces
                    .into_iter()
                    .map(|fnet_multicast_admin::OutgoingInterfaces { id, min_ttl }| {
                        let output_interface: DeviceId<BindingsCtx> =
                            TryFromFidlWithContext::try_from_fidl_with_ctx(
                                ctx,
                                NonZeroU64::new(id).ok_or(AddRouteError::InterfaceNotFound)?,
                            )
                            .map_err(|DeviceNotFoundError {}| AddRouteError::InterfaceNotFound)?;
                        Ok(MulticastRouteTarget { output_interface, min_ttl })
                    })
                    .collect::<Result<Vec<_>, AddRouteError>>()?;
                MulticastRoute::new_forward(input_interface, targets.into()).map_err(|e| match e {
                    ForwardMulticastRouteError::DuplicateTarget => AddRouteError::DuplicateOutput,
                    ForwardMulticastRouteError::EmptyTargetList => {
                        AddRouteError::RequiredRouteFieldsMissing
                    }
                    ForwardMulticastRouteError::InputInterfaceIsTarget => {
                        AddRouteError::InputCannotBeOutput
                    }
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bindings::integration_tests::{StackSetupBuilder, TestSetupBuilder};
    use crate::bindings::util::testutils::FakeConversionContext;
    use crate::bindings::BindingId;
    use crate::NetstackSeed;

    use assert_matches::assert_matches;
    use const_unwrap::const_unwrap_option;
    use fidl::endpoints::Proxy;
    use fidl_fuchsia_net_multicast_ext::TableControllerProxy as _;
    use futures::task::Poll;
    use futures::{poll, FutureExt};
    use ip_test_macro::ip_test;
    use net_declare::{net_ip_v4, net_ip_v6};
    use net_types::ip::{Ipv4Addr, Ipv6Addr};
    use test_case::test_case;

    const UNICAST_V4: Ipv4Addr = net_ip_v4!("192.0.2.1");
    const MULTICAST_V4: Ipv4Addr = net_ip_v4!("224.0.1.1");
    const UNICAST_V6: Ipv6Addr = net_ip_v6!("2001:0DB8::1");
    const MULTICAST_V6: Ipv6Addr = net_ip_v6!("ff0e::1");

    #[fuchsia_async::run_singlethreaded(test)]
    async fn worker_teardown() {
        let mut netstack = NetstackSeed::default();
        let ctx = netstack.netstack.ctx;

        let worker_fut = netstack.multicast_admin_workers.run(ctx.clone());
        futures::pin_mut!(worker_fut);

        // Verify the worker isn't terminated until we close the sink.
        assert_eq!(poll!(&mut worker_fut), Poll::Pending);
        ctx.bindings_ctx().multicast_admin.close();
        worker_fut.await;
    }

    #[netstack3_core::context_ip_bounds(I, BindingsCtx)]
    #[ip_test(I)]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn worker_teardown_with_client<I: IpExt + FidlMulticastAdminIpExt>() {
        let mut netstack = NetstackSeed::default();
        let ctx = netstack.netstack.ctx;

        let (client, request_stream) =
            fidl::endpoints::create_proxy_and_stream::<I::TableControllerMarker>()
                .expect("should be able to create table controller fidl endpoints");
        ctx.bindings_ctx().multicast_admin.sink::<I>().serve_multicast_admin_client(request_stream);

        let worker_fut = netstack.multicast_admin_workers.run(ctx.clone()).boxed();
        futures::pin_mut!(worker_fut);

        // Verify the client and worker aren't terminated.
        assert_eq!(poll!(&mut worker_fut), Poll::Pending);
        assert!(!client.is_closed());

        // Close the sink, and verify the worker terminates, and the client
        // observes PEER_CLOSED.
        ctx.bindings_ctx().multicast_admin.close();
        worker_fut.await;
        let _: fidl::Signals = client.on_closed().await.expect("should observe closure");
    }

    #[netstack3_core::context_ip_bounds(I, BindingsCtx)]
    #[ip_test(I)]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn duplicate_table_controller<I: IpExt + FidlMulticastAdminIpExt>() {
        let mut netstack = NetstackSeed::default();
        let ctx = netstack.netstack.ctx;

        let (client1, request_stream1) =
            fidl::endpoints::create_proxy_and_stream::<I::TableControllerMarker>()
                .expect("should be able to create table controller fidl endpoints");
        let (client2, request_stream2) =
            fidl::endpoints::create_proxy_and_stream::<I::TableControllerMarker>()
                .expect("should be able to create table controller fidl endpoints");

        // Install client2 after client1; client 2 should be rejected.
        ctx.bindings_ctx()
            .multicast_admin
            .sink::<I>()
            .serve_multicast_admin_client(request_stream1);
        ctx.bindings_ctx()
            .multicast_admin
            .sink::<I>()
            .serve_multicast_admin_client(request_stream2);

        let worker_fut = netstack.multicast_admin_workers.run(ctx.clone()).fuse();
        futures::pin_mut!(worker_fut);
        let mut client2_event_stream = client2.take_event_stream();
        let client2_fut = client2_event_stream.next().fuse();
        futures::pin_mut!(client2_fut);

        futures::select!(
            () = worker_fut => panic!("worker shouldn't terminate"),
            event = client2_fut => {
                assert_matches!(event, Some(Ok(TableControllerCloseReason::AlreadyInUse)));
                let _: fidl::Signals = client2.on_closed().await.expect("should observe closure");
                assert!(!client1.is_closed())
            }
        );

        ctx.bindings_ctx().multicast_admin.close();
        worker_fut.await;
    }

    #[netstack3_core::context_ip_bounds(I, BindingsCtx)]
    #[ip_test(I)]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn new_client_enables_forwarding<I: IpExt + FidlMulticastAdminIpExt>() {
        let mut netstack = NetstackSeed::default();
        let mut ctx = netstack.netstack.ctx;

        let worker_fut = netstack.multicast_admin_workers.run(ctx.clone()).fuse();
        futures::pin_mut!(worker_fut);

        // Verify that without a client, multicast forwarding is disabled.
        assert!(!ctx.api().multicast_forwarding().disable(), "shouldn't be newly disabled");

        // Create a client, and verify multicast forwarding becomes enabled.
        // NB: Poll the worker fut to ensure it connects the new client.
        let (client, request_stream) =
            fidl::endpoints::create_proxy_and_stream::<I::TableControllerMarker>()
                .expect("should be able to create table controller fidl endpoints");
        ctx.bindings_ctx().multicast_admin.sink::<I>().serve_multicast_admin_client(request_stream);
        assert_eq!(poll!(&mut worker_fut), Poll::Pending);
        assert!(!ctx.api().multicast_forwarding().enable(), "shouldn't be newly enabled");

        // Disconnect the client, and verify multicast forwarding becomes
        // disabled.
        // NB: Poll the worker fut to ensure it disconnects the client.
        std::mem::drop(client);
        assert_eq!(poll!(&mut worker_fut), Poll::Pending);
        assert!(!ctx.api().multicast_forwarding().disable(), "shouldn't be newly disabled");

        ctx.bindings_ctx().multicast_admin.close();
        worker_fut.await;
    }

    enum DeviceRemovalTestCase {
        WrongDevice,
        InputDevice,
        OutputDevice,
    }

    #[netstack3_core::context_ip_bounds(I, BindingsCtx)]
    #[ip_test(I)]
    #[test_case(DeviceRemovalTestCase::WrongDevice, false; "wrong_device_no_change")]
    #[test_case(DeviceRemovalTestCase::InputDevice, true; "removed_by_input")]
    #[test_case(DeviceRemovalTestCase::OutputDevice, true; "removed_by_output")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn device_removal_purges_route_table<I: IpExt + FidlMulticastAdminIpExt>(
        which_device: DeviceRemovalTestCase,
        expect_removal: bool,
    ) {
        // Create a test_setup with 3 interfaces.
        // NB: Don't use ID `1`, as that will conflict with Loopback.
        const WRONG_BINDING_ID: BindingId = const_unwrap_option(NonZeroU64::new(2));
        const INPUT_BINDING_ID: BindingId = const_unwrap_option(NonZeroU64::new(3));
        const OUTPUT_BINDING_ID: BindingId = const_unwrap_option(NonZeroU64::new(4));
        let mut test_setup = TestSetupBuilder::new()
            .add_endpoint()
            .add_endpoint()
            .add_endpoint()
            .add_stack(
                StackSetupBuilder::new()
                    .add_endpoint(1, None)
                    .add_endpoint(2, None)
                    .add_endpoint(3, None),
            )
            .build()
            .await;
        let test_stack = test_setup.get_mut(0);
        test_stack.wait_for_interface_online(WRONG_BINDING_ID).await;
        test_stack.wait_for_interface_online(INPUT_BINDING_ID).await;
        test_stack.wait_for_interface_online(OUTPUT_BINDING_ID).await;
        let (unicast_source, multicast_destination) =
            I::map_ip((), |()| (UNICAST_V4, MULTICAST_V4), |()| (UNICAST_V6, MULTICAST_V6));
        let addresses =
            UnicastSourceAndMulticastDestination { unicast_source, multicast_destination };

        let (client, request_stream) =
            fidl::endpoints::create_proxy_and_stream::<I::TableControllerMarker>()
                .expect("should be able to create table controller fidl endpoints");
        test_stack
            .ctx()
            .bindings_ctx()
            .multicast_admin
            .sink::<I>()
            .serve_multicast_admin_client(request_stream);

        let route = fnet_multicast_admin::Route {
            expected_input_interface: Some(INPUT_BINDING_ID.get()),
            action: Some(fnet_multicast_admin::Action::OutgoingInterfaces(vec![
                fnet_multicast_admin::OutgoingInterfaces {
                    id: OUTPUT_BINDING_ID.get(),
                    min_ttl: 0,
                },
            ])),
            __source_breaking: fidl::marker::SourceBreaking,
        };
        client
            .add_route(addresses.clone(), &route)
            .await
            .expect("add route request should be sent")
            .expect("add route should succeed");

        // Removal all routes referencing the device.
        let id = match which_device {
            DeviceRemovalTestCase::WrongDevice => WRONG_BINDING_ID.get(),
            DeviceRemovalTestCase::InputDevice => INPUT_BINDING_ID.get(),
            DeviceRemovalTestCase::OutputDevice => OUTPUT_BINDING_ID.get(),
        };
        test_stack.remove_interface(id).await;

        // Verify the route was/wasn't removed.
        let expected_result = if expect_removal { Err(DelRouteError::NotFound) } else { Ok(()) };
        assert_eq!(
            client.del_route(addresses).await.expect("del_route_request_should be sent"),
            expected_result
        );

        test_setup.shutdown().await;
    }

    #[test_case(UnicastSourceAndMulticastDestination{
        unicast_source: UNICAST_V4,
        multicast_destination: MULTICAST_V4,
        } => None; "success")]
    #[test_case(UnicastSourceAndMulticastDestination{
        unicast_source: UNICAST_V4,
        multicast_destination: UNICAST_V4,
        } => Some(MulticastRouteAddressError); "unicast_dst")]
    #[test_case(UnicastSourceAndMulticastDestination{
        unicast_source: MULTICAST_V4,
        multicast_destination: MULTICAST_V4,
        } => Some(MulticastRouteAddressError); "multicast_src")]
    fn key_from_fidl_ipv4(
        addrs: UnicastSourceAndMulticastDestination<Ipv4>,
    ) -> Option<MulticastRouteAddressError> {
        MulticastRouteKey::try_from_fidl(addrs).err()
    }

    #[test_case(UnicastSourceAndMulticastDestination{
        unicast_source: UNICAST_V6,
        multicast_destination: MULTICAST_V6,
        } => None; "success")]
    #[test_case(UnicastSourceAndMulticastDestination{
        unicast_source: UNICAST_V6,
        multicast_destination: UNICAST_V6,
        } => Some(MulticastRouteAddressError); "unicast_dst")]
    #[test_case(UnicastSourceAndMulticastDestination{
        unicast_source: MULTICAST_V6,
        multicast_destination: MULTICAST_V6,
        } => Some(MulticastRouteAddressError); "multicast_src")]
    fn key_from_fidl_ipv6(
        addrs: UnicastSourceAndMulticastDestination<Ipv6>,
    ) -> Option<MulticastRouteAddressError> {
        MulticastRouteKey::try_from_fidl(addrs).err()
    }

    #[test_case(FidlExtRoute {
        expected_input_interface: FakeConversionContext::BINDING_ID1.into(),
        action: fnet_multicast_admin::Action::OutgoingInterfaces(vec![
            fnet_multicast_admin::OutgoingInterfaces {
                id: FakeConversionContext::BINDING_ID2.into(),
                min_ttl: 0,
            }
        ]),
        } => None; "success")]
    #[test_case(FidlExtRoute {
        expected_input_interface: FakeConversionContext::INVALID_BINDING_ID.get(),
        action: fnet_multicast_admin::Action::OutgoingInterfaces(vec![
            fnet_multicast_admin::OutgoingInterfaces {
                id: FakeConversionContext::BINDING_ID2.into(),
                min_ttl: 0,
            }
        ]),
        } => Some(AddRouteError::InterfaceNotFound); "invalid_iif")]
    #[test_case(FidlExtRoute {
        expected_input_interface: FakeConversionContext::BINDING_ID1.into(),
        action: fnet_multicast_admin::Action::OutgoingInterfaces(vec![
            fnet_multicast_admin::OutgoingInterfaces {
                id: FakeConversionContext::INVALID_BINDING_ID.into(),
                min_ttl: 0,
            }
        ]),
        } => Some(AddRouteError::InterfaceNotFound); "invalid_oif")]
    #[test_case(FidlExtRoute {
        expected_input_interface: FakeConversionContext::BINDING_ID1.into(),
        action: fnet_multicast_admin::Action::OutgoingInterfaces(vec![]),
        } => Some(AddRouteError::RequiredRouteFieldsMissing); "no_oif")]
    #[test_case(FidlExtRoute {
        expected_input_interface: FakeConversionContext::BINDING_ID1.into(),
        action: fnet_multicast_admin::Action::OutgoingInterfaces(vec![
            fnet_multicast_admin::OutgoingInterfaces {
                id: FakeConversionContext::BINDING_ID1.into(),
                min_ttl: 0,
            }
        ]),
        } => Some(AddRouteError::InputCannotBeOutput); "iff_is_oif")]
    #[test_case(FidlExtRoute {
        expected_input_interface: FakeConversionContext::BINDING_ID1.into(),
        action: fnet_multicast_admin::Action::OutgoingInterfaces(vec![
            fnet_multicast_admin::OutgoingInterfaces {
                id: FakeConversionContext::BINDING_ID2.into(),
                min_ttl: 0,
            },
            fnet_multicast_admin::OutgoingInterfaces {
                id: FakeConversionContext::BINDING_ID2.into(),
                min_ttl: 0,
            }
        ]),
        } => Some(AddRouteError::DuplicateOutput); "duplicate_oif")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn route_from_fidl(route: FidlExtRoute) -> Option<AddRouteError> {
        let ctx = FakeConversionContext::new().await;
        let outcome = MulticastRoute::try_from_fidl_with_ctx(&ctx, route).err();
        ctx.shutdown().await;
        outcome
    }
}
