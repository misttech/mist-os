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
    fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd},
    fidl_fuchsia_component as fcomponent, fuchsia_zircon as zx,
    futures::{
        channel::mpsc::{unbounded, UnboundedSender},
        prelude::*,
    },
    lazy_static::lazy_static,
    namespace::NamespaceError,
    sandbox::Capability,
    serve_processargs::{BuildNamespaceError, NamespaceBuilder},
    std::sync::Arc,
    tracing::warn,
    vfs::execution_scope::ExecutionScope,
};

lazy_static! {
    static ref CAPABILITY_NAME: Name = fcomponent::NamespaceMarker::PROTOCOL_NAME.parse().unwrap();
}

struct NamespaceCapabilityProvider {
    host: Arc<NamespaceCapabilityHost>,
}

impl NamespaceCapabilityProvider {
    pub fn new(host: Arc<NamespaceCapabilityHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl InternalCapabilityProvider for NamespaceCapabilityProvider {
    async fn open_protocol(self: Box<Self>, server_end: zx::Channel) {
        let server_end = ServerEnd::<fcomponent::NamespaceMarker>::new(server_end);
        let serve_result = self.host.serve(server_end.into_stream().unwrap()).await;
        if let Err(error) = serve_result {
            // TODO: Set an epitaph to indicate this was an unexpected error.
            warn!(%error, "serve failed");
        }
    }
}

pub struct NamespaceCapabilityHost {
    namespace_scope: ExecutionScope,
}

impl Drop for NamespaceCapabilityHost {
    fn drop(&mut self) {
        self.namespace_scope.shutdown();
    }
}

impl NamespaceCapabilityHost {
    pub fn new() -> Self {
        Self { namespace_scope: ExecutionScope::new() }
    }

    pub async fn serve(
        &self,
        mut stream: fcomponent::NamespaceRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Some(request) = stream.try_next().await? {
            let method_name = request.method_name();
            let result = self.handle_request(request).await;
            match result {
                // If the error was PEER_CLOSED then we don't need to log it as a client can
                // disconnect while we are processing its request.
                Err(error) if !error.is_closed() => {
                    warn!(%method_name, %error, "Couldn't send Namespace response");
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn handle_request(
        &self,
        request: fcomponent::NamespaceRequest,
    ) -> Result<(), fidl::Error> {
        match request {
            fcomponent::NamespaceRequest::Create { entries, responder } => {
                let res = self.create(entries).await;
                responder.send(res)?;
            }
            fcomponent::NamespaceRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "fuchsia.component/Namespace received unknown method");
            }
        }
        Ok(())
    }

    async fn create(
        &self,
        entries: Vec<fcomponent::NamespaceInputEntry>,
    ) -> Result<Vec<fcomponent::NamespaceEntry>, fcomponent::NamespaceError> {
        let mut namespace_builder =
            NamespaceBuilder::new(self.namespace_scope.clone(), Self::ignore_not_found());
        for entry in entries {
            let path = entry.path;
            let dict = entry.dictionary.into_proxy().unwrap();
            let items: Vec<_> =
                dict.read().await.map_err(|_| fcomponent::NamespaceError::DictionaryRead)?;
            for item in items {
                let capability: Capability = item.value.try_into().unwrap();
                let path = namespace::Path::new(format!("{}/{}", path, item.key))
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

pub struct NamespaceFrameworkCapability {
    host: Arc<NamespaceCapabilityHost>,
}

impl NamespaceFrameworkCapability {
    pub fn new() -> Self {
        Self { host: Arc::new(NamespaceCapabilityHost::new()) }
    }
}

impl FrameworkCapability for NamespaceFrameworkCapability {
    fn matches(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&CAPABILITY_NAME)
    }

    fn new_provider(
        &self,
        _scope: WeakComponentInstance,
        _target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        Box::new(NamespaceCapabilityProvider::new(self.host.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use {
        assert_matches::assert_matches,
        fidl::endpoints::{self, ProtocolMarker, Proxy},
        fidl_fidl_examples_routing_echo as fecho, fidl_fuchsia_component_sandbox as fsandbox,
        fuchsia_async as fasync,
        fuchsia_component::client,
        futures::TryStreamExt,
        sandbox::{Dict, Receiver},
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

    #[fuchsia::test]
    async fn namespace_create() {
        let mut tasks = fasync::TaskGroup::new();

        let host = NamespaceCapabilityHost::new();
        let (namespace_proxy, stream) =
            endpoints::create_proxy_and_stream::<fcomponent::NamespaceMarker>().unwrap();
        tasks.add(fasync::Task::spawn(async move {
            host.serve(stream).await.unwrap();
        }));

        let mut namespace_pairs = vec![];
        for (path, response) in [("/svc", "first"), ("/zzz/svc", "second")] {
            // Initialize the host and sender/receiver pair.
            let (receiver, sender) = Receiver::new();

            // Serve an Echo request handler on the Receiver.
            tasks.add(fasync::Task::spawn(async move {
                loop {
                    let msg = receiver.receive().await.unwrap();
                    let stream: fecho::EchoRequestStream =
                        ServerEnd::<fecho::EchoMarker>::from(msg.channel).into_stream().unwrap();
                    handle_echo_request_stream(response, stream).await;
                }
            }));

            // Create a dictionary and add the Sender to it.
            let mut dict = Dict::new();
            dict.lock_entries()
                .insert(fecho::EchoMarker::DEBUG_NAME.to_string(), Capability::Sender(sender));

            let (dict_proxy, stream) =
                endpoints::create_proxy_and_stream::<fsandbox::DictionaryMarker>().unwrap();
            tasks.add(fasync::Task::spawn(async move {
                dict.serve_dict(stream).await.unwrap();
            }));

            namespace_pairs.push(fcomponent::NamespaceInputEntry {
                path: path.into(),
                dictionary: dict_proxy.into_channel().unwrap().into_zx_channel().into(),
            })
        }

        // Convert the dictionaries to a namespace.
        let mut namespace_entries = namespace_proxy.create(namespace_pairs).await.unwrap().unwrap();

        // Confirm that the Sender in the dictionary was converted to a service node, and we
        // can access the Echo protocol (served by the Receiver) through this node.
        let entry = namespace_entries.remove(0);
        assert_matches!(entry.path, Some(p) if p == "/svc");
        let dir = entry.directory.unwrap().into_proxy().unwrap();
        let echo = client::connect_to_protocol_at_dir_root::<fecho::EchoMarker>(&dir).unwrap();
        let response = echo.echo_string(None).await.unwrap();
        assert_matches!(response, Some(m) if m == "first");

        let entry = namespace_entries.remove(0);
        assert!(namespace_entries.is_empty());
        assert_matches!(entry.path, Some(p) if p == "/zzz/svc");
        let dir = entry.directory.unwrap().into_proxy().unwrap();
        let echo = client::connect_to_protocol_at_dir_root::<fecho::EchoMarker>(&dir).unwrap();
        let response = echo.echo_string(None).await.unwrap();
        assert_matches!(response, Some(m) if m == "second");
    }

    #[fuchsia::test]
    async fn namespace_create_err_shadow() {
        let mut tasks = fasync::TaskGroup::new();

        let host = NamespaceCapabilityHost::new();
        let (namespace_proxy, stream) =
            endpoints::create_proxy_and_stream::<fcomponent::NamespaceMarker>().unwrap();
        tasks.add(fasync::Task::spawn(async move {
            host.serve(stream).await.unwrap();
        }));

        // Two entries with a shadowing path.
        let mut namespace_pairs = vec![];
        for path in ["/svc", "/svc/shadow"] {
            // Initialize the host and sender/receiver pair.
            let (receiver, sender) = Receiver::new();

            // Serve an Echo request handler on the Receiver.
            tasks.add(fasync::Task::spawn(async move {
                while let Some(msg) = receiver.receive().await {
                    let stream: fecho::EchoRequestStream =
                        ServerEnd::<fecho::EchoMarker>::from(msg.channel).into_stream().unwrap();
                    handle_echo_request_stream("hello", stream).await;
                }
            }));

            // Create a dictionary and add the Sender to it.
            let mut dict = Dict::new();
            dict.lock_entries()
                .insert(fecho::EchoMarker::DEBUG_NAME.to_string(), Capability::Sender(sender));

            let (dict_proxy, stream) =
                endpoints::create_proxy_and_stream::<fsandbox::DictionaryMarker>().unwrap();
            tasks.add(fasync::Task::spawn(async move {
                dict.serve_dict(stream).await.unwrap();
            }));

            namespace_pairs.push(fcomponent::NamespaceInputEntry {
                path: path.into(),
                dictionary: dict_proxy.into_channel().unwrap().into_zx_channel().into(),
            })
        }

        // Try to convert the dictionaries to a namespace. Expect an error because one path
        // shadows another.
        let res = namespace_proxy.create(namespace_pairs).await.unwrap();
        assert_matches!(res, Err(fcomponent::NamespaceError::Shadow));
    }

    #[fuchsia::test]
    async fn namespace_create_err_dict_read() {
        let mut tasks = fasync::TaskGroup::new();

        let host = NamespaceCapabilityHost::new();
        let (namespace_proxy, stream) =
            endpoints::create_proxy_and_stream::<fcomponent::NamespaceMarker>().unwrap();
        tasks.add(fasync::Task::spawn(async move {
            host.serve(stream).await.unwrap();
        }));

        // Create a dictionary and close the server end.
        let (dict_proxy, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::DictionaryMarker>().unwrap();
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
