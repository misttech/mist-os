// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fdf_component::{driver_register, Driver, DriverContext, Node, NodeBuilder};
use fidl_fuchsia_hardware_i2c as i2c;
use log::{error, info};
use zx::Status;

/// The implementation of our driver will live in this object, which implements [`Driver`].
#[allow(unused)]
struct ZirconChildDriver {
    /// The [`Node`] is our handle to the node we bound to. We need to keep this handle
    /// open to keep the node around.
    node: Node,
}

// This creates the exported driver registration structures that allow the driver host to
// find and run the start and stop methods on our `ZirconChildDriver`.
driver_register!(ZirconChildDriver);

impl Driver for ZirconChildDriver {
    const NAME: &str = "zircon_child_rust_driver";

    async fn start(mut context: DriverContext) -> Result<Self, Status> {
        info!("Binding node client. Every driver needs to do this for the driver to be considered loaded.");
        let node = context.take_node()?;

        let device = get_i2c_device(&context).unwrap();
        let device_name = device.get_name().await.unwrap().unwrap();
        info!("i2c device name: {device_name}");

        info!("Adding child node with i2c device name as a property value");
        let child_node = NodeBuilder::new("transport-child")
            .add_property(bind_fuchsia_test::TEST_CHILD, device_name)
            .build();
        node.add_child(child_node).await?;

        Ok(Self { node })
    }

    async fn stop(&self) {
        info!("ZirconChildDriver::stop() was invoked. Use this function to do any cleanup needed.");
    }
}

fn get_i2c_device(context: &DriverContext) -> Result<i2c::DeviceProxy, Status> {
    let service_proxy = context.incoming.service_marker(i2c::ServiceMarker).connect()?;

    service_proxy.connect_to_device().map_err(|err| {
        error!("Error connecting to i2c device proxy at driver startup: {err}");
        Status::INTERNAL
    })
}
