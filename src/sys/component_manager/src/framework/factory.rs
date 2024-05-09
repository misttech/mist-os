// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::{CapabilityProvider, FrameworkCapability, InternalCapabilityProvider},
        model::component::WeakComponentInstance,
    },
    ::routing::capability_source::InternalCapability,
    async_trait::async_trait,
    cm_types::Name,
    cm_util::TaskGroup,
    fidl::endpoints::{self, ClientEnd, DiscoverableProtocolMarker, ServerEnd},
    fidl_fuchsia_component_sandbox as fsandbox,
    fuchsia_zircon::{self as zx, AsHandleRef},
    futures::prelude::*,
    lazy_static::lazy_static,
    router_error::Explain,
    sandbox::{Dict, Receiver},
    std::sync::Arc,
    tracing::warn,
};

lazy_static! {
    static ref CAPABILITY_NAME: Name = fsandbox::FactoryMarker::PROTOCOL_NAME.parse().unwrap();
}

struct FactoryCapabilityProvider {
    host: Arc<FactoryCapabilityHost>,
}

impl FactoryCapabilityProvider {
    fn new(host: Arc<FactoryCapabilityHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl InternalCapabilityProvider for FactoryCapabilityProvider {
    async fn open_protocol(self: Box<Self>, server_end: zx::Channel) {
        let server_end = ServerEnd::<fsandbox::FactoryMarker>::new(server_end);
        // We only need to look up the component matching this scope.
        // These operations should all work, even if the component is not running.
        let serve_result = self.host.serve(server_end.into_stream().unwrap()).await;
        if let Err(error) = serve_result {
            // TODO: Set an epitaph to indicate this was an unexpected error.
            warn!(%error, "serve failed");
        }
    }
}

pub struct FactoryCapabilityHost {
    tasks: TaskGroup,
}

impl FactoryCapabilityHost {
    pub fn new() -> Self {
        Self { tasks: TaskGroup::new() }
    }

    pub async fn serve(
        &self,
        mut stream: fsandbox::FactoryRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Some(request) = stream.try_next().await? {
            let method_name = request.method_name();
            let result = self.handle_request(request).await;
            match result {
                // If the error was PEER_CLOSED then we don't need to log it as a client can
                // disconnect while we are processing its request.
                Err(error) if !error.is_closed() => {
                    warn!(%method_name, %error, "Couldn't send Factory response");
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn handle_request(&self, request: fsandbox::FactoryRequest) -> Result<(), fidl::Error> {
        match request {
            fsandbox::FactoryRequest::ConnectToHandle {
                capability,
                server_end,
                control_handle: _,
            } => match sandbox::Capability::try_from(fsandbox::Capability::Handle(capability)) {
                Ok(capability) => match capability {
                    sandbox::Capability::OneShotHandle(handle) => {
                        let server_end: ServerEnd<fsandbox::HandleMarker> = server_end.into();
                        self.tasks.spawn(serve_handle(handle, server_end.into_stream().unwrap()));
                    }
                    _ => unreachable!(),
                },
                Err(err) => {
                    warn!("Error converting token to capability: {err:?}");
                    _ = server_end.close_with_epitaph(err.as_zx_status());
                }
            },
            fsandbox::FactoryRequest::ConnectToConnector {
                capability,
                server_end,
                control_handle: _,
            } => match sandbox::Capability::try_from(fsandbox::Capability::Connector(capability)) {
                Ok(capability) => match capability {
                    sandbox::Capability::Connector(connector) => {
                        let server_end: ServerEnd<fsandbox::ConnectorMarker> = server_end.into();
                        self.tasks
                            .spawn(serve_connector(connector, server_end.into_stream().unwrap()));
                    }
                    _ => unreachable!(),
                },
                Err(err) => {
                    warn!("Error converting token to capability: {err:?}");
                    _ = server_end.close_with_epitaph(err.as_zx_status());
                }
            },
            fsandbox::FactoryRequest::CreateConnector { receiver, responder } => {
                let sender = self.create_connector(receiver);
                responder.send(sender)?;
            }
            fsandbox::FactoryRequest::CreateHandle { handle, responder } => {
                let handle = self.create_handle(handle);
                responder.send(handle)?;
            }
            fsandbox::FactoryRequest::CreateDictionary { responder } => {
                let client_end = self.create_dictionary();
                responder.send(client_end)?;
            }
            fsandbox::FactoryRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "fuchsia.component.sandbox/Factory received unknown method");
            }
        }
        Ok(())
    }

    fn create_handle(&self, handle: zx::Handle) -> fsandbox::HandleCapability {
        fsandbox::HandleCapability::from(sandbox::OneShotHandle::new(handle))
    }

    fn create_connector(
        &self,
        receiver_client: ClientEnd<fsandbox::ReceiverMarker>,
    ) -> fsandbox::ConnectorCapability {
        let (receiver, sender) = Receiver::new();
        self.tasks.spawn(async move {
            receiver.handle_receiver(receiver_client.into_proxy().unwrap()).await;
        });
        fsandbox::ConnectorCapability::from(sender)
    }

    fn create_dictionary(&self) -> ClientEnd<fsandbox::DictionaryMarker> {
        let (client_end, server_end) = endpoints::create_endpoints();
        let dict = Dict::new();
        let client_end_koid = server_end.basic_info().unwrap().related_koid;
        dict.serve_and_register(server_end.into_stream().unwrap(), client_end_koid);
        client_end
    }
}

async fn serve_handle(handle: sandbox::OneShotHandle, mut stream: fsandbox::HandleRequestStream) {
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fsandbox::HandleRequest::GetHandle { responder } => {
                if let Err(_err) = responder.send(handle.get_handle()) {
                    return;
                }
            }
            fsandbox::HandleRequest::_UnknownMethod { ordinal, .. } => {
                warn!("Received unknown Handle request with ordinal {ordinal}");
            }
        }
    }
}

async fn serve_connector(sender: sandbox::Connector, mut stream: fsandbox::ConnectorRequestStream) {
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fsandbox::ConnectorRequest::Open { channel, control_handle: _ } => {
                if let Err(_err) = sender.send(sandbox::Message { channel }) {
                    return;
                }
            }
            fsandbox::ConnectorRequest::_UnknownMethod { ordinal, .. } => {
                warn!("Received unknown Sender request with ordinal {ordinal}");
            }
        }
    }
}

pub struct FactoryFrameworkCapability {
    host: Arc<FactoryCapabilityHost>,
}

impl FactoryFrameworkCapability {
    pub fn new(host: Arc<FactoryCapabilityHost>) -> Self {
        Self { host }
    }
}

impl FrameworkCapability for FactoryFrameworkCapability {
    fn matches(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&CAPABILITY_NAME)
    }

    fn new_provider(
        &self,
        _scope: WeakComponentInstance,
        _target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        Box::new(FactoryCapabilityProvider::new(self.host.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::create_endpoints;
    use fidl_fuchsia_component_sandbox as fsandbox;
    use fuchsia_async as fasync;
    use fuchsia_zircon::{self as zx, HandleBased};
    use futures::join;
    use sandbox::OneShotHandle;

    fn factory() -> (fasync::TaskGroup, fsandbox::FactoryProxy) {
        let mut tasks = fasync::TaskGroup::new();
        let host = FactoryCapabilityHost::new();
        let (factory_proxy, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::FactoryMarker>().unwrap();
        tasks.spawn(async move {
            host.serve(stream).await.unwrap();
        });
        (tasks, factory_proxy)
    }

    #[fuchsia::test]
    async fn create_handle() {
        let (tasks, factory_proxy) = factory();

        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        let one_shot = factory_proxy.create_handle(event.into()).await.unwrap();
        let (handle_client, handle_server) = create_endpoints::<fsandbox::HandleMarker>();
        factory_proxy.connect_to_handle(one_shot, handle_server.into()).unwrap();
        let handle_proxy = handle_client.into_proxy().unwrap();
        let event_back = handle_proxy.get_handle().await.unwrap().unwrap();
        assert!(event_back.get_koid().unwrap() == expected_koid);

        drop(handle_proxy);
        drop(factory_proxy);
        tasks.join().await;
    }

    /// Tests that the OneShotHandle implementation of the GetHandle method
    /// returns the handle held by the OneShotHandle.
    #[fuchsia::test]
    async fn one_shot_serve_get_handle() {
        let (tasks, factory_proxy) = factory();

        // Create an Event and get its koid.
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        let one_shot = OneShotHandle::from(event.into_handle());
        let fidl_one_shot: fsandbox::HandleCapability = one_shot.into();

        let (handle_client, handle_server) = create_endpoints::<fsandbox::HandleMarker>();
        factory_proxy.connect_to_handle(fidl_one_shot, handle_server).unwrap();
        let handle_proxy = handle_client.into_proxy().unwrap();

        let client = async move {
            let handle = handle_proxy.get_handle().await.unwrap().unwrap();

            // The handle should be for same Event that was in the OneShotHandle.
            let got_koid = handle.get_koid().unwrap();
            assert_eq!(got_koid, expected_koid);
        };

        drop(factory_proxy);
        join!(client, tasks.join());
    }

    /// Tests that the OneShotHandle implementation of the HandleCapability.GetHandle method
    /// returns the Unavailable error if GetHandle is called twice.
    #[fuchsia::test]
    async fn one_shot_serve_get_handle_unavailable() {
        let (tasks, factory_proxy) = factory();

        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        let one_shot = OneShotHandle::from(event.into_handle());
        let fidl_one_shot: fsandbox::HandleCapability = one_shot.into();

        let (handle_client, handle_server) = create_endpoints::<fsandbox::HandleMarker>();
        factory_proxy.connect_to_handle(fidl_one_shot, handle_server).unwrap();
        let handle_proxy = handle_client.into_proxy().unwrap();

        let client = async move {
            let handle = handle_proxy.get_handle().await.unwrap().unwrap();

            // The handle should be for same Event that was in the OneShotHandle.
            let got_koid = handle.get_koid().unwrap();
            assert_eq!(got_koid, expected_koid);
        };

        drop(factory_proxy);
        join!(client, tasks.join());
    }

    #[fuchsia::test]
    async fn create_connector() {
        let (_tasks, factory_proxy) = factory();

        let (receiver_client_end, mut receiver_stream) =
            endpoints::create_request_stream::<fsandbox::ReceiverMarker>().unwrap();
        let connector = factory_proxy.create_connector(receiver_client_end).await.unwrap();
        let (connector_client, connector_server) =
            fidl::endpoints::create_endpoints::<fsandbox::ConnectorMarker>();
        factory_proxy.connect_to_connector(connector, connector_server.into()).unwrap();
        let connector = connector_client.into_proxy().unwrap();

        let (ch1, _ch2) = zx::Channel::create();
        let expected_koid = ch1.get_koid().unwrap();
        connector.open(ch1).unwrap();

        let request = receiver_stream.try_next().await.unwrap().unwrap();
        if let fsandbox::ReceiverRequest::Receive { channel, .. } = request {
            assert_eq!(channel.get_koid().unwrap(), expected_koid);
        } else {
            panic!("unexpected request");
        }
    }

    #[fuchsia::test]
    async fn create_dictionary() {
        let (_tasks, factory_proxy) = factory();

        let dict = factory_proxy.create_dictionary().await.unwrap();
        let dict = dict.into_proxy().unwrap();

        // The dictionary is empty.
        let items = dict.read().await.unwrap();
        assert_eq!(items.len(), 0);
    }
}
