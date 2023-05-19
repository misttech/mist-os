// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::{binder::create_binders, starnix::StarnixDevice};
use crate::fs::{devtmpfs::dev_tmp_fs, SpecialNode};
use crate::logging::log_warn;
use crate::task::CurrentTask;
use crate::types::*;

use fidl_fuchsia_ui_composition as fuicomposition;

/// Parses and runs the features from the provided "program strvec". Some features,
/// should be enabled on a per-component basis. We run this when we first
/// make the container. When we start the component, we run the run_component_features
/// function.
pub fn run_features(entries: &Vec<String>, current_task: &CurrentTask) -> Result<(), Errno> {
    for entry in entries {
        match entry.as_str() {
            "binder" => {
                // Creates the various binder drivers (/dev/binder, /dev/hwbinder, /dev/vndbinder).
                create_binders(current_task)?;
            }
            "selinux_enabled" => {}
            "framebuffer" => {
                let kernel = current_task.kernel();
                let mut dev_reg = kernel.device_registry.write();

                // Register a framebuffer.
                dev_reg.register_chrdev_major(kernel.framebuffer.clone(), FB_MAJOR)?;

                // Also register an input device, which can be used to read pointer events
                // associated with the framebuffer's `View`.
                //
                // Note: input requires the `framebuffer` feature, because Starnix cannot receive
                // input events without a Fuchsia `View`.
                //
                // TODO(quiche): When adding support for multiple input devices, ensure
                // that the appropriate `InputFile` is associated with the appropriate
                // `INPUT_MINOR`.
                dev_reg.register_chrdev_major(kernel.input_file.clone(), INPUT_MAJOR)?;
            }
            "magma" => {
                // Add the starnix device group.
                current_task
                    .kernel()
                    .device_registry
                    .write()
                    .register_chrdev_major(StarnixDevice, STARNIX_MAJOR)?;

                dev_tmp_fs(current_task).root().add_node_ops_dev(
                    current_task,
                    b"magma0",
                    mode!(IFCHR, 0o600),
                    DeviceType::new(STARNIX_MAJOR, STARNIX_MINOR_MAGMA),
                    SpecialNode,
                )?;
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
    current_task: &CurrentTask,
    outgoing_dir: &mut Option<fidl::endpoints::ServerEnd<fidl_fuchsia_io::DirectoryMarker>>,
) -> Result<(), Errno> {
    for entry in entries {
        match entry.as_str() {
            "framebuffer" => {
                let kernel = current_task.kernel();
                let (touch_source_proxy, touch_source_stream) =
                    fidl::endpoints::create_proxy().expect("failed to create TouchSourceProxy");
                let view_bound_protocols = fuicomposition::ViewBoundProtocols {
                    touch_source: Some(touch_source_stream),
                    ..Default::default()
                };
                kernel.framebuffer.start_server(view_bound_protocols, outgoing_dir.take().unwrap());
                kernel.input_file.start_relay(touch_source_proxy);
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
