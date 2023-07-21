// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{
        binder::create_binders, framebuffer::fb_device_init, input::init_input_devices,
        starnix::magma_device_init,
    },
    logging::log_warn,
    task::Kernel,
    types::*,
};
use std::sync::Arc;

use fidl_fuchsia_ui_composition as fuicomposition;
use fidl_fuchsia_ui_input3 as fuiinput;
use fidl_fuchsia_ui_views as fuiviews;

/// Parses and runs the features from the provided "program strvec". Some features,
/// should be enabled on a per-component basis. We run this when we first
/// make the container. When we start the component, we run the run_component_features
/// function.
pub fn run_features(entries: &Vec<String>, kernel: &Arc<Kernel>) -> Result<(), Errno> {
    for entry in entries {
        match entry.as_str() {
            "binder" => {
                // Creates the various binder drivers (/dev/binder, /dev/hwbinder, /dev/vndbinder).
                create_binders(kernel)?;
            }
            "selinux_enabled" => {}
            "framebuffer" => {
                fb_device_init(kernel);
                init_input_devices(kernel);
            }
            "magma" => {
                magma_device_init(kernel);
            }
            "test_data" => {}
            "custom_artifacts" => {}
            feature => {
                log_warn!("Unsupported feature: {:?}", feature);
            }
        }
    }
    Ok(())
}

/// Runs features requested by individual components
pub fn run_component_features(
    entries: &Vec<String>,
    kernel: &Arc<Kernel>,
    outgoing_dir: &mut Option<fidl::endpoints::ServerEnd<fidl_fuchsia_io::DirectoryMarker>>,
) -> Result<(), Errno> {
    for entry in entries {
        match entry.as_str() {
            "framebuffer" => {
                let (touch_source_proxy, touch_source_stream) =
                    fidl::endpoints::create_proxy().expect("failed to create TouchSourceProxy");
                let view_bound_protocols = fuicomposition::ViewBoundProtocols {
                    touch_source: Some(touch_source_stream),
                    ..Default::default()
                };
                let view_identity = fuiviews::ViewIdentityOnCreation::from(
                    fuchsia_scenic::ViewRefPair::new().expect("Failed to create ViewRefPair"),
                );
                let view_ref = fuchsia_scenic::duplicate_view_ref(&view_identity.view_ref)
                    .expect("Failed to dup view ref.");
                let keyboard =
                    fuchsia_component::client::connect_to_protocol::<fuiinput::KeyboardMarker>()
                        .expect("Failed to connect to keyboard");
                kernel.framebuffer.start_server(
                    view_bound_protocols,
                    view_identity,
                    outgoing_dir.take().unwrap(),
                );
                kernel.input_device.start_relay(touch_source_proxy, keyboard, view_ref);
            }
            "binder" => {}
            "logd" => {}
            "selinux_enabled" => {}
            "magma" => {}
            "test_data" => {}
            "custom_artifacts" => {}
            feature => {
                log_warn!("Unsupported component feature: {:?}", feature);
            }
        }
    }
    Ok(())
}
