// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::{CapabilityProvider, FrameworkCapability, FrameworkCapabilityProvider},
        model::component::WeakComponentInstance,
    },
    ::routing::capability_source::InternalCapability,
    async_trait::async_trait,
    cm_types::Name,
    cm_util::TaskGroup,
    fidl::endpoints::{ClientEnd, DiscoverableProtocolMarker, ServerEnd},
    fidl_fuchsia_component_sandbox as fsandbox,
    futures::prelude::*,
    lazy_static::lazy_static,
    sandbox::{Dict, Handle, Receiver},
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
impl FrameworkCapabilityProvider for FactoryCapabilityProvider {
    type Marker = fsandbox::FactoryMarker;
    async fn open_protocol(self: Box<Self>, server_end: ServerEnd<Self::Marker>) {
        // We only need to look up the component matching this scope.
        // These operations should all work, even if the component is not running.
        let serve_result = self.host.serve(server_end.into_stream().unwrap()).await;
        if let Err(error) = serve_result {
            // TODO: Set an epitaph to indicate this was an unexpected error.
            warn!(%error, "serve failed");
        }
    }
}

struct FactoryCapabilityHost {
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
            fsandbox::FactoryRequest::CreateConnector { sender, receiver, control_handle: _ } => {
                self.create_connector(sender, receiver);
            }
            fsandbox::FactoryRequest::CreateDict { items, server_end, responder } => {
                let res = self.create_dict(items, server_end);
                responder.send(res)?;
            }
            fsandbox::FactoryRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "fuchsia.component.sandbox/Factory received unknown method");
            }
        }
        Ok(())
    }

    fn create_connector(
        &self,
        sender_server: ServerEnd<fsandbox::SenderMarker>,
        receiver_client: ClientEnd<fsandbox::ReceiverMarker>,
    ) {
        let receiver = Receiver::<Handle>::new();
        let sender = receiver.new_sender();
        self.tasks.spawn(async move {
            receiver.handle_receiver(receiver_client.into_proxy().unwrap()).await;
        });
        self.tasks.spawn(async move {
            sender.serve_sender(sender_server.into_stream().unwrap()).await;
        });
    }

    fn create_dict(
        &self,
        items: Vec<fsandbox::DictItem>,
        server_end: ServerEnd<fsandbox::DictMarker>,
    ) -> Result<(), fsandbox::DictError> {
        let mut dict = Dict::new();
        for item in items {
            let cap = Box::new(Handle::from(item.value));
            if dict.entries.insert(item.key, cap).is_some() {
                return Err(fsandbox::DictError::AlreadyExists);
            }
        }
        self.tasks.spawn(async move {
            let _ = dict.serve_dict(server_end.into_stream().unwrap()).await;
        });
        Ok(())
    }
}

pub struct FactoryFrameworkCapability {
    host: Arc<FactoryCapabilityHost>,
}

impl FactoryFrameworkCapability {
    pub fn new() -> Self {
        Self { host: Arc::new(FactoryCapabilityHost::new()) }
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
        assert_matches::assert_matches,
        fidl::endpoints,
        fuchsia_async as fasync,
        fuchsia_zircon::{self as zx, AsHandleRef, HandleBased},
    };

    #[fuchsia::test]
    async fn create_connector() {
        let mut tasks = fasync::TaskGroup::new();

        let host = FactoryCapabilityHost::new();
        let (factory_proxy, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::FactoryMarker>().unwrap();
        tasks.spawn(async move {
            host.serve(stream).await.unwrap();
        });

        let (sender_proxy, sender_server_end) =
            endpoints::create_proxy::<fsandbox::SenderMarker>().unwrap();
        let (receiver_client_end, mut receiver_stream) =
            endpoints::create_request_stream::<fsandbox::ReceiverMarker>().unwrap();
        factory_proxy.create_connector(sender_server_end, receiver_client_end).unwrap();

        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();
        sender_proxy.send_(event.into_handle()).unwrap();

        let request = receiver_stream.try_next().await.unwrap().unwrap();
        let fsandbox::ReceiverRequest::Receive { capability, .. } = request;
        assert_eq!(capability.get_koid().unwrap(), expected_koid);
    }

    #[fuchsia::test]
    async fn create_dict() {
        let mut tasks = fasync::TaskGroup::new();

        let host = FactoryCapabilityHost::new();
        let (factory_proxy, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::FactoryMarker>().unwrap();
        tasks.spawn(async move {
            host.serve(stream).await.unwrap();
        });

        let (items, expected_koids): (Vec<_>, Vec<_>) = (0..2)
            .into_iter()
            .map(|i| {
                let event = zx::Event::create();
                let key = format!("key{}", i);
                let koid = event.get_koid().unwrap();
                let value = event.into_handle();
                (fsandbox::DictItem { key, value }, koid)
            })
            .unzip();
        let (dict_proxy, server_end) = endpoints::create_proxy::<fsandbox::DictMarker>().unwrap();
        factory_proxy.create_dict(items, server_end).await.unwrap().unwrap();

        let mut items = dict_proxy.read().await.unwrap().unwrap();
        let item = items.remove(0);
        assert_matches!(
            item,
            fsandbox::DictItem {
                key,
                value,
            }
            if key == "key0" && value.get_koid().unwrap() == expected_koids[0]
        );
        let item = items.remove(0);
        assert_matches!(
            item,
            fsandbox::DictItem {
                key,
                value,
            }
            if key == "key1" && value.get_koid().unwrap() == expected_koids[1]
        );
        assert!(items.is_empty());
    }

    #[fuchsia::test]
    async fn create_dict_err() {
        let mut tasks = fasync::TaskGroup::new();

        let host = FactoryCapabilityHost::new();
        let (factory_proxy, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::FactoryMarker>().unwrap();
        tasks.spawn(async move {
            host.serve(stream).await.unwrap();
        });

        let items: Vec<_> = (0..2)
            .into_iter()
            .map(|_| {
                let event = zx::Event::create();
                let key = "dup_key".into();
                let value = event.into_handle();
                fsandbox::DictItem { key, value }
            })
            .collect();
        let (_, server_end) = endpoints::create_proxy::<fsandbox::DictMarker>().unwrap();
        assert_eq!(
            factory_proxy.create_dict(items, server_end).await.unwrap().unwrap_err(),
            fsandbox::DictError::AlreadyExists,
        );
    }
}
