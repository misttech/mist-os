// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fdf_component::{driver_register, Driver, DriverContext, Node, NodeBuilder};
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_driver_framework::NodeMarker;
use tracing::info;
use zx::Status;

/// The implementation of our driver will live in this object, which implements [`Driver`].
#[allow(unused)]
struct SimpleRustDriver {
    /// The [`NodeProxy`] is our handle to the node we bound to. We need to keep this handle
    /// open to keep the node around.
    node: Node,
    /// After creating a child node, we need to keep a handle to its [`ClientEnd`] so the
    /// node isn't removed.
    child_node: ClientEnd<NodeMarker>,
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
        let node = context.take_node()?;

        info!("Creating an owned child node with a property");
        let node_args = NodeBuilder::new("simple_child")
            .add_property(bind_fuchsia_test::TEST_CHILD, "simple")
            .build();
        let (child_node, _) = node.add_owned_child(node_args).await?;

        Ok(Self { node, child_node })
    }

    async fn stop(&self) {
        info!("SimpleRustDriver::stop() was invoked. Use this function to do any cleanup needed.");
    }
}
