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
    fidl::endpoints::{ClientEnd, DiscoverableProtocolMarker, ServerEnd},
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    fuchsia_zircon::AsHandleRef,
    futures::prelude::*,
    lazy_static::lazy_static,
    sandbox::{Capability, Dict, Directory, Open, Receiver},
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
            fsandbox::FactoryRequest::CreateConnector { sender, receiver, control_handle: _ } => {
                self.create_connector(sender, receiver);
            }
            fsandbox::FactoryRequest::CreateDictionary { items, server_end, responder } => {
                let res = self.create_dictionary(items, server_end);
                responder.send(res)?;
            }
            fsandbox::FactoryRequest::CreateOpen { client_end, server_end, responder } => {
                self.create_open(client_end, server_end);
                responder.send()?;
            }
            fsandbox::FactoryRequest::CreateDirectory { client_end, responder } => {
                let capability = self.create_directory(client_end);
                responder.send(capability)?;
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
        let (receiver, sender) = Receiver::new();
        self.tasks.spawn(async move {
            receiver.handle_receiver(receiver_client.into_proxy().unwrap()).await;
        });
        let sender_client_end_koid = sender_server.basic_info().unwrap().related_koid;
        sender.serve_and_register(sender_server.into_stream().unwrap(), sender_client_end_koid);
    }

    fn create_open(
        &self,
        openable_client: ClientEnd<fio::OpenableMarker>,
        openable_server: ServerEnd<fio::OpenableMarker>,
    ) {
        let open: Open = openable_client.into();
        let client_end_koid = openable_server.basic_info().unwrap().related_koid;
        open.serve_and_register(openable_server.into_stream().unwrap(), client_end_koid);
    }

    fn create_directory(
        &self,
        client_end: ClientEnd<fio::DirectoryMarker>,
    ) -> fsandbox::Capability {
        let directory = Directory::new(client_end);
        // This will add the Directory to the registry.
        let client_end: ClientEnd<fio::DirectoryMarker> = directory.into();
        fsandbox::Capability::Directory(client_end)
    }

    fn create_dictionary(
        &self,
        items: Vec<fsandbox::DictionaryItem>,
        server_end: ServerEnd<fsandbox::DictionaryMarker>,
    ) -> Result<(), fsandbox::DictionaryError> {
        let dict = Dict::new();
        let mut entries = dict.lock_entries();
        for item in items {
            let cap = Capability::try_from(item.value)
                .map_err(|_| fsandbox::DictionaryError::BadCapability)?;
            if entries.insert(item.key, cap).is_some() {
                return Err(fsandbox::DictionaryError::AlreadyExists);
            }
        }
        drop(entries);
        let client_end_koid = server_end.basic_info().unwrap().related_koid;
        dict.serve_and_register(server_end.into_stream().unwrap(), client_end_koid);
        Ok(())
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
        fidl::endpoints,
        fidl_fuchsia_io as fio, fuchsia_async as fasync,
        fuchsia_zircon::{self as zx, HandleBased},
        sandbox::OneShotHandle,
    };

    async fn get_handle_from_fidl_capability(capability: fsandbox::Capability) -> zx::Handle {
        let fsandbox::Capability::Handle(handle_capability) = capability else {
            panic!("unexpected FIDL Capability variant");
        };
        let handle_capability = handle_capability.into_proxy().unwrap();
        handle_capability
            .get_handle()
            .await
            .expect("failed to call GetHandle")
            .expect("GetHandle returned an error")
    }

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

        let (ch1, _ch2) = zx::Channel::create();
        let expected_koid = ch1.get_koid().unwrap();
        sender_proxy.send_(ch1, fio::OpenFlags::empty()).unwrap();

        let request = receiver_stream.try_next().await.unwrap().unwrap();
        if let fsandbox::ReceiverRequest::Receive { channel, .. } = request {
            assert_eq!(channel.get_koid().unwrap(), expected_koid);
        } else {
            panic!("unexpected request");
        }
    }

    #[fuchsia::test]
    async fn create_directory() {
        let mut tasks = fasync::TaskGroup::new();

        let host = FactoryCapabilityHost::new();
        let (factory_proxy, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::FactoryMarker>().unwrap();
        tasks.spawn(async move {
            host.serve(stream).await.unwrap();
        });

        let (client_end, _server_end) = endpoints::create_endpoints::<fio::DirectoryMarker>();
        let expected_koid = client_end.get_koid().unwrap();
        let directory = factory_proxy.create_directory(client_end).await.unwrap();
        let fsandbox::Capability::Directory(directory) = directory else {
            panic!("wrong type");
        };
        assert_eq!(directory.get_koid().unwrap(), expected_koid);
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

        // Create a dict with items that contain `OneShotHandle`s, and get the handle koids.
        let (items, expected_koids): (Vec<_>, Vec<_>) = (0..2)
            .into_iter()
            .map(|i| {
                let event = zx::Event::create();
                let key = format!("key{}", i);
                let koid = event.get_koid().unwrap();
                let value: fsandbox::Capability = OneShotHandle::from(event.into_handle()).into();
                (fsandbox::DictionaryItem { key, value }, koid)
            })
            .unzip();
        let (dict_proxy, server_end) =
            endpoints::create_proxy::<fsandbox::DictionaryMarker>().unwrap();
        factory_proxy.create_dictionary(items, server_end).await.unwrap().unwrap();

        // Read the items back from the dict.
        let items = dict_proxy.read().await.unwrap();

        assert_eq!(items.len(), 2);

        // Each item's key koid should match the item passed to CreateDict.
        for (i, item) in items.into_iter().enumerate() {
            assert_eq!(item.key, format!("key{}", i));
            let handle = get_handle_from_fidl_capability(item.value).await;
            assert!(handle.get_koid().unwrap() == expected_koids[i]);
        }
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
                let value: fsandbox::Capability = OneShotHandle::from(event.into_handle()).into();
                fsandbox::DictionaryItem { key, value }
            })
            .collect();
        let (_, server_end) = endpoints::create_proxy::<fsandbox::DictionaryMarker>().unwrap();
        assert_eq!(
            factory_proxy.create_dictionary(items, server_end).await.unwrap().unwrap_err(),
            fsandbox::DictionaryError::AlreadyExists,
        );
    }
}
