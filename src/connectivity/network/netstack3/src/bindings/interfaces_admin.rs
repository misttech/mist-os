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

use std::{collections::hash_map, ops::DerefMut as _};

use assert_matches::assert_matches;
use fidl::endpoints::{ProtocolMarker, ServerEnd};
use fidl_fuchsia_hardware_network as fhardware_network;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::{
    future::FusedFuture as _, stream::FusedStream as _, FutureExt as _, SinkExt as _,
    StreamExt as _, TryFutureExt as _, TryStreamExt as _,
};
use net_types::{ip::AddrSubnetEither, ip::IpAddr, SpecifiedAddr, Witness};
use netstack3_core::{
    device::{update_ipv4_configuration, update_ipv6_configuration, DeviceId},
    ip::device::{
        state::{AddrSubnetAndManualConfigEither, Ipv4AddrConfig, Ipv6AddrManualConfig, Lifetime},
        IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate, Ipv6DeviceConfigurationUpdate,
    },
};

use crate::bindings::{
    devices, netdevice_worker, util::IntoCore as _, util::TryIntoCore as _, BindingId, Ctx,
    DeviceIdExt as _, Netstack, StackTime,
};

pub(crate) async fn serve(ns: Netstack, req: fnet_interfaces_admin::InstallerRequestStream) {
    req.filter_map(|req| {
        let req = match req {
            Ok(req) => req,
            Err(e) => {
                if !e.is_closed() {
                    tracing::error!(
                        "{} request error {e:?}",
                        fnet_interfaces_admin::InstallerMarker::DEBUG_NAME
                    );
                }
                return futures::future::ready(None);
            }
        };
        match req {
            fnet_interfaces_admin::InstallerRequest::InstallDevice {
                device,
                device_control,
                control_handle: _,
            } => futures::future::ready(Some(fasync::Task::spawn(
                run_device_control(
                    ns.clone(),
                    device,
                    device_control.into_stream().expect("failed to obtain stream"),
                )
                .map(|r| {
                    r.unwrap_or_else(|e| tracing::warn!("device control finished with {:?}", e))
                }),
            ))),
        }
    })
    .for_each_concurrent(None, |task| {
        // Wait for all created devices on this installer to finish.
        task
    })
    .await;
}

#[derive(thiserror::Error, Debug)]
enum DeviceControlError {
    #[error("worker error: {0}")]
    Worker(#[from] netdevice_worker::Error),
    #[error("fidl error: {0}")]
    Fidl(#[from] fidl::Error),
}

async fn run_device_control(
    mut ns: Netstack,
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
                    if let Some(interface_tasks) = create_interface(
                        port,
                        control,
                        options,
                        &mut ns,
                        &handler,
                        stop_event.wait_or_dropped(),
                    )
                    .await
                    {
                        tasks.extend(interface_tasks);
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
pub(crate) struct OwnedControlHandle {
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
/// Returns `Some([fuchsia_async::Task;2])` if an interface was created
/// successfully. The returned `Task`s must be polled to completion and are tied
/// to the created interface's lifetime.
async fn create_interface(
    port: fhardware_network::PortId,
    control: fidl::endpoints::ServerEnd<fnet_interfaces_admin::ControlMarker>,
    options: fnet_interfaces_admin::Options,
    ns: &mut Netstack,
    handler: &netdevice_worker::DeviceHandler,
    device_stopped_fut: async_utils::event::EventWaitResult,
) -> Option<[fuchsia_async::Task<()>; 2]> {
    tracing::debug!("creating interface from {:?} with {:?}", port, options);
    let fnet_interfaces_admin::Options { name, metric, .. } = options;
    let (control_sender, mut control_receiver) =
        OwnedControlHandle::new_channel_with_owned_handle(control).await;
    match handler
        .add_port(ns, netdevice_worker::InterfaceOptions { name, metric }, port, control_sender)
        .await
    {
        Ok((binding_id, status_stream, tx_task)) => {
            let interface_control_task = fasync::Task::spawn(run_netdevice_interface_control(
                ns.ctx.clone(),
                binding_id,
                status_stream,
                device_stopped_fut,
                control_receiver,
            ));
            Some([interface_control_task, tx_task])
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
    let status_stream = status_stream.scan((), |(), status| {
        futures::future::ready(match status {
            Ok(netdevice_client::client::PortStatus { flags, mtu: _ }) => {
                Some(DeviceState { online: flags.contains(fhardware_network::StatusFlags::ONLINE) })
            }
            Err(e) => {
                match e {
                    netdevice_client::Error::Fidl(e) if e.is_closed() => {
                        tracing::warn!(
                            "error operating port state stream {:?} for interface {}",
                            e,
                            id
                        )
                    }
                    e => {
                        tracing::error!(
                            "error operating port state stream {:?} for interface {}",
                            e,
                            id
                        )
                    }
                }
                // Terminate the stream in case of any errors.
                None
            }
        })
    });

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
        status_stream,
    )
    .fuse();
    futures::pin_mut!(interface_control_fut);
    futures::select! {
        o = device_stopped_fut => {
            o.expect("event was orphaned");
        },
        () = interface_control_fut => (),
    };
    if !interface_control_fut.is_terminated() {
        // Cancel interface control and drive it to completion, allowing it to terminate each
        // control handle.
        interface_control_stop_sender
            .send(fnet_interfaces_admin::InterfaceRemovedReason::PortClosed)
            .expect("failed to cancel interface control");
        interface_control_fut.await
    }
}

pub(crate) struct DeviceState {
    online: bool,
}

/// Runs a worker to serve incoming `fuchsia.net.interfaces.admin/Control`
/// handles.
pub(crate) async fn run_interface_control<S: futures::Stream<Item = DeviceState>>(
    ctx: Ctx,
    id: BindingId,
    mut stop_receiver: futures::channel::oneshot::Receiver<
        fnet_interfaces_admin::InterfaceRemovedReason,
    >,
    control_receiver: futures::channel::mpsc::Receiver<OwnedControlHandle>,
    removable: bool,
    device_state: S,
) {
    // An event indicating that the individual control request streams should stop.
    let cancel_request_streams = async_utils::event::Event::new();

    let enabled_controller = enabled::InterfaceEnabledController::new(ctx.clone(), id);
    let enabled_controller = &enabled_controller;
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
                            tracing::error!(
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
                                enabled_controller,
                            )
                            .await
                            {
                                Err(e) => {
                                    tracing::error!(
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

    let device_state = {
        device_state
            .then(|DeviceState { online }| async move {
                tracing::debug!("observed interface {} online = {}", id, online);
                enabled_controller.set_phy_up(online).await;
            })
            .fuse()
            .collect::<()>()
    };

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

        futures::pin_mut!(device_state);
        futures::select! {
            // One of the interface's owning channels hung up or `Remove` was
            // called; inform the other channels.
            removal_requester = interface_control_stream.next() => {
                (fnet_interfaces_admin::InterfaceRemovedReason::User, removal_requester.flatten())
            }
            // Cancelation event was received with a specified remove reason.
            reason = stop_receiver => (reason.expect("failed to receive stop"), None),
            // Device state stream ended, assume device was removed.
            () = device_state => (fnet_interfaces_admin::InterfaceRemovedReason::PortClosed, None),
        }
    };

    // Close the control_receiver, preventing new RequestStreams from attaching.
    stream_of_fut.get_mut().get_mut().close();
    // Cancel the active request streams, and drive the remaining `ControlRequestStreams` to
    // completion, allowing each handle to send termination events.
    assert!(cancel_request_streams.signal(), "expected the event to be unsignaled");
    let control_handles = if !stream_of_fut.is_terminated() {
        // Accumulate all the control handles first so we stop operating on
        // them.
        stream_of_fut
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
            .await
    } else {
        // Drop the terminated stream of fut to get rid of context borrows.
        std::mem::drop(stream_of_fut);
        Vec::new()
    };
    // Cancel the `AddressStateProvider` workers and drive them to completion.
    let address_state_providers = {
        let core_id =
            ctx.non_sync_ctx().devices.get_core_id(id).expect("missing device info for interface");
        core_id.external_state().with_common_info_mut(|i| {
            futures::stream::FuturesUnordered::from_iter(i.addresses.values_mut().map(
                |devices::AddressInfo {
                     address_state_provider: devices::FidlWorkerInfo { worker, cancelation_sender },
                     assignment_state_sender: _,
                 }| {
                    if let Some(cancelation_sender) = cancelation_sender.take() {
                        cancelation_sender
                            .send(AddressStateProviderCancellationReason::InterfaceRemoved)
                            .expect("failed to stop AddressStateProvider");
                    }
                    worker.clone()
                },
            ))
        })
    };
    address_state_providers.collect::<()>().await;

    // Nothing else should be borrowing ctx by now. Moving to a mutable bind
    // proves this.
    let mut ctx = ctx;
    remove_interface(&mut ctx, id).await;

    // Send the termination reason for all handles we had on removal.
    control_handles.into_iter().for_each(|control_handle| {
        control_handle.send_on_interface_removed(remove_reason).unwrap_or_else(|e| {
            tracing::error!("failed to send terminal event: {:?} for interface {}", e, id)
        });
    });
}

enum ControlRequestResult {
    Continue,
    Remove,
}

/// Serves a `fuchsia.net.interfaces.admin/Control` Request.
async fn dispatch_control_request(
    req: fnet_interfaces_admin::ControlRequest,
    ctx: &mut Ctx,
    id: BindingId,
    removable: bool,
    owns_interface: &mut bool,
    enabled_controller: &enabled::InterfaceEnabledController,
) -> Result<ControlRequestResult, fidl::Error> {
    tracing::debug!("serving {:?}", req);

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
            responder.send(set_configuration(ctx, id, config).as_ref().map_err(|e| *e))
        }
        fnet_interfaces_admin::ControlRequest::GetConfiguration { responder } => {
            responder.send(Ok(&get_configuration(ctx, id)))
        }
        fnet_interfaces_admin::ControlRequest::Enable { responder } => {
            responder.send(Ok(enabled_controller.set_admin_enabled(true).await))
        }
        fnet_interfaces_admin::ControlRequest::Disable { responder } => {
            responder.send(Ok(enabled_controller.set_admin_enabled(false).await))
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
async fn remove_interface(ctx: &mut Ctx, id: BindingId) {
    let devices::NetdeviceInfo { handler, mac: _, static_common_info: _, dynamic: _ } = {
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        let core_id =
            non_sync_ctx.devices.remove_device(id).expect("device was not removed since retrieval");
        match core_id {
            DeviceId::Ethernet(id) => {
                netstack3_core::device::remove_ethernet_device(sync_ctx, non_sync_ctx, id)
            }
            DeviceId::Loopback(id) => {
                let devices::LoopbackInfo {
                    static_common_info: _,
                    dynamic_common_info: _,
                    rx_notifier: _,
                } = netstack3_core::device::remove_loopback_device(sync_ctx, non_sync_ctx, id);
                // Allow the loopback interface to be removed as part of clean
                // shutdown, but emit a warning about it.
                tracing::warn!("loopback interface was removed");
                return;
            }
        }
    };

    handler.uninstall().await.unwrap_or_else(|e| {
        tracing::warn!("error uninstalling netdevice handler for interface {}: {:?}", id, e)
    })
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
        let core_id =
            ctx.non_sync_ctx().devices.get_core_id(id).expect("missing device info for interface");
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
                .send(AddressStateProviderCancellationReason::UserRemoved)
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
    ctx: &mut Ctx,
    id: BindingId,
    config: fnet_interfaces_admin::Configuration,
) -> fnet_interfaces_admin::ControlSetConfigurationResult {
    let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
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
fn get_configuration(ctx: &mut Ctx, id: BindingId) -> fnet_interfaces_admin::Configuration {
    let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
    let core_id = non_sync_ctx
        .devices
        .get_core_id(id)
        .expect("device lifetime should be tied to channel lifetime");
    fnet_interfaces_admin::Configuration {
        ipv4: Some(fnet_interfaces_admin::Ipv4Configuration {
            forwarding: Some(
                netstack3_core::device::get_ipv4_configuration_and_flags(&sync_ctx, &core_id)
                    .config
                    .ip_config
                    .forwarding_enabled,
            ),
            ..Default::default()
        }),
        ipv6: Some(fnet_interfaces_admin::Ipv6Configuration {
            forwarding: Some(
                netstack3_core::device::get_ipv6_configuration_and_flags(&sync_ctx, &core_id)
                    .config
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
    ctx: &mut Ctx,
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
            send_address_removal_event(
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

    let valid_until = if valid_lifetime_end == INFINITE_NANOS {
        Lifetime::Infinite
    } else {
        Lifetime::Finite(StackTime(fasync::Time::from_nanos(valid_lifetime_end)))
    };

    let addr_subnet_either = match addr_subnet_either {
        AddrSubnetEither::V4(addr_subnet) => {
            AddrSubnetAndManualConfigEither::V4(addr_subnet, Ipv4AddrConfig { valid_until })
        }
        AddrSubnetEither::V6(addr_subnet) => {
            AddrSubnetAndManualConfigEither::V6(addr_subnet, Ipv6AddrManualConfig { valid_until })
        }
    };

    let core_id =
        ctx.non_sync_ctx().devices.get_core_id(id).expect("missing device info for interface");
    core_id.external_state().with_common_info_mut(|i| {
        let vacant_address_entry = match i.addresses.entry(specified_addr) {
            hash_map::Entry::Occupied(_occupied) => {
                send_address_removal_event(
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AddressStateProviderCancellationReason {
    UserRemoved,
    DadFailed,
    InterfaceRemoved,
}

impl From<AddressStateProviderCancellationReason> for fnet_interfaces_admin::AddressRemovalReason {
    fn from(value: AddressStateProviderCancellationReason) -> Self {
        match value {
            AddressStateProviderCancellationReason::UserRemoved => {
                fnet_interfaces_admin::AddressRemovalReason::UserRemoved
            }
            AddressStateProviderCancellationReason::DadFailed => {
                fnet_interfaces_admin::AddressRemovalReason::DadFailed
            }
            AddressStateProviderCancellationReason::InterfaceRemoved => {
                fnet_interfaces_admin::AddressRemovalReason::InterfaceRemoved
            }
        }
    }
}

/// A worker for `fuchsia.net.interfaces.admin/AddressStateProvider`.
async fn run_address_state_provider(
    mut ctx: Ctx,
    addr_subnet_and_config: AddrSubnetAndManualConfigEither<StackTime>,
    add_subnet_route: bool,
    id: BindingId,
    control_handle: fnet_interfaces_admin::AddressStateProviderControlHandle,
    req_stream: fnet_interfaces_admin::AddressStateProviderRequestStream,
    mut assignment_state_receiver: futures::channel::mpsc::UnboundedReceiver<
        fnet_interfaces::AddressAssignmentState,
    >,
    mut stop_receiver: futures::channel::oneshot::Receiver<AddressStateProviderCancellationReason>,
) {
    let addr_subnet_either = addr_subnet_and_config.addr_subnet_either();
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
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        let device_id = non_sync_ctx.devices.get_core_id(id).expect("interface not found");
        netstack3_core::add_ip_addr_subnet(
            sync_ctx,
            non_sync_ctx,
            &device_id,
            addr_subnet_and_config,
        )
    };
    let (state_to_remove_from_core, removal_reason) = match add_to_core_result {
        Err(netstack3_core::error::ExistsError) => {
            send_address_removal_event(
                *address,
                id,
                control_handle.clone(),
                fnet_interfaces_admin::AddressRemovalReason::AlreadyAssigned,
            );
            // The address already existed, so don't attempt to remove it.
            // Otherwise we would accidentally remove an address we didn't add!
            (StateInCore { address: false, subnet_route: false }, None)
        }
        Ok(()) => {
            let state_to_remove = if add_subnet_route {
                let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
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

            control_handle.send_on_address_added().unwrap_or_else(|e| {
                tracing::error!(
                    "failed to send address added event for addr {:?} on interface {}: {:?}",
                    address,
                    id,
                    e
                );
            });

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
            let (needs_removal, removal_reason) = address_state_provider_main_loop(
                &mut ctx,
                address,
                id,
                req_stream,
                &mut assignment_state_receiver,
                initial_assignment_state,
                &mut stop_receiver,
            )
            .await;

            (
                match needs_removal {
                    AddressNeedsExplicitRemovalFromCore::Yes => state_to_remove,
                    AddressNeedsExplicitRemovalFromCore::No => {
                        StateInCore { address: false, ..state_to_remove }
                    }
                },
                removal_reason,
            )
        }
    };

    // Remove the address.
    let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
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

    if let Some(removal_reason) = removal_reason {
        send_address_removal_event(address.get(), id, control_handle, removal_reason.into());
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
    ctx: &mut Ctx,
    address: SpecifiedAddr<IpAddr>,
    id: BindingId,
    req_stream: fnet_interfaces_admin::AddressStateProviderRequestStream,
    assignment_state_receiver: &mut futures::channel::mpsc::UnboundedReceiver<
        fnet_interfaces::AddressAssignmentState,
    >,
    initial_assignment_state: fnet_interfaces::AddressAssignmentState,
    stop_receiver: &mut futures::channel::oneshot::Receiver<AddressStateProviderCancellationReason>,
) -> (AddressNeedsExplicitRemovalFromCore, Option<AddressStateProviderCancellationReason>) {
    // When detached, the lifetime of `req_stream` should not be tied to the
    // lifetime of `address`.
    let mut detached = false;
    let mut watch_state = AddressAssignmentWatcherState {
        fsm: AddressAssignmentWatcherStateMachine::UnreportedUpdate(initial_assignment_state),
        last_response: None,
    };
    enum AddressStateProviderEvent {
        Request(Result<Option<fnet_interfaces_admin::AddressStateProviderRequest>, fidl::Error>),
        AssignmentStateChange(fnet_interfaces::AddressAssignmentState),
        Canceled(AddressStateProviderCancellationReason),
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
                Ok(Some(request)) => match dispatch_address_state_provider_request(
                    ctx,
                    request,
                    address,
                    id,
                    &mut detached,
                    &mut watch_state,
                ) {
                    Ok(Some(UserRemove)) => {
                        break Some(AddressStateProviderCancellationReason::UserRemoved)
                    }
                    Ok(None) => {}
                    Err(err @ AddressStateProviderError::PreviousPendingWatchRequest) => {
                        tracing::warn!(
                            "failed to handle request for address {:?} on interface {}: {}",
                            address,
                            id,
                            err
                        );
                        break None;
                    }
                    Err(err @ AddressStateProviderError::Fidl(_)) => tracing::error!(
                        "failed to handle request for address {:?} on interface {}: {}",
                        address,
                        id,
                        err
                    ),
                },
                Err(e) => {
                    tracing::error!(
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
                        tracing::error!(
                            "failed to respond to pending watch request for address {:?} on interface {}: {:?}",
                            address,
                            id,
                            e
                        )
                    });
            }
            AddressStateProviderEvent::Canceled(reason) => {
                break Some(reason);
            }
        }
    };

    (
        match cancelation_reason {
            Some(AddressStateProviderCancellationReason::DadFailed)
            | Some(AddressStateProviderCancellationReason::InterfaceRemoved) => {
                AddressNeedsExplicitRemovalFromCore::No
            }
            Some(AddressStateProviderCancellationReason::UserRemoved) | None => {
                AddressNeedsExplicitRemovalFromCore::Yes
            }
        },
        cancelation_reason,
    )
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
    UnreportedUpdate(fnet_interfaces::AddressAssignmentState),
    // Holds the hanging responder waiting for a new assignment state to send.
    HangingRequest(fnet_interfaces_admin::AddressStateProviderWatchAddressAssignmentStateResponder),
    Idle,
}
struct AddressAssignmentWatcherState {
    // The state of the `WatchAddressAssignmentState` "Hanging-Get" FIDL API.
    fsm: AddressAssignmentWatcherStateMachine,
    // The last response to a `WatchAddressAssignmentState` FIDL request.
    // `None` until the first request, after which it will always be `Some`.
    last_response: Option<fnet_interfaces::AddressAssignmentState>,
}

impl AddressAssignmentWatcherState {
    // Handle a change in `AddressAssignmentState` as published by Core.
    fn on_new_assignment_state(
        &mut self,
        new_state: fnet_interfaces::AddressAssignmentState,
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

struct UserRemove;

/// Serves a `fuchsia.net.interfaces.admin/AddressStateProvider` request.
///
/// If `Ok(Some(UserRemove))` is returned, the client has explicitly
/// requested removal of the address, but the address has not been removed yet.
fn dispatch_address_state_provider_request(
    ctx: &mut Ctx,
    req: fnet_interfaces_admin::AddressStateProviderRequest,
    address: SpecifiedAddr<IpAddr>,
    id: BindingId,
    detached: &mut bool,
    watch_state: &mut AddressAssignmentWatcherState,
) -> Result<Option<UserRemove>, AddressStateProviderError> {
    tracing::debug!("serving {:?}", req);
    match req {
        fnet_interfaces_admin::AddressStateProviderRequest::UpdateAddressProperties {
            address_properties:
                fnet_interfaces_admin::AddressProperties {
                    valid_lifetime_end: valid_lifetime_end_nanos,
                    preferred_lifetime_info,
                    ..
                },
            responder,
        } => {
            if preferred_lifetime_info.is_some() {
                tracing::warn!("Updating preferred lifetime info is not yet supported (https://fxbug.dev/105011)");
            }
            let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
            let device_id = non_sync_ctx.devices.get_core_id(id).expect("interface not found");
            let result = match address.into() {
                IpAddr::V4(address) => netstack3_core::device::set_ip_addr_properties(
                    sync_ctx,
                    non_sync_ctx,
                    &device_id,
                    address,
                    valid_lifetime_end_nanos
                        .map(|nanos| Lifetime::Finite(StackTime(fasync::Time::from_nanos(nanos))))
                        .unwrap_or(Lifetime::Infinite),
                ),
                IpAddr::V6(address) => netstack3_core::device::set_ip_addr_properties(
                    sync_ctx,
                    non_sync_ctx,
                    &device_id,
                    address,
                    valid_lifetime_end_nanos
                        .map(|nanos| Lifetime::Finite(StackTime(fasync::Time::from_nanos(nanos))))
                        .unwrap_or(Lifetime::Infinite),
                ),
            };
            match result {
                Ok(()) => {
                    responder.send().map_err(AddressStateProviderError::Fidl)?;
                    Ok(None)
                }
                Err(netstack3_core::error::SetIpAddressPropertiesError::NotFound(
                    netstack3_core::error::NotFoundError,
                )) => {
                    panic!("address not found")
                }
                Err(netstack3_core::error::SetIpAddressPropertiesError::NotManual) => {
                    panic!("address not manual")
                }
            }
        }
        fnet_interfaces_admin::AddressStateProviderRequest::WatchAddressAssignmentState {
            responder,
        } => {
            watch_state.on_new_watch_req(responder)?;
            Ok(None)
        }
        fnet_interfaces_admin::AddressStateProviderRequest::Detach { control_handle: _ } => {
            *detached = true;
            Ok(None)
        }
        fnet_interfaces_admin::AddressStateProviderRequest::Remove { control_handle: _ } => {
            Ok(Some(UserRemove))
        }
    }
}

fn send_address_removal_event(
    addr: IpAddr,
    id: BindingId,
    control_handle: fnet_interfaces_admin::AddressStateProviderControlHandle,
    reason: fnet_interfaces_admin::AddressRemovalReason,
) {
    control_handle.send_on_address_removed(reason).unwrap_or_else(|e| {
        tracing::error!(
            "failed to send address removal reason for addr {:?} on interface {}: {:?}",
            addr,
            id,
            e
        );
    })
}

mod enabled {
    use super::*;
    use crate::bindings::{DeviceSpecificInfo, DynamicCommonInfo, DynamicNetdeviceInfo};
    use futures::lock::Mutex as AsyncMutex;

    /// A helper that provides interface enabling and disabling under an
    /// interface-scoped lock.
    ///
    /// All interface enabling and disabling must go through this controller to
    /// guarantee that futures are not racing to act on the same status that is
    /// protected by locks owned by device structures in core.
    pub(super) struct InterfaceEnabledController {
        id: BindingId,
        ctx: AsyncMutex<Ctx>,
    }

    impl InterfaceEnabledController {
        pub(super) fn new(ctx: Ctx, id: BindingId) -> Self {
            Self { id, ctx: AsyncMutex::new(ctx) }
        }

        /// Sets this controller's interface to `admin_enabled = enabled`.
        ///
        /// Returns `true` if the value of `admin_enabled` changed in response
        /// to this call.
        pub(super) async fn set_admin_enabled(&self, enabled: bool) -> bool {
            let Self { id, ctx } = self;
            let mut ctx = ctx.lock().await;
            enum Info<A, B> {
                Loopback(A),
                Netdevice(B),
            }

            let core_id = ctx.non_sync_ctx().devices.get_core_id(*id).expect("device not present");
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
            Self::update_enabled_state(ctx.deref_mut(), *id);
            true
        }

        /// Sets this controller's interface to `phy_up = online`.
        pub(super) async fn set_phy_up(&self, online: bool) {
            let Self { id, ctx } = self;
            let mut ctx = ctx.lock().await;
            match ctx
                .non_sync_ctx()
                .devices
                .get_core_id(*id)
                .expect("device not present")
                .external_state()
            {
                devices::DeviceSpecificInfo::Netdevice(i) => {
                    i.with_dynamic_info_mut(|i| i.phy_up = online)
                }
                i @ devices::DeviceSpecificInfo::Loopback(_) => {
                    unreachable!("unexpected device info {:?} for interface {}", i, *id)
                }
            };
            // Enable or disable interface with context depending on new
            // online status. The helper functions take care of checking if
            // admin enable is the expected value.
            Self::update_enabled_state(&mut ctx, *id);
        }

        /// Commits interface enabled state to core.
        ///
        /// # Panics
        ///
        /// Panics if `id` is not a valid installed interface identifier.
        ///
        /// Panics if `should_enable` is `false` but the device state reflects
        /// that it should be enabled.
        fn update_enabled_state(ctx: &mut Ctx, id: BindingId) {
            let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
            let core_id = non_sync_ctx
                .devices
                .get_core_id(id)
                .expect("tried to enable/disable nonexisting device");

            let dev_enabled = match core_id.external_state() {
                DeviceSpecificInfo::Netdevice(i) => i.with_dynamic_info(
                    |DynamicNetdeviceInfo {
                         phy_up,
                         common_info:
                             DynamicCommonInfo {
                                 admin_enabled,
                                 mtu: _,
                                 events: _,
                                 control_hook: _,
                                 addresses: _,
                             },
                     }| *phy_up && *admin_enabled,
                ),
                DeviceSpecificInfo::Loopback(i) => i.with_dynamic_info(
                    |DynamicCommonInfo {
                         admin_enabled,
                         mtu: _,
                         events: _,
                         control_hook: _,
                         addresses: _,
                     }| { *admin_enabled },
                ),
            };

            let ip_config = Some(IpDeviceConfigurationUpdate {
                ip_enabled: Some(dev_enabled),
                ..Default::default()
            });

            // The update functions from core are already capable of identifying
            // deltas and return the previous values for us. Log the deltas for
            // info.
            let v4_was_enabled = netstack3_core::device::update_ipv4_configuration(
                sync_ctx,
                non_sync_ctx,
                &core_id,
                Ipv4DeviceConfigurationUpdate { ip_config, ..Default::default() },
            )
            .map(|update| {
                update
                    .ip_config
                    .expect("ip config must be informed")
                    .ip_enabled
                    .expect("ip enabled must be informed")
            })
            .expect("changing ip_enabled should never fail");
            let v6_was_enabled = netstack3_core::device::update_ipv6_configuration(
                sync_ctx,
                non_sync_ctx,
                &core_id,
                Ipv6DeviceConfigurationUpdate { ip_config, ..Default::default() },
            )
            .map(|update| {
                update
                    .ip_config
                    .expect("ip config must be informed")
                    .ip_enabled
                    .expect("ip enabled must be informed")
            })
            .expect("changing ip_enabled should never fail");
            tracing::info!("updated core ip_enabled state to {dev_enabled}, prev v4={v4_was_enabled},v6={v6_was_enabled}");
        }
    }
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
        crate::bindings::integration_tests::set_logger_for_test();
        let mut ctx: Ctx = Default::default();
        let (control_client_end, control_server_end) =
            fnet_interfaces_ext::admin::Control::create_endpoints().expect("create control proxy");
        let (control_sender, control_receiver) =
            OwnedControlHandle::new_channel_with_owned_handle(control_server_end).await;
        let (event_sender, event_receiver) = futures::channel::mpsc::unbounded();

        // Add the interface.
        let binding_id = {
            let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
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
                            tx_notifier: Default::default(),
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
            ctx.clone(),
            binding_id,
            stop_receiver,
            control_receiver,
            removable,
            futures::stream::pending(),
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
        let mut asp_event_stream = asp_client_end.take_event_stream();
        let event = asp_event_stream
            .try_next()
            .await
            .expect("read AddressStateProvider event")
            .expect("AddressStateProvider event stream unexpectedly empty");
        assert_matches!(event, fnet_interfaces_admin::AddressStateProviderEvent::OnAddressAdded {});

        // Drop the control handle and expect interface control to exit
        drop(control_client_end);
        interface_control_fut.await;

        // Expect that the event receiver has closed, and that it does not
        // contain, an `AddressRemoved` event, which would indicate the address
        // was explicitly removed.
        assert_matches!(event_receiver.next().await,
            Some(InterfaceEvent::Removed( id)) if id == binding_id);
        assert_matches!(event_receiver.next().await, None);

        // Verify the ASP closed for the correct reason.
        let event = asp_event_stream
            .try_next()
            .await
            .expect("read AddressStateProvider event")
            .expect("AddressStateProvider event stream unexpectedly empty");
        assert_matches!(
            event,
            fnet_interfaces_admin::AddressStateProviderEvent::OnAddressRemoved { error } => {
                assert_eq!(error, fnet_interfaces_admin::AddressRemovalReason::InterfaceRemoved)
            }
        );
        // Make sure the context lives all the way through the end of the test.
        std::mem::drop(ctx);
    }
}
