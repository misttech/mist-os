// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FIDL Worker for the `fuchsia.net.interfaces.admin` API, including the
//! `DeviceControl`, `Control` and `AddressStateProvider` Protocols.
//!
//! Each instance of these protocols is tied to the lifetime of a particular
//! entity in the Netstack:
//!    `DeviceControl`        => device
//!    `Control`              => interface
//!    `AddressStateProvider` => address
//! meaning the entity is removed if the protocol is closed, and the protocol is
//! closed if the entity is removed. Some protocols expose a `detach` method
//! that allows the lifetimes to be decoupled.
//!
//! These protocols (and their corresponding entities) are nested:
//! `DeviceControl` allows a new `Control` protocol to be connected (creating a
//! new interface on the device), while `Control` allows a new
//! `AddressStateProvider` protocol to be connected (creating a new address on
//! the interface).
//!
//! In general, each protocol is served by a "worker", either a
//! [`fuchsia_async::Task`] or a bare [`futures::future::Future`], that handles
//! incoming requests, spawns the workers for the nested protocols, and tears
//! down its associated entity if canceled.
//!
//! The `fuchsia.net.debug/Interfaces.GetAdmin` method exposes a backdoor that
//! allows clients to deviate from the behavior described above. Clients can
//! attach a new `Control` protocol to an existing interface with one-way
//! ownership semantics (removing the interface closes the protocol; closing
//! protocol does not remove the interface).

use std::{
    borrow::Borrow,
    collections::hash_map,
    ops::{Deref as _, DerefMut as _},
};

use assert_matches::assert_matches;
use fidl::endpoints::{ProtocolMarker, ServerEnd};
use fidl_fuchsia_hardware_network as fhardware_network;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::{
    future::FusedFuture as _, lock::Mutex as AsyncMutex, stream::FusedStream as _, FutureExt as _,
    SinkExt as _, StreamExt as _, TryFutureExt as _, TryStreamExt as _,
};
use net_types::{ip::AddrSubnetEither, ip::IpAddr, SpecifiedAddr};
use netstack3_core::{
    device::{update_ipv4_configuration, update_ipv6_configuration, DeviceId},
    ip::device::{
        IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate, Ipv6DeviceConfigurationUpdate,
    },
};

use crate::bindings::{
    devices, netdevice_worker, util, util::IntoCore as _, util::TryIntoCore as _, BindingId, Ctx,
    DeviceIdExt as _, Netstack,
};

pub(crate) fn serve(
    ns: Netstack,
    req: fnet_interfaces_admin::InstallerRequestStream,
) -> impl futures::Stream<Item = Result<fasync::Task<()>, fidl::Error>> {
    req.map_ok(
        move |fnet_interfaces_admin::InstallerRequest::InstallDevice {
                  device,
                  device_control,
                  control_handle: _,
              }| {
            fasync::Task::spawn(
                run_device_control(
                    ns.clone(),
                    device,
                    device_control.into_stream().expect("failed to obtain stream"),
                )
                .map(|r| {
                    r.unwrap_or_else(|e| tracing::warn!("device control finished with {:?}", e))
                }),
            )
        },
    )
}

#[derive(thiserror::Error, Debug)]
enum DeviceControlError {
    #[error("worker error: {0}")]
    Worker(#[from] netdevice_worker::Error),
    #[error("fidl error: {0}")]
    Fidl(#[from] fidl::Error),
}

async fn run_device_control(
    ns: Netstack,
    device: fidl::endpoints::ClientEnd<fhardware_network::DeviceMarker>,
    req_stream: fnet_interfaces_admin::DeviceControlRequestStream,
) -> Result<(), DeviceControlError> {
    let worker = netdevice_worker::NetdeviceWorker::new(ns.ctx.clone(), device).await?;
    let handler = worker.new_handler();
    let worker_fut = worker.run().map_err(DeviceControlError::Worker);
    let stop_event = async_utils::event::Event::new();
    let req_stream =
        req_stream.take_until(stop_event.wait_or_dropped()).map_err(DeviceControlError::Fidl);
    futures::pin_mut!(worker_fut);
    futures::pin_mut!(req_stream);
    let mut detached = false;
    let mut tasks = futures::stream::FuturesUnordered::new();
    let res = loop {
        let result = futures::select! {
            req = req_stream.try_next() => req,
            r = worker_fut => match r {
                Ok(never) => match never {},
                Err(e) => Err(e)
            },
            ready_task = tasks.next() => {
                let () = ready_task.unwrap_or_else(|| ());
                continue;
            }
        };
        match result {
            Ok(None) => {
                // The client hung up; stop serving if not detached.
                if !detached {
                    break Ok(());
                }
            }
            Ok(Some(req)) => match req {
                fnet_interfaces_admin::DeviceControlRequest::CreateInterface {
                    port,
                    control,
                    options,
                    control_handle: _,
                } => {
                    if let Some(interface_control_task) = create_interface(
                        port,
                        control,
                        options,
                        &ns,
                        &handler,
                        stop_event.wait_or_dropped(),
                    )
                    .await
                    {
                        tasks.push(interface_control_task);
                    }
                }
                fnet_interfaces_admin::DeviceControlRequest::Detach { control_handle: _ } => {
                    detached = true;
                }
            },
            Err(e) => break Err(e),
        }
    };

    // Send a stop signal to all tasks.
    assert!(stop_event.signal(), "event was already signaled");
    // Run all the tasks to completion. We sent the stop signal, they should all
    // complete and perform interface cleanup.
    tasks.collect::<()>().await;

    res
}

const INTERFACES_ADMIN_CHANNEL_SIZE: usize = 16;
/// A wrapper over `fuchsia.net.interfaces.admin/Control` handles to express ownership semantics.
///
/// If `owns_interface` is true, this handle 'owns' the interfaces, and when the handle closes the
/// interface should be removed.
pub struct OwnedControlHandle {
    request_stream: fnet_interfaces_admin::ControlRequestStream,
    control_handle: fnet_interfaces_admin::ControlControlHandle,
    owns_interface: bool,
}

impl OwnedControlHandle {
    pub(crate) fn new_unowned(
        handle: fidl::endpoints::ServerEnd<fnet_interfaces_admin::ControlMarker>,
    ) -> OwnedControlHandle {
        let (stream, control) =
            handle.into_stream_and_control_handle().expect("failed to decompose control handle");
        OwnedControlHandle {
            request_stream: stream,
            control_handle: control,
            owns_interface: false,
        }
    }

    // Constructs a new channel of `OwnedControlHandle` with no owner.
    pub(crate) fn new_channel() -> (
        futures::channel::mpsc::Sender<OwnedControlHandle>,
        futures::channel::mpsc::Receiver<OwnedControlHandle>,
    ) {
        futures::channel::mpsc::channel(INTERFACES_ADMIN_CHANNEL_SIZE)
    }

    // Constructs a new channel of `OwnedControlHandle` with the given handle as the owner.
    // TODO(https://fxbug.dev/87963): This currently enforces that there is only ever one owner,
    // which will need to be revisited to implement `Clone`.
    pub(crate) async fn new_channel_with_owned_handle(
        handle: fidl::endpoints::ServerEnd<fnet_interfaces_admin::ControlMarker>,
    ) -> (
        futures::channel::mpsc::Sender<OwnedControlHandle>,
        futures::channel::mpsc::Receiver<OwnedControlHandle>,
    ) {
        let (mut sender, receiver) = Self::new_channel();
        let (stream, control) =
            handle.into_stream_and_control_handle().expect("failed to decompose control handle");
        sender
            .send(OwnedControlHandle {
                request_stream: stream,
                control_handle: control,
                owns_interface: true,
            })
            .await
            .expect("failed to attach initial control handle");
        (sender, receiver)
    }

    // Consumes the OwnedControlHandle and returns its `control_handle`.
    pub(crate) fn into_control_handle(self) -> fnet_interfaces_admin::ControlControlHandle {
        self.control_handle
    }
}

/// Operates a fuchsia.net.interfaces.admin/DeviceControl.CreateInterface
/// request.
///
/// Returns `Some(fuchsia_async::Task)` if an interface was created
/// successfully. The returned `Task` must be polled to completion and is tied
/// to the created interface's lifetime.
async fn create_interface(
    port: fhardware_network::PortId,
    control: fidl::endpoints::ServerEnd<fnet_interfaces_admin::ControlMarker>,
    options: fnet_interfaces_admin::Options,
    ns: &Netstack,
    handler: &netdevice_worker::DeviceHandler,
    device_stopped_fut: async_utils::event::EventWaitResult,
) -> Option<fuchsia_async::Task<()>> {
    tracing::debug!("creating interface from {:?} with {:?}", port, options);
    let fnet_interfaces_admin::Options { name, metric, .. } = options;
    let (control_sender, mut control_receiver) =
        OwnedControlHandle::new_channel_with_owned_handle(control).await;
    match handler
        .add_port(&ns, netdevice_worker::InterfaceOptions { name, metric }, port, control_sender)
        .await
    {
        Ok((binding_id, status_stream)) => {
            Some(fasync::Task::spawn(run_netdevice_interface_control(
                ns.ctx.clone(),
                binding_id,
                status_stream,
                device_stopped_fut,
                control_receiver,
            )))
        }
        Err(e) => {
            tracing::warn!("failed to add port {:?} to device: {:?}", port, e);
            let removed_reason = match e {
                netdevice_worker::Error::Client(e) => match e {
                    // Assume any fidl errors are port closed
                    // errors.
                    netdevice_client::Error::Fidl(_) => {
                        Some(fnet_interfaces_admin::InterfaceRemovedReason::PortClosed)
                    }
                    netdevice_client::Error::RxFlags(_)
                    | netdevice_client::Error::FrameType(_)
                    | netdevice_client::Error::NoProgress
                    | netdevice_client::Error::Config(_)
                    | netdevice_client::Error::LargeChain(_)
                    | netdevice_client::Error::Index(_, _)
                    | netdevice_client::Error::Pad(_, _)
                    | netdevice_client::Error::TxLength
                    | netdevice_client::Error::Open(_, _)
                    | netdevice_client::Error::Vmo(_, _)
                    | netdevice_client::Error::Fifo(_, _, _)
                    | netdevice_client::Error::VmoSize(_, _)
                    | netdevice_client::Error::Map(_, _)
                    | netdevice_client::Error::DeviceInfo(_)
                    | netdevice_client::Error::PortStatus(_)
                    | netdevice_client::Error::InvalidPortId(_)
                    | netdevice_client::Error::Attach(_, _)
                    | netdevice_client::Error::Detach(_, _)
                    | netdevice_client::Error::TooSmall { size: _, offset: _, length: _ } => None,
                },
                netdevice_worker::Error::AlreadyInstalled(_) => {
                    Some(fnet_interfaces_admin::InterfaceRemovedReason::PortAlreadyBound)
                }
                netdevice_worker::Error::CantConnectToPort(_)
                | netdevice_worker::Error::PortClosed => {
                    Some(fnet_interfaces_admin::InterfaceRemovedReason::PortClosed)
                }
                netdevice_worker::Error::ConfigurationNotSupported
                | netdevice_worker::Error::MacNotUnicast { .. } => {
                    Some(fnet_interfaces_admin::InterfaceRemovedReason::BadPort)
                }
                netdevice_worker::Error::DuplicateName(_) => {
                    Some(fnet_interfaces_admin::InterfaceRemovedReason::DuplicateName)
                }
                netdevice_worker::Error::SystemResource(_)
                | netdevice_worker::Error::InvalidPortInfo(_) => None,
            };
            if let Some(removed_reason) = removed_reason {
                // Retrieve the original control handle from the receiver.
                let OwnedControlHandle { request_stream: _, control_handle, owns_interface: _ } =
                    control_receiver
                        .try_next()
                        .expect("expected control handle to be ready in the receiver")
                        .expect("expected receiver to not be closed/empty");
                control_handle.send_on_interface_removed(removed_reason).unwrap_or_else(|e| {
                    tracing::warn!("failed to send removed reason: {:?}", e);
                });
            }
            None
        }
    }
}

/// Manages the lifetime of a newly created Netdevice interface, including spawning an
/// interface control worker, spawning a link state worker, and cleaning up the interface on
/// deletion.
async fn run_netdevice_interface_control<
    S: futures::Stream<Item = netdevice_client::Result<netdevice_client::client::PortStatus>>,
>(
    ctx: Ctx,
    id: BindingId,
    status_stream: S,
    mut device_stopped_fut: async_utils::event::EventWaitResult,
    control_receiver: futures::channel::mpsc::Receiver<OwnedControlHandle>,
) {
    let interface_access_synchronizer = AsyncMutex::new(());

    let link_state_fut =
        run_link_state_watcher(ctx.clone(), id, status_stream, &interface_access_synchronizer)
            .fuse();
    let (interface_control_stop_sender, interface_control_stop_receiver) =
        futures::channel::oneshot::channel();

    // Device-backed interfaces are always removable.
    let removable = true;
    let interface_control_fut = run_interface_control(
        ctx.clone(),
        id,
        interface_control_stop_receiver,
        control_receiver,
        removable,
        &interface_access_synchronizer,
    )
    .fuse();
    futures::pin_mut!(link_state_fut);
    futures::pin_mut!(interface_control_fut);
    let cleanup_iface_control = futures::select! {
        o = device_stopped_fut => {
            o.expect("event was orphaned");
            None
        },
        () = link_state_fut => None,
        cleanup = interface_control_fut => cleanup,
    };
    let cleanup_iface_control = if !interface_control_fut.is_terminated() {
        // Cancel interface control and drive it to completion, allowing it to terminate each
        // control handle.
        interface_control_stop_sender
            .send(fnet_interfaces_admin::InterfaceRemovedReason::PortClosed)
            .expect("failed to cancel interface control");
        interface_control_fut.await
    } else {
        cleanup_iface_control
    };
    remove_interface(ctx, id).await;

    // Run the cleanup only after the interface is completely removed.
    if let Some(cleanup_iface_control) = cleanup_iface_control {
        cleanup_iface_control();
    }
}

/// Runs a worker to watch the given status_stream and update the interface state accordingly.
async fn run_link_state_watcher<
    S: futures::Stream<Item = netdevice_client::Result<netdevice_client::client::PortStatus>>,
>(
    ctx: Ctx,
    id: BindingId,
    status_stream: S,
    interface_access_synchronizer: &AsyncMutex<()>,
) {
    let result = status_stream
        .try_for_each(|netdevice_client::client::PortStatus { flags, mtu: _ }| {
            let ctx = &ctx;
            async move {
                // Take the lock to synchronize with other operations that may
                // mutate state for this device (e.g. admin up/down).
                let guard = interface_access_synchronizer.lock().await;
                let _: &() = guard.deref();

                let online = flags.contains(fhardware_network::StatusFlags::ONLINE);
                tracing::debug!("observed interface {} online = {}", id, online);
                let mut ctx = ctx.clone();
                match ctx
                    .non_sync_ctx
                    .devices
                    .get_core_id(id)
                    .expect("device not present")
                    .external_state()
                {
                    devices::DeviceSpecificInfo::Netdevice(i) => {
                        i.with_dynamic_info_mut(|i| i.phy_up = online)
                    }
                    i @ devices::DeviceSpecificInfo::Loopback(_) => {
                        unreachable!("unexpected device info {:?} for interface {}", i, id)
                    }
                };
                // Enable or disable interface with context depending on new online
                // status. The helper functions take care of checking if admin
                // enable is the expected value.
                crate::bindings::set_interface_enabled(&mut ctx, id, online).unwrap_or_else(|e| {
                    panic!("failed to set interface enabled={}: {:?}", online, e)
                });
                Ok(())
            }
        })
        .await;
    match result {
        Ok(()) => tracing::debug!("state stream closed for interface {}", id),
        Err(e) => {
            let level = match &e {
                netdevice_client::Error::Fidl(e) if e.is_closed() => tracing::Level::DEBUG,
                _ => tracing::Level::ERROR,
            };
            log_error!(level, "error operating port state stream {:?} for interface {}", e, id);
        }
    }
}

/// Runs a worker to serve incoming `fuchsia.net.interfaces.admin/Control`
/// handles.
///
/// Returns a function that must be called after the interface has been removed
/// to notify all clients of removal.
pub(crate) async fn run_interface_control(
    ctx: Ctx,
    id: BindingId,
    mut stop_receiver: futures::channel::oneshot::Receiver<
        fnet_interfaces_admin::InterfaceRemovedReason,
    >,
    control_receiver: futures::channel::mpsc::Receiver<OwnedControlHandle>,
    removable: bool,
    interface_access_synchronizer: impl Borrow<AsyncMutex<()>>,
) -> Option<impl FnOnce() -> ()> {
    let interface_access_synchronizer = interface_access_synchronizer.borrow();

    // An event indicating that the individual control request streams should stop.
    let cancel_request_streams = async_utils::event::Event::new();
    // A struct to retain per-handle state of the individual request streams in `control_receiver`.
    struct ReqStreamState {
        owns_interface: bool,
        control_handle: fnet_interfaces_admin::ControlControlHandle,
        ctx: Ctx,
        id: BindingId,
    }
    // Convert `control_receiver` (a stream-of-streams) into a stream of futures, where each future
    // represents the termination of an inner `ControlRequestStream`.
    let stream_of_fut = control_receiver.map(
        |OwnedControlHandle { request_stream, control_handle, owns_interface }| {
            let initial_state =
                ReqStreamState { owns_interface, control_handle, ctx: ctx.clone(), id };
            // Attach `cancel_request_streams` as a short-circuit mechanism to stop handling new
            // `ControlRequest`.
            let request_stream =
                request_stream.take_until(cancel_request_streams.wait_or_dropped());

            // Convert the request stream into a future, dispatching each incoming
            // `ControlRequest` and retaining the `ReqStreamState` along the way.
            async_utils::fold::fold_while(
                request_stream,
                initial_state,
                |mut state, request| async move {
                    let ReqStreamState { ctx, id, owns_interface, control_handle: _ } = &mut state;
                    match request {
                        Err(e) => {
                            log_error!(
                                util::fidl_err_log_level(&e),
                                "error operating {} stream for interface {}: {:?}",
                                fnet_interfaces_admin::ControlMarker::DEBUG_NAME,
                                id,
                                e
                            );
                            async_utils::fold::FoldWhile::Continue(state)
                        }
                        Ok(req) => {
                            match dispatch_control_request(
                                req,
                                ctx,
                                *id,
                                removable,
                                owns_interface,
                                interface_access_synchronizer,
                            )
                            .await
                            {
                                Err(e) => {
                                    log_error!(
                                        util::fidl_err_log_level(&e),
                                        "failed to handle request for interface {}: {:?}",
                                        id,
                                        e
                                    );
                                    async_utils::fold::FoldWhile::Continue(state)
                                }
                                Ok(ControlRequestResult::Continue) => {
                                    async_utils::fold::FoldWhile::Continue(state)
                                }
                                Ok(ControlRequestResult::Remove) => {
                                    // Short-circuit the stream, user called
                                    // remove.
                                    async_utils::fold::FoldWhile::Done(state.control_handle)
                                }
                            }
                        }
                    }
                },
            )
        },
    );
    // Enable the stream of futures to be polled concurrently.
    let mut stream_of_fut = stream_of_fut.buffer_unordered(std::usize::MAX);

    let (remove_reason, removal_requester) = {
        // Drive the `ControlRequestStreams` to completion, short-circuiting if
        // an owner terminates or if interface removal is requested.
        let mut interface_control_stream = stream_of_fut.by_ref().filter_map(|stream_result| {
            futures::future::ready(match stream_result {
                async_utils::fold::FoldResult::StreamEnded(ReqStreamState {
                    owns_interface,
                    control_handle: _,
                    ctx: _,
                    id: _,
                }) => {
                    // If we own the interface, return `Some(None)` to stop
                    // operating on the request streams.
                    owns_interface.then(|| None)
                }
                async_utils::fold::FoldResult::ShortCircuited(control_handle) => {
                    // If it was short-circuited by a call to remove, return
                    // `Some(Some(control_handle))` so we stop operating on
                    // the request streams and inform the requester of
                    // removal when done.
                    Some(Some(control_handle))
                }
            })
        });

        futures::select! {
            // One of the interface's owning channels hung up or `Remove` was
            // called; inform the other channels.
            removal_requester = interface_control_stream.next() => {
                (fnet_interfaces_admin::InterfaceRemovedReason::User, removal_requester.flatten())
            }
            // Cancelation event was received with a specified remove reason.
            reason = stop_receiver => (reason.expect("failed to receive stop"), None),
        }
    };

    // Close the control_receiver, preventing new RequestStreams from attaching.
    stream_of_fut.get_mut().get_mut().close();
    // Cancel the active request streams, and drive the remaining `ControlRequestStreams` to
    // completion, allowing each handle to send termination events.
    assert!(cancel_request_streams.signal(), "expected the event to be unsignaled");
    let cleanup_fn = if !stream_of_fut.is_terminated() {
        // Accumulate all the control handles first. We can't return
        // `stream_of_fut` because it's borrowing from stack variables.
        let control_handles = stream_of_fut
            .map(|fold_result| match fold_result {
                async_utils::fold::FoldResult::StreamEnded(ReqStreamState {
                    owns_interface: _,
                    control_handle,
                    ctx: _,
                    id: _,
                }) => control_handle,
                async_utils::fold::FoldResult::ShortCircuited(control_handle) => control_handle,
            })
            // Append the client that requested that the interface be removed
            // (if there is one) so it receives the terminal event too.
            .chain(futures::stream::iter(removal_requester))
            .collect::<Vec<_>>()
            .await;
        Some(move || {
            control_handles.into_iter().for_each(|control_handle| {
                control_handle.send_on_interface_removed(remove_reason).unwrap_or_else(|e| {
                    log_error!(
                        util::fidl_err_log_level(&e),
                        "failed to send terminal event: {:?} for interface {}",
                        e,
                        id
                    )
                });
            });
        })
    } else {
        None
    };
    // Cancel the `AddressStateProvider` workers and drive them to completion.
    let address_state_providers = {
        let ctx = ctx.clone();
        let core_id =
            ctx.non_sync_ctx.devices.get_core_id(id).expect("missing device info for interface");
        core_id.external_state().with_common_info_mut(|i| {
            futures::stream::FuturesUnordered::from_iter(i.addresses.values_mut().map(
                |devices::AddressInfo {
                     address_state_provider: devices::FidlWorkerInfo { worker, cancelation_sender },
                     assignment_state_sender: _,
                 }| {
                    if let Some(cancelation_sender) = cancelation_sender.take() {
                        cancelation_sender
                            .send(fnet_interfaces_admin::AddressRemovalReason::InterfaceRemoved)
                            .expect("failed to stop AddressStateProvider");
                    }
                    worker.clone()
                },
            ))
        })
    };
    address_state_providers.collect::<()>().await;
    cleanup_fn
}

enum ControlRequestResult {
    Continue,
    Remove,
}

/// Serves a `fuchsia.net.interfaces.admin/Control` Request.
async fn dispatch_control_request(
    req: fnet_interfaces_admin::ControlRequest,
    ctx: &Ctx,
    id: BindingId,
    removable: bool,
    owns_interface: &mut bool,
    interface_access_synchronizer: &AsyncMutex<()>,
) -> Result<ControlRequestResult, fidl::Error> {
    tracing::debug!("serving {:?}", req);
    // Take the lock to synchronize with other operations that may mutate state
    // for this device (e.g. phy link up/down handlers).
    let guard = interface_access_synchronizer.lock().await;
    let _: &() = guard.deref();

    match req {
        fnet_interfaces_admin::ControlRequest::AddAddress {
            address,
            parameters,
            address_state_provider,
            control_handle: _,
        } => Ok(add_address(ctx, id, address, parameters, address_state_provider)),
        fnet_interfaces_admin::ControlRequest::RemoveAddress { address, responder } => {
            responder.send(Ok(remove_address(ctx, id, address).await))
        }
        fnet_interfaces_admin::ControlRequest::GetId { responder } => responder.send(id.get()),
        fnet_interfaces_admin::ControlRequest::SetConfiguration { config, responder } => {
            responder.send(&mut set_configuration(ctx, id, config))
        }
        fnet_interfaces_admin::ControlRequest::GetConfiguration { responder } => {
            responder.send(&mut Ok(get_configuration(ctx, id)))
        }
        fnet_interfaces_admin::ControlRequest::Enable { responder } => {
            responder.send(Ok(set_interface_enabled(ctx, true, id).await))
        }
        fnet_interfaces_admin::ControlRequest::Disable { responder } => {
            responder.send(Ok(set_interface_enabled(ctx, false, id).await))
        }
        fnet_interfaces_admin::ControlRequest::Detach { control_handle: _ } => {
            *owns_interface = false;
            Ok(())
        }
        fnet_interfaces_admin::ControlRequest::Remove { responder } => {
            if removable {
                return responder.send(Ok(())).map(|()| ControlRequestResult::Remove);
            }
            responder.send(Err(fnet_interfaces_admin::ControlRemoveError::NotAllowed))
        }
        fnet_interfaces_admin::ControlRequest::GetAuthorizationForInterface { responder: _ } => {
            todo!("https://fxbug.dev/117844 support GetAuthorizationForInterface")
        }
    }
    .map(|()| ControlRequestResult::Continue)
}

/// Cleans up and removes the specified NetDevice interface.
///
/// # Panics
///
/// Panics if `id` points to a loopback device.
async fn remove_interface(ctx: Ctx, id: BindingId) {
    let devices::NetdeviceInfo { handler, mac: _, static_common_info: _, dynamic: _ } = {
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
        let core_id =
            non_sync_ctx.devices.remove_device(id).expect("device was not removed since retrieval");
        match core_id {
            DeviceId::Ethernet(id) => {
                netstack3_core::device::remove_ethernet_device(sync_ctx, non_sync_ctx, id)
            }
            DeviceId::Loopback(id) => panic!("loopback device {} may not be removed", id),
        }
    };

    handler.uninstall().await.unwrap_or_else(|e| {
        tracing::warn!("error uninstalling netdevice handler for interface {}: {:?}", id, e)
    })
}

/// Sets interface with `id` to `admin_enabled = enabled`.
///
/// Returns `true` if the value of `admin_enabled` changed in response to
/// this call.
async fn set_interface_enabled(ctx: &Ctx, enabled: bool, id: BindingId) -> bool {
    enum Info<A, B> {
        Loopback(A),
        Netdevice(B),
    }

    let mut ctx = ctx.clone();
    let core_id = ctx.non_sync_ctx.devices.get_core_id(id).expect("device not present");
    let port_handler = {
        let mut info = match core_id.external_state() {
            devices::DeviceSpecificInfo::Loopback(devices::LoopbackInfo {
                static_common_info: _,
                dynamic_common_info,
                rx_notifier: _,
            }) => Info::Loopback((dynamic_common_info.write().unwrap(), None)),
            devices::DeviceSpecificInfo::Netdevice(devices::NetdeviceInfo {
                static_common_info: _,
                handler,
                mac: _,
                dynamic,
            }) => Info::Netdevice((dynamic.write().unwrap(), Some(handler))),
        };
        let (common_info, port_handler) = match info {
            Info::Loopback((ref mut common_info, port_handler)) => {
                (common_info.deref_mut(), port_handler)
            }
            Info::Netdevice((ref mut dynamic, port_handler)) => {
                (&mut dynamic.common_info, port_handler)
            }
        };

        // Already set to expected value.
        if common_info.admin_enabled == enabled {
            return false;
        }
        common_info.admin_enabled = enabled;

        port_handler
    };

    if let Some(handler) = port_handler {
        let r = if enabled { handler.attach().await } else { handler.detach().await };
        match r {
            Ok(()) => (),
            Err(e) => {
                tracing::warn!("failed to set port {:?} to {}: {:?}", handler, enabled, e);
                // NB: There might be other errors here to consider in the
                // future, we start with a very strict set of known errors to
                // allow and panic on anything that is unexpected.
                match e {
                    // We can race with the port being removed or the device
                    // being destroyed.
                    netdevice_client::Error::Attach(_, zx::Status::NOT_FOUND)
                    | netdevice_client::Error::Detach(_, zx::Status::NOT_FOUND) => (),
                    netdevice_client::Error::Fidl(e) if e.is_closed() => (),
                    e => panic!(
                        "unexpected error setting enabled={} on port {:?}: {:?}",
                        enabled, handler, e
                    ),
                }
            }
        }
    }
    crate::bindings::set_interface_enabled(&mut ctx, id, enabled)
        .unwrap_or_else(|e| panic!("failed to set interface enabled={}: {:?}", enabled, e));
    true
}

/// Removes the given `address` from the interface with the given `id`.
///
/// Returns `true` if the address existed and was removed; otherwise `false`.
async fn remove_address(ctx: &Ctx, id: BindingId, address: fnet::Subnet) -> bool {
    let specified_addr = match address.addr.try_into_core() {
        Ok(addr) => addr,
        Err(e) => {
            tracing::warn!("not removing unspecified address {:?}: {:?}", address.addr, e);
            return false;
        }
    };
    let Some((worker, cancelation_sender)) = ({
        let ctx = ctx.clone();
        let core_id =
            ctx.non_sync_ctx.devices.get_core_id(id).expect("missing device info for interface");
        core_id.external_state().with_common_info_mut(|i| {
            i.addresses.get_mut(&specified_addr)
            .map(|devices::AddressInfo {
                    address_state_provider: devices::FidlWorkerInfo { worker, cancelation_sender },
                    assignment_state_sender: _,
                }| (worker.clone(), cancelation_sender.take()),
            )
        })
    }) else { return false };
    let did_cancel_worker = match cancelation_sender {
        Some(cancelation_sender) => {
            cancelation_sender
                .send(fnet_interfaces_admin::AddressRemovalReason::UserRemoved)
                .expect("failed to stop AddressStateProvider");
            true
        }
        // The worker was already canceled by some other task.
        None => false,
    };
    // Wait for the worker to finish regardless of if we were the task to cancel
    // it. Doing so prevents us from prematurely returning while the address is
    // pending cleanup.
    worker.await;
    // Because the worker removes the address on teardown, `did_cancel_worker`
    // is a suitable proxy for `did_remove_addr`.
    return did_cancel_worker;
}

/// Sets the provided `config` on the interface with the given `id`.
///
/// Returns the previously set configuration on the interface.
fn set_configuration(
    ctx: &Ctx,
    id: BindingId,
    config: fnet_interfaces_admin::Configuration,
) -> fnet_interfaces_admin::ControlSetConfigurationResult {
    let mut ctx = ctx.clone();
    let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
    let core_id = non_sync_ctx
        .devices
        .get_core_id(id)
        .expect("device lifetime should be tied to channel lifetime");

    let fnet_interfaces_admin::Configuration { ipv4, ipv6, .. } = config;

    let is_loopback = match core_id {
        DeviceId::Loopback(_) => true,
        DeviceId::Ethernet(_) => false,
    };

    if let Some(fnet_interfaces_admin::Ipv4Configuration {
        igmp,
        multicast_forwarding,
        forwarding,
        ..
    }) = ipv4.as_ref()
    {
        if let Some(_) = igmp {
            tracing::warn!("TODO(https://fxbug.dev/120293): support IGMP configuration changes")
        }
        if let Some(_) = multicast_forwarding {
            tracing::warn!(
                "TODO(https://fxbug.dev/124237): setting multicast_forwarding not yet supported"
            )
        }
        if let Some(forwarding) = forwarding {
            if *forwarding && is_loopback {
                return Err(
                    fnet_interfaces_admin::ControlSetConfigurationError::Ipv4ForwardingUnsupported,
                );
            }
        }
    }
    if let Some(fnet_interfaces_admin::Ipv6Configuration {
        mld,
        multicast_forwarding,
        forwarding,
        ..
    }) = ipv6.as_ref()
    {
        if let Some(_) = mld {
            tracing::warn!("TODO(https://fxbug.dev/120293): support MLD configuration changes")
        }
        if let Some(_) = multicast_forwarding {
            tracing::warn!(
                "TODO(https://fxbug.dev/124237): setting multicast_forwarding not yet supported"
            )
        }
        if let Some(forwarding) = forwarding {
            if *forwarding && is_loopback {
                return Err(
                    fnet_interfaces_admin::ControlSetConfigurationError::Ipv6ForwardingUnsupported,
                );
            }
        }
    }

    Ok(fnet_interfaces_admin::Configuration {
        ipv4: ipv4.map(|fnet_interfaces_admin::Ipv4Configuration { forwarding, .. }| {
            fnet_interfaces_admin::Ipv4Configuration {
                forwarding: forwarding.and_then(|enable| {
                    update_ipv4_configuration(
                        sync_ctx,
                        non_sync_ctx,
                        &core_id,
                        Ipv4DeviceConfigurationUpdate {
                            ip_config: Some(IpDeviceConfigurationUpdate {
                                forwarding_enabled: Some(enable),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )
                    .expect("checked supported configuration before calling")
                    .ip_config
                    .expect("IP configuration was updated")
                    .forwarding_enabled
                }),
                ..Default::default()
            }
        }),
        ipv6: ipv6.map(|fnet_interfaces_admin::Ipv6Configuration { forwarding, .. }| {
            fnet_interfaces_admin::Ipv6Configuration {
                forwarding: forwarding.and_then(|enable| {
                    update_ipv6_configuration(
                        sync_ctx,
                        non_sync_ctx,
                        &core_id,
                        Ipv6DeviceConfigurationUpdate {
                            ip_config: Some(IpDeviceConfigurationUpdate {
                                forwarding_enabled: Some(enable),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )
                    .expect("checked supported configuration before calling")
                    .ip_config
                    .expect("IP configuration was updated")
                    .forwarding_enabled
                }),
                ..Default::default()
            }
        }),
        ..Default::default()
    })
}

/// Returns the configuration used for the interface with the given `id`.
fn get_configuration(ctx: &Ctx, id: BindingId) -> fnet_interfaces_admin::Configuration {
    let mut ctx = ctx.clone();
    let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
    let core_id = non_sync_ctx
        .devices
        .get_core_id(id)
        .expect("device lifetime should be tied to channel lifetime");
    fnet_interfaces_admin::Configuration {
        ipv4: Some(fnet_interfaces_admin::Ipv4Configuration {
            forwarding: Some(
                netstack3_core::device::get_ipv4_configuration(&sync_ctx, &core_id)
                    .ip_config
                    .forwarding_enabled,
            ),
            ..Default::default()
        }),
        ipv6: Some(fnet_interfaces_admin::Ipv6Configuration {
            forwarding: Some(
                netstack3_core::device::get_ipv6_configuration(&sync_ctx, &core_id)
                    .ip_config
                    .forwarding_enabled,
            ),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Adds the given `address` to the interface with the given `id`.
///
/// If the address cannot be added, the appropriate removal reason will be sent
/// to the address_state_provider.
fn add_address(
    ctx: &Ctx,
    id: BindingId,
    address: fnet::Subnet,
    params: fnet_interfaces_admin::AddressParameters,
    address_state_provider: ServerEnd<fnet_interfaces_admin::AddressStateProviderMarker>,
) {
    let (req_stream, control_handle) = address_state_provider
        .into_stream_and_control_handle()
        .expect("failed to decompose AddressStateProvider handle");
    let addr_subnet_either: AddrSubnetEither = match address.try_into_core() {
        Ok(addr) => addr,
        Err(e) => {
            tracing::warn!("not adding invalid address {:?} to interface {}: {:?}", address, id, e);
            close_address_state_provider(
                address.addr.into_core(),
                id,
                control_handle,
                fnet_interfaces_admin::AddressRemovalReason::Invalid,
            );
            return;
        }
    };
    let specified_addr = addr_subnet_either.addr();

    if params.temporary.unwrap_or(false) {
        todo!("https://fxbug.dev/105630: support adding temporary addresses");
    }
    const INFINITE_NANOS: i64 = zx::Time::INFINITE.into_nanos();
    let initial_properties =
        params.initial_properties.unwrap_or(fnet_interfaces_admin::AddressProperties::default());
    let valid_lifetime_end = initial_properties.valid_lifetime_end.unwrap_or(INFINITE_NANOS);
    if valid_lifetime_end != INFINITE_NANOS {
        tracing::warn!(
            "TODO(https://fxbug.dev/105630): ignoring valid_lifetime_end: {:?}",
            valid_lifetime_end
        );
    }
    match initial_properties
        .preferred_lifetime_info
        .unwrap_or(fnet_interfaces::PreferredLifetimeInfo::PreferredUntil(INFINITE_NANOS))
    {
        fnet_interfaces::PreferredLifetimeInfo::Deprecated(_) => {
            todo!("https://fxbug.dev/105630: support adding deprecated addresses")
        }
        fnet_interfaces::PreferredLifetimeInfo::PreferredUntil(preferred_until) => {
            if preferred_until != INFINITE_NANOS {
                tracing::warn!(
                    "TODO(https://fxbug.dev/105630): ignoring preferred_until: {:?}",
                    preferred_until
                );
            }
        }
    }

    let locked_ctx = ctx.clone();
    let core_id =
        locked_ctx.non_sync_ctx.devices.get_core_id(id).expect("missing device info for interface");
    core_id.external_state().with_common_info_mut(|i| {
        let vacant_address_entry = match i.addresses.entry(specified_addr) {
            hash_map::Entry::Occupied(_occupied) => {
                close_address_state_provider(
                    address.addr.into_core(),
                    id,
                    control_handle,
                    fnet_interfaces_admin::AddressRemovalReason::AlreadyAssigned,
                );
                return;
            }
            hash_map::Entry::Vacant(vacant) => vacant,
        };
        // Cancelation mechanism for the `AddressStateProvider` worker.
        let (cancelation_sender, cancelation_receiver) = futures::channel::oneshot::channel();
        // Sender/receiver for updates in `AddressAssignmentState`, as
        // published by Core.
        let (assignment_state_sender, assignment_state_receiver) =
            futures::channel::mpsc::unbounded();
        // Spawn the `AddressStateProvider` worker, which during
        // initialization, will add the address to Core.
        let worker = fasync::Task::spawn(run_address_state_provider(
            ctx.clone(),
            addr_subnet_either,
            params.add_subnet_route.unwrap_or(false),
            id,
            control_handle,
            req_stream,
            assignment_state_receiver,
            cancelation_receiver,
        ))
        .shared();
        let _: &mut devices::AddressInfo = vacant_address_entry.insert(devices::AddressInfo {
            address_state_provider: devices::FidlWorkerInfo {
                worker,
                cancelation_sender: Some(cancelation_sender),
            },
            assignment_state_sender,
        });
    })
}

/// A worker for `fuchsia.net.interfaces.admin/AddressStateProvider`.
async fn run_address_state_provider(
    ctx: Ctx,
    addr_subnet_either: AddrSubnetEither,
    add_subnet_route: bool,
    id: BindingId,
    control_handle: fnet_interfaces_admin::AddressStateProviderControlHandle,
    req_stream: fnet_interfaces_admin::AddressStateProviderRequestStream,
    mut assignment_state_receiver: futures::channel::mpsc::UnboundedReceiver<
        fnet_interfaces_admin::AddressAssignmentState,
    >,
    mut stop_receiver: futures::channel::oneshot::Receiver<
        fnet_interfaces_admin::AddressRemovalReason,
    >,
) {
    let (address, subnet) = addr_subnet_either.addr_subnet();
    struct StateInCore {
        address: bool,
        subnet_route: bool,
    }

    // Add the address to Core. Note that even though we verified the address
    // was absent from `ctx` before spawning this worker, it's still possible
    // for the address to exist in core (e.g. auto-configured addresses such as
    // loopback or SLAAC).
    let add_to_core_result = {
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
        let device_id = non_sync_ctx.devices.get_core_id(id).expect("interface not found");
        netstack3_core::add_ip_addr_subnet(sync_ctx, non_sync_ctx, &device_id, addr_subnet_either)
    };
    let state_to_remove_from_core = match add_to_core_result {
        Err(netstack3_core::error::ExistsError) => {
            close_address_state_provider(
                *address,
                id,
                control_handle,
                fnet_interfaces_admin::AddressRemovalReason::AlreadyAssigned,
            );
            // The address already existed, so don't attempt to remove it.
            // Otherwise we would accidentally remove an address we didn't add!
            StateInCore { address: false, subnet_route: false }
        }
        Ok(()) => {
            let state_to_remove = if add_subnet_route {
                let mut ctx = ctx.clone();
                let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
                let core_id = non_sync_ctx
                    .devices
                    .get_core_id(id)
                    .expect("missing device info for interface");
                match netstack3_core::add_route(
                    sync_ctx,
                    non_sync_ctx,
                    netstack3_core::ip::types::AddableEntryEither::without_gateway(
                        subnet,
                        core_id,
                        netstack3_core::ip::types::AddableMetric::MetricTracksInterface,
                    ),
                ) {
                    Ok(()) => StateInCore { address: true, subnet_route: true },
                    Err(e) => {
                        tracing::warn!("failed to add subnet route {}: {}", addr_subnet_either, e);
                        StateInCore { address: true, subnet_route: false }
                    }
                }
            } else {
                StateInCore { address: true, subnet_route: false }
            };

            // Receive the initial assignment state from Core. The message
            // must already be in the channel, so don't await.
            let initial_assignment_state = assignment_state_receiver
                .next()
                .now_or_never()
                .expect("receiver unexpectedly empty")
                .expect("sender unexpectedly closed");

            // Run the `AddressStateProvider` main loop.
            // Pass in the `assignment_state_receiver` and `stop_receiver` by
            // ref so that they don't get dropped after the main loop exits
            // (before the senders are removed from `ctx`).
            match address_state_provider_main_loop(
                address,
                id,
                control_handle,
                req_stream,
                &mut assignment_state_receiver,
                initial_assignment_state,
                &mut stop_receiver,
            )
            .await
            {
                AddressNeedsExplicitRemovalFromCore::Yes => state_to_remove,
                AddressNeedsExplicitRemovalFromCore::No => {
                    StateInCore { address: false, ..state_to_remove }
                }
            }
        }
    };

    // Remove the address.
    let mut ctx = ctx.clone();
    let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
    let core_id = non_sync_ctx.devices.get_core_id(id).expect("missing device info for interface");
    // Don't drop the worker yet; it's what's driving THIS function.
    let _worker: futures::future::Shared<fuchsia_async::Task<()>> =
        match core_id.external_state().with_common_info_mut(|i| i.addresses.remove(&address)) {
            Some(devices::AddressInfo {
                address_state_provider: devices::FidlWorkerInfo { worker, cancelation_sender: _ },
                assignment_state_sender: _,
            }) => worker,
            None => {
                panic!("`AddressInfo` unexpectedly missing for {:?}", address)
            }
        };

    let StateInCore { address: remove_address, subnet_route: remove_subnet_route } =
        state_to_remove_from_core;
    let device_id = non_sync_ctx.devices.get_core_id(id).expect("interface not found");
    if remove_subnet_route {
        // TODO(https://fxbug.dev/123319): Migrate away from the
        // `add_subnet_route` flag and only remove the route when the
        // last handle for it is gone.
        //
        // The `add_subnet_route` flag is deprecated and is only being
        // used to support the DHCP client. Once the new routes admin
        // API is implemented, the flag will be removed and its usages
        // will be migrated. Until then, provide best-effort support
        // without complex reference counting.
        netstack3_core::del_route(sync_ctx, non_sync_ctx, subnet).unwrap_or_else(|e| {
            tracing::warn!(
                "failed to remove route for {} added via add_subnet_route: {}",
                addr_subnet_either,
                e
            )
        })
    }
    if remove_address {
        assert_matches!(
            netstack3_core::del_ip_addr(sync_ctx, non_sync_ctx, &device_id, address),
            Ok(())
        );
    }
}

enum AddressNeedsExplicitRemovalFromCore {
    /// The caller is expected to delete the address from Core.
    Yes,
    /// The caller need not delete the address from Core (e.g. interface removal,
    /// which implicitly removes all addresses.)
    No,
}

async fn address_state_provider_main_loop(
    address: SpecifiedAddr<IpAddr>,
    id: BindingId,
    control_handle: fnet_interfaces_admin::AddressStateProviderControlHandle,
    req_stream: fnet_interfaces_admin::AddressStateProviderRequestStream,
    assignment_state_receiver: &mut futures::channel::mpsc::UnboundedReceiver<
        fnet_interfaces_admin::AddressAssignmentState,
    >,
    initial_assignment_state: fnet_interfaces_admin::AddressAssignmentState,
    stop_receiver: &mut futures::channel::oneshot::Receiver<
        fnet_interfaces_admin::AddressRemovalReason,
    >,
) -> AddressNeedsExplicitRemovalFromCore {
    // When detached, the lifetime of `req_stream` should not be tied to the
    // lifetime of `address`.
    let mut detached = false;
    let mut watch_state = AddressAssignmentWatcherState {
        fsm: AddressAssignmentWatcherStateMachine::UnreportedUpdate(initial_assignment_state),
        last_response: None,
    };
    enum AddressStateProviderEvent {
        Request(Result<Option<fnet_interfaces_admin::AddressStateProviderRequest>, fidl::Error>),
        AssignmentStateChange(fnet_interfaces_admin::AddressAssignmentState),
        Canceled(fnet_interfaces_admin::AddressRemovalReason),
    }
    futures::pin_mut!(req_stream);
    futures::pin_mut!(stop_receiver);
    futures::pin_mut!(assignment_state_receiver);
    let cancelation_reason = loop {
        let next_event = futures::select! {
            req = req_stream.try_next() => AddressStateProviderEvent::Request(req),
            state = assignment_state_receiver.next() => {
                    AddressStateProviderEvent::AssignmentStateChange(
                        // It's safe to `expect` here, because the
                        // AddressStateProvider worker is responsible for
                        // cleaning up the sender, and only does so after this
                        // function exits.
                        state.expect("sender unexpectedly closed"))
                },
            reason = stop_receiver => AddressStateProviderEvent::Canceled(reason.expect("failed to receive stop")),
        };
        match next_event {
            AddressStateProviderEvent::Request(req) => match req {
                // The client hung up, stop serving.
                Ok(None) => {
                    // If detached, wait to be canceled before exiting.
                    if detached {
                        // N.B. The `Canceled` arm of this match exits the loop,
                        // meaning we can't already be canceled here.
                        debug_assert!(!stop_receiver.is_terminated());
                        break Some(stop_receiver.await.expect("failed to receive stop"));
                    }
                    break None;
                }
                Ok(Some(request)) => {
                    let e = match dispatch_address_state_provider_request(
                        request,
                        &mut detached,
                        &mut watch_state,
                    ) {
                        Ok(()) => continue,
                        Err(e) => e,
                    };
                    let (log_level, should_terminate) = match &e {
                        AddressStateProviderError::PreviousPendingWatchRequest => {
                            (tracing::Level::WARN, true)
                        }
                        AddressStateProviderError::Fidl(e) => (util::fidl_err_log_level(e), false),
                    };
                    log_error!(
                        log_level,
                        "failed to handle request for address {:?} on interface {}: {}",
                        address,
                        id,
                        e
                    );
                    if should_terminate {
                        break None;
                    }
                }
                Err(e) => {
                    log_error!(
                        util::fidl_err_log_level(&e),
                        "error operating {} stream for address {:?} on interface {}: {:?}",
                        fnet_interfaces_admin::AddressStateProviderMarker::DEBUG_NAME,
                        address,
                        id,
                        e
                    );
                    break None;
                }
            },
            AddressStateProviderEvent::AssignmentStateChange(state) => {
                watch_state.on_new_assignment_state(state).unwrap_or_else(|e|{
                        log_error!(
                            util::fidl_err_log_level(&e),
                            "failed to respond to pending watch request for address {:?} on interface {}: {:?}",
                            address,
                            id,
                            e
                        )
                    });
            }
            AddressStateProviderEvent::Canceled(reason) => {
                close_address_state_provider(*address, id, control_handle, reason);
                break Some(reason);
            }
        }
    };

    match cancelation_reason {
        Some(fnet_interfaces_admin::AddressRemovalReason::DadFailed)
        | Some(fnet_interfaces_admin::AddressRemovalReason::InterfaceRemoved) => {
            return AddressNeedsExplicitRemovalFromCore::No
        }
        Some(fnet_interfaces_admin::AddressRemovalReason::Invalid)
        | Some(fnet_interfaces_admin::AddressRemovalReason::AlreadyAssigned)
        | Some(fnet_interfaces_admin::AddressRemovalReason::UserRemoved)
        | None => AddressNeedsExplicitRemovalFromCore::Yes,
    }
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum AddressStateProviderError {
    #[error(
        "received a `WatchAddressAssignmentState` request while a previous request is pending"
    )]
    PreviousPendingWatchRequest,
    #[error("FIDL error: {0}")]
    Fidl(fidl::Error),
}

// State Machine for the `WatchAddressAssignmentState` "Hanging-Get" FIDL API.
#[derive(Debug)]
enum AddressAssignmentWatcherStateMachine {
    // Holds the new assignment state waiting to be sent.
    UnreportedUpdate(fnet_interfaces_admin::AddressAssignmentState),
    // Holds the hanging responder waiting for a new assignment state to send.
    HangingRequest(fnet_interfaces_admin::AddressStateProviderWatchAddressAssignmentStateResponder),
    Idle,
}
struct AddressAssignmentWatcherState {
    // The state of the `WatchAddressAssignmentState` "Hanging-Get" FIDL API.
    fsm: AddressAssignmentWatcherStateMachine,
    // The last response to a `WatchAddressAssignmentState` FIDL request.
    // `None` until the first request, after which it will always be `Some`.
    last_response: Option<fnet_interfaces_admin::AddressAssignmentState>,
}

impl AddressAssignmentWatcherState {
    // Handle a change in `AddressAssignmentState` as published by Core.
    fn on_new_assignment_state(
        &mut self,
        new_state: fnet_interfaces_admin::AddressAssignmentState,
    ) -> Result<(), fidl::Error> {
        use AddressAssignmentWatcherStateMachine::*;
        let Self { fsm, last_response } = self;
        // Use `Idle` as a placeholder value to take ownership of `fsm`.
        let old_fsm = std::mem::replace(fsm, Idle);
        let (new_fsm, result) = match old_fsm {
            UnreportedUpdate(old_state) => {
                if old_state == new_state {
                    tracing::warn!("received duplicate AddressAssignmentState event from Core.");
                }
                if self.last_response == Some(new_state) {
                    // Return to `Idle` because we've coalesced
                    // multiple updates and no-longer have new state to send.
                    (Idle, Ok(()))
                } else {
                    (UnreportedUpdate(new_state), Ok(()))
                }
            }
            HangingRequest(responder) => {
                *last_response = Some(new_state);
                (Idle, responder.send(new_state))
            }
            Idle => (UnreportedUpdate(new_state), Ok(())),
        };
        assert_matches!(std::mem::replace(fsm, new_fsm), Idle);
        result
    }

    // Handle a new `WatchAddressAssignmentState` FIDL request.
    fn on_new_watch_req(
        &mut self,
        responder: fnet_interfaces_admin::AddressStateProviderWatchAddressAssignmentStateResponder,
    ) -> Result<(), AddressStateProviderError> {
        use AddressAssignmentWatcherStateMachine::*;
        let Self { fsm, last_response } = self;
        // Use `Idle` as a placeholder value to take ownership of `fsm`.
        let old_fsm = std::mem::replace(fsm, Idle);
        let (new_fsm, result) = match old_fsm {
            UnreportedUpdate(state) => {
                *last_response = Some(state);
                (Idle, responder.send(state).map_err(AddressStateProviderError::Fidl))
            }
            HangingRequest(_existing_responder) => (
                HangingRequest(responder),
                Err(AddressStateProviderError::PreviousPendingWatchRequest),
            ),
            Idle => (HangingRequest(responder), Ok(())),
        };
        assert_matches!(std::mem::replace(fsm, new_fsm), Idle);
        result
    }
}

/// Serves a `fuchsia.net.interfaces.admin/AddressStateProvider` request.
fn dispatch_address_state_provider_request(
    req: fnet_interfaces_admin::AddressStateProviderRequest,
    detached: &mut bool,
    watch_state: &mut AddressAssignmentWatcherState,
) -> Result<(), AddressStateProviderError> {
    tracing::debug!("serving {:?}", req);
    match req {
        fnet_interfaces_admin::AddressStateProviderRequest::UpdateAddressProperties {
            address_properties: _,
            responder: _,
        } => todo!("https://fxbug.dev/105011 Support updating address properties"),
        fnet_interfaces_admin::AddressStateProviderRequest::WatchAddressAssignmentState {
            responder,
        } => watch_state.on_new_watch_req(responder),
        fnet_interfaces_admin::AddressStateProviderRequest::Detach { control_handle: _ } => {
            *detached = true;
            Ok(())
        }
    }
}

fn close_address_state_provider(
    addr: IpAddr,
    id: BindingId,
    control_handle: fnet_interfaces_admin::AddressStateProviderControlHandle,
    reason: fnet_interfaces_admin::AddressRemovalReason,
) {
    control_handle.send_on_address_removed(reason).unwrap_or_else(|e| {
        let log_level = if e.is_closed() { tracing::Level::DEBUG } else { tracing::Level::ERROR };
        log_error!(
            log_level,
            "failed to send address removal reason for addr {:?} on interface {}: {:?}",
            addr,
            id,
            e
        );
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext;
    use net_declare::fidl_subnet;
    use netstack3_core::ip::types::RawMetric;

    use crate::bindings::{
        interfaces_watcher::InterfaceEvent, interfaces_watcher::InterfaceUpdate, util::IntoFidl,
        Ctx, InterfaceEventProducer, DEFAULT_INTERFACE_METRIC, DEFAULT_LOOPBACK_MTU,
    };

    // Verifies that when an an interface is removed, its addresses are
    // implicitly removed, rather then explicitly removed one-by-one. Explicit
    // removal would be redundant and is unnecessary.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn implicit_address_removal_on_interface_removal() {
        let ctx: Ctx = Default::default();
        let (control_client_end, control_server_end) =
            fnet_interfaces_ext::admin::Control::create_endpoints().expect("create control proxy");
        let (control_sender, control_receiver) =
            OwnedControlHandle::new_channel_with_owned_handle(control_server_end).await;
        let (event_sender, event_receiver) = futures::channel::mpsc::unbounded();

        // Add the interface.
        let binding_id = {
            let mut ctx = ctx.clone();
            let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
            let binding_id = non_sync_ctx.devices.alloc_new_id();
            let core_id = netstack3_core::device::add_loopback_device_with_state(
                sync_ctx,
                DEFAULT_LOOPBACK_MTU,
                RawMetric(DEFAULT_INTERFACE_METRIC),
                || {
                    const LOOPBACK_NAME: &'static str = "lo";
                    devices::LoopbackInfo {
                        static_common_info: devices::StaticCommonInfo {
                            binding_id,
                            name: LOOPBACK_NAME.to_string(),
                        },
                        dynamic_common_info: devices::DynamicCommonInfo {
                            mtu: DEFAULT_LOOPBACK_MTU,
                            admin_enabled: true,
                            events: InterfaceEventProducer::new(binding_id, event_sender),
                            control_hook: control_sender,
                            addresses: HashMap::new(),
                        }
                        .into(),
                        rx_notifier: Default::default(),
                    }
                },
            )
            .expect("failed to add loopback to core");
            non_sync_ctx.devices.add_device(binding_id, core_id.into());
            binding_id
        };

        // Start the interface control worker.
        let (_stop_sender, stop_receiver) = futures::channel::oneshot::channel();
        let removable = false;
        let interface_control_fut = run_interface_control(
            ctx,
            binding_id,
            stop_receiver,
            control_receiver,
            removable,
            AsyncMutex::new(()),
        );

        // Add an address.
        let (asp_client_end, asp_server_end) =
            fidl::endpoints::create_proxy::<fnet_interfaces_admin::AddressStateProviderMarker>()
                .expect("create ASP proxy");
        let mut addr = fidl_subnet!("1.1.1.1/32");
        control_client_end
            .add_address(
                &mut addr,
                fnet_interfaces_admin::AddressParameters::default(),
                asp_server_end,
            )
            .expect("failed to add address");

        // Observe the `AddressAdded` event.
        let event_receiver = event_receiver.fuse();
        let interface_control_fut = interface_control_fut.fuse();
        futures::pin_mut!(event_receiver);
        futures::pin_mut!(interface_control_fut);
        let event = futures::select!(
            _cleanup = interface_control_fut => panic!("interface control unexpectedly ended"),
            event = event_receiver.next() => event
        );
        assert_matches!(event, Some(InterfaceEvent::Changed {
            id,
            event: InterfaceUpdate::AddressAdded {
                addr: address,
                assignment_state: _,
                valid_until: _,
            }
        }) if (id == binding_id && address.into_fidl() == addr ));

        // Drop the control handle and expect interface control to exit
        drop(control_client_end);
        if let Some(cleanup) = interface_control_fut.await {
            cleanup();
        }

        // Expect that the event receiver has closed, and that it does not
        // contain, an `AddressRemoved` event, which would indicate the address
        // was explicitly removed.
        assert_matches!(event_receiver.next().await,
            Some(InterfaceEvent::Removed( id)) if id == binding_id);
        assert_matches!(event_receiver.next().await, None);

        // Verify the ASP closed for the correct reason.
        let fnet_interfaces_admin::AddressStateProviderEvent::OnAddressRemoved { error: reason } =
            asp_client_end
                .take_event_stream()
                .try_next()
                .await
                .expect("read AddressStateProvider event")
                .expect("AddressStateProvider event stream unexpectedly empty");
        assert_eq!(reason, fnet_interfaces_admin::AddressRemovalReason::InterfaceRemoved)
    }
}
