// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::{CapabilityProvider, FrameworkCapability, InternalCapabilityProvider};
use crate::model::component::WeakComponentInstance;
use ::routing::capability_source::InternalCapability;
use async_trait::async_trait;
use cm_types::{Name, Path};
use fidl::endpoints::{self, DiscoverableProtocolMarker, ServerEnd};
use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::prelude::*;
use lazy_static::lazy_static;
use log::warn;
use namespace::NamespaceError;
use sandbox::Capability;
use serve_processargs::{BuildNamespaceError, NamespaceBuilder};
use vfs::execution_scope::ExecutionScope;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_sandbox as fsandbox,
    fuchsia_async as fasync,
};

lazy_static! {
    static ref CAPABILITY_NAME: Name = fcomponent::NamespaceMarker::PROTOCOL_NAME.parse().unwrap();
}

struct NamespaceCapabilityProvider {
    namespace_scope: ExecutionScope,
}

#[async_trait]
impl InternalCapabilityProvider for NamespaceCapabilityProvider {
    async fn open_protocol(self: Box<Self>, server_end: zx::Channel) {
        let server_end = ServerEnd::<fcomponent::NamespaceMarker>::new(server_end);
        let serve_result = self.serve(server_end.into_stream()).await;
        if let Err(error) = serve_result {
            // TODO: Set an epitaph to indicate this was an unexpected error.
            warn!(error:%; "serve failed");
        }
    }
}

impl Drop for Namespace {
    fn drop(&mut self) {
        self.namespace_scope.shutdown();
    }
}

impl NamespaceCapabilityProvider {
    async fn serve(
        &self,
        mut stream: fcomponent::NamespaceRequestStream,
    ) -> Result<(), fidl::Error> {
        let (store, store_stream) =
            endpoints::create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>();
        let _store_task = fasync::Task::spawn(async move {
            let _ = sandbox::serve_capability_store(store_stream).await;
        });
        while let Some(request) = stream.try_next().await? {
            let method_name = request.method_name();
            let result = self.handle_request(&store, request).await;
            match result {
                // If the error was PEER_CLOSED then we don't need to log it as a client can
                // disconnect while we are processing its request.
                Err(error) if !error.is_closed() => {
                    warn!(method_name:%, error:%; "Couldn't send Namespace response");
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn handle_request(
        &self,
        store: &fsandbox::CapabilityStoreProxy,
        request: fcomponent::NamespaceRequest,
    ) -> Result<(), fidl::Error> {
        match request {
            fcomponent::NamespaceRequest::Create { entries, responder } => {
                let res = self.create(store, entries).await;
                responder.send(res)?;
            }
            fcomponent::NamespaceRequest::_UnknownMethod { ordinal, .. } => {
                warn!(ordinal:%; "fuchsia.component/Namespace received unknown method");
            }
        }
        Ok(())
    }

    async fn create(
        &self,
        store: &fsandbox::CapabilityStoreProxy,
        entries: Vec<fcomponent::NamespaceInputEntry>,
    ) -> Result<Vec<fcomponent::NamespaceEntry>, fcomponent::NamespaceError> {
        let mut namespace_builder =
            NamespaceBuilder::new(self.namespace_scope.clone(), Self::ignore_not_found());
        for entry in entries {
            const ERR: fcomponent::NamespaceError = fcomponent::NamespaceError::DictionaryRead;

            // This API accepts legacy [Dictionary] channel. Round-trip through the import/export
            // CapabilityStore API to convert the channel to a local Dict object that we can
            // enumerate.
            let path = entry.path;
            let dict_id = 1;
            store
                .dictionary_legacy_import(dict_id, entry.dictionary.into())
                .await
                .map_err(|_| ERR)?
                .map_err(|_| ERR)?;
            let dict = store.export(dict_id).await.map_err(|_| ERR)?.map_err(|_| ERR)?;
            let dict = Capability::try_from(dict).map_err(|_| ERR)?;
            let Capability::Dictionary(dict) = dict else {
                return Err(ERR);
            };
            for (key, capability) in dict.enumerate() {
                let capability = capability.map_err(|_| fcomponent::NamespaceError::Conversion)?;
                let path = Path::new(format!("{}/{}", path, key))
                    .map_err(|_| fcomponent::NamespaceError::BadEntry)?;
                namespace_builder.add_object(capability, &path).map_err(Self::error_to_fidl)?;
            }
        }
        let namespace = namespace_builder.serve().map_err(Self::error_to_fidl)?;
        let out = namespace.flatten().into_iter().map(Into::into).collect();
        Ok(out)
    }

    fn error_to_fidl(e: BuildNamespaceError) -> fcomponent::NamespaceError {
        match e {
            BuildNamespaceError::NamespaceError(e) => match e {
                NamespaceError::Shadow(_) => fcomponent::NamespaceError::Shadow,
                NamespaceError::Duplicate(_) => fcomponent::NamespaceError::Duplicate,
                NamespaceError::EntryError(_) => fcomponent::NamespaceError::BadEntry,
            },
            BuildNamespaceError::Conversion { .. } | BuildNamespaceError::Serve { .. } => {
                fcomponent::NamespaceError::Conversion
            }
        }
    }

    fn ignore_not_found() -> UnboundedSender<String> {
        let (sender, _receiver) = unbounded();
        sender
    }
}

pub struct Namespace {
    namespace_scope: ExecutionScope,
}

impl Namespace {
    pub fn new() -> Self {
        Self { namespace_scope: ExecutionScope::new() }
    }
}

impl FrameworkCapability for Namespace {
    fn matches(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&CAPABILITY_NAME)
    }

    fn new_provider(
        &self,
        _scope: WeakComponentInstance,
        _target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        Box::new(NamespaceCapabilityProvider { namespace_scope: self.namespace_scope.clone() })
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
    use assert_matches::assert_matches;
    use fidl::endpoints::{ProtocolMarker, Proxy};
    use fuchsia_component::client;
    use futures::TryStreamExt;
    use sandbox::{Connector, Dict};
    use std::sync::{Arc, Weak};
    use {
        fidl_fidl_examples_routing_echo as fecho, fidl_fuchsia_component_sandbox as fsandbox,
        fuchsia_async as fasync,
    };

    async fn handle_echo_request_stream(response: &str, mut stream: fecho::EchoRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fecho::EchoRequest::EchoString { value: _, responder } => {
                    responder.send(Some(response)).unwrap();
                }
            }
        }
    }

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

    async fn namespace(
        instance: &Arc<ComponentInstance>,
    ) -> (fcomponent::NamespaceProxy, Namespace) {
        let host = Namespace::new();
        let (proxy, server) = endpoints::create_proxy::<fcomponent::NamespaceMarker>();
        capability::open_framework(&host, instance, server.into()).await.unwrap();
        (proxy, host)
    }

    #[fuchsia::test]
    async fn namespace_create() {
        let mut tasks = fasync::TaskGroup::new();
        let root = new_root().await;
        let (namespace_proxy, _host) = namespace(&root).await;

        let (store, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>();
        tasks.spawn(async move { sandbox::serve_capability_store(stream).await.unwrap() });

        let mut namespace_pairs = vec![];
        let mut next_id = 1;
        for (path, response) in [("/svc", "first"), ("/zzz/svc", "second")] {
            // Initialize the host and sender/receiver pair.
            let (receiver, sender) = Connector::new();

            // Serve an Echo request handler on the Receiver.
            tasks.spawn(async move {
                loop {
                    let msg = receiver.receive().await.unwrap();
                    let stream: fecho::EchoRequestStream =
                        ServerEnd::<fecho::EchoMarker>::from(msg.channel).into_stream();
                    handle_echo_request_stream(response, stream).await;
                }
            });

            // Create a dictionary and add the Sender to it.
            let dict = Dict::new();
            dict.insert(
                fecho::EchoMarker::DEBUG_NAME.parse().unwrap(),
                Capability::Connector(sender),
            )
            .expect("dict entry already exists");

            let dict_id = next_id;
            next_id += 1;
            store.import(dict_id, Capability::from(dict).into()).await.unwrap().unwrap();
            let (client_end, server_end) = fidl::Channel::create();
            store.dictionary_legacy_export(dict_id, server_end).await.unwrap().unwrap();

            namespace_pairs.push(fcomponent::NamespaceInputEntry {
                path: path.into(),
                dictionary: client_end.into(),
            })
        }

        // Convert the dictionaries to a namespace.
        let mut namespace_entries = namespace_proxy.create(namespace_pairs).await.unwrap().unwrap();

        // Confirm that the Sender in the dictionary was converted to a service node, and we
        // can access the Echo protocol (served by the Receiver) through this node.
        let entry = namespace_entries.remove(0);
        assert_matches!(entry.path, Some(p) if p == "/svc");
        let dir = entry.directory.unwrap().into_proxy();
        let echo = client::connect_to_protocol_at_dir_root::<fecho::EchoMarker>(&dir).unwrap();
        let response = echo.echo_string(None).await.unwrap();
        assert_matches!(response, Some(m) if m == "first");

        let entry = namespace_entries.remove(0);
        assert!(namespace_entries.is_empty());
        assert_matches!(entry.path, Some(p) if p == "/zzz/svc");
        let dir = entry.directory.unwrap().into_proxy();
        let echo = client::connect_to_protocol_at_dir_root::<fecho::EchoMarker>(&dir).unwrap();
        let response = echo.echo_string(None).await.unwrap();
        assert_matches!(response, Some(m) if m == "second");
    }

    #[fuchsia::test]
    async fn namespace_create_err_shadow() {
        let mut tasks = fasync::TaskGroup::new();
        let root = new_root().await;
        let (namespace_proxy, _host) = namespace(&root).await;

        let (store, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>();
        tasks.spawn(async move { sandbox::serve_capability_store(stream).await.unwrap() });

        // Two entries with a shadowing path.
        let mut namespace_pairs = vec![];
        let mut next_id = 1;
        for path in ["/svc", "/svc/shadow"] {
            // Initialize the host and sender/receiver pair.
            let (receiver, sender) = Connector::new();

            // Serve an Echo request handler on the Receiver.
            tasks.spawn(async move {
                while let Some(msg) = receiver.receive().await {
                    let stream: fecho::EchoRequestStream =
                        ServerEnd::<fecho::EchoMarker>::from(msg.channel).into_stream();
                    handle_echo_request_stream("hello", stream).await;
                }
            });

            // Create a dictionary and add the Sender to it.
            let dict = Dict::new();
            dict.insert(
                fecho::EchoMarker::DEBUG_NAME.parse().unwrap(),
                Capability::Connector(sender),
            )
            .expect("dict entry already exists");

            let dict_id = next_id;
            next_id += 1;
            store.import(dict_id, Capability::from(dict).into()).await.unwrap().unwrap();
            let (client_end, server_end) = fidl::Channel::create();
            store.dictionary_legacy_export(dict_id, server_end).await.unwrap().unwrap();

            namespace_pairs.push(fcomponent::NamespaceInputEntry {
                path: path.into(),
                dictionary: client_end.into(),
            })
        }

        // Try to convert the dictionaries to a namespace. Expect an error because one path
        // shadows another.
        let res = namespace_proxy.create(namespace_pairs).await.unwrap();
        assert_matches!(res, Err(fcomponent::NamespaceError::Shadow));
    }

    #[fuchsia::test]
    async fn namespace_create_err_dict_read() {
        let root = new_root().await;
        let (namespace_proxy, _host) = namespace(&root).await;

        // Create a dictionary and close the server end.
        let (dict_proxy, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::DictionaryMarker>();
        drop(stream);
        let namespace_pairs = vec![fcomponent::NamespaceInputEntry {
            path: "/svc".into(),
            dictionary: dict_proxy.into_channel().unwrap().into_zx_channel().into(),
        }];

        // Try to convert the dictionaries to a namespace. Expect an error because the dictionary
        // was unreadable.
        let res = namespace_proxy.create(namespace_pairs).await.unwrap();
        assert_matches!(res, Err(fcomponent::NamespaceError::DictionaryRead));
    }
}
