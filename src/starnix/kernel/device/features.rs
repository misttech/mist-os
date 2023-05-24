// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::{binder::create_binders, starnix::StarnixDevice};
use crate::fs::sysfs::sys_fs;
use crate::fs::{
    devtmpfs::dev_tmp_fs,
    kobject::{KObjectDeviceAttribute, KType},
    SpecialNode,
};
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

                // Add framebuffer and input devices in sysfs.
                let sysfs = sys_fs(kernel);
                sysfs.add_device(
                    sysfs.virtual_bus().get_or_create_child(b"graphics", KType::Class),
                    KObjectDeviceAttribute::new(b"fb0", b"fb0", DeviceType::FB0),
                );
                sysfs.add_device(
                    sysfs.virtual_bus().get_or_create_child(b"input", KType::Class),
                    KObjectDeviceAttribute::new(
                        b"event0",
                        b"input/event0",
                        DeviceType::new(INPUT_MAJOR, 0),
                    ),
                );

                let mut device_registry = kernel.device_registry.write();
                // Register a framebuffer.
                device_registry.register_chrdev_major(kernel.framebuffer.clone(), FB_MAJOR)?;

                // Also register an input device, which can be used to read pointer events
                // associated with the framebuffer's `View`.
                //
                // Note: input requires the `framebuffer` feature, because Starnix cannot receive
                // input events without a Fuchsia `View`.
                //
                // TODO(quiche): When adding support for multiple input devices, ensure
                // that the appropriate `InputFile` is associated with the appropriate
                // `INPUT_MINOR`.
                device_registry.register_chrdev_major(kernel.input_file.clone(), INPUT_MAJOR)?;
            }
            "magma" => {
                let sysfs = sys_fs(current_task.kernel());
                let magma_type = DeviceType::new(STARNIX_MAJOR, STARNIX_MINOR_MAGMA);
                sysfs.add_device(
                    sysfs.virtual_bus().get_or_create_child(b"starnix", KType::Class),
                    KObjectDeviceAttribute::new(b"magma0", b"magma0", magma_type),
                );

                // Add the starnix device group.
                current_task
                    .kernel()
                    .device_registry
                    .write()
                    .register_chrdev_major(StarnixDevice, STARNIX_MAJOR)?;

                // TODO(fxb/119437): Remove after devtmpfs listens to uevent.
                dev_tmp_fs(current_task).root().add_node_ops_dev(
                    current_task,
                    b"magma0",
                    mode!(IFCHR, 0o600),
                    magma_type,
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
