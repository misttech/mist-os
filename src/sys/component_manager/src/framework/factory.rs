// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::{CapabilityProvider, FrameworkCapability, InternalCapabilityProvider};
use crate::model::component::WeakComponentInstance;
use ::routing::capability_source::InternalCapability;
use async_trait::async_trait;
use cm_types::Name;
use cm_util::TaskGroup;
use fidl::endpoints::{ClientEnd, DiscoverableProtocolMarker, ServerEnd};
use fidl::epitaph::ChannelEpitaphExt;
use fidl_fuchsia_component_sandbox as fsandbox;
use fuchsia_zircon::{self as zx};
use futures::prelude::*;
use lazy_static::lazy_static;
use router_error::Explain;
use sandbox::Receiver;
use tracing::warn;

lazy_static! {
    static ref CAPABILITY_NAME: Name = fsandbox::FactoryMarker::PROTOCOL_NAME.parse().unwrap();
}

struct FactoryCapabilityProvider {
    tasks: TaskGroup,
}

#[async_trait]
impl InternalCapabilityProvider for FactoryCapabilityProvider {
    async fn open_protocol(self: Box<Self>, server_end: zx::Channel) {
        let server_end = ServerEnd::<fsandbox::FactoryMarker>::new(server_end);
        // We only need to look up the component matching this scope.
        // These operations should all work, even if the component is not running.
        let serve_result = self.serve(server_end.into_stream().unwrap()).await;
        if let Err(error) = serve_result {
            // TODO: Set an epitaph to indicate this was an unexpected error.
            warn!(%error, "serve failed");
        }
    }
}

impl FactoryCapabilityProvider {
    async fn serve(&self, mut stream: fsandbox::FactoryRequestStream) -> Result<(), fidl::Error> {
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
            fsandbox::FactoryRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "fuchsia.component.sandbox/Factory received unknown method");
            }
        }
        Ok(())
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
}

#[derive(Clone)]
pub struct Factory {
    tasks: TaskGroup,
}

impl Factory {
    pub fn new() -> Self {
        Self { tasks: TaskGroup::new() }
    }
}

impl FrameworkCapability for Factory {
    fn matches(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&CAPABILITY_NAME)
    }

    fn new_provider(
        &self,
        _scope: WeakComponentInstance,
        _target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        Box::new(FactoryCapabilityProvider { tasks: self.tasks.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability;
    use crate::model::component::ComponentInstance;
    use crate::model::context::ModelContext;
    use crate::model::environment::Environment;
    use ::routing::bedrock::structured_dict::ComponentInput;
    use fidl::endpoints;
    use fidl_fuchsia_component_sandbox as fsandbox;
    use fuchsia_zircon::{self as zx, AsHandleRef};
    use std::sync::{Arc, Weak};

    async fn new_root() -> Arc<ComponentInstance> {
        ComponentInstance::new_root(
            ComponentInput::default(),
            Environment::empty(),
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "test:///root".parse().unwrap(),
        )
        .await
    }

    async fn factory(instance: &Arc<ComponentInstance>) -> (fsandbox::FactoryProxy, Factory) {
        let host = Factory::new();
        let (proxy, server) = endpoints::create_proxy::<fsandbox::FactoryMarker>().unwrap();
        capability::open_framework(&host, instance, server.into()).await.unwrap();
        (proxy, host)
    }

    #[fuchsia::test]
    async fn create_connector() {
        let root = new_root().await;
        let (factory_proxy, _host) = factory(&root).await;

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
}
