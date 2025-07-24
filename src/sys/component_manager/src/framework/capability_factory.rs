// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::WeakComponentInstance;
use crate::sandbox_util::take_handle_as_stream;
use anyhow::{format_err, Error};
use async_trait::async_trait;
use cm_types::{Name, RelativePath};
use cm_util::WeakTaskGroup;
use fidl::endpoints::{create_endpoints, ClientEnd, ProtocolMarker, Proxy, ServerEnd};
use fuchsia_sync::Mutex;
use futures::channel::oneshot;
use futures::future::BoxFuture;
use futures::stream::StreamExt;
use futures::FutureExt;
use moniker::Moniker;
use router_error::RouterError;
use sandbox::{
    Capability, CapabilityBound, Connectable, Connector, Data, Dict, DirConnectable, DirConnector,
    Message, Request, Routable, Router, RouterResponse, WeakInstanceToken,
};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;
use zx::sys::ZX_CHANNEL_MAX_MSG_BYTES;
use zx::AsHandleRef;
use {fidl_fuchsia_component_runtime as fruntime, fidl_fuchsia_io as fio};

/// These two constants are needed in computing how many strings we can fit into a FIDL message.
/// There's unfortunately no better solution than doing the math ourselves.
const FIDL_VECTOR_OVERHEAD: usize = 48;
const FIDL_STRING_OVERHEAD: usize = 16;

/// The set of runtime capabilities which can be accessed outside of component manager, along with
/// the KOIDs associated with each capability.
///
/// The design of `fuchsia.component.runtime.CapabilityFactory` is rather asynchronous, and in many
/// cases it's possible that we want to look up a capability by KOID before we've stored the
/// capability's KOID in this hash map. To handle this synchronization issue, when we attempt to
/// look up a capability by KOID if it is not present we instead store a oneshot sender under the
/// KOID. Then when the capability is discovered, it is sent over the oneshot instead of kept in
/// this hash map.
pub type RemotedRuntimeCapabilities = HashMap<zx::Koid, CapabilityOrWaiter>;

/// Holds either a `Capability` to represent a capability stored in the remoted runtime
/// capabilities hash map, or a `oneshot::Sender<Capability>` to represent a task waiting on the
/// discovery of the capability.
#[derive(Debug)]
pub enum CapabilityOrWaiter {
    Capability(Capability),
    Waiter(oneshot::Sender<Capability>),
}

pub fn serve(
    channel: zx::Channel,
    // This argument only exists so we can match the function signature of
    // `crate::framework::add_protocol`.
    _weak_target_component: WeakComponentInstance,
    weak_source_component: WeakComponentInstance,
) -> BoxFuture<'static, Result<(), Error>> {
    async move {
        let source_component = weak_source_component.upgrade()?;
        let remote_capabilities = source_component.context.remote_capabilities().clone();
        let weak_task_group = source_component.nonblocking_task_group().as_weak();
        let stream = take_handle_as_stream::<fruntime::CapabilityFactoryMarker>(channel);
        let moniker = source_component.moniker.clone();
        CapabilityFactory { remote_capabilities, weak_task_group, moniker }
            .handle_stream(stream)
            .await
    }
    .boxed()
}

#[derive(Clone)]
pub struct CapabilityFactory {
    pub remote_capabilities: Arc<Mutex<RemotedRuntimeCapabilities>>,
    pub weak_task_group: WeakTaskGroup,
    pub moniker: Moniker,
}

impl CapabilityFactory {
    pub async fn handle_stream(
        self,
        mut stream: fruntime::CapabilityFactoryRequestStream,
    ) -> Result<(), Error> {
        while let Some(Ok(request)) = stream.next().await {
            match request {
                fruntime::CapabilityFactoryRequest::CreateConnector {
                    receiver_client_end,
                    connector_server_end,
                    ..
                } => {
                    let connector = Connector::new_sendable(RemoteReceiver {
                        remote_receiver: receiver_client_end.into_proxy(),
                    });
                    self.weak_task_group
                        .spawn(self.clone().serve_connector(connector, connector_server_end));
                }
                fruntime::CapabilityFactoryRequest::CreateDirConnector {
                    dir_receiver_client_end,
                    dir_connector_server_end,
                    ..
                } => {
                    let dir_connector = DirConnector::new_sendable(RemoteDirReceiver {
                        remote_receiver: dir_receiver_client_end.into_proxy(),
                    });
                    self.weak_task_group.spawn(
                        self.clone().serve_dir_connector(dir_connector, dir_connector_server_end),
                    );
                }
                fruntime::CapabilityFactoryRequest::CreateDictionary {
                    dictionary_server_end,
                    ..
                } => {
                    let dictionary = Dict::new();
                    self.weak_task_group
                        .spawn(self.clone().serve_dictionary(dictionary, dictionary_server_end));
                }
                fruntime::CapabilityFactoryRequest::CreateConnectorRouter {
                    router_client_end,
                    router_server_end,
                    ..
                } => {
                    let router = Router::new(RemoteRouter {
                        router_proxy: router_client_end.into_proxy(),
                        factory: self.clone(),
                    });
                    self.weak_task_group
                        .spawn(self.clone().serve_connector_router(router, router_server_end));
                }
                fruntime::CapabilityFactoryRequest::CreateDirConnectorRouter {
                    router_client_end,
                    router_server_end,
                    ..
                } => {
                    let router = Router::new(RemoteRouter {
                        router_proxy: router_client_end.into_proxy(),
                        factory: self.clone(),
                    });
                    self.weak_task_group
                        .spawn(self.clone().serve_dir_connector_router(router, router_server_end));
                }
                fruntime::CapabilityFactoryRequest::CreateDictionaryRouter {
                    router_client_end,
                    router_server_end,
                    ..
                } => {
                    let router = Router::new(RemoteRouter {
                        router_proxy: router_client_end.into_proxy(),
                        factory: self.clone(),
                    });
                    self.weak_task_group
                        .spawn(self.clone().serve_dictionary_router(router, router_server_end));
                }
                fruntime::CapabilityFactoryRequest::CreateDataRouter {
                    router_client_end,
                    router_server_end,
                    ..
                } => {
                    let router = Router::new(RemoteRouter {
                        router_proxy: router_client_end.into_proxy(),
                        factory: self.clone(),
                    });
                    self.weak_task_group
                        .spawn(self.clone().serve_data_router(router, router_server_end));
                }
                unknown_request => return Err(format_err!("unable to process unrecognized request to fuchsia.component.runtime.CapabilityFactory: {unknown_request:?}")),
            }
        }
        Ok(())
    }

    async fn store_instance_token(&self, token: WeakInstanceToken) -> fruntime::WeakInstanceToken {
        let (our_instance_token, their_instance_token) = zx::EventPair::create();
        self.store_remote(&our_instance_token, token).await;
        self.weak_task_group.spawn(async move {
            // Move our end of the event pair into a new task that stays alive until the other end
            // of the event pair is closed. We use `expect` here because we've created the event
            // pair and thus surely have the rights to do this.
            fuchsia_async::OnSignals::new(our_instance_token, zx::Signals::OBJECT_PEER_CLOSED)
                .await
                .expect("failed to wait on event pair");
        });
        fruntime::WeakInstanceToken { token: their_instance_token }
    }

    async fn retrieve_instance_token(
        &self,
        token: &fruntime::WeakInstanceToken,
    ) -> Option<WeakInstanceToken> {
        let fruntime::WeakInstanceToken { token } = token;
        let koid = token.basic_info().ok()?.koid;
        match self.remote_capabilities.lock().remove(&koid) {
            Some(CapabilityOrWaiter::Capability(Capability::Instance(token))) => Some(token),
            _ => None,
        }
    }

    async fn store_remote(&self, handle: impl AsHandleRef, capability: impl Into<Capability>) {
        let koid = handle.basic_info().expect("failed to get basic info on handle").koid;
        match self.remote_capabilities.lock().entry(koid) {
            Entry::Occupied(occupied_entry) => match occupied_entry.remove() {
                CapabilityOrWaiter::Capability(prior_capability) => panic!(
                    "duplicate koids found, first one was assigned to capability \
                           {prior_capability:?}"
                ),
                CapabilityOrWaiter::Waiter(sender) => {
                    let _ = sender.send(capability.into());
                }
            },
            Entry::Vacant(vacant_entry) => {
                let _ = vacant_entry.insert(CapabilityOrWaiter::Capability(capability.into()));
            }
        }
    }

    async fn remove_remote(&self, koid: zx::Koid) {
        let _ = self.remote_capabilities.lock().remove(&koid);
    }

    async fn retrieve_remote(&self, capability: fruntime::Capability) -> Option<Capability> {
        let koid = match capability {
            fruntime::Capability::Connector(connector_client_end) => {
                let connector_proxy = connector_client_end.into_proxy();
                connector_proxy.as_channel().basic_info().ok()?.related_koid
            }
            fruntime::Capability::DirConnector(dir_connector_client_end) => {
                let dir_connector_proxy = dir_connector_client_end.into_proxy();
                dir_connector_proxy.as_channel().basic_info().ok()?.related_koid
            }
            fruntime::Capability::Dictionary(dictionary_client_end) => {
                let dictionary_proxy = dictionary_client_end.into_proxy();
                dictionary_proxy.as_channel().basic_info().ok()?.related_koid
            }
            fruntime::Capability::ConnectorRouter(router_client_end) => {
                let router_proxy = router_client_end.into_proxy();
                router_proxy.as_channel().basic_info().ok()?.related_koid
            }
            fruntime::Capability::DirConnectorRouter(router_client_end) => {
                let router_proxy = router_client_end.into_proxy();
                router_proxy.as_channel().basic_info().ok()?.related_koid
            }
            fruntime::Capability::DictionaryRouter(router_client_end) => {
                let router_proxy = router_client_end.into_proxy();
                router_proxy.as_channel().basic_info().ok()?.related_koid
            }
            fruntime::Capability::DataRouter(router_client_end) => {
                let router_proxy = router_client_end.into_proxy();
                router_proxy.as_channel().basic_info().ok()?.related_koid
            }
            fruntime::Capability::Data(data) => match data {
                fruntime::Data::Bytes(bytes) => return Some(Data::Bytes(bytes).into()),
                fruntime::Data::String(string) => return Some(Data::String(string).into()),
                fruntime::Data::Int64(num) => return Some(Data::Int64(num).into()),
                fruntime::Data::Uint64(num) => return Some(Data::Uint64(num).into()),
                _ => return None,
            },
            _ => return None,
        };
        let receiver = match self.remote_capabilities.lock().entry(koid) {
            Entry::Occupied(occupied_entry) => match occupied_entry.remove() {
                CapabilityOrWaiter::Capability(capability) => return Some(capability),
                CapabilityOrWaiter::Waiter(_sender) => {
                    panic!("someone else is trying to remove this capability")
                }
            },
            Entry::Vacant(vacant_entry) => {
                let (sender, receiver) = oneshot::channel();
                vacant_entry.insert(CapabilityOrWaiter::Waiter(sender));
                receiver
            }
        };
        receiver.await.ok()
    }

    fn serve_connector(
        self,
        connector: Connector,
        connector_server_end: ServerEnd<fruntime::ConnectorMarker>,
    ) -> BoxFuture<'static, ()> {
        async move {
            self.store_remote(&connector_server_end, connector.clone()).await;
            let koid =
                connector_server_end.basic_info().expect("failed to get basic info on handle").koid;
            let mut stream = connector_server_end.into_stream();
            while let Some(Ok(request)) = stream.next().await {
                match request {
                    fruntime::ConnectorRequest::Connect { channel, .. } => {
                        let _ = connector.send(Message { channel });
                    }
                    fruntime::ConnectorRequest::Clone { request, .. } => {
                        self.weak_task_group.spawn(self.clone().serve_connector(
                            connector.clone(),
                            ServerEnd::new(request.into_channel()),
                        ));
                    }
                    _ => return,
                }
            }
            self.remove_remote(koid).await;
        }
        .boxed()
    }

    fn serve_dir_connector(
        self,
        dir_connector: DirConnector,
        dir_connector_server_end: ServerEnd<fruntime::DirConnectorMarker>,
    ) -> BoxFuture<'static, ()> {
        async move {
            self.store_remote(&dir_connector_server_end, dir_connector.clone()).await;
            let koid = dir_connector_server_end
                .basic_info()
                .expect("failed to get basic info on handle")
                .koid;
            let mut stream = dir_connector_server_end.into_stream();
            while let Some(Ok(request)) = stream.next().await {
                match request {
                    fruntime::DirConnectorRequest::Connect { channel, .. } => {
                        let _ = dir_connector.send(channel, RelativePath::dot(), None);
                    }
                    fruntime::DirConnectorRequest::Clone { request, .. } => {
                        self.weak_task_group.spawn(self.clone().serve_dir_connector(
                            dir_connector.clone(),
                            ServerEnd::new(request.into_channel()),
                        ));
                    }
                    _ => return,
                }
            }
            self.remove_remote(koid).await;
        }
        .boxed()
    }

    fn serve_dictionary(
        self,
        dictionary: Dict,
        dictionary_server_end: ServerEnd<fruntime::DictionaryMarker>,
    ) -> BoxFuture<'static, ()> {
        async move {
            self.store_remote(&dictionary_server_end, dictionary.clone()).await;
            let koid = dictionary_server_end.basic_info().expect("failed to get basic info on handle").koid;
            let mut stream = dictionary_server_end.into_stream();
            while let Some(Ok(request)) = stream.next().await {
                match request {
                    fruntime::DictionaryRequest::Clone { request, .. } => {
                        self.weak_task_group.spawn(self.clone().serve_dictionary(
                            dictionary.clone(),
                            ServerEnd::new(request.into_channel()),
                        ));
                    }
                    fruntime::DictionaryRequest::Insert { key, capability, .. } => {
                        let Some(capability) = self.retrieve_remote(capability).await else {
                            log::warn!("unknown capability type received by fuchsia.component.runtime.Dictionary/Insert");
                            return;
                        };
                        let Ok(key) = Name::new(key) else {
                            log::warn!("invalid name received by fuchsia.component.runtime.Dictionary/Insert");
                            return;
                        };
                        // Inserts still succeed even when they return an error.
                        let _ = dictionary.insert(key, capability);
                    }
                    fruntime::DictionaryRequest::Get { key, responder, .. } => {
                        let Ok(key) = Name::new(key) else {
                            log::warn!("invalid name received by fuchsia.component.runtime.Dictionary/Get");
                            return;
                        };
                        let Ok(maybe_capability) = dictionary.get(&key) else {
                            log::warn!("unable to retrieve capability for fuchsia.component.runtime.Dictionary/Get because it's not cloneable");
                            return;
                        };
                        let maybe_remote_capability = match maybe_capability {
                            Some(capability) => Some(self.to_remote_capability(capability).await),
                            None => None,
                        };
                        let _ = responder.send(maybe_remote_capability);
                    }
                    fruntime::DictionaryRequest::Remove { key, responder, .. } => {
                        let Ok(key) = Name::new(key) else {
                            log::warn!("invalid name received by fuchsia.component.runtime.Dictionary/Remove");
                            return;
                        };
                        let maybe_capability = dictionary.remove(&key);
                        let maybe_remote_capability = match maybe_capability {
                            Some(capability) => Some(self.to_remote_capability(capability).await),
                            None => None,
                        };
                        let _ = responder.send(maybe_remote_capability);
                    }
                    fruntime::DictionaryRequest::IterateKeys { key_iterator, .. } => {
                        let iterator_stream = key_iterator.into_stream();
                        self.weak_task_group.spawn(
                            self.clone()
                                .handle_key_iterator_stream(dictionary.clone(), iterator_stream),
                        );
                    }
                    fruntime::DictionaryRequest::LegacyExport { responder, .. } => {
                        let dictionary_ref: fidl_fuchsia_component_sandbox::DictionaryRef =
                            dictionary.clone().into();
                        let _ = responder.send(dictionary_ref);
                    }
                    _ => return,
                }
            }
            self.remove_remote(koid).await;
        }
        .boxed()
    }

    async fn to_remote_capability(&self, capability: Capability) -> fruntime::Capability {
        match capability {
            Capability::Connector(connector) => {
                fruntime::Capability::Connector(connector.to_remote(&self).await)
            }
            Capability::DirConnector(dir_connector) => {
                fruntime::Capability::DirConnector(dir_connector.to_remote(&self).await)
            }
            Capability::Dictionary(dictionary) => {
                fruntime::Capability::Dictionary(dictionary.to_remote(&self).await)
            }
            Capability::ConnectorRouter(router) => {
                let (client_end, router_server_end) = create_endpoints();
                self.weak_task_group
                    .spawn(self.clone().serve_connector_router(router, router_server_end));
                fruntime::Capability::ConnectorRouter(client_end)
            }
            Capability::DirConnectorRouter(router) => {
                let (client_end, router_server_end) = create_endpoints();
                self.weak_task_group
                    .spawn(self.clone().serve_dir_connector_router(router, router_server_end));
                fruntime::Capability::DirConnectorRouter(client_end)
            }
            Capability::DictionaryRouter(router) => {
                let (client_end, router_server_end) = create_endpoints();
                self.weak_task_group
                    .spawn(self.clone().serve_dictionary_router(router, router_server_end));
                fruntime::Capability::DictionaryRouter(client_end)
            }
            Capability::DataRouter(router) => {
                let (client_end, router_server_end) = create_endpoints();
                self.weak_task_group
                    .spawn(self.clone().serve_data_router(router, router_server_end));
                fruntime::Capability::DataRouter(client_end)
            }
            Capability::Data(data) => match data {
                Data::Bytes(bytes) => fruntime::Capability::Data(fruntime::Data::Bytes(bytes)),
                Data::String(string) => fruntime::Capability::Data(fruntime::Data::String(string)),
                Data::Int64(num) => fruntime::Capability::Data(fruntime::Data::Int64(num)),
                Data::Uint64(num) => fruntime::Capability::Data(fruntime::Data::Uint64(num)),
            },
            other_capability_type => {
                panic!("unable to remote capabilities of this type: {:?}", other_capability_type)
            }
        }
    }

    async fn handle_key_iterator_stream(
        self,
        dictionary: Dict,
        mut stream: fruntime::DictionaryKeyIteratorRequestStream,
    ) {
        fn round_up_to_nearest_8(num: usize) -> usize {
            num + 7 & !7
        }

        let mut dictionary_iterator = dictionary.keys().map(|key| key.to_string()).peekable();
        while let Some(Ok(request)) = stream.next().await {
            match request {
                fruntime::DictionaryKeyIteratorRequest::GetNext { responder, .. } => {
                    let mut next_elements = vec![];
                    let mut bytes_used: usize = FIDL_VECTOR_OVERHEAD;
                    while let Some(next_element) = dictionary_iterator.peek() {
                        // A FIDL string takes up the number of bytes of the string rounded up to
                        // the nearest 8 plus the overhead size for the type.
                        bytes_used += FIDL_STRING_OVERHEAD;
                        // String::len returns number of bytes, not characters.
                        bytes_used += round_up_to_nearest_8(next_element.len());
                        if bytes_used > ZX_CHANNEL_MAX_MSG_BYTES as usize {
                            break;
                        }
                        next_elements.push(dictionary_iterator.next().unwrap());
                    }
                    let _ = responder.send(&next_elements);
                }
                other_request => {
                    log::warn!(
                        "dictionary keys iterator exiting due to unrecognized request: \
                               {other_request:?}"
                    );
                    return;
                }
            }
        }
    }

    fn serve_connector_router(
        self,
        router: Router<Connector>,
        router_server_end: ServerEnd<fruntime::ConnectorRouterMarker>,
    ) -> BoxFuture<'static, ()> {
        async move {
            self.store_remote(&router_server_end, router.clone()).await;
            let koid =
                router_server_end.basic_info().expect("failed to get basic info on handle").koid;
            let mut stream = router_server_end.into_stream();
            while let Some(Ok(request)) = stream.next().await {
                match request {
                    fruntime::ConnectorRouterRequest::Clone { request, .. } => {
                        self.weak_task_group.spawn(self.clone().serve_connector_router(
                            router.clone(),
                            ServerEnd::new(request.into_channel()),
                        ));
                    }
                    fruntime::ConnectorRouterRequest::Route {
                        request,
                        connector_server_end,
                        responder,
                        ..
                    } => {
                        let maybe_route_request = match self.convert_route_request(request).await {
                            Ok(maybe_request) => maybe_request,
                            Err(e) => {
                                let _ = responder.send(Err(e));
                                continue;
                            }
                        };
                        match router.route(maybe_route_request, false).await {
                            Ok(RouterResponse::Capability(connector)) => {
                                self.weak_task_group.spawn(
                                    self.clone().serve_connector(connector, connector_server_end),
                                );
                                let _ = responder.send(Ok(fruntime::RouterResponse::Success));
                            }
                            Ok(RouterResponse::Unavailable) => {
                                let _ = responder.send(Ok(fruntime::RouterResponse::Unavailable));
                            }
                            Ok(RouterResponse::Debug(_)) => {
                                panic!("got debug response to non-debug route, which is disallowed")
                            }
                            Err(e) => {
                                let _ = responder.send(Err(e.into()));
                            }
                        }
                    }
                    _ => return,
                }
            }
            self.remove_remote(koid).await;
        }
        .boxed()
    }

    fn serve_dir_connector_router(
        self,
        router: Router<DirConnector>,
        router_server_end: ServerEnd<fruntime::DirConnectorRouterMarker>,
    ) -> BoxFuture<'static, ()> {
        async move {
            self.store_remote(&router_server_end, router.clone()).await;
            let koid =
                router_server_end.basic_info().expect("failed to get basic info on handle").koid;
            let mut stream = router_server_end.into_stream();
            while let Some(Ok(request)) = stream.next().await {
                match request {
                    fruntime::DirConnectorRouterRequest::Clone { request, .. } => {
                        self.weak_task_group.spawn(self.clone().serve_dir_connector_router(
                            router.clone(),
                            ServerEnd::new(request.into_channel()),
                        ));
                    }
                    fruntime::DirConnectorRouterRequest::Route {
                        request,
                        dir_connector_server_end,
                        responder,
                        ..
                    } => {
                        let maybe_route_request = match self.convert_route_request(request).await {
                            Ok(maybe_request) => maybe_request,
                            Err(e) => {
                                let _ = responder.send(Err(e));
                                continue;
                            }
                        };
                        match router.route(maybe_route_request, false).await {
                            Ok(RouterResponse::Capability(dir_connector)) => {
                                self.weak_task_group.spawn(
                                    self.clone().serve_dir_connector(
                                        dir_connector,
                                        dir_connector_server_end,
                                    ),
                                );
                                let _ = responder.send(Ok(fruntime::RouterResponse::Success));
                            }
                            Ok(RouterResponse::Unavailable) => {
                                let _ = responder.send(Ok(fruntime::RouterResponse::Unavailable));
                            }
                            Ok(RouterResponse::Debug(_)) => {
                                panic!("got debug response to non-debug route, which is disallowed")
                            }
                            Err(e) => {
                                let _ = responder.send(Err(e.into()));
                            }
                        }
                    }
                    _ => return,
                }
            }
            self.remove_remote(koid).await;
        }
        .boxed()
    }

    fn serve_dictionary_router(
        self,
        router: Router<Dict>,
        router_server_end: ServerEnd<fruntime::DictionaryRouterMarker>,
    ) -> BoxFuture<'static, ()> {
        async move {
            self.store_remote(&router_server_end, router.clone()).await;
            let koid =
                router_server_end.basic_info().expect("failed to get basic info on handle").koid;
            let mut stream = router_server_end.into_stream();
            while let Some(Ok(request)) = stream.next().await {
                match request {
                    fruntime::DictionaryRouterRequest::Clone { request, .. } => {
                        self.weak_task_group.spawn(self.clone().serve_dictionary_router(
                            router.clone(),
                            ServerEnd::new(request.into_channel()),
                        ));
                    }
                    fruntime::DictionaryRouterRequest::Route {
                        request,
                        dictionary_server_end,
                        responder,
                        ..
                    } => {
                        let maybe_route_request = match self.convert_route_request(request).await {
                            Ok(maybe_request) => maybe_request,
                            Err(e) => {
                                let _ = responder.send(Err(e));
                                continue;
                            }
                        };
                        match router.route(maybe_route_request, false).await {
                            Ok(RouterResponse::Capability(dictionary)) => {
                                self.weak_task_group.spawn(
                                    self.clone()
                                        .serve_dictionary(dictionary, dictionary_server_end),
                                );
                                let _ = responder.send(Ok(fruntime::RouterResponse::Success));
                            }
                            Ok(RouterResponse::Unavailable) => {
                                let _ = responder.send(Ok(fruntime::RouterResponse::Unavailable));
                            }
                            Ok(RouterResponse::Debug(_)) => {
                                panic!("got debug response to non-debug route, which is disallowed")
                            }
                            Err(e) => {
                                let _ = responder.send(Err(e.into()));
                            }
                        }
                    }
                    _ => return,
                }
            }
            self.remove_remote(koid).await;
        }
        .boxed()
    }

    fn serve_data_router(
        self,
        router: Router<Data>,
        router_server_end: ServerEnd<fruntime::DataRouterMarker>,
    ) -> BoxFuture<'static, ()> {
        async move {
            self.store_remote(&router_server_end, router.clone()).await;
            let koid =
                router_server_end.basic_info().expect("failed to get basic info on handle").koid;
            let mut stream = router_server_end.into_stream();
            while let Some(Ok(request)) = stream.next().await {
                match request {
                    fruntime::DataRouterRequest::Clone { request, .. } => {
                        self.weak_task_group.spawn(self.clone().serve_data_router(
                            router.clone(),
                            ServerEnd::new(request.into_channel()),
                        ));
                    }
                    fruntime::DataRouterRequest::Route { request, responder, .. } => {
                        let maybe_route_request = match self.convert_route_request(request).await {
                            Ok(maybe_request) => maybe_request,
                            Err(e) => {
                                let _ = responder.send(Err(e));
                                continue;
                            }
                        };
                        match router.route(maybe_route_request, false).await {
                            Ok(RouterResponse::Capability(data)) => {
                                let data = match data {
                                    Data::Bytes(bytes) => fruntime::Data::Bytes(bytes),
                                    Data::String(string) => fruntime::Data::String(string),
                                    Data::Int64(num) => fruntime::Data::Int64(num),
                                    Data::Uint64(num) => fruntime::Data::Uint64(num),
                                };
                                let _ = responder
                                    .send(Ok((fruntime::RouterResponse::Success, Some(&data))));
                            }
                            Ok(RouterResponse::Unavailable) => {
                                let _ = responder
                                    .send(Ok((fruntime::RouterResponse::Unavailable, None)));
                            }
                            Ok(RouterResponse::Debug(_)) => {
                                panic!("got debug response to non-debug route, which is disallowed")
                            }
                            Err(e) => {
                                let _ = responder.send(Err(e.into()));
                            }
                        }
                    }
                    _ => return,
                }
            }
            self.remove_remote(koid).await;
        }
        .boxed()
    }

    async fn convert_route_request(
        &self,
        request: fruntime::RouteRequest,
    ) -> Result<Option<Request>, fruntime::RouterError> {
        match request {
            fruntime::RouteRequest { metadata: Some(metadata), target: Some(target), .. } => {
                let metadata = Dict::from_remote(metadata, &self)
                    .await
                    .ok_or(fruntime::RouterError::InvalidArgs)?;
                let target = self
                    .retrieve_instance_token(&target)
                    .await
                    .ok_or(fruntime::RouterError::InvalidArgs)?;
                Ok(Some(Request { metadata, target }))
            }
            fruntime::RouteRequest { metadata: None, target: None, .. } => Ok(None),
            _ => Err(fruntime::RouterError::InvalidArgs),
        }
    }
}

#[derive(Debug)]
struct RemoteReceiver {
    remote_receiver: fruntime::ReceiverProxy,
}

impl Connectable for RemoteReceiver {
    fn send(&self, message: Message) -> Result<(), ()> {
        let _ = self.remote_receiver.receive(message.channel);
        Ok(())
    }
}

#[derive(Debug)]
struct RemoteDirReceiver {
    remote_receiver: fruntime::DirReceiverProxy,
}

impl DirConnectable for RemoteDirReceiver {
    fn send(
        &self,
        server_end: ServerEnd<fio::DirectoryMarker>,
        _subdir: RelativePath,
        _rights: Option<fio::Operations>,
    ) -> Result<(), ()> {
        let _ = self.remote_receiver.receive(server_end);
        Ok(())
    }
}

/// This trait exists to let us generically handle the non-router capability type that can be
/// converted into "remote" capabilities (fuchsia.component.runtime.Capability) and back.
#[async_trait]
trait Remotable: CapabilityBound {
    /// The protocol marker from the FIDL bindings for the remoted version of this capability.
    type Marker: ProtocolMarker + Send + Sync;
    /// The proxy type for the router of the capability associated with the above marker.
    type RouterProxy: Send + Sync;
    /// The remoted capability, which is often represented by a client end of Self::Marker but not
    /// always (see the Data type for an example that's not a client end).
    type RemotedType: Send + Sync;

    /// Converts this capability into a remoted capability that can be sent over FIDL to a
    /// component.
    async fn to_remote(self, factory: &CapabilityFactory) -> Self::RemotedType;

    /// Converts this capability from a remoted capability given to us by a component into a
    /// standard Rust capability.
    async fn from_remote(
        client_end: Self::RemotedType,
        factory: &CapabilityFactory,
    ) -> Option<Self>;

    /// Performs a route operation for a capability of this type.
    async fn route(
        router_proxy: &Self::RouterProxy,
        request: fruntime::RouteRequest,
    ) -> Result<Option<Self::RemotedType>, fruntime::RouterError>;
}

#[async_trait]
impl Remotable for Connector {
    type Marker = fruntime::ConnectorMarker;
    type RouterProxy = fruntime::ConnectorRouterProxy;
    type RemotedType = ClientEnd<Self::Marker>;

    async fn to_remote(self, factory: &CapabilityFactory) -> Self::RemotedType {
        let (client_end, server_end) = create_endpoints();
        factory.weak_task_group.spawn(factory.clone().serve_connector(self, server_end));
        client_end
    }

    async fn from_remote(
        client_end: Self::RemotedType,
        factory: &CapabilityFactory,
    ) -> Option<Self> {
        let capability =
            factory.retrieve_remote(fruntime::Capability::Connector(client_end)).await?;
        match capability {
            Capability::Connector(connector) => Some(connector),
            _ => None,
        }
    }

    async fn route(
        router_proxy: &Self::RouterProxy,
        request: fruntime::RouteRequest,
    ) -> Result<Option<Self::RemotedType>, fruntime::RouterError> {
        let (client_end, server_end) = create_endpoints();
        match router_proxy.route(request, server_end).await {
            Ok(Ok(fruntime::RouterResponse::Success)) => Ok(Some(client_end)),
            Ok(Ok(_)) => Ok(None),
            Ok(Err(e)) => Err(e),
            Err(_fidl_error) => return Err(fruntime::RouterError::Internal),
        }
    }
}

#[async_trait]
impl Remotable for DirConnector {
    type Marker = fruntime::DirConnectorMarker;
    type RouterProxy = fruntime::DirConnectorRouterProxy;
    type RemotedType = ClientEnd<Self::Marker>;

    async fn to_remote(self, factory: &CapabilityFactory) -> Self::RemotedType {
        let (client_end, server_end) = create_endpoints();
        factory.weak_task_group.spawn(factory.clone().serve_dir_connector(self, server_end));
        client_end
    }

    async fn from_remote(
        client_end: Self::RemotedType,
        factory: &CapabilityFactory,
    ) -> Option<Self> {
        let capability =
            factory.retrieve_remote(fruntime::Capability::DirConnector(client_end)).await?;
        match capability {
            Capability::DirConnector(dir_connector) => Some(dir_connector),
            _ => None,
        }
    }

    async fn route(
        router_proxy: &Self::RouterProxy,
        request: fruntime::RouteRequest,
    ) -> Result<Option<Self::RemotedType>, fruntime::RouterError> {
        let (client_end, server_end) = create_endpoints();
        match router_proxy.route(request, server_end).await {
            Ok(Ok(fruntime::RouterResponse::Success)) => Ok(Some(client_end)),
            Ok(Ok(_)) => Ok(None),
            Ok(Err(e)) => Err(e),
            Err(_fidl_error) => return Err(fruntime::RouterError::Internal),
        }
    }
}

#[async_trait]
impl Remotable for Dict {
    type Marker = fruntime::DictionaryMarker;
    type RouterProxy = fruntime::DictionaryRouterProxy;
    type RemotedType = ClientEnd<Self::Marker>;

    async fn to_remote(self, factory: &CapabilityFactory) -> Self::RemotedType {
        let (client_end, server_end) = create_endpoints();
        factory.weak_task_group.spawn(factory.clone().serve_dictionary(self, server_end));
        client_end
    }

    async fn from_remote(
        client_end: Self::RemotedType,
        factory: &CapabilityFactory,
    ) -> Option<Self> {
        let capability =
            factory.retrieve_remote(fruntime::Capability::Dictionary(client_end)).await?;
        match capability {
            Capability::Dictionary(dictionary) => Some(dictionary),
            _ => None,
        }
    }

    async fn route(
        router_proxy: &Self::RouterProxy,
        request: fruntime::RouteRequest,
    ) -> Result<Option<Self::RemotedType>, fruntime::RouterError> {
        let (client_end, server_end) = create_endpoints();
        match router_proxy.route(request, server_end).await {
            Ok(Ok(fruntime::RouterResponse::Success)) => Ok(Some(client_end)),
            Ok(Ok(_)) => Ok(None),
            Ok(Err(e)) => Err(e),
            Err(_fidl_error) => return Err(fruntime::RouterError::Internal),
        }
    }
}

#[async_trait]
impl Remotable for Data {
    type Marker = fruntime::DictionaryMarker;
    type RouterProxy = fruntime::DataRouterProxy;
    type RemotedType = fruntime::Data;

    async fn to_remote(self, _factory: &CapabilityFactory) -> Self::RemotedType {
        match self {
            Data::Bytes(bytes) => fruntime::Data::Bytes(bytes),
            Data::String(string) => fruntime::Data::String(string),
            Data::Int64(num) => fruntime::Data::Int64(num),
            Data::Uint64(num) => fruntime::Data::Uint64(num),
        }
    }

    async fn from_remote(data: Self::RemotedType, _factory: &CapabilityFactory) -> Option<Self> {
        match data {
            fruntime::Data::Bytes(bytes) => Some(Data::Bytes(bytes)),
            fruntime::Data::String(string) => Some(Data::String(string)),
            fruntime::Data::Int64(num) => Some(Data::Int64(num)),
            fruntime::Data::Uint64(num) => Some(Data::Uint64(num)),
            _ => None,
        }
    }

    async fn route(
        router_proxy: &Self::RouterProxy,
        request: fruntime::RouteRequest,
    ) -> Result<Option<Self::RemotedType>, fruntime::RouterError> {
        match router_proxy.route(request).await {
            Ok(Ok((fruntime::RouterResponse::Success, Some(data)))) => Ok(Some(*data)),
            Ok(Ok(_)) => Ok(None),
            Ok(Err(e)) => Err(e),
            Err(_fidl_error) => return Err(fruntime::RouterError::Internal),
        }
    }
}

struct RemoteRouter<R>
where
    R: Remotable + Send + Sync,
{
    router_proxy: R::RouterProxy,
    factory: CapabilityFactory,
}

#[async_trait]
impl<R> Routable<R> for RemoteRouter<R>
where
    R: Remotable + CapabilityBound + Send + Sync,
{
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<R>, RouterError> {
        if debug {
            return Err(RouterError::RemotedAt { moniker: self.factory.moniker.clone() }.into());
        }

        let (metadata, target) = match request {
            Some(Request { metadata, target }) => {
                let target = self.factory.store_instance_token(target).await;
                (Some(metadata.to_remote(&self.factory).await), Some(target))
            }
            None => (None, None),
        };
        let request = fruntime::RouteRequest { target, metadata, ..Default::default() };
        let result = R::route(&self.router_proxy, request).await;
        match result {
            Ok(Some(client_end)) => {
                let Some(capability) = R::from_remote(client_end, &self.factory).await else {
                    return Err(RouterError::InvalidArgs);
                };
                Ok(RouterResponse::Capability(capability))
            }
            Ok(None) => Ok(RouterResponse::Unavailable),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use cm_util::TaskGroup;

    fn new_factory_connection(
    ) -> (fruntime::CapabilityFactoryProxy, Arc<Mutex<RemotedRuntimeCapabilities>>, TaskGroup) {
        let task_group = TaskGroup::new();
        let remote_capabilities = Arc::new(Mutex::new(HashMap::new()));
        let capability_factory = CapabilityFactory {
            remote_capabilities: remote_capabilities.clone(),
            weak_task_group: task_group.as_weak(),
            moniker: Moniker::root(),
        };
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fruntime::CapabilityFactoryMarker>();
        task_group.spawn(async move { capability_factory.handle_stream(stream).await.unwrap() });
        (proxy, remote_capabilities, task_group)
    }

    fn create_connector(
        proxy: &fruntime::CapabilityFactoryProxy,
    ) -> (fruntime::ConnectorProxy, fruntime::ReceiverRequestStream) {
        let (receiver_client_end, receiver_stream) =
            fidl::endpoints::create_request_stream::<fruntime::ReceiverMarker>();
        let (connector_proxy, connector_server_end) =
            fidl::endpoints::create_proxy::<fruntime::ConnectorMarker>();
        proxy.create_connector(receiver_client_end, connector_server_end).unwrap();
        (connector_proxy, receiver_stream)
    }

    fn create_dir_connector(
        proxy: &fruntime::CapabilityFactoryProxy,
    ) -> (fruntime::DirConnectorProxy, fruntime::DirReceiverRequestStream) {
        let (dir_receiver_client_end, dir_receiver_stream) =
            fidl::endpoints::create_request_stream::<fruntime::DirReceiverMarker>();
        let (dir_connector_proxy, dir_connector_server_end) =
            fidl::endpoints::create_proxy::<fruntime::DirConnectorMarker>();
        proxy.create_dir_connector(dir_receiver_client_end, dir_connector_server_end).unwrap();
        (dir_connector_proxy, dir_receiver_stream)
    }

    async fn test_connector_is_connected(
        connector_proxy: &fruntime::ConnectorProxy,
        receiver_stream: &mut fruntime::ReceiverRequestStream,
    ) {
        let (c1, c2) = zx::Channel::create();
        connector_proxy.connect(c1).unwrap();
        let received_channel = match receiver_stream.next().await {
            Some(Ok(fruntime::ReceiverRequest::Receive { channel, .. })) => channel,
            other_message => panic!("unexpected message: {other_message:?}"),
        };
        assert_eq!(
            c2.basic_info().unwrap().koid,
            received_channel.basic_info().unwrap().related_koid
        );
    }

    async fn test_dir_connector_is_connected(
        dir_connector_proxy: &fruntime::DirConnectorProxy,
        dir_receiver_stream: &mut fruntime::DirReceiverRequestStream,
    ) {
        let (client_end, server_end) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
        dir_connector_proxy.connect(server_end).unwrap();
        let received_server_end = match dir_receiver_stream.next().await {
            Some(Ok(fruntime::DirReceiverRequest::Receive { channel, .. })) => channel,
            other_message => panic!("unexpected message: {other_message:?}"),
        };
        assert_eq!(
            client_end.basic_info().unwrap().koid,
            received_server_end.basic_info().unwrap().related_koid
        );
    }

    async fn assert_no_remote_capabilities(capabilities: &Arc<Mutex<RemotedRuntimeCapabilities>>) {
        // Cleanup happens asynchronously and there's no way for us to block on it from here, so we
        // have to rely on a timer to wait for cleanup to happen.
        fuchsia_async::Timer::new(std::time::Duration::from_secs(1)).await;
        assert_eq!(0, capabilities.lock().len());
    }

    #[fuchsia::test]
    async fn create_connector_test() {
        let (proxy, remote_capabilities, _task_group) = new_factory_connection();
        let (connector_proxy, mut receiver_stream) = create_connector(&proxy);

        test_connector_is_connected(&connector_proxy, &mut receiver_stream).await;
        let store_koid = connector_proxy.as_channel().basic_info().unwrap().related_koid;
        assert_matches!(
            remote_capabilities.lock().get(&store_koid),
            Some(CapabilityOrWaiter::Capability(Capability::Connector(_)))
        );
        drop(connector_proxy);
        assert_no_remote_capabilities(&remote_capabilities).await;
    }

    #[fuchsia::test]
    async fn create_dir_connector_test() {
        let (proxy, remote_capabilities, _task_group) = new_factory_connection();
        let (dir_connector_proxy, mut dir_receiver_stream) = create_dir_connector(&proxy);

        test_dir_connector_is_connected(&dir_connector_proxy, &mut dir_receiver_stream).await;
        let store_koid = dir_connector_proxy.as_channel().basic_info().unwrap().related_koid;
        assert_matches!(
            remote_capabilities.lock().get(&store_koid),
            Some(CapabilityOrWaiter::Capability(Capability::DirConnector(_)))
        );
        drop(dir_connector_proxy);
        assert_no_remote_capabilities(&remote_capabilities).await;
    }

    #[fuchsia::test]
    async fn create_dictionary_test() {
        let (proxy, remote_capabilities, _task_group) = new_factory_connection();
        let (dictionary_proxy, dictionary_server_end) =
            fidl::endpoints::create_proxy::<fruntime::DictionaryMarker>();
        proxy.create_dictionary(dictionary_server_end).unwrap();

        // We need to call something synchronous on the dictionary so we can ensure that the
        // `create_dictionary` call has been handled before we peek in remote_capabilities.
        assert!(dictionary_proxy.get("nonexistent").await.unwrap().is_none());

        let store_koid = dictionary_proxy.as_channel().basic_info().unwrap().related_koid;
        assert_matches!(
            remote_capabilities.lock().get(&store_koid),
            Some(CapabilityOrWaiter::Capability(Capability::Dictionary(_)))
        );
        drop(dictionary_proxy);
        assert_no_remote_capabilities(&remote_capabilities).await;
    }

    #[fuchsia::test]
    async fn insert_get_remove_dictionary_test() {
        let (proxy, _remote_capabilities, _task_group) = new_factory_connection();
        let (dictionary_proxy, dictionary_server_end) =
            fidl::endpoints::create_proxy::<fruntime::DictionaryMarker>();
        proxy.create_dictionary(dictionary_server_end).unwrap();

        dictionary_proxy.insert("a", fruntime::Capability::Data(fruntime::Data::Int64(1))).unwrap();
        assert_eq!(
            dictionary_proxy.get("a").await.unwrap(),
            Some(Box::new(fruntime::Capability::Data(fruntime::Data::Int64(1))))
        );
        assert_eq!(
            dictionary_proxy.remove("a").await.unwrap(),
            Some(Box::new(fruntime::Capability::Data(fruntime::Data::Int64(1))))
        );
        assert_eq!(dictionary_proxy.get("a").await.unwrap(), None);
    }

    #[fuchsia::test]
    async fn iterate_keys_dictionary_test() {
        let (proxy, _remote_capabilities, _task_group) = new_factory_connection();
        let (dictionary_proxy, dictionary_server_end) =
            fidl::endpoints::create_proxy::<fruntime::DictionaryMarker>();
        proxy.create_dictionary(dictionary_server_end).unwrap();

        let mut expected_dictionary_contents = vec![];
        // We create enough dictionary entries to be sure that they can't be given back to us in a
        // single message.
        for i in 0..(ZX_CHANNEL_MAX_MSG_BYTES / 100) {
            // formats the number as hex with up to 98 leading 0s. The formatter also prepends
            // "0x", meaning this is always 100 characters long (the max name size)
            let name = format!("{i:#098x}");
            dictionary_proxy
                .insert(&name, fruntime::Capability::Data(fruntime::Data::Int64(1)))
                .unwrap();
            expected_dictionary_contents.push(name);
            // There's no backpressure, so we can accidentally fill the channel before we have a
            // chance to read the messages out of the other end. To ensure that this test isn't
            // flaky, let's sleep a bit between each insert.
            fuchsia_async::Timer::new(std::time::Duration::from_nanos(500)).await;
        }
        let (iterator_proxy, iterator_server_end) =
            fidl::endpoints::create_proxy::<fruntime::DictionaryKeyIteratorMarker>();
        dictionary_proxy.iterate_keys(iterator_server_end).unwrap();
        let mut actual_dictionary_contents = vec![];
        loop {
            let mut next_elements = iterator_proxy.get_next().await.unwrap();
            if next_elements.is_empty() {
                break;
            }
            actual_dictionary_contents.append(&mut next_elements);
        }
        expected_dictionary_contents.sort_unstable();
        actual_dictionary_contents.sort_unstable();
        assert_eq!(expected_dictionary_contents, actual_dictionary_contents);
    }

    #[fuchsia::test]
    async fn create_connector_router_test() {
        let (proxy, remote_capabilities, _task_group) = new_factory_connection();
        let (router_client_end, mut router_stream) =
            fidl::endpoints::create_request_stream::<fruntime::ConnectorRouterMarker>();
        let (router_proxy, router_server_end) =
            fidl::endpoints::create_proxy::<fruntime::ConnectorRouterMarker>();
        proxy.create_connector_router(router_client_end, router_server_end).unwrap();

        let (connector_proxy, connector_server_end) =
            fidl::endpoints::create_proxy::<fruntime::ConnectorMarker>();
        let success_route_fut = router_proxy.route(Default::default(), connector_server_end);
        let mut receiver_stream = match router_stream.next().await {
            Some(Ok(fruntime::ConnectorRouterRequest::Route {
                connector_server_end,
                responder,
                ..
            })) => {
                let (connector_proxy, receiver_stream) = create_connector(&proxy);
                connector_proxy.clone(ServerEnd::new(connector_server_end.into_channel())).unwrap();
                responder.send(Ok(fruntime::RouterResponse::Success)).unwrap();
                receiver_stream
            }
            other_message => panic!("unexpected message: {other_message:?}"),
        };
        assert_eq!(Ok(fruntime::RouterResponse::Success), success_route_fut.await.unwrap());
        test_connector_is_connected(&connector_proxy, &mut receiver_stream).await;

        let store_koid = router_proxy.as_channel().basic_info().unwrap().related_koid;
        assert_matches!(
            remote_capabilities.lock().get(&store_koid),
            Some(CapabilityOrWaiter::Capability(Capability::ConnectorRouter(_)))
        );
        drop(router_proxy);
        drop(connector_proxy);
        assert_no_remote_capabilities(&remote_capabilities).await;
    }

    #[fuchsia::test]
    async fn create_dir_connector_router_test() {
        let (proxy, remote_capabilities, _task_group) = new_factory_connection();
        let (router_client_end, mut router_stream) =
            fidl::endpoints::create_request_stream::<fruntime::DirConnectorRouterMarker>();
        let (router_proxy, router_server_end) =
            fidl::endpoints::create_proxy::<fruntime::DirConnectorRouterMarker>();
        proxy.create_dir_connector_router(router_client_end, router_server_end).unwrap();

        let (dir_connector_proxy, dir_connector_server_end) =
            fidl::endpoints::create_proxy::<fruntime::DirConnectorMarker>();
        let success_route_fut = router_proxy.route(Default::default(), dir_connector_server_end);
        let mut dir_receiver_stream = match router_stream.next().await {
            Some(Ok(fruntime::DirConnectorRouterRequest::Route {
                dir_connector_server_end,
                responder,
                ..
            })) => {
                let (dir_connector_proxy, dir_receiver_stream) = create_dir_connector(&proxy);
                dir_connector_proxy
                    .clone(ServerEnd::new(dir_connector_server_end.into_channel()))
                    .unwrap();
                responder.send(Ok(fruntime::RouterResponse::Success)).unwrap();
                dir_receiver_stream
            }
            other_message => panic!("unexpected message: {other_message:?}"),
        };
        assert_eq!(Ok(fruntime::RouterResponse::Success), success_route_fut.await.unwrap());
        test_dir_connector_is_connected(&dir_connector_proxy, &mut dir_receiver_stream).await;

        let store_koid = router_proxy.as_channel().basic_info().unwrap().related_koid;
        assert_matches!(
            remote_capabilities.lock().get(&store_koid),
            Some(CapabilityOrWaiter::Capability(Capability::DirConnectorRouter(_)))
        );
        drop(router_proxy);
        drop(dir_connector_proxy);
        assert_no_remote_capabilities(&remote_capabilities).await;
    }

    #[fuchsia::test]
    async fn create_dictionary_router_test() {
        let (proxy, remote_capabilities, _task_group) = new_factory_connection();
        let (router_client_end, mut router_stream) =
            fidl::endpoints::create_request_stream::<fruntime::DictionaryRouterMarker>();
        let (router_proxy, router_server_end) =
            fidl::endpoints::create_proxy::<fruntime::DictionaryRouterMarker>();
        proxy.create_dictionary_router(router_client_end, router_server_end).unwrap();

        let (dictionary_proxy, dictionary_server_end) =
            fidl::endpoints::create_proxy::<fruntime::DictionaryMarker>();
        let success_route_fut = router_proxy.route(Default::default(), dictionary_server_end);
        match router_stream.next().await {
            Some(Ok(fruntime::DictionaryRouterRequest::Route {
                dictionary_server_end,
                responder,
                ..
            })) => {
                proxy.create_dictionary(dictionary_server_end).unwrap();
                responder.send(Ok(fruntime::RouterResponse::Success)).unwrap();
            }
            other_message => panic!("unexpected message: {other_message:?}"),
        };
        assert_eq!(Ok(fruntime::RouterResponse::Success), success_route_fut.await.unwrap());

        // Let's call something synchronous to ensure that our server end got connected to
        // something.
        assert!(dictionary_proxy.get("nonexistent").await.unwrap().is_none());

        let store_koid = router_proxy.as_channel().basic_info().unwrap().related_koid;
        assert_matches!(
            remote_capabilities.lock().get(&store_koid),
            Some(CapabilityOrWaiter::Capability(Capability::DictionaryRouter(_)))
        );
        drop(router_proxy);
        drop(dictionary_proxy);
        assert_no_remote_capabilities(&remote_capabilities).await;
    }

    #[fuchsia::test]
    async fn create_data_router_test() {
        let (proxy, remote_capabilities, _task_group) = new_factory_connection();
        let (router_client_end, mut router_stream) =
            fidl::endpoints::create_request_stream::<fruntime::DataRouterMarker>();
        let (router_proxy, router_server_end) =
            fidl::endpoints::create_proxy::<fruntime::DataRouterMarker>();
        proxy.create_data_router(router_client_end, router_server_end).unwrap();

        let success_route_fut = router_proxy.route(Default::default());
        match router_stream.next().await {
            Some(Ok(fruntime::DataRouterRequest::Route { responder, .. })) => {
                responder
                    .send(Ok((fruntime::RouterResponse::Success, Some(&fruntime::Data::Int64(1)))))
                    .unwrap();
            }
            other_message => panic!("unexpected message: {other_message:?}"),
        };
        assert_eq!(
            Ok((fruntime::RouterResponse::Success, Some(Box::new(fruntime::Data::Int64(1))))),
            success_route_fut.await.unwrap()
        );

        let store_koid = router_proxy.as_channel().basic_info().unwrap().related_koid;
        assert_matches!(
            remote_capabilities.lock().get(&store_koid),
            Some(CapabilityOrWaiter::Capability(Capability::DataRouter(_)))
        );
        drop(router_proxy);
        assert_no_remote_capabilities(&remote_capabilities).await;
    }
}
