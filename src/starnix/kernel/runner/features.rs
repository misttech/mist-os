// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Error};
use bstr::BString;
use fuchsia_zircon as zx;
use gpu::gpu_device_init;
use gralloc::gralloc_device_init;
use magma_device::magma_device_init;
use selinux::security_server;
use starnix_core::{
    device::{
        ashmem::ashmem_device_init,
        framebuffer::{fb_device_init, AspectRatio},
        input::init_input_devices,
        perfetto_consumer::start_perfetto_consumer_thread,
    },
    task::{CurrentTask, Kernel, KernelFeatures},
    vfs::FsString,
};
use starnix_sync::{Locked, Unlocked};
use starnix_uapi::{error, errors::Errno};
use std::sync::Arc;

use fidl_fuchsia_io as fio;
use fidl_fuchsia_sysinfo as fsysinfo;
use fidl_fuchsia_ui_composition as fuicomposition;
use fidl_fuchsia_ui_input3 as fuiinput;
use fidl_fuchsia_ui_policy as fuipolicy;
use fidl_fuchsia_ui_views as fuiviews;

/// A collection of parsed features, and their arguments.
#[derive(Default, Debug)]
pub struct Features {
    pub kernel: KernelFeatures,

    /// Configures whether SELinux is fully enabled, faked, or unavailable.
    pub selinux: Option<security_server::Mode>,

    pub ashmem: bool,

    pub framebuffer: bool,

    pub gralloc: bool,

    pub magma: bool,

    pub gfxstream: bool,

    pub test_data: bool,

    pub custom_artifacts: bool,

    pub android_serialno: bool,

    pub self_profile: bool,

    pub aspect_ratio: Option<AspectRatio>,

    pub perfetto: Option<FsString>,
}

/// Parses all the featurse in `entries`.
///
/// Returns an error if parsing fails, or if an unsupported feature is present in `features`.
pub fn parse_features(entries: &Vec<String>) -> Result<Features, Error> {
    let mut features = Features::default();
    for entry in entries {
        let (raw_flag, raw_args) =
            entry.split_once(':').map(|(f, a)| (f, Some(a.to_string()))).unwrap_or((entry, None));
        match (raw_flag, raw_args) {
            ("android_serialno", _) => features.android_serialno = true,
            ("aspect_ratio", Some(args)) => {
                let e = anyhow!("Invalid aspect_ratio: {:?}", args);
                let components: Vec<_> = args.split(':').collect();
                if components.len() != 2 {
                    return Err(e);
                }
                let width: u32 = components[0].parse().map_err(|_| anyhow!("Invalid aspect ratio width"))?;
                let height: u32 = components[1].parse().map_err(|_| anyhow!("Invalid aspect ratio height"))?;
                features.aspect_ratio = Some(AspectRatio { width, height });
            }
            ("aspect_ratio", None) => {
                return Err(anyhow!(
                    "Aspect ratio feature must contain the aspect ratio in the format: aspect_ratio:w:h"
                ))
            }
            ("custom_artifacts", _) => features.custom_artifacts = true,
            ("ashmem", _) => features.ashmem = true,
            ("framebuffer", _) => features.framebuffer = true,
            ("gralloc", _) => features.gralloc = true,
            ("magma", _) => features.magma = true,
            ("gfxstream", _) => features.gfxstream = true,
            ("bpf", Some(version)) => features.kernel.bpf_v2 = version == "v2",
            ("log_dump_on_exit", _) => features.kernel.log_dump_on_exit = true,
            ("perfetto", Some(socket_path)) => {
                features.perfetto = Some(socket_path.into());
            }
            ("perfetto", None) => {
                return Err(anyhow!("Perfetto feature must contain a socket path"));
            }
            ("self_profile", _) => features.self_profile = true,
            ("selinux", mode_arg) => features.selinux = match mode_arg.as_ref() {
                Some(mode) => if mode == "fake" {
                    Some(security_server::Mode::Fake)
                } else {
                    return Err(anyhow!("Invalid SELinux mode"));
                },
                None => Some(security_server::Mode::Enable),
            },
            ("test_data", _) => features.test_data = true,
            (f, _) => {
                return Err(anyhow!("Unsupported feature: {}", f));
            }
        };
    }
    Ok(features)
}

/// Runs all the features that are enabled in `system_task.kernel()`.
pub fn run_container_features(
    locked: &mut Locked<'_, Unlocked>,
    system_task: &CurrentTask,
    features: &Features,
) -> Result<(), Error> {
    let kernel = system_task.kernel();

    let mut enabled_profiling = false;
    if features.framebuffer {
        fb_device_init(locked, system_task);
        init_input_devices(locked, system_task);
    }
    if features.gralloc {
        // The virtgralloc0 device allows vulkan_selector to indicate to gralloc
        // whether swiftshader or magma will be used. This is separate from the
        // magma feature because the policy choice whether to use magma or
        // swiftshader is in vulkan_selector, and it can potentially choose
        // switfshader for testing purposes even when magma0 is present. Also,
        // it's nice to indicate swiftshader the same way regardless of whether
        // the magma feature is enabled or disabled. If a call to gralloc AIDL
        // IAllocator allocate2 occurs with this feature disabled, the call will
        // fail.
        gralloc_device_init(locked, system_task);
    }
    if features.magma {
        magma_device_init(locked, system_task);
    }
    if features.gfxstream {
        gpu_device_init(locked, system_task);
    }
    if let Some(socket_path) = features.perfetto.clone() {
        start_perfetto_consumer_thread(kernel, socket_path)
            .context("Failed to start perfetto consumer thread")?;
    }
    if features.self_profile {
        enabled_profiling = true;
        fuchsia_inspect::component::inspector().root().record_lazy_child(
            "self_profile",
            fuchsia_inspect_contrib::ProfileDuration::lazy_node_callback,
        );
        fuchsia_inspect_contrib::start_self_profiling();
    }
    if features.ashmem {
        ashmem_device_init(locked, system_task);
    }
    if !enabled_profiling {
        fuchsia_inspect_contrib::stop_self_profiling();
    }

    Ok(())
}

/// Runs features requested by individual components inside the container.
pub fn run_component_features(
    entries: &Vec<String>,
    kernel: &Arc<Kernel>,
    outgoing_dir: &mut Option<fidl::endpoints::ServerEnd<fidl_fuchsia_io::DirectoryMarker>>,
    mut maybe_svc: Option<fio::DirectorySynchronousProxy>,
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
                kernel
                    .framebuffer
                    .start_server(
                        kernel,
                        view_bound_protocols,
                        view_identity,
                        outgoing_dir.take().unwrap(),
                        maybe_svc.take(),
                    )
                    .expect("Failed to start framebuffer server");
                kernel.input_device.start_relay(
                    touch_source_proxy,
                    keyboard,
                    registry_proxy,
                    view_ref,
                );
            }
            feature => {
                return error!(ENOSYS, format!("Unsupported feature: {}", feature));
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
