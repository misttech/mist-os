// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fdf_component::{driver_register, Driver, DriverContext};
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_driver_framework::{
    NodeAddArgs, NodeControllerMarker, NodeProperty, NodePropertyKey, NodePropertyValue, NodeProxy,
};
use tracing::{error, info};
use zx::Status;

/// The implementation of our driver will live in this object, which implements [`Driver`].
#[allow(unused)]
struct SimpleRustDriver {
    /// The [`NodeProxy`] is our handle to the node we bound to. We need to keep this handle
    /// open to keep the node around.
    node: NodeProxy,
    /// After creating a child node, we need to keep a handle to its [`ClientEnd`] so the
    /// node isn't removed.
    child_controller: ClientEnd<NodeControllerMarker>,
}

// This creates the exported driver registration structures that allow the driver host to
// find and run the start and stop methods on our `SimpleRustDriver`.
driver_register!(SimpleRustDriver);

impl Driver for SimpleRustDriver {
    const NAME: &str = "simple_rust_driver";

    async fn start(mut context: DriverContext) -> Result<Self, Status> {
        info!(concat!(
            "SimpleRustDriver::start() was invoked. Use this function to do basic initialization ",
            "like taking ownership over the node proxy, creating children, and connecting ",
            "to resources in the incoming namespace or serving resources to the ",
            "outgoing namespace."
        ));

        info!("Binding node client. Every driver needs to do this for the driver to be considered loaded.");
        let node_client = context.start_args.node.take().ok_or(Status::INVALID_ARGS)?;
        // TODO(https://fxbug.dev/319159026): when this is infallible the expect can be removed.
        let node = node_client.into_proxy().expect("into_proxy failed");

        info!("Creating child node with a property");
        let property = NodeProperty {
            key: NodePropertyKey::StringValue(bind_fuchsia_test::TEST_CHILD.to_owned()),
            value: NodePropertyValue::StringValue("simple".to_owned()),
        };
        let args = NodeAddArgs {
            properties: Some(vec![property]),
            name: Some("simple_child".to_owned()),
            ..Default::default()
        };
        let (child_controller, server_end) = fidl::endpoints::create_endpoints();
        node.add_child(args, server_end, None)
            .await
            .map_err(|err| {
                error!("transport error trying to create child node: {err}");
                Status::INTERNAL
            })?
            .map_err(|err| {
                error!("failed to create child node: {err:?}");
                Status::INTERNAL
            })?;

        Ok(Self { node, child_controller })
    }

    async fn stop(&self) {
        info!("SimpleRustDriver::stop() was invoked. Use this function to do any cleanup needed.");
    }
}
