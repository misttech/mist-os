// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{
        framebuffer::fb_device_init, input::init_input_devices,
        perfetto_consumer::start_perfetto_consumer_thread, starnix::magma_device_init,
    },
    logging::log_warn,
    task::Kernel,
    types::*,
};
use bstr::BString;
use fuchsia_zircon as zx;
use std::sync::Arc;

use fidl_fuchsia_sysinfo as fsysinfo;
use fidl_fuchsia_ui_composition as fuicomposition;
use fidl_fuchsia_ui_input3 as fuiinput;
use fidl_fuchsia_ui_policy as fuipolicy;
use fidl_fuchsia_ui_views as fuiviews;

/// Parses and runs the features from the provided "program strvec". Some features,
/// should be enabled on a per-component basis. We run this when we first
/// make the container. When we start the component, we run the run_component_features
/// function.
pub fn run_features(entries: &Vec<String>, kernel: &Arc<Kernel>) -> Result<(), Errno> {
    let mut enabled_profiling = false;
    for entry in entries {
        let entry_type = entry.split_once(':').map(|(ty, _)| ty).unwrap_or(entry);
        match entry_type {
            "mock_selinux" => {}
            "selinux_enabled" => {}
            "selinux" => {}
            "framebuffer" => {
                fb_device_init(kernel);
                init_input_devices(kernel);
            }
            "magma" => {
                magma_device_init(kernel);
            }
            "test_data" => {}
            "custom_artifacts" => {}
            "perfetto" => {
                let socket_path = entry
                    .split_once(':')
                    .expect("Perfetto feature must have a socket path specified")
                    .1;
                start_perfetto_consumer_thread(kernel, socket_path.as_bytes())?;
            }
            "android_serialno" => {}
            "self_profile" => {
                enabled_profiling = true;
                fuchsia_inspect::component::inspector().root().record_lazy_child(
                    "self_profile",
                    fuchsia_inspect_contrib::ProfileDuration::lazy_node_callback,
                );
                fuchsia_inspect_contrib::start_self_profiling();
            }
            feature => {
                log_warn!("Unsupported feature: {:?}", feature);
            }
        }
    }
    if !enabled_profiling {
        fuchsia_inspect_contrib::stop_self_profiling();
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
                let registry_proxy = fuchsia_component::client::connect_to_protocol::<
                    fuipolicy::DeviceListenerRegistryMarker,
                >()
                .expect("Failed to connect to device listener registry");
                kernel.framebuffer.start_server(
                    view_bound_protocols,
                    view_identity,
                    outgoing_dir.take().unwrap(),
                );
                kernel.input_device.start_relay(
                    touch_source_proxy,
                    keyboard,
                    registry_proxy,
                    view_ref,
                );
            }
            "binder" => {}
            "logd" => {}
            "mock_selinux" => {}
            "selinux_enabled" => {}
            "selinux" => {}
            "magma" => {}
            "test_data" => {}
            "custom_artifacts" => {}
            "android_serialno" => {}
            feature => {
                log_warn!("Unsupported component feature: {:?}", feature);
            }
        }
    }
    Ok(())
}

pub async fn get_serial_number() -> anyhow::Result<BString> {
    let sysinfo = fuchsia_component::client::connect_to_protocol::<fsysinfo::SysInfoMarker>()?;
    let serial = sysinfo.get_serial_number().await?.map_err(zx::Status::from_raw)?;
    Ok(BString::from(serial))
}
