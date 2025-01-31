// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use anyhow::{anyhow, Context as _, Error};
use assert_matches::assert_matches;
use fidl::endpoints::create_endpoints;
use fidl_fuchsia_net::SocketAddress;
use fidl_fuchsia_posix_socket::{self as fposix_socket, MarkDomain, OptionalUint32};
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{
    Capability, ChildOptions, LocalComponentHandles, RealmBuilder, Ref, Route,
};
use futures::lock::Mutex;
use futures::{FutureExt as _, StreamExt as _, TryStreamExt as _};
use net_declare::{fidl_ip, fidl_socket_addr};
use pretty_assertions::assert_eq;
use socket_proxy_testing::{RegistryType, ToDnsServerList as _, ToNetwork as _};
use std::sync::Arc;
use test_case::test_case;
use {
    fidl_fuchsia_netpol_socketproxy as fnp_socketproxy, fidl_fuchsia_posix as fposix,
    fidl_fuchsia_posix_socket_raw as fposix_socket_raw,
};

enum IncomingService {
    Provider(fposix_socket::ProviderRequestStream),
    RawProvider(fposix_socket_raw::ProviderRequestStream),
}

trait SetMarkRespond {
    fn send(self, result: Result<(), fposix::Errno>) -> Result<(), fidl::Error>;
}

trait GetMarkRespond {
    fn send(self, result: Result<&OptionalUint32, fposix::Errno>) -> Result<(), fidl::Error>;
}

trait SocketRequestExt {
    type SetMarkResponder: SetMarkRespond;
    type GetMarkResponder: GetMarkRespond;

    fn get_request(self) -> GenericSocketRequest<Self::SetMarkResponder, Self::GetMarkResponder>;
}

enum GenericSocketRequest<SetMarkResponder, GetMarkResponder> {
    SetMark { domain: MarkDomain, mark: OptionalUint32, responder: SetMarkResponder },
    GetMark { domain: MarkDomain, responder: GetMarkResponder },
    Other,
}

macro_rules! impl_socket_request_ext {
    ($($request:path => ($set_mark:path, $get_mark:path)),*) => {
        $(
            impl SetMarkRespond for $set_mark {
                fn send(self, result: Result<(), fposix::Errno>) -> Result<(), fidl::Error> {
                    <$set_mark>::send(self, result)
                }
            }

            impl GetMarkRespond for $get_mark {
                fn send(
                    self,
                    result: Result<&OptionalUint32, fposix::Errno>,
                ) -> Result<(), fidl::Error> {
                    <$get_mark>::send(self, result)
                }
            }

            impl SocketRequestExt for $request {
                type SetMarkResponder = $set_mark;
                type GetMarkResponder = $get_mark;

                fn get_request(
                    self,
                ) -> GenericSocketRequest<Self::SetMarkResponder, Self::GetMarkResponder> {
                    use $request::*;
                    match self {
                        SetMark { domain, mark, responder } => {
                            GenericSocketRequest::SetMark { domain, mark, responder }
                        }
                        GetMark { domain, responder } => {
                            GenericSocketRequest::GetMark { domain, responder }
                        }
                        _ => GenericSocketRequest::Other,
                    }
                }
            }
        )*
    };
    ($($request:path => ($set_mark:path, $get_mark:path)),*,) => {
        impl_socket_request_ext!($($request => ($set_mark, $get_mark)),*);
    };
}

impl_socket_request_ext! {
    fposix_socket::StreamSocketRequest => (
        fposix_socket::StreamSocketSetMarkResponder,
        fposix_socket::StreamSocketGetMarkResponder
    ),
    fposix_socket::SynchronousDatagramSocketRequest => (
        fposix_socket::SynchronousDatagramSocketSetMarkResponder,
        fposix_socket::SynchronousDatagramSocketGetMarkResponder
    ),
    fposix_socket::DatagramSocketRequest => (
        fposix_socket::DatagramSocketSetMarkResponder,
        fposix_socket::DatagramSocketGetMarkResponder
    ),
    fposix_socket_raw::SocketRequest => (
        fposix_socket_raw::SocketSetMarkResponder,
        fposix_socket_raw::SocketGetMarkResponder
    ),
}

async fn run_stream_socket<Stream, Request>(
    stream: Stream,
    mark_1: Arc<Mutex<OptionalUint32>>,
    mark_2: Arc<Mutex<OptionalUint32>>,
) -> Result<(), Error>
where
    Stream: futures::Stream<Item = Result<Request, fidl::Error>>,
    Request: SocketRequestExt,
{
    stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| {
            let mark_1 = mark_1.clone();
            let mark_2 = mark_2.clone();
            async move {
                match request.get_request() {
                    GenericSocketRequest::SetMark { domain, mark, responder } => {
                        responder.send(match domain {
                            MarkDomain::Mark1 => {
                                *mark_1.lock().await = mark;
                                Ok(())
                            }
                            MarkDomain::Mark2 => {
                                *mark_2.lock().await = mark;
                                Ok(())
                            }
                        })
                    }
                    GenericSocketRequest::GetMark { domain, responder } => {
                        let lock_1 = *mark_1.lock().await;
                        let lock_2 = *mark_2.lock().await;
                        responder.send(match domain {
                            MarkDomain::Mark1 => Ok(&lock_1),
                            MarkDomain::Mark2 => Ok(&lock_2),
                        })
                    }
                    GenericSocketRequest::Other => {
                        unimplemented!("This method is unimplemented in this test")
                    }
                }
                .context("while responding")
            }
        })
        .await
}

async fn inner_provider_mock(
    handles: LocalComponentHandles,
    marks: Arc<Mutex<Vec<(Arc<Mutex<OptionalUint32>>, Arc<Mutex<OptionalUint32>>)>>>,
) -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    let _ = fs
        .dir("svc")
        .add_fidl_service(IncomingService::Provider)
        .add_fidl_service(IncomingService::RawProvider);
    let _ = fs.serve_connection(handles.outgoing_dir)?;

    fs.for_each_concurrent(0, |service| {
        let marks = marks.clone();
        async move {
            match service {
                IncomingService::Provider(stream) => stream
                    .map(|result| result.context("Result came with error"))
                    .try_for_each(|request| {
                        let marks = marks.clone();
                        async move {
                            match request {
                                fposix_socket::ProviderRequest::StreamSocket {
                                    domain: _,
                                    proto: _,
                                    responder,
                                } => {
                                    let mark_1 = Arc::new(Mutex::new(OptionalUint32::Unset(
                                        fposix_socket::Empty,
                                    )));
                                    let mark_2 = Arc::new(Mutex::new(OptionalUint32::Unset(
                                        fposix_socket::Empty,
                                    )));
                                    marks.lock().await.push((mark_1.clone(), mark_2.clone()));
                                    let (client, server) =
                                        create_endpoints::<fposix_socket::StreamSocketMarker>();
                                    responder
                                        .send(Ok(client))
                                        .expect("could not respond to StreamSocket call");
                                    run_stream_socket(server.into_stream(), mark_1, mark_2).await?;
                                }
                                fposix_socket::ProviderRequest::DatagramSocketDeprecated {
                                    domain: _,
                                    proto: _,
                                    responder,
                                } => {
                                    let mark_1 = Arc::new(Mutex::new(OptionalUint32::Unset(
                                        fposix_socket::Empty,
                                    )));
                                    let mark_2 = Arc::new(Mutex::new(OptionalUint32::Unset(
                                        fposix_socket::Empty,
                                    )));
                                    marks.lock().await.push((mark_1.clone(), mark_2.clone()));
                                    let (client, server) = create_endpoints::<
                                        fposix_socket::SynchronousDatagramSocketMarker,
                                    >();
                                    responder
                                        .send(Ok(client))
                                        .expect("could not respond to StreamSocket call");
                                    run_stream_socket(server.into_stream(), mark_1, mark_2).await?;
                                }
                                fposix_socket::ProviderRequest::DatagramSocket {
                                    domain: _,
                                    proto,
                                    responder,
                                } => {
                                    use fposix_socket::ProviderDatagramSocketResponse::*;
                                    let mark_1 = Arc::new(Mutex::new(OptionalUint32::Unset(
                                        fposix_socket::Empty,
                                    )));
                                    let mark_2 = Arc::new(Mutex::new(OptionalUint32::Unset(
                                        fposix_socket::Empty,
                                    )));
                                    marks.lock().await.push((mark_1.clone(), mark_2.clone()));
                                    match proto {
                                        fposix_socket::DatagramSocketProtocol::Udp => {
                                            let (client, server) = create_endpoints::<
                                                fposix_socket::DatagramSocketMarker,
                                            >(
                                            );
                                            responder
                                                .send(Ok(DatagramSocket(client)))
                                                .expect("could not respond to StreamSocket call");
                                            run_stream_socket(server.into_stream(), mark_1, mark_2)
                                                .await?;
                                        }
                                        fposix_socket::DatagramSocketProtocol::IcmpEcho => {
                                            let (client, server) = create_endpoints::<
                                                fposix_socket::SynchronousDatagramSocketMarker,
                                            >(
                                            );
                                            responder
                                                .send(Ok(SynchronousDatagramSocket(client)))
                                                .expect("could not respond to StreamSocket call");
                                            run_stream_socket(server.into_stream(), mark_1, mark_2)
                                                .await?;
                                        }
                                    }
                                }
                                fposix_socket::ProviderRequest::StreamSocketWithOptions {
                                    domain: _,
                                    proto: _,
                                    opts,
                                    responder,
                                } => {
                                    let mut mark_1 = OptionalUint32::Unset(fposix_socket::Empty);
                                    let mut mark_2 = OptionalUint32::Unset(fposix_socket::Empty);
                                    for fposix_socket::Marks { domain, mark } in opts.marks.unwrap()
                                    {
                                        match domain {
                                            fposix_socket::MarkDomain::Mark1 => {
                                                mark_1 = mark;
                                            }
                                            fposix_socket::MarkDomain::Mark2 => {
                                                mark_2 = mark;
                                            }
                                        }
                                    }
                                    marks.lock().await.push((
                                        Arc::new(Mutex::new(mark_1)),
                                        Arc::new(Mutex::new(mark_2)),
                                    ));
                                    let (client, server) =
                                        create_endpoints::<fposix_socket::StreamSocketMarker>();
                                    responder.send(Ok(client)).expect(
                                        "could not respond to StreamSocketWithOptions call",
                                    );
                                    run_stream_socket(
                                        server.into_stream(),
                                        Arc::new(Mutex::new(mark_1)),
                                        Arc::new(Mutex::new(mark_2)),
                                    )
                                    .await?;
                                }
                                fposix_socket::ProviderRequest::DatagramSocketWithOptions {
                                    domain: _,
                                    proto,
                                    opts,
                                    responder,
                                } => {
                                    use fposix_socket::ProviderDatagramSocketWithOptionsResponse::*;
                                    let mut mark_1 = OptionalUint32::Unset(fposix_socket::Empty);
                                    let mut mark_2 = OptionalUint32::Unset(fposix_socket::Empty);
                                    for fposix_socket::Marks { domain, mark } in opts.marks.unwrap()
                                    {
                                        match domain {
                                            fposix_socket::MarkDomain::Mark1 => {
                                                mark_1 = mark;
                                            }
                                            fposix_socket::MarkDomain::Mark2 => {
                                                mark_2 = mark;
                                            }
                                        }
                                    }
                                    marks.lock().await.push((
                                        Arc::new(Mutex::new(mark_1)),
                                        Arc::new(Mutex::new(mark_2)),
                                    ));
                                    match proto {
                                        fposix_socket::DatagramSocketProtocol::Udp => {
                                            let (client, server) = create_endpoints::<
                                                fposix_socket::DatagramSocketMarker,
                                            >(
                                            );
                                            responder
                                                .send(Ok(DatagramSocket(client)))
                                                .expect("could not respond to DatagramSocketWithOptions call");
                                            run_stream_socket(
                                                server.into_stream(),
                                                Arc::new(Mutex::new(mark_1)),
                                                Arc::new(Mutex::new(mark_2)),
                                            )
                                            .await?;
                                        }
                                        fposix_socket::DatagramSocketProtocol::IcmpEcho => {
                                            let (client, server) = create_endpoints::<
                                                fposix_socket::SynchronousDatagramSocketMarker,
                                            >(
                                            );
                                            responder
                                                .send(Ok(SynchronousDatagramSocket(client)))
                                                .expect("could not respond to DatagramSocketWithOptions call");
                                            run_stream_socket(
                                                server.into_stream(),
                                                Arc::new(Mutex::new(mark_1)),
                                                Arc::new(Mutex::new(mark_2)),
                                            )
                                            .await?;
                                        }
                                    }
                                }
                                _ => unimplemented!("this method is not used in this test"),
                            }
                            Ok(())
                        }
                    })
                    .await
                    .context("Failed to serve request stream")
                    .unwrap_or_else(|e| eprintln!("Error encountered: {e:?}")),
                IncomingService::RawProvider(stream) => stream
                    .map(|result| result.context("Result came with error"))
                    .try_for_each(|request| {
                        let marks = marks.clone();
                        async move {
                            match request {
                                fposix_socket_raw::ProviderRequest::Socket {
                                    domain: _,
                                    proto: _,
                                    responder,
                                } => {
                                    let mark_1 = Arc::new(Mutex::new(OptionalUint32::Unset(
                                        fposix_socket::Empty,
                                    )));
                                    let mark_2 = Arc::new(Mutex::new(OptionalUint32::Unset(
                                        fposix_socket::Empty,
                                    )));
                                    marks.lock().await.push((mark_1.clone(), mark_2.clone()));
                                    let (client, server) =
                                        create_endpoints::<fposix_socket_raw::SocketMarker>();
                                    responder
                                        .send(Ok(client))
                                        .expect("could not respond to StreamSocket call");
                                    run_stream_socket(server.into_stream(), mark_1, mark_2).await?;
                                }
                                fposix_socket_raw::ProviderRequest::SocketWithOptions {
                                    domain: _,
                                    proto: _,
                                    opts,
                                    responder,
                                } => {
                                    let mut mark_1 = OptionalUint32::Unset(fposix_socket::Empty);
                                    let mut mark_2 = OptionalUint32::Unset(fposix_socket::Empty);
                                    for fposix_socket::Marks { domain, mark } in opts.marks.unwrap()
                                    {
                                        match domain {
                                            fposix_socket::MarkDomain::Mark1 => {
                                                mark_1 = mark;
                                            }
                                            fposix_socket::MarkDomain::Mark2 => {
                                                mark_2 = mark;
                                            }
                                        }
                                    }
                                    marks.lock().await.push((
                                        Arc::new(Mutex::new(mark_1)),
                                        Arc::new(Mutex::new(mark_2)),
                                    ));
                                    let (client, server) =
                                        create_endpoints::<fposix_socket_raw::SocketMarker>();
                                    responder
                                        .send(Ok(client))
                                        .expect("could not respond to SocketWithOptions call");
                                    run_stream_socket(
                                        server.into_stream(),
                                        Arc::new(Mutex::new(mark_1)),
                                        Arc::new(Mutex::new(mark_2)),
                                    )
                                    .await?;
                                }
                            }
                            Ok(())
                        }
                    })
                    .await
                    .context("Failed to serve request stream")
                    .unwrap_or_else(|e| eprintln!("Error encountered: {e:?}")),
            }
        }
    })
    .await;

    Ok(())
}

fn create_starnix_network(id: u32, mark: u32) -> fnp_socketproxy::Network {
    fnp_socketproxy::Network {
        network_id: Some(id),
        info: Some(fnp_socketproxy::NetworkInfo::Starnix(fnp_socketproxy::StarnixNetworkInfo {
            mark: Some(mark),
            ..Default::default()
        })),
        dns_servers: Some(fnp_socketproxy::NetworkDnsServers { ..Default::default() }),
        ..Default::default()
    }
}

fn create_fuchsia_network(id: u32) -> fnp_socketproxy::Network {
    fnp_socketproxy::Network {
        network_id: Some(id),
        info: Some(fnp_socketproxy::NetworkInfo::Fuchsia(fnp_socketproxy::FuchsiaNetworkInfo {
            ..Default::default()
        })),
        dns_servers: Some(fnp_socketproxy::NetworkDnsServers { ..Default::default() }),
        ..Default::default()
    }
}

#[test_case(false, OptionalUint32::Value(0); "default unset")]
#[test_case(true, OptionalUint32::Value(123); "default set")]
#[fuchsia::test]
/// Test making every possible type of socket and check that the socket mark is
// set as expected. Starnix and Fuchsia registries have the same handling
// logic, so use the Starnix registry to confirm this behavior.
async fn integration(should_set_default: bool, expected_mark: OptionalUint32) -> Result<(), Error> {
    let marks = Arc::new(Mutex::new(Vec::new()));
    let builder = RealmBuilder::new().await?;
    let inner_provider = builder
        .add_local_child(
            "inner_provider",
            {
                let marks = marks.clone();
                move |handles: LocalComponentHandles| {
                    Box::pin(inner_provider_mock(handles, marks.clone()))
                }
            },
            ChildOptions::new(),
        )
        .await?;
    let socket_proxy = builder
        .add_child("socket_proxy", "#meta/network-socket-proxy.cm", ChildOptions::new().eager())
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fposix_socket::ProviderMarker>())
                .capability(Capability::protocol::<fposix_socket_raw::ProviderMarker>())
                .from(&inner_provider)
                .to(&socket_proxy),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fposix_socket::ProviderMarker>())
                .capability(Capability::protocol::<fposix_socket_raw::ProviderMarker>())
                .capability(Capability::protocol::<fnp_socketproxy::StarnixNetworksMarker>())
                .from(&socket_proxy)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;

    let posix_socket = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fposix_socket::ProviderMarker>()
        .context("While connecting to provider")?;

    let posix_socket_raw = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fposix_socket_raw::ProviderMarker>()
        .context("While connecting to raw provider")?;

    {
        let socket = posix_socket
            .stream_socket(fposix_socket::Domain::Ipv4, fposix_socket::StreamSocketProtocol::Tcp)
            .await?
            .map_err(|e| anyhow!("Could not get socket: {e:?}"))?
            .into_proxy();

        // With no registered networks, the mark should be unset.
        assert_eq!(
            socket.get_mark(MarkDomain::Mark1).await?,
            Ok(OptionalUint32::Unset(fposix_socket::Empty))
        );
        let locked_marks = marks.lock().await;
        assert_eq!(locked_marks.len(), 1);
        let first_mark = locked_marks[0].0.lock().await;
        assert_eq!(*first_mark, OptionalUint32::Unset(fposix_socket::Empty));
    }

    let starnix_networks = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::StarnixNetworksMarker>()
        .context("while connecting to StarnixNetworks")?;

    starnix_networks
        .add(&create_starnix_network(1 /* id */, 123 /* mark */))
        .await?
        .map_err(|e| anyhow!("Could not add network: {e:?}"))?;

    if should_set_default {
        // Setting the default network alters the expected mark below to be the
        // mark from the default network instead of `0`
        starnix_networks
            .set_default(&fposix_socket::OptionalUint32::Value(1))
            .await?
            .map_err(|e| anyhow!("Could not set default network: {e:?}"))?;
    }

    {
        let socket = posix_socket
            .stream_socket(fposix_socket::Domain::Ipv4, fposix_socket::StreamSocketProtocol::Tcp)
            .await?
            .map_err(|e| anyhow!("Could not get socket: {e:?}"))?
            .into_proxy();

        // With a registered network, the mark should be set to the expected mark.
        assert_eq!(socket.get_mark(MarkDomain::Mark1).await?, Ok(expected_mark));
        let locked_marks = marks.lock().await;
        assert_eq!(locked_marks.len(), 2);
        let first_mark = locked_marks[1].0.lock().await;
        assert_eq!(*first_mark, expected_mark)
    }

    {
        let socket = posix_socket
            .datagram_socket_deprecated(
                fposix_socket::Domain::Ipv4,
                fposix_socket::DatagramSocketProtocol::Udp,
            )
            .await?
            .map_err(|e| anyhow!("Could not get socket: {e:?}"))?
            .into_proxy();

        // With a registered network, the mark should be set to the expected mark.
        assert_eq!(socket.get_mark(MarkDomain::Mark1).await?, Ok(expected_mark));
        let locked_marks = marks.lock().await;
        assert_eq!(locked_marks.len(), 3);
        let first_mark = locked_marks[2].0.lock().await;
        assert_eq!(*first_mark, expected_mark)
    }

    {
        let response = posix_socket
            .datagram_socket(
                fposix_socket::Domain::Ipv4,
                fposix_socket::DatagramSocketProtocol::Udp,
            )
            .await?
            .map_err(|e| anyhow!("Could not get socket: {e:?}"))?;

        let socket = if let fposix_socket::ProviderDatagramSocketResponse::DatagramSocket(socket) =
            response
        {
            socket
        } else {
            panic!("Expected DatagramSocket response");
        }
        .into_proxy();

        // With a registered network, the mark should be set to the expected mark.
        assert_eq!(socket.get_mark(MarkDomain::Mark1).await?, Ok(expected_mark));
        let locked_marks = marks.lock().await;
        assert_eq!(locked_marks.len(), 4);
        let first_mark = locked_marks[3].0.lock().await;
        assert_eq!(*first_mark, expected_mark)
    }

    {
        let response = posix_socket
            .datagram_socket(
                fposix_socket::Domain::Ipv4,
                fposix_socket::DatagramSocketProtocol::IcmpEcho,
            )
            .await?
            .map_err(|e| anyhow!("Could not get socket: {e:?}"))?;

        let socket =
            if let fposix_socket::ProviderDatagramSocketResponse::SynchronousDatagramSocket(
                socket,
            ) = response
            {
                socket
            } else {
                panic!("Expected SynchronousDatagramSocket response");
            }
            .into_proxy();

        // With a registered network, the mark should be set to the expected mark.
        assert_eq!(socket.get_mark(MarkDomain::Mark1).await?, Ok(expected_mark));
        let locked_marks = marks.lock().await;
        assert_eq!(locked_marks.len(), 5);
        let first_mark = locked_marks[4].0.lock().await;
        assert_eq!(*first_mark, expected_mark)
    }

    {
        let socket = posix_socket_raw
            .socket(
                fposix_socket::Domain::Ipv4,
                &fposix_socket_raw::ProtocolAssociation::Unassociated(fposix_socket_raw::Empty),
            )
            .await?
            .map_err(|e| anyhow!("Could not get socket: {e:?}"))?
            .into_proxy();

        // With a registered network, the mark should be set to the expected mark.
        assert_eq!(socket.get_mark(MarkDomain::Mark1).await?, Ok(expected_mark));
        let locked_marks = marks.lock().await;
        assert_eq!(locked_marks.len(), 6);
        let first_mark = locked_marks[5].0.lock().await;
        assert_eq!(*first_mark, expected_mark)
    }

    // When the network is set as default, it must be unset as default prior to
    // removing the network from the registry.
    if should_set_default {
        starnix_networks
            .set_default(&fposix_socket::OptionalUint32::Unset(fposix_socket::Empty))
            .await?
            .map_err(|e| anyhow!("Could not unset default network: {e:?}"))?;
    }
    starnix_networks.remove(1).await?.map_err(|e| anyhow!("Could not remove network: {e:?}"))?;

    {
        let socket = posix_socket
            .stream_socket(fposix_socket::Domain::Ipv4, fposix_socket::StreamSocketProtocol::Tcp)
            .await?
            .map_err(|e| anyhow!("Could not get socket: {e:?}"))?
            .into_proxy();

        // With no registered networks, the mark should be unset
        assert_eq!(
            socket.get_mark(MarkDomain::Mark1).await?,
            Ok(OptionalUint32::Unset(fposix_socket::Empty))
        );
        let locked_marks = marks.lock().await;
        assert_eq!(locked_marks.len(), 7);
        let first_mark = locked_marks[6].0.lock().await;
        assert_eq!(*first_mark, OptionalUint32::Unset(fposix_socket::Empty));
    }

    Ok(())
}

#[fuchsia::test]
async fn integration_across_registries() -> Result<(), Error> {
    const STARNIX_NETWORK_ID: u32 = 1;
    const STARNIX_NETWORK_MARK: u32 = 123;
    const FUCHSIA_NETWORK_ID: u32 = 2;

    let marks = Arc::new(Mutex::new(Vec::new()));
    let builder = RealmBuilder::new().await?;
    let inner_provider = builder
        .add_local_child(
            "inner_provider",
            {
                let marks = marks.clone();
                move |handles: LocalComponentHandles| {
                    Box::pin(inner_provider_mock(handles, marks.clone()))
                }
            },
            ChildOptions::new(),
        )
        .await?;
    let socket_proxy = builder
        .add_child("socket_proxy", "#meta/network-socket-proxy.cm", ChildOptions::new().eager())
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fposix_socket::ProviderMarker>())
                .from(&inner_provider)
                .to(&socket_proxy),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fposix_socket::ProviderMarker>())
                .capability(Capability::protocol::<fnp_socketproxy::StarnixNetworksMarker>())
                .capability(Capability::protocol::<fnp_socketproxy::FuchsiaNetworksMarker>())
                .from(&socket_proxy)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;

    let posix_socket = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fposix_socket::ProviderMarker>()
        .context("While connecting to provider")?;

    {
        let socket = posix_socket
            .stream_socket(fposix_socket::Domain::Ipv4, fposix_socket::StreamSocketProtocol::Tcp)
            .await?
            .map_err(|e| anyhow!("Could not get socket: {e:?}"))?
            .into_proxy();

        // With no registered networks, the mark should be unset
        assert_eq!(
            socket.get_mark(MarkDomain::Mark1).await?,
            Ok(OptionalUint32::Unset(fposix_socket::Empty))
        );
        let locked_marks = marks.lock().await;
        assert_eq!(locked_marks.len(), 1);
        let first_mark = locked_marks[0].0.lock().await;
        assert_eq!(*first_mark, OptionalUint32::Unset(fposix_socket::Empty));
    }

    let starnix_networks = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::StarnixNetworksMarker>()
        .context("while connecting to StarnixNetworks")?;
    let fuchsia_networks = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::FuchsiaNetworksMarker>()
        .context("while connecting to FuchsiaNetworks")?;

    // Add a network to the Starnix and Fuchsia registries.
    starnix_networks
        .add(&create_starnix_network(STARNIX_NETWORK_ID, STARNIX_NETWORK_MARK))
        .await?
        .map_err(|e| anyhow!("Could not add network: {e:?}"))?;
    fuchsia_networks
        .add(&create_fuchsia_network(FUCHSIA_NETWORK_ID))
        .await?
        .map_err(|e| anyhow!("Could not add network: {e:?}"))?;

    // Set the Starnix network as default in the Starnix registry to use the
    // Starnix default network's mark.
    starnix_networks
        .set_default(&OptionalUint32::Value(STARNIX_NETWORK_ID))
        .await?
        .map_err(|e| anyhow!("Could not set default network: {e:?}"))?;

    {
        let socket = posix_socket
            .stream_socket(fposix_socket::Domain::Ipv4, fposix_socket::StreamSocketProtocol::Tcp)
            .await?
            .map_err(|e| anyhow!("Could not get socket: {e:?}"))?
            .into_proxy();

        // With a Starnix default network and no Fuchsia default network, the
        // mark should be set to the Starnix default network's mark.
        assert_eq!(
            socket.get_mark(MarkDomain::Mark1).await?,
            Ok(OptionalUint32::Value(STARNIX_NETWORK_MARK))
        );
        let locked_marks = marks.lock().await;
        assert_eq!(locked_marks.len(), 2);
        let first_mark = locked_marks[1].0.lock().await;
        assert_eq!(*first_mark, OptionalUint32::Value(STARNIX_NETWORK_MARK))
    }

    // Set the Fuchsia network as default in the Fuchsia registry to use the
    // Fuchsia default network's mark since the Fuchsia default network
    // is preferred.
    fuchsia_networks
        .set_default(&fposix_socket::OptionalUint32::Value(FUCHSIA_NETWORK_ID))
        .await?
        .map_err(|e| anyhow!("Could not set default network: {e:?}"))?;

    {
        let socket = posix_socket
            .stream_socket(fposix_socket::Domain::Ipv4, fposix_socket::StreamSocketProtocol::Tcp)
            .await?
            .map_err(|e| anyhow!("Could not get socket: {e:?}"))?
            .into_proxy();

        // With a Fuchsia default network, the mark should be set to the
        // Fuchsia default network's mark.
        assert_eq!(
            socket.get_mark(MarkDomain::Mark1).await?,
            Ok(OptionalUint32::Unset(fposix_socket::Empty))
        );
        let locked_marks = marks.lock().await;
        assert_eq!(locked_marks.len(), 3);
        let first_mark = locked_marks[2].0.lock().await;
        assert_eq!(*first_mark, OptionalUint32::Unset(fposix_socket::Empty))
    }

    // When the Fuchsia network is unset, the mark should fallback to the
    // Starnix default network's mark.
    fuchsia_networks
        .set_default(&OptionalUint32::Unset(fposix_socket::Empty))
        .await?
        .map_err(|e| anyhow!("Could not unset default network: {e:?}"))?;

    {
        let socket = posix_socket
            .stream_socket(fposix_socket::Domain::Ipv4, fposix_socket::StreamSocketProtocol::Tcp)
            .await?
            .map_err(|e| anyhow!("Could not get socket: {e:?}"))?
            .into_proxy();

        // The mark should reflect the Starnix default network's mark.
        assert_eq!(
            socket.get_mark(MarkDomain::Mark1).await?,
            Ok(OptionalUint32::Value(STARNIX_NETWORK_MARK))
        );
        let locked_marks = marks.lock().await;
        assert_eq!(locked_marks.len(), 4);
        let first_mark = locked_marks[3].0.lock().await;
        assert_eq!(*first_mark, OptionalUint32::Value(STARNIX_NETWORK_MARK));
    }

    Ok(())
}

#[fuchsia::test]
async fn test_socket_proxy_no_double_connect() -> Result<(), Error> {
    let builder = RealmBuilder::new().await?;
    let socket_proxy = builder
        .add_child("socket_proxy", "#meta/network-socket-proxy.cm", ChildOptions::new().eager())
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fnp_socketproxy::StarnixNetworksMarker>())
                .from(&socket_proxy)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;

    // Make two simultaneous connections to StarnixNetworksMarker
    let starnix_networks = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::StarnixNetworksMarker>()
        .context("While connecting to StarnixNetworks 1")?;
    // The first connection should work fine
    assert_eq!(
        starnix_networks.remove(1).await?,
        Err(fnp_socketproxy::NetworkRegistryRemoveError::NotFound)
    );

    let starnix_networks2 = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::StarnixNetworksMarker>()
        .context("While connecting to StarnixNetworks 2")?;
    // The second connection should fail
    assert_matches!(
        starnix_networks2.remove(1).await,
        Err(fidl::Error::ClientChannelClosed { status: fidl::Status::ACCESS_DENIED, .. })
    );

    Ok(())
}

#[fuchsia::test]
async fn test_dns_server_watcher_no_double_connect() -> Result<(), Error> {
    let builder = RealmBuilder::new().await?;
    let socket_proxy = builder
        .add_child("socket_proxy", "#meta/network-socket-proxy.cm", ChildOptions::new().eager())
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fnp_socketproxy::DnsServerWatcherMarker>())
                .from(&socket_proxy)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;
    // Make two simultaneous connections to DnsServerWatcherMarker
    let dns_server_watcher = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::DnsServerWatcherMarker>()
        .context("While connecting to DnsServerWatcher 1")?;
    // The first connection should work fine
    assert_eq!(dns_server_watcher.watch_servers().await?, vec![]);

    let dns_server_watcher2 = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::DnsServerWatcherMarker>()
        .context("While connecting to DnsServerWatcher 2")?;
    // The second connection should fail
    assert_matches!(
        dns_server_watcher2.watch_servers().await,
        Err(fidl::Error::ClientChannelClosed { status: fidl::Status::ACCESS_DENIED, .. })
    );

    Ok(())
}

// Query the DNS servers from the DnsServerWatcher until the
// list of servers matches the expected value.
async fn wait_on_dns_server_list_response(
    proxy: &fnp_socketproxy::DnsServerWatcherProxy,
    expected: Vec<(u32, Vec<SocketAddress>)>,
) -> Result<(), Error> {
    let expected = expected.into_iter().map(|info| info.to_dns_server_list()).collect::<Vec<_>>();
    loop {
        let mut incoming = proxy.watch_servers().await?;
        incoming.sort_by_key(|a| a.source_network_id);
        if incoming == expected {
            break Ok(());
        }
    }
}

#[fuchsia::test]
async fn watch_dns_with_registry() -> Result<(), Error> {
    let builder = RealmBuilder::new().await?;
    let socket_proxy = builder
        .add_child("socket_proxy", "#meta/network-socket-proxy.cm", ChildOptions::new().eager())
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fnp_socketproxy::StarnixNetworksMarker>())
                .capability(Capability::protocol::<fnp_socketproxy::DnsServerWatcherMarker>())
                .from(&socket_proxy)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;

    let dns_watcher = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::DnsServerWatcherMarker>()
        .context("trying to connect to DNS Server watcher")?;

    let starnix_networks = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::StarnixNetworksMarker>()
        .context("trying to connect to StarnixNetworks")?;

    // Add network 1 with 1 v4 address
    assert_eq!(
        starnix_networks
            .add(&(1, vec![fidl_ip!["192.0.2.0"]]).to_network(RegistryType::Starnix))
            .await?,
        Ok(())
    );
    wait_on_dns_server_list_response(
        &dns_watcher,
        vec![(1, vec![fidl_socket_addr!("192.0.2.0:53")])],
    )
    .await?;

    // Add network 2 with 1 v6 address
    assert_eq!(
        starnix_networks
            .add(&(2, vec![fidl_ip!["2001:db8::3"]]).to_network(RegistryType::Starnix))
            .await?,
        Ok(())
    );
    wait_on_dns_server_list_response(
        &dns_watcher,
        vec![
            (1, vec![fidl_socket_addr!("192.0.2.0:53")]),
            (2, vec![fidl_socket_addr!("[2001:db8::3]:53")]),
        ],
    )
    .await?;

    // Update network 1 so that it has 1 v4 address and 1 v6 address.
    assert_eq!(
        starnix_networks
            .update(
                &(1, vec![fidl_ip!["192.0.2.1"], fidl_ip!["2001:db8::4"]])
                    .to_network(RegistryType::Starnix)
            )
            .await?,
        Ok(())
    );
    wait_on_dns_server_list_response(
        &dns_watcher,
        vec![
            (1, vec![fidl_socket_addr!("192.0.2.1:53"), fidl_socket_addr!("[2001:db8::4]:53")]),
            (2, vec![fidl_socket_addr!("[2001:db8::3]:53")]),
        ],
    )
    .await?;

    // Remove network 2.
    assert_eq!(starnix_networks.remove(2).await?, Ok(()));
    wait_on_dns_server_list_response(
        &dns_watcher,
        vec![(1, vec![fidl_socket_addr!("192.0.2.1:53"), fidl_socket_addr!("[2001:db8::4]:53")])],
    )
    .await?;

    // Update network 1 with the same information. Should not cause any DNS updates
    assert_eq!(
        starnix_networks
            .update(
                &(1, vec![fidl_ip!["192.0.2.1"], fidl_ip!["2001:db8::4"]])
                    .to_network(RegistryType::Starnix)
            )
            .await?,
        Ok(())
    );
    assert!(dns_watcher.watch_servers().now_or_never().is_none());

    Ok(())
}

#[fuchsia::test]
async fn watch_dns_across_registries() -> Result<(), Error> {
    let builder = RealmBuilder::new().await?;
    let socket_proxy = builder
        .add_child("socket_proxy", "#meta/network-socket-proxy.cm", ChildOptions::new().eager())
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fnp_socketproxy::StarnixNetworksMarker>())
                .capability(Capability::protocol::<fnp_socketproxy::FuchsiaNetworksMarker>())
                .capability(Capability::protocol::<fnp_socketproxy::DnsServerWatcherMarker>())
                .from(&socket_proxy)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;

    let dns_watcher = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::DnsServerWatcherMarker>()
        .context("trying to connect to DNS Server watcher")?;

    let starnix_networks = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::StarnixNetworksMarker>()
        .context("trying to connect to StarnixNetworks")?;

    let fuchsia_networks = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::FuchsiaNetworksMarker>()
        .context("trying to connect to FuchsiaNetworks")?;

    // Add a network with 1 v4 address to the Starnix registry.
    assert_eq!(
        starnix_networks
            .add(&(1, vec![fidl_ip!["192.0.2.0"]]).to_network(RegistryType::Starnix))
            .await?,
        Ok(())
    );

    // Add two networks with one address to the Fuchsia registry. There will
    // be no DNS update due to the Starnix DNS still being used: Fuchsia's
    // default network is unset.
    assert_eq!(
        fuchsia_networks
            .add(&(2, vec![fidl_ip!["192.0.2.1"]]).to_network(RegistryType::Fuchsia))
            .await?,
        Ok(())
    );
    assert_eq!(
        fuchsia_networks
            .add(&(3, vec![fidl_ip!["2001:db8::1"]]).to_network(RegistryType::Fuchsia))
            .await?,
        Ok(())
    );

    // This DNS update should only reflect the Starnix network's DNS.
    wait_on_dns_server_list_response(
        &dns_watcher,
        vec![(1, vec![fidl_socket_addr!("192.0.2.0:53")])],
    )
    .await?;

    // Set Fuchsia's default network.
    fuchsia_networks
        .set_default(&fposix_socket::OptionalUint32::Value(3))
        .await?
        .map_err(|e| anyhow!("Could not set default network: {e:?}"))?;

    // This DNS update should reflect all DNS servers from Fuchsia networks
    // since Fuchsia now has a default network set.
    wait_on_dns_server_list_response(
        &dns_watcher,
        vec![
            (2, vec![fidl_socket_addr!("192.0.2.1:53")]),
            (3, vec![fidl_socket_addr!("[2001:db8::1]:53")]),
        ],
    )
    .await?;

    // Update the Starnix network with 1 v4 and 1 v6 address. This should
    // result in no update from the DNS server watcher because there is a
    // Fuchsia default network set.
    assert_eq!(
        starnix_networks
            .update(
                &(1, vec![fidl_ip!["192.0.2.0"], fidl_ip!["2001:db8::4"]])
                    .to_network(RegistryType::Starnix)
            )
            .await?,
        Ok(())
    );

    // After unsetting the Fuchsia default network, the next update is expected
    // to reflect DNS servers from the Starnix networks.
    fuchsia_networks
        .set_default(&fposix_socket::OptionalUint32::Unset(fposix_socket::Empty))
        .await?
        .map_err(|e| anyhow!("Could not unset default network: {e:?}"))?;
    wait_on_dns_server_list_response(
        &dns_watcher,
        vec![(1, vec![fidl_socket_addr!("192.0.2.0:53"), fidl_socket_addr!("[2001:db8::4]:53")])],
    )
    .await?;

    // Remove a network from the Fuchsia registry. This should result in no
    // DNS update due to there not being a Fuchsia default network set.
    assert_eq!(fuchsia_networks.remove(2).await?, Ok(()));
    assert!(dns_watcher.watch_servers().now_or_never().is_none());

    Ok(())
}
