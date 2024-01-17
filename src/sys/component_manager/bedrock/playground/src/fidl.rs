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
    use fidl::endpoints::{ClientEnd, ProtocolMarker, ServerEnd};
    use fidl_fidl_examples_routing_echo as fecho;
    use fidl_fuchsia_component as fcomponent;
    use fidl_fuchsia_component_sandbox as fsandbox;
    use fuchsia_async as fasync;
    use fuchsia_component::client;
    use futures::{
        channel::mpsc::{unbounded, UnboundedSender},
        prelude::*,
    };
    use sandbox::{AnyCapability, Dict, Receiver};
    use serve_processargs::NamespaceBuilder;
    use vfs::execution_scope::ExecutionScope;

    fn ignore_not_found() -> UnboundedSender<String> {
        let (sender, _receiver) = unbounded();
        sender
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
        let mut namespace_builder =
            NamespaceBuilder::new(ExecutionScope::new(), ignore_not_found());
        for entry in entries {
            let path = entry.path;
            let dict = entry.dict.into_proxy().unwrap();
            let items: Vec<_> = dict.read().await.context("failed to call Read")?;
            for item in items {
                let capability: AnyCapability = item.value.try_into().unwrap();
                let path =
                    namespace::Path::new(format!("{}/{}", path, item.key)).context("bad path")?;
                namespace_builder.add_object(capability, &path).context("failed to add object")?;
            }
        }
        let namespace = namespace_builder.serve().context("failed to serve namespace")?;
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

    #[fuchsia::test]
    async fn sender_fidl_interface() {
        let mut namespace_pairs = vec![];

        let path = "/svc";
        let response = "first";

        // Initialize the host and sender/receiver pair.
        let (receiver, sender) = Receiver::<()>::new();

        // Serve an Echo request handler on the Receiver.
        fasync::Task::spawn(async move {
            loop {
                let msg = receiver.receive().await.unwrap();
                let stream: fecho::EchoRequestStream =
                    ServerEnd::<fecho::EchoMarker>::from(msg.payload.channel)
                        .into_stream()
                        .unwrap();
                handle_echo_request_stream(response, stream).await;
            }
        })
        .detach();

        // Create a dictionary and add the Sender to it.
        let dict = Dict::new();
        dict.lock_entries().insert(fecho::EchoMarker::DEBUG_NAME.to_string(), Box::new(sender));

        // Add the Dict into the namespace.
        let dict_client_end: ClientEnd<fsandbox::DictMarker> = dict.into();
        namespace_pairs.push(fcomponent::NamespaceDictPair {
            path: path.into(),
            dict: dict_client_end.into_channel().into(),
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
