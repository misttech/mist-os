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
    bedrock_error::Explain,
    cm_types::Name,
    cm_util::TaskGroup,
    fidl::endpoints::{self, ClientEnd, DiscoverableProtocolMarker, ServerEnd},
    fidl_fuchsia_component_sandbox as fsandbox,
    fuchsia_zircon::{self as zx, AsHandleRef},
    futures::prelude::*,
    lazy_static::lazy_static,
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
            fsandbox::FactoryRequest::ConnectToSender {
                capability,
                server_end,
                control_handle: _,
            } => match sandbox::Capability::try_from(fsandbox::Capability::Sender(capability)) {
                Ok(capability) => match capability {
                    sandbox::Capability::Sender(sender) => {
                        let server_end: ServerEnd<fsandbox::SenderMarker> = server_end.into();
                        self.tasks.spawn(serve_sender(sender, server_end.into_stream().unwrap()));
                    }
                    _ => unreachable!(),
                },
                Err(err) => {
                    warn!("Error converting token to capability: {err:?}");
                    _ = server_end.close_with_epitaph(err.as_zx_status());
                }
            },
            fsandbox::FactoryRequest::CreateSender { receiver, responder } => {
                let sender = self.create_sender(receiver);
                responder.send(sender)?;
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

    fn create_sender(
        &self,
        receiver_client: ClientEnd<fsandbox::ReceiverMarker>,
    ) -> fsandbox::SenderCapability {
        let (receiver, sender) = Receiver::new();
        self.tasks.spawn(async move {
            receiver.handle_receiver(receiver_client.into_proxy().unwrap()).await;
        });
        fsandbox::SenderCapability::from(sender)
    }

    fn create_dictionary(&self) -> ClientEnd<fsandbox::DictionaryMarker> {
        let (client_end, server_end) = endpoints::create_endpoints();
        let dict = Dict::new();
        let client_end_koid = server_end.basic_info().unwrap().related_koid;
        dict.serve_and_register(server_end.into_stream().unwrap(), client_end_koid);
        client_end
    }
}

async fn serve_sender(sender: sandbox::Sender, mut stream: fsandbox::SenderRequestStream) {
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fsandbox::SenderRequest::Send_ { channel, control_handle: _ } => {
                if let Err(_err) = sender.send(sandbox::Message { channel }) {
                    return;
                }
            }
            fsandbox::SenderRequest::_UnknownMethod { ordinal, .. } => {
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
    use {
        fuchsia_async as fasync,
        fuchsia_zircon::{self as zx},
    };

    #[fuchsia::test]
    async fn create_sender() {
        let mut tasks = fasync::TaskGroup::new();

        let host = FactoryCapabilityHost::new();
        let (factory_proxy, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::FactoryMarker>().unwrap();
        tasks.spawn(async move {
            host.serve(stream).await.unwrap();
        });

        let (receiver_client_end, mut receiver_stream) =
            endpoints::create_request_stream::<fsandbox::ReceiverMarker>().unwrap();
        let sender = factory_proxy.create_sender(receiver_client_end).await.unwrap();
        let (sender_client, sender_server) =
            fidl::endpoints::create_endpoints::<fsandbox::SenderMarker>();
        factory_proxy.connect_to_sender(sender, sender_server.into()).unwrap();
        let sender = sender_client.into_proxy().unwrap();

        let (ch1, _ch2) = zx::Channel::create();
        let expected_koid = ch1.get_koid().unwrap();
        sender.send_(ch1).unwrap();

        let request = receiver_stream.try_next().await.unwrap().unwrap();
        if let fsandbox::ReceiverRequest::Receive { channel, .. } = request {
            assert_eq!(channel.get_koid().unwrap(), expected_koid);
        } else {
            panic!("unexpected request");
        }
    }

    #[fuchsia::test]
    async fn create_dictionary() {
        let mut tasks = fasync::TaskGroup::new();

        let host = FactoryCapabilityHost::new();
        let (factory_proxy, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::FactoryMarker>().unwrap();
        tasks.spawn(async move {
            host.serve(stream).await.unwrap();
        });

        let dict = factory_proxy.create_dictionary().await.unwrap();
        let dict = dict.into_proxy().unwrap();

        // The dictionary is empty.
        let items = dict.read().await.unwrap();
        assert_eq!(items.len(), 0);
    }
}
