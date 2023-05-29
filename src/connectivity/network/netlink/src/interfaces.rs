// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing RTM_LINK and RTM_ADDR information by generating
//! RTM_LINK and RTM_ADDR Netlink messages based on events received from
//! Netstack's interface watcher.

use std::collections::{HashMap, HashSet};

use anyhow::anyhow;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_interfaces_ext::{self as fnet_interfaces_ext, Update as _};
use futures::{pin_mut, StreamExt as _, TryStreamExt as _};
use tracing::debug;

use crate::NETLINK_LOG_TAG;

/// Contains the asynchronous work related to RTM_LINK and RTM_ADDR messages.
///
/// Connects to the interfaces watcher and can respond to RTM_LINK and RTM_ADDR
/// message requests.
pub(crate) struct EventLoop {
    /// A `StateProxy` to connect to the interfaces watcher.
    state_proxy: fnet_interfaces::StateProxy,
}

/// RTM_LINK and RTM_ADDR related event loop errors.
#[derive(Debug)]
pub(crate) enum InterfacesEventLoopError {
    /// Errors at the FIDL layer.
    ///
    /// Such as: cannot connect to protocol or watcher, loaded FIDL error
    /// from stream.
    Fidl(FidlError),
    /// Errors at the Netstack layer.
    ///
    /// Such as: interface watcher event stream ended, or a struct from
    /// Netstack failed conversion.
    Netstack(NetstackError),
}

#[derive(Debug)]
pub(crate) enum FidlError {
    /// Error in the FIDL event stream.
    EventStream(fidl::Error),
    /// Error that could not cleanly be wrapped with an error type.
    Unspecified(anyhow::Error),
    /// Error in getting event stream from state.
    WatcherCreation(fnet_interfaces_ext::WatcherCreationError),
}

#[derive(Debug)]
pub(crate) enum NetstackError {
    /// Event stream ended unexpectedly.
    EventStreamEnded,
    /// An existing event was received while handling interface events.
    ExistingEventReceived(fnet_interfaces_ext::Properties),
    /// Error that could not cleanly be wrapped with an error type.
    Unspecified(anyhow::Error),
    /// Inconsistent state between Netstack and interface properties.
    Update(fnet_interfaces_ext::UpdateError),
}

impl EventLoop {
    /// `new` returns a `Result<EventLoop, InterfacesEventLoopError>` instance.
    /// This is fallible iff it is not possible to obtain the `StateProxy`.
    pub(crate) fn new() -> Result<Self, InterfacesEventLoopError> {
        use fuchsia_component::client::connect_to_protocol;
        let state_proxy = connect_to_protocol::<fnet_interfaces::StateMarker>()
            .map_err(|e| InterfacesEventLoopError::Fidl(FidlError::Unspecified(e)))?;

        Ok(EventLoop { state_proxy })
    }

    /// Run the asynchronous work related to RTM_LINK and RTM_ADDR messages.
    ///
    /// The event loop can track interface properties, and is never
    /// expected to complete.
    /// Returns: `InterfacesEventLoopError` that requires restarting the
    /// event loop task, for example, if the watcher stream ends or if
    /// the FIDL protocol cannot be connected.
    pub(crate) async fn run(self) -> InterfacesEventLoopError {
        let EventLoop { state_proxy } = self;
        let if_event_stream = {
            let stream_res = fnet_interfaces_ext::event_stream_from_state(&state_proxy);

            match stream_res {
                Ok(stream) => stream.fuse(),

                Err(e) => {
                    return InterfacesEventLoopError::Fidl(FidlError::WatcherCreation(e));
                }
            }
        };

        pin_mut!(if_event_stream);

        let interfaces_res =
            fnet_interfaces_ext::existing(if_event_stream.by_ref(), HashMap::new()).await;
        let mut interface_properties = match interfaces_res {
            Ok(interfaces) => interfaces,
            Err(e) => {
                return InterfacesEventLoopError::Netstack(NetstackError::Unspecified(anyhow!(
                    "could not fetch existing interface events: {:?}",
                    e
                )));
            }
        };

        loop {
            let stream_res = if_event_stream.try_next().await;
            let event = match stream_res {
                Ok(Some(event)) => event,
                Ok(None) => {
                    return InterfacesEventLoopError::Netstack(NetstackError::EventStreamEnded);
                }
                Err(e) => {
                    return InterfacesEventLoopError::Fidl(FidlError::EventStream(e));
                }
            };

            match handle_interface_watcher_event(&mut interface_properties, event) {
                Ok(()) => {}
                Err(InterfaceEventHandlerError::ExistingEventReceived(properties)) => {
                    // This error indicates there is an inconsistent interface state shared
                    // between Netlink and Netstack.
                    return InterfacesEventLoopError::Netstack(
                        NetstackError::ExistingEventReceived(properties),
                    );
                }
                Err(InterfaceEventHandlerError::Update(e)) => {
                    // This error is severe enough to indicate a larger problem in Netstack.
                    return InterfacesEventLoopError::Netstack(NetstackError::Update(e));
                }
            }
        }
    }
}

// Errors related to handling interface events.
#[derive(Debug)]
enum InterfaceEventHandlerError {
    // Interface event handler updated the HashMap with an event, but received an
    // unexpected response.
    Update(fnet_interfaces_ext::UpdateError),
    // Interface event handler attempted to process an event for an interface that already existed.
    ExistingEventReceived(fnet_interfaces_ext::Properties),
}

/// Handles events observed from the interface watcher by updating interfaces
/// from the underlying interface properties HashMap.
///
/// Returns an `InterfaceEventLoopError` when unexpected events occur, or an
/// `UpdateError` when updates are not consistent with the current state.
fn handle_interface_watcher_event(
    interface_properties: &mut HashMap<u64, fnet_interfaces_ext::Properties>,
    event: fnet_interfaces::Event,
) -> Result<(), InterfaceEventHandlerError> {
    let update = match interface_properties.update(event) {
        Ok(update) => update,
        Err(e) => return Err(InterfaceEventHandlerError::Update(e.into())),
    };

    match update {
        fnet_interfaces_ext::UpdateResult::Added(properties) => {
            // TODO(issuetracker.google.com/283826506): Send update to RTMGRP_LINK group
            // TODO(issuetracker.google.com/283826112): Send update to RTMGRP_IPV{4,6}_ADDR group
            debug!(tag = NETLINK_LOG_TAG, "processed add/existing event for id {}", properties.id);
        }
        fnet_interfaces_ext::UpdateResult::Changed {
            previous:
                fnet_interfaces::Properties {
                    online,
                    addresses,
                    id: _,
                    name: _,
                    device_class: _,
                    has_default_ipv4_route: _,
                    has_default_ipv6_route: _,
                    ..
                },
            current:
                fnet_interfaces_ext::Properties {
                    addresses: current_addresses,
                    id,
                    name: _,
                    device_class: _,
                    online: _,
                    has_default_ipv4_route: _,
                    has_default_ipv6_route: _,
                },
        } => {
            if online.is_some() {
                // TODO: Send update to RTMGRP_LINK group
                debug!(
                    tag = NETLINK_LOG_TAG,
                    "processed interface link change event for id {}", id
                );
            };
            if addresses.map_or(false, |addresses| {
                let previous = addresses
                    .iter()
                    .filter_map(|fnet_interfaces::Address { addr, .. }| addr.as_ref())
                    .collect::<HashSet<_>>();
                let current = current_addresses
                    .iter()
                    .map(|fnet_interfaces_ext::Address { addr, valid_until: _ }| addr)
                    .collect::<HashSet<_>>();
                previous.ne(&current)
            }) {
                // TODO: Send update to RTM_GRP_IPV{4,6}_ADDR group
                debug!(
                    tag = NETLINK_LOG_TAG,
                    "processed interface address change event for id {}", id
                );
            };
        }
        fnet_interfaces_ext::UpdateResult::Removed(properties) => {
            // TODO: Send deletion to RTMGRP_LINK group
            // TODO: Send deletion to RTMGRP_IPV{4,6}_ADDR group
            debug!(
                tag = NETLINK_LOG_TAG,
                "processed interface remove event for id {}", properties.id
            );
        }
        fnet_interfaces_ext::UpdateResult::Existing(properties) => {
            return Err(InterfaceEventHandlerError::ExistingEventReceived(properties.clone()));
        }
        fnet_interfaces_ext::UpdateResult::NoChange => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fidl as _;
    use fidl_fuchsia_hardware_network;
    use fuchsia_async::{self as fasync};
    use std::num::NonZeroU64;
    const ETHERNET: fnet_interfaces::DeviceClass =
        fnet_interfaces::DeviceClass::Device(fidl_fuchsia_hardware_network::DeviceClass::Ethernet);
    const LOOPBACK: fnet_interfaces::DeviceClass =
        fnet_interfaces::DeviceClass::Loopback(fnet_interfaces::Empty {});

    fn create_no_address_interface(
        id: u64,
        name: String,
        device_class: fnet_interfaces::DeviceClass,
        online: bool,
    ) -> fnet_interfaces_ext::Properties {
        fnet_interfaces_ext::Properties {
            id: NonZeroU64::new(id).unwrap(),
            name,
            device_class,
            online,
            addresses: vec![],
            has_default_ipv4_route: false,
            has_default_ipv6_route: false,
        }
    }

    #[fuchsia::test]
    fn test_handle_interface_watcher_event() {
        let mut interface_properties: HashMap<u64, fnet_interfaces_ext::Properties> =
            HashMap::new();

        let mut interface1 = create_no_address_interface(1, "test".into(), ETHERNET, true);
        let interface2 = create_no_address_interface(2, "lo".into(), LOOPBACK, true);

        let interface1_add_event = fnet_interfaces::Event::Added(interface1.clone().into());
        assert_matches!(
            handle_interface_watcher_event(&mut interface_properties, interface1_add_event),
            Ok(())
        );
        assert_eq!(interface_properties.get(&1).unwrap(), &interface1);

        // Sending an updated interface properties with a different field
        // should update the properties stored under the same interface id.
        interface1.online = false;
        let interface1_change_event = fnet_interfaces::Event::Changed(interface1.clone().into());
        assert_matches!(
            handle_interface_watcher_event(&mut interface_properties, interface1_change_event),
            Ok(())
        );
        assert_eq!(interface_properties.get(&1).unwrap(), &interface1);

        let interface2_add_event = fnet_interfaces::Event::Added(interface2.clone().into());
        assert_matches!(
            handle_interface_watcher_event(&mut interface_properties, interface2_add_event),
            Ok(())
        );
        assert_eq!(interface_properties.get(&1).unwrap(), &interface1);
        assert_eq!(interface_properties.get(&2).unwrap(), &interface2);

        // A remove event should result in no longer seeing the LinkMessage in the
        // interface properties HashMap.
        let interface1_remove_event = fnet_interfaces::Event::Removed(1);
        assert_matches!(
            handle_interface_watcher_event(&mut interface_properties, interface1_remove_event),
            Ok(())
        );
        assert_eq!(interface_properties.get(&1), None);
        assert_eq!(interface_properties.get(&2).unwrap(), &interface2);
    }

    fn get_fake_interface(
        id: u64,
        name: &'static str,
        device_class: fnet_interfaces::DeviceClass,
    ) -> fnet_interfaces_ext::Properties {
        fnet_interfaces_ext::Properties {
            id: id.try_into().unwrap(),
            name: name.to_string(),
            device_class,
            online: true,
            addresses: Vec::new(),
            has_default_ipv4_route: false,
            has_default_ipv6_route: false,
        }
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_event_loop_errors_stream_ended() {
        let (state_proxy, if_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_interfaces::StateMarker>().unwrap();
        let event_loop = EventLoop { state_proxy };
        let event_loop_fut = event_loop.run();

        let watcher_stream = if_stream
            .and_then(|req| match req {
                fnet_interfaces::StateRequest::GetWatcher {
                    options: _,
                    watcher,
                    control_handle: _,
                } => futures::future::ready(watcher.into_stream()),
            })
            .try_flatten()
            .map(|res| res.expect("watcher stream error"));

        let interfaces: Vec<_> = vec![get_fake_interface(1, "lo", LOOPBACK)];

        let interfaces =
            futures::stream::iter(interfaces.into_iter().map(Some).chain(std::iter::once(None)));
        let watcher_fut = watcher_stream.zip(interfaces).for_each(|(req, properties)| match req {
            fnet_interfaces::WatcherRequest::Watch { responder } => {
                let event = properties.map_or(
                    fnet_interfaces::Event::Idle(fnet_interfaces::Empty),
                    |fnet_interfaces_ext::Properties {
                         id,
                         name,
                         device_class,
                         online,
                         addresses,
                         has_default_ipv4_route,
                         has_default_ipv6_route,
                     }| {
                        fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                            id: Some(id.get()),
                            name: Some(name),
                            device_class: Some(device_class),
                            online: Some(online),
                            addresses: Some(
                                addresses
                                    .into_iter()
                                    .map(|fnet_interfaces_ext::Address { addr, valid_until }| {
                                        fnet_interfaces::Address {
                                            addr: Some(addr),
                                            valid_until: Some(valid_until),
                                            ..Default::default()
                                        }
                                    })
                                    .collect(),
                            ),
                            has_default_ipv4_route: Some(has_default_ipv4_route),
                            has_default_ipv6_route: Some(has_default_ipv6_route),
                            ..Default::default()
                        })
                    },
                );
                let () = responder.send(&event).expect("send watcher event");
                futures::future::ready(())
            }
        });

        let (err, ()) = futures::join!(event_loop_fut, watcher_fut);
        assert_matches!(
            err,
            InterfacesEventLoopError::Fidl(FidlError::EventStream(
                fidl::Error::ClientChannelClosed { .. }
            ))
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_event_loop_errors_duplicate_events() {
        let (state_proxy, if_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_interfaces::StateMarker>().unwrap();
        let event_loop = EventLoop { state_proxy };
        let event_loop_fut = event_loop.run();

        let watcher_stream = if_stream
            .and_then(|req| match req {
                fnet_interfaces::StateRequest::GetWatcher {
                    options: _,
                    watcher,
                    control_handle: _,
                } => futures::future::ready(watcher.into_stream()),
            })
            .try_flatten()
            .map(|res| res.expect("watcher stream error"));

        let interfaces: Vec<_> =
            vec![get_fake_interface(1, "lo", LOOPBACK), get_fake_interface(1, "lo", LOOPBACK)];

        let interfaces =
            futures::stream::iter(interfaces.into_iter().map(Some).chain(std::iter::once(None)));
        let watcher_fut = watcher_stream.zip(interfaces).for_each(|(req, properties)| match req {
            fnet_interfaces::WatcherRequest::Watch { responder } => {
                let event = properties.map_or(
                    fnet_interfaces::Event::Idle(fnet_interfaces::Empty),
                    |fnet_interfaces_ext::Properties {
                         id,
                         name,
                         device_class,
                         online,
                         addresses,
                         has_default_ipv4_route,
                         has_default_ipv6_route,
                     }| {
                        fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                            id: Some(id.get()),
                            name: Some(name),
                            device_class: Some(device_class),
                            online: Some(online),
                            addresses: Some(
                                addresses
                                    .into_iter()
                                    .map(|fnet_interfaces_ext::Address { addr, valid_until }| {
                                        fnet_interfaces::Address {
                                            addr: Some(addr),
                                            valid_until: Some(valid_until),
                                            ..Default::default()
                                        }
                                    })
                                    .collect(),
                            ),
                            has_default_ipv4_route: Some(has_default_ipv4_route),
                            has_default_ipv6_route: Some(has_default_ipv6_route),
                            ..Default::default()
                        })
                    },
                );
                let () = responder.send(&event).expect("send watcher event");
                futures::future::ready(())
            }
        });

        let (err, ()) = futures::join!(event_loop_fut, watcher_fut);
        assert_matches!(err, InterfacesEventLoopError::Netstack(NetstackError::Unspecified(_)));
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_event_loop_errors_duplicate_adds() {
        let (state_proxy, if_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_interfaces::StateMarker>().unwrap();
        let event_loop = EventLoop { state_proxy };
        let event_loop_fut = event_loop.run();

        let watcher_stream = if_stream
            .and_then(|req| match req {
                fnet_interfaces::StateRequest::GetWatcher {
                    options: _,
                    watcher,
                    control_handle: _,
                } => futures::future::ready(watcher.into_stream()),
            })
            .try_flatten()
            .map(|res| res.expect("watcher stream error"));

        let interfaces_existing: Vec<_> = vec![get_fake_interface(1, "lo", LOOPBACK)];
        let interfaces_new: Vec<_> = vec![get_fake_interface(1, "lo", LOOPBACK)];

        let interfaces = futures::stream::iter(
            interfaces_existing
                .into_iter()
                .map(Some)
                .chain(std::iter::once(None))
                .chain(interfaces_new.into_iter().map(Some)),
        );
        let mut req_num = 0;
        let watcher_fut = watcher_stream.zip(interfaces).for_each(|(req, properties)| match req {
            fnet_interfaces::WatcherRequest::Watch { responder } => {
                let event = properties.map_or(
                    fnet_interfaces::Event::Idle(fnet_interfaces::Empty),
                    |fnet_interfaces_ext::Properties {
                         id,
                         name,
                         device_class,
                         online,
                         addresses,
                         has_default_ipv4_route,
                         has_default_ipv6_route,
                     }| {
                        let inner = fnet_interfaces::Properties {
                            id: Some(id.get()),
                            name: Some(name),
                            device_class: Some(device_class),
                            online: Some(online),
                            addresses: Some(
                                addresses
                                    .into_iter()
                                    .map(|fnet_interfaces_ext::Address { addr, valid_until }| {
                                        fnet_interfaces::Address {
                                            addr: Some(addr),
                                            valid_until: Some(valid_until),
                                            ..Default::default()
                                        }
                                    })
                                    .collect(),
                            ),
                            has_default_ipv4_route: Some(has_default_ipv4_route),
                            has_default_ipv6_route: Some(has_default_ipv6_route),
                            ..Default::default()
                        };

                        // The first request will add the interface in the call to `existing` and
                        // the second will attempt to add the interface via the
                        // `handle_interface_watcher_event` call.
                        req_num += 1;
                        if req_num == 1 {
                            fnet_interfaces::Event::Existing(inner)
                        } else {
                            fnet_interfaces::Event::Added(inner)
                        }
                    },
                );
                let () = responder.send(&event).expect("send watcher event");
                futures::future::ready(())
            }
        });

        let (err, ()) = futures::join!(event_loop_fut, watcher_fut);
        // The properties that are being added again has interface id 1.
        assert_matches!(err,
            InterfacesEventLoopError::Netstack(
                NetstackError::Update(
                    fnet_interfaces_ext::UpdateError::DuplicateAdded(properties)
                    )
                )
            if properties.id.unwrap() == 1);
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_event_loop_errors_existing_after_add() {
        let (state_proxy, if_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_interfaces::StateMarker>().unwrap();
        let event_loop = EventLoop { state_proxy };
        let event_loop_fut = event_loop.run();

        let watcher_stream = if_stream
            .and_then(|req| match req {
                fnet_interfaces::StateRequest::GetWatcher {
                    options: _,
                    watcher,
                    control_handle: _,
                } => futures::future::ready(watcher.into_stream()),
            })
            .try_flatten()
            .map(|res| res.expect("watcher stream error"));

        let interfaces_existing: Vec<_> =
            vec![get_fake_interface(1, "lo", LOOPBACK), get_fake_interface(2, "eth001", ETHERNET)];
        let interfaces_new: Vec<_> = vec![get_fake_interface(3, "eth002", ETHERNET)];

        let interfaces = futures::stream::iter(
            interfaces_existing
                .into_iter()
                .map(Some)
                .chain(std::iter::once(None))
                .chain(interfaces_new.into_iter().map(Some)),
        );
        let watcher_fut = watcher_stream.zip(interfaces).for_each(|(req, properties)| match req {
            fnet_interfaces::WatcherRequest::Watch { responder } => {
                let event = properties.map_or(
                    fnet_interfaces::Event::Idle(fnet_interfaces::Empty),
                    |fnet_interfaces_ext::Properties {
                         id,
                         name,
                         device_class,
                         online,
                         addresses,
                         has_default_ipv4_route,
                         has_default_ipv6_route,
                     }| {
                        let inner = fnet_interfaces::Properties {
                            id: Some(id.get()),
                            name: Some(name),
                            device_class: Some(device_class),
                            online: Some(online),
                            addresses: Some(
                                addresses
                                    .into_iter()
                                    .map(|fnet_interfaces_ext::Address { addr, valid_until }| {
                                        fnet_interfaces::Address {
                                            addr: Some(addr),
                                            valid_until: Some(valid_until),
                                            ..Default::default()
                                        }
                                    })
                                    .collect(),
                            ),
                            has_default_ipv4_route: Some(has_default_ipv4_route),
                            has_default_ipv6_route: Some(has_default_ipv6_route),
                            ..Default::default()
                        };
                        fnet_interfaces::Event::Existing(inner)
                    },
                );
                let () = responder.send(&event).expect("send watcher event");
                futures::future::ready(())
            }
        });

        let (err, ()) = futures::join!(event_loop_fut, watcher_fut);
        // The second existing properties has interface id 3.
        assert_matches!(
            err,
            InterfacesEventLoopError::Netstack(
                NetstackError::ExistingEventReceived(properties)
            ) if properties.id.get() == 3
        );
    }
}
