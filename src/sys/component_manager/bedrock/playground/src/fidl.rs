// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Test that creates a Dict with a Sender for a protocol, converts the Dict to a namespace, and
//! verifies that client can connect to and use the protocol through the namespace.
//!
//! Extracted from https://fxrev.dev/886692

#[cfg(test)]
mod test {
    use anyhow::Context;
    use assert_matches::assert_matches;
    use fidl::endpoints::{create_proxy_and_stream, ProtocolMarker, Proxy, ServerEnd};
    use fidl_fidl_examples_routing_echo as fecho;
    use fidl_fuchsia_component as fcomponent;
    use fidl_fuchsia_component_sandbox as fsandbox;
    use fuchsia_async as fasync;
    use fuchsia_component::client;
    use fuchsia_zircon as zx;
    use futures::{
        channel::mpsc::{unbounded, UnboundedSender},
        future::BoxFuture,
        prelude::*,
    };
    use sandbox::{AnyCapability, Capability, Dict, Receiver};
    use serve_processargs::NamespaceBuilder;

    fn ignore_not_found() -> UnboundedSender<String> {
        let (sender, _receiver) = unbounded();
        sender
    }

    async fn handle_receiver<T, Fut, F>(
        mut receiver_stream: fsandbox::ReceiverRequestStream,
        request_stream_handler: F,
    ) where
        T: fidl::endpoints::ProtocolMarker,
        Fut: Future<Output = ()> + Send,
        F: Fn(T::RequestStream) -> Fut + Send + Sync + Copy + 'static,
    {
        let mut task_group = fasync::TaskGroup::new();
        while let Some(request) = receiver_stream.try_next().await.unwrap() {
            match request {
                fsandbox::ReceiverRequest::Receive { capability, control_handle: _ } => {
                    task_group.spawn(async move {
                        let server_end = ServerEnd::<T>::new(fidl::Channel::from(capability));
                        let stream: T::RequestStream = server_end.into_stream().unwrap();
                        request_stream_handler(stream).await;
                    });
                }
            }
        }
    }

    async fn handle_echo_request_stream(response: &str, mut stream: fecho::EchoRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fecho::EchoRequest::EchoString { value: _, responder } => {
                    responder.send(Some(response)).unwrap();
                }
            }
        }
    }

    async fn create_from_dicts(
        entries: Vec<fcomponent::NamespaceDictPair>,
    ) -> Result<Vec<fcomponent::NamespaceEntry>, anyhow::Error> {
        let mut namespace_builder = NamespaceBuilder::new(ignore_not_found());
        for entry in entries {
            let path = entry.path;
            let dict = entry.dict.into_proxy().unwrap();
            let items: Vec<_> = dict
                .read()
                .await
                .context("failed to call Read")?
                .expect("failed to read dict entries");
            for item in items {
                let capability: AnyCapability = item.value.try_into().unwrap();
                let path =
                    namespace::Path::new(format!("{}/{}", path, item.key)).context("bad path")?;
                namespace_builder.add_object(capability, &path).context("failed to add object")?;
            }
        }
        let (namespace, task) = namespace_builder.serve().context("failed to serve namespace")?;
        fasync::Task::spawn(task).detach();
        let out = namespace
            .flatten()
            .into_iter()
            .map(|entry| fcomponent::NamespaceEntry {
                path: Some(entry.path.into()),
                directory: Some(entry.directory),
                ..Default::default()
            })
            .collect();
        Ok(out)
    }

    #[derive(Capability, Debug)]
    struct Handle {
        handle: zx::Handle,
    }

    impl From<zx::Handle> for Handle {
        fn from(handle: zx::Handle) -> Self {
            Self { handle }
        }
    }

    impl Capability for Handle {
        fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
            (self.handle, None)
        }
    }

    #[fuchsia::test]
    async fn sender_fidl_interface() {
        // let dict = Dict::new();
        let mut namespace_pairs = vec![];

        let path = "/svc";
        let response = "first";

        // Initialize the host and sender/receiver pair.
        let receiver: Receiver<Handle> = Receiver::new();
        let sender = receiver.new_sender();

        // Serve an Echo request handler on the Receiver.
        let (handle, receiver_fut) = receiver.to_zx_handle();
        let receiver_stream =
            ServerEnd::<fsandbox::ReceiverMarker>::new(fidl::Channel::from(handle));
        let receiver_stream = receiver_stream.into_stream().unwrap();
        fasync::Task::spawn(async move {
            let t1 = receiver_fut.unwrap();
            let t2 = handle_receiver::<fecho::EchoMarker, _, _>(receiver_stream, |stream| {
                handle_echo_request_stream(response, stream)
            });
            futures::join!(t1, t2);
        })
        .detach();

        // Create a dictionary and add the Sender to it.
        let mut dict = Dict::new();
        let (dict_proxy, stream) = create_proxy_and_stream::<fsandbox::DictMarker>().unwrap();
        fasync::Task::spawn(async move {
            dict.serve_dict(stream).await.unwrap();
        })
        .detach();
        let (handle, _) = sender.to_zx_handle();
        // let cap = fsandbox::Capability { handle, type_: fsandbox::CapabilityType::Sender };
        dict_proxy.insert(fecho::EchoMarker::DEBUG_NAME, handle).await.unwrap().unwrap();
        namespace_pairs.push(fcomponent::NamespaceDictPair {
            path: path.into(),
            dict: dict_proxy.into_channel().unwrap().into_zx_channel().into(),
        });

        // Convert to namespace entries.
        let mut namespace_entries = create_from_dicts(namespace_pairs).await.unwrap();

        // Confirm that the Sender in the dictionary was converted to a service node, and we
        // can access the Echo protocol (served by the Receiver) through this node.
        let entry = namespace_entries.remove(0);
        assert_matches!(entry.path, Some(p) if p == "/svc");
        let dir = entry.directory.unwrap().into_proxy().unwrap();
        let echo = client::connect_to_protocol_at_dir_root::<fecho::EchoMarker>(&dir).unwrap();
        let response = echo.echo_string(None).await.unwrap();
        assert_matches!(response, Some(m) if m == "first");
    }
}
