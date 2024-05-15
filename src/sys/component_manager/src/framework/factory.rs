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
    fidl::{
        endpoints::{self, ClientEnd, DiscoverableProtocolMarker, ServerEnd},
        epitaph::ChannelEpitaphExt,
    },
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
            fsandbox::FactoryRequest::TakeHandle { capability, responder } => {
                match sandbox::Capability::try_from(fsandbox::Capability::Handle(capability)) {
                    Ok(capability) => match capability {
                        sandbox::Capability::OneShotHandle(handle) => {
                            responder.send(
                                handle.take().ok_or_else(|| fsandbox::FactoryError::Unavailable),
                            )?;
                        }
                        _ => unreachable!(),
                    },
                    Err(err) => {
                        warn!("Error converting token to capability: {err:?}");
                    }
                }
            }
            fsandbox::FactoryRequest::OpenConnector {
                capability,
                server_end,
                control_handle: _,
            } => match sandbox::Capability::try_from(fsandbox::Capability::Connector(capability)) {
                Ok(capability) => match capability {
                    sandbox::Capability::Connector(connector) => {
                        let _ = connector.send(sandbox::Message { channel: server_end });
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
            fsandbox::FactoryRequest::CreateOneShotHandle { handle, responder } => {
                let handle = self.create_one_shot_handle(handle);
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

    fn create_one_shot_handle(&self, handle: zx::Handle) -> fsandbox::OneShotHandle {
        fsandbox::OneShotHandle::from(sandbox::OneShotHandle::new(handle))
    }

    fn create_connector(
        &self,
        receiver_client: ClientEnd<fsandbox::ReceiverMarker>,
    ) -> fsandbox::Connector {
        let (receiver, sender) = Receiver::new();
        self.tasks.spawn(async move {
            receiver.handle_receiver(receiver_client.into_proxy().unwrap()).await;
        });
        fsandbox::Connector::from(sender)
    }

    fn create_dictionary(&self) -> ClientEnd<fsandbox::DictionaryMarker> {
        let (client_end, server_end) = endpoints::create_endpoints();
        let dict = Dict::new();
        let client_end_koid = server_end.basic_info().unwrap().related_koid;
        dict.serve_and_register(server_end.into_stream().unwrap(), client_end_koid);
        client_end
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
    use assert_matches::assert_matches;
    use fidl_fuchsia_component_sandbox as fsandbox;
    use fuchsia_async as fasync;
    use fuchsia_zircon::{self as zx, HandleBased};

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
    async fn create_one_shot_handle() {
        let (_tasks, factory_proxy) = factory();

        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        let one_shot = factory_proxy.create_one_shot_handle(event.into()).await.unwrap();
        let one_shot2 = fsandbox::OneShotHandle {
            token: one_shot.token.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
        };
        assert_matches!(
            factory_proxy.take_handle(one_shot).await.unwrap(),
            Ok(handle) if handle.get_koid().unwrap() == expected_koid
        );
        assert_matches!(
            factory_proxy.take_handle(one_shot2).await.unwrap(),
            Err(fsandbox::FactoryError::Unavailable)
        );

        drop(factory_proxy);
    }

    #[fuchsia::test]
    async fn create_connector() {
        let (_tasks, factory_proxy) = factory();

        let (receiver_client_end, mut receiver_stream) =
            endpoints::create_request_stream::<fsandbox::ReceiverMarker>().unwrap();
        let connector = factory_proxy.create_connector(receiver_client_end).await.unwrap();
        let (ch1, _ch2) = zx::Channel::create();
        let expected_koid = ch1.get_koid().unwrap();
        factory_proxy.open_connector(connector, ch1).unwrap();

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
        let (iterator, server_end) = endpoints::create_proxy().unwrap();
        dict.enumerate(server_end).unwrap();
        let items = iterator.get_next().await.unwrap();
        assert_eq!(items.len(), 0);
    }
}
