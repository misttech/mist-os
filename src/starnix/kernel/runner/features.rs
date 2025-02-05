// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Config;

#[cfg(not(feature = "starnix_lite"))]
use anyhow::{anyhow, Context, Error};
#[cfg(feature = "starnix_lite")]
use anyhow::{anyhow, Error};
#[cfg(not(feature = "starnix_lite"))]
use bstr::BString;
#[cfg(not(feature = "starnix_lite"))]
use starnix_core::device::android::bootloader_message_store::android_bootloader_message_store_init;
#[cfg(not(feature = "starnix_lite"))]
use starnix_core::device::framebuffer::{fb_device_init, AspectRatio};
#[cfg(not(feature = "starnix_lite"))]
use starnix_core::device::remote_block_device::remote_block_device_init;
use starnix_core::task::{CurrentTask, Kernel, KernelFeatures};
#[cfg(not(feature = "starnix_lite"))]
use starnix_core::vfs::FsString;
use starnix_logging::log_error;
#[cfg(not(feature = "starnix_lite"))]
use starnix_modules_ashmem::ashmem_device_init;
#[cfg(not(feature = "starnix_lite"))]
use starnix_modules_gpu::gpu_device_init;
#[cfg(not(feature = "starnix_lite"))]
use starnix_modules_gralloc::gralloc_device_init;
#[cfg(not(feature = "starnix_lite"))]
use starnix_modules_input::uinput::register_uinput_device;
#[cfg(not(feature = "starnix_lite"))]
use starnix_modules_input::{
    EventProxyMode, InputDevice, InputEventsRelay, DEFAULT_KEYBOARD_DEVICE_ID,
    DEFAULT_TOUCH_DEVICE_ID,
};
#[cfg(not(feature = "starnix_lite"))]
use starnix_modules_magma::magma_device_init;
use starnix_modules_nanohub::nanohub_device_init;
#[cfg(not(feature = "starnix_lite"))]
use starnix_modules_perfetto_consumer::start_perfetto_consumer_thread;
#[cfg(not(feature = "starnix_lite"))]
use starnix_modules_touch_power_policy::TouchPowerPolicyDevice;
use starnix_sync::{Locked, Unlocked};
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
#[cfg(not(feature = "starnix_lite"))]
use std::sync::mpsc::channel;
use std::sync::Arc;

#[cfg(not(feature = "starnix_lite"))]
use {
    fidl_fuchsia_sysinfo as fsysinfo, fidl_fuchsia_ui_composition as fuicomposition,
    fidl_fuchsia_ui_input3 as fuiinput, fidl_fuchsia_ui_policy as fuipolicy,
    fidl_fuchsia_ui_views as fuiviews,
};

/// A collection of parsed features, and their arguments.
#[derive(Default, Debug)]
pub struct Features {
    pub kernel: KernelFeatures,

    pub selinux: SELinuxFeature,

    #[cfg(not(feature = "starnix_lite"))]
    pub ashmem: bool,

    #[cfg(not(feature = "starnix_lite"))]
    pub framebuffer: bool,

    #[cfg(not(feature = "starnix_lite"))]
    pub gralloc: bool,

    #[cfg(not(feature = "starnix_lite"))]
    pub magma_supported_vendors: Option<Vec<u16>>,

    #[cfg(not(feature = "starnix_lite"))]
    pub gfxstream: bool,

    /// Include the /container directory in the root file system.
    pub container: bool,

    /// Include the /test_data directory in the root file system.
    pub test_data: bool,

    /// Include the /custom_artifacts directory in the root file system.
    pub custom_artifacts: bool,

    #[cfg(not(feature = "starnix_lite"))]
    pub android_serialno: bool,

    pub self_profile: bool,

    #[cfg(not(feature = "starnix_lite"))]
    pub aspect_ratio: Option<AspectRatio>,

    #[cfg(not(feature = "starnix_lite"))]
    pub perfetto: Option<FsString>,

    #[cfg(not(feature = "starnix_lite"))]
    pub android_fdr: bool,

    pub rootfs_rw: bool,

    pub network_manager: bool,

    pub nanohub: bool,
}

#[derive(Default, Debug)]
pub struct SELinuxFeature {
    pub enabled: bool,

    /// A path to a file in the Container's package that lists all the permission checks
    /// that are allowed to fail.
    pub exceptions_path: Option<String>,
}

impl Features {
    pub fn record_inspect(&self, parent_node: &fuchsia_inspect::Node) {
        parent_node.record_child("features", |inspect_node| match self {
            Features {
                kernel:
                    KernelFeatures {
                        bpf_v2,
                        enable_suid,
                        io_uring,
                        error_on_failed_reboot,
                        enable_visual_debugging,
                        default_uid,
                        default_seclabel,
                        default_fsseclabel,
                    },
                selinux,
                ashmem,
                framebuffer,
                gralloc,
                magma_supported_vendors,
                gfxstream,
                container,
                test_data,
                custom_artifacts,
                android_serialno,
                self_profile,
                aspect_ratio,
                perfetto,
                android_fdr,
                rootfs_rw,
                network_manager,
                nanohub,
            } => {
                inspect_node.record_bool("selinux", selinux.enabled);
                inspect_node.record_bool("ashmem", *ashmem);
                inspect_node.record_bool("framebuffer", *framebuffer);
                inspect_node.record_bool("gralloc", *gralloc);
                inspect_node.record_string(
                    "magma_supported_vendors",
                    match magma_supported_vendors {
                        Some(vendors) => vendors
                            .iter()
                            .map(|vendor| format!("0x{:x}", vendor))
                            .collect::<Vec<String>>()
                            .join(","),
                        None => "".to_string(),
                    },
                );
                inspect_node.record_bool("gfxstream", *gfxstream);
                inspect_node.record_bool("container", *container);
                inspect_node.record_bool("test_data", *test_data);
                inspect_node.record_bool("custom_artifacts", *custom_artifacts);
                inspect_node.record_bool("android_serialno", *android_serialno);
                inspect_node.record_bool("self_profile", *self_profile);
                inspect_node.record_string(
                    "aspect_ratio",
                    aspect_ratio
                        .as_ref()
                        .map(|aspect_ratio| {
                            format!("width: {} height: {}", aspect_ratio.width, aspect_ratio.height)
                        })
                        .unwrap_or_default(),
                );
                inspect_node.record_string(
                    "perfetto",
                    perfetto.as_ref().map(|p| p.to_string()).unwrap_or_default(),
                );
                inspect_node.record_bool("android_fdr", *android_fdr);
                inspect_node.record_bool("rootfs_rw", *rootfs_rw);
                inspect_node.record_bool("network_manager", *network_manager);
                inspect_node.record_bool("nanohub", *nanohub);

                inspect_node.record_child("kernel", |kernel_node| {
                    kernel_node.record_bool("bpf_v2", *bpf_v2);
                    kernel_node.record_bool("enable_suid", *enable_suid);
                    kernel_node.record_bool("io_uring", *io_uring);
                    kernel_node.record_bool("error_on_failed_reboot", *error_on_failed_reboot);
                    kernel_node.record_bool("enable_visual_debugging", *enable_visual_debugging);
                    kernel_node.record_int("default_uid", (*default_uid).into());
                    kernel_node.record_string(
                        "default_seclabel",
                        default_seclabel.as_deref().unwrap_or_default(),
                    );
                    kernel_node.record_string(
                        "default_fsseclabel",
                        default_fsseclabel.as_ref().map_or("", |s| s),
                    );
                });
            }
        });
    }
}

/// Parses all the featurse in `entries`.
///
/// Returns an error if parsing fails, or if an unsupported feature is present in `features`.
pub fn parse_features(
    config: &Config,
    structured_config: &starnix_kernel_structured_config::Config,
) -> Result<Features, Error> {
    let mut features = Features::default();
    for entry in &config.features {
        let (raw_flag, raw_args) =
            entry.split_once(':').map(|(f, a)| (f, Some(a.to_string()))).unwrap_or((entry, None));
        match (raw_flag, raw_args) {
            #[cfg(not(feature = "starnix_lite"))]
            ("android_fdr", _) => features.android_fdr = true,
            #[cfg(not(feature = "starnix_lite"))]
            ("android_serialno", _) => features.android_serialno = true,
            #[cfg(not(feature = "starnix_lite"))]
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
            #[cfg(not(feature = "starnix_lite"))]
            ("aspect_ratio", None) => {
                return Err(anyhow!(
                    "Aspect ratio feature must contain the aspect ratio in the format: aspect_ratio:w:h"
                ))
            }
            ("container", _) => features.container = true,
            ("custom_artifacts", _) => features.custom_artifacts = true,
            #[cfg(not(feature = "starnix_lite"))]
            ("ashmem", _) => features.ashmem = true,
            #[cfg(not(feature = "starnix_lite"))]
            ("framebuffer", _) => features.framebuffer = true,
            #[cfg(not(feature = "starnix_lite"))]
            ("gralloc", _) => features.gralloc = true,
            #[cfg(not(feature = "starnix_lite"))]
            ("magma", _) => if features.magma_supported_vendors.is_none() {
                const VENDOR_ARM: u16 = 0x13B5;
                const VENDOR_INTEL: u16 = 0x8086;
                features.magma_supported_vendors = Some(vec![VENDOR_ARM, VENDOR_INTEL])
            },
            #[cfg(not(feature = "starnix_lite"))]
            ("magma_supported_vendors", Some(arg)) => {
                features.magma_supported_vendors = Some(
                    arg.split(',')
                        .map(|s| {
                            let err = anyhow!("Feature format must be: magma_supported_vendors:0x1234[,0xabcd]");
                            let trimmed = s.trim_start_matches("0x");
                            u16::from_str_radix(trimmed, 16).map_err(|_| err)
                        }).collect::<Result<Vec<u16>, Error>>()?);
            },
            ("nanohub", _) => features.nanohub = true,
            ("network_manager", _) => features.network_manager = true,
            #[cfg(not(feature = "starnix_lite"))]
            ("gfxstream", _) => features.gfxstream = true,
            #[cfg(not(feature = "starnix_lite"))]
            ("bpf", Some(version)) => features.kernel.bpf_v2 = version == "v2",
            ("enable_suid", _) => features.kernel.enable_suid = true,
            #[cfg(not(feature = "starnix_lite"))]
            ("io_uring", _) => features.kernel.io_uring = true,
            ("error_on_failed_reboot", _) => features.kernel.error_on_failed_reboot = true,
            #[cfg(not(feature = "starnix_lite"))]
            ("perfetto", Some(socket_path)) => {
                features.perfetto = Some(socket_path.into());
            }
            #[cfg(not(feature = "starnix_lite"))]
            ("perfetto", None) => {
                return Err(anyhow!("Perfetto feature must contain a socket path"));
            }
            ("rootfs_rw", _) => features.rootfs_rw = true,
            ("self_profile", _) => features.self_profile = true,
            ("selinux", arg) => {
                features.selinux = SELinuxFeature { enabled: true, exceptions_path: arg };
            }
            ("test_data", _) => features.test_data = true,
            (f, _) => {
                return Err(anyhow!("Unsupported feature: {}", f));
            }
        };
    }

    if structured_config.ui_visual_debugging_level > 0 {
        features.kernel.enable_visual_debugging = true;
    }

    features.kernel.default_uid = config.default_uid;
    features.kernel.default_seclabel = config.default_seclabel.clone();

    Ok(features)
}

/// Runs all the features that are enabled in `system_task.kernel()`.
pub fn run_container_features(
    #[cfg(not(feature = "starnix_lite"))] locked: &mut Locked<'_, Unlocked>,
    #[cfg(feature = "starnix_lite")] _locked: &mut Locked<'_, Unlocked>,
    system_task: &CurrentTask,
    features: &Features,
) -> Result<(), Error> {
    let kernel = system_task.kernel();

    let mut enabled_profiling = false;

    #[cfg(not(feature = "starnix_lite"))]
    if features.framebuffer {
        fb_device_init(locked, system_task);

        let (touch_source_client, touch_source_server) = fidl::endpoints::create_endpoints();
        let view_bound_protocols = fuicomposition::ViewBoundProtocols {
            touch_source: Some(touch_source_server),
            ..Default::default()
        };
        let view_identity = fuiviews::ViewIdentityOnCreation::from(
            fuchsia_scenic::ViewRefPair::new().expect("Failed to create ViewRefPair"),
        );
        let view_ref = fuchsia_scenic::duplicate_view_ref(&view_identity.view_ref)
            .expect("Failed to dup view ref.");
        let keyboard =
            fuchsia_component::client::connect_to_protocol_sync::<fuiinput::KeyboardMarker>()
                .expect("Failed to connect to keyboard");
        let registry_proxy = fuchsia_component::client::connect_to_protocol_sync::<
            fuipolicy::DeviceListenerRegistryMarker,
        >()
        .expect("Failed to connect to device listener registry");

        // These need to be set before `Framebuffer::start_server` is called.
        // `Framebuffer::start_server` is only called when the `framebuffer` component feature is
        // enabled. The container is the runner for said components, and `run_container_features`
        // is performed before the Container is fully initialized. Therefore, it's safe to set
        // these values at this point.
        //
        // In the future, we would like to avoid initializing a framebuffer unconditionally on the
        // Kernel, at which point this logic will need to change.
        *kernel.framebuffer.view_identity.lock() = Some(view_identity);
        *kernel.framebuffer.view_bound_protocols.lock() = Some(view_bound_protocols);

        let framebuffer = kernel.framebuffer.info.read();

        let display_width = framebuffer.xres as i32;
        let display_height = framebuffer.yres as i32;

        let touch_device =
            InputDevice::new_touch(display_width, display_height, &kernel.inspect_node);
        let keyboard_device = InputDevice::new_keyboard(&kernel.inspect_node);

        touch_device.clone().register(
            locked,
            &kernel.kthreads.system_task(),
            DEFAULT_TOUCH_DEVICE_ID,
        );
        keyboard_device.clone().register(
            locked,
            &kernel.kthreads.system_task(),
            DEFAULT_KEYBOARD_DEVICE_ID,
        );

        let input_events_relay = InputEventsRelay::new();
        input_events_relay.start_relays(
            &kernel,
            EventProxyMode::WakeContainer,
            touch_source_client,
            keyboard,
            view_ref,
            registry_proxy,
            touch_device.open_files.clone(),
            keyboard_device.open_files.clone(),
            Some(touch_device.inspect_status.clone()),
            Some(keyboard_device.inspect_status.clone()),
        );

        register_uinput_device(locked, &kernel.kthreads.system_task(), input_events_relay);

        // Channel we use to inform the relay of changes to `touch_standby`
        let (touch_standby_sender, touch_standby_receiver) = channel::<bool>();
        let touch_policy_device = TouchPowerPolicyDevice::new(touch_standby_sender);
        touch_policy_device.clone().register(locked, &kernel.kthreads.system_task());
        touch_policy_device.start_relay(&kernel, touch_standby_receiver);

        kernel.framebuffer.start_server(kernel, None).expect("Failed to start framebuffer server");
    }
    #[cfg(not(feature = "starnix_lite"))]
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
    #[cfg(not(feature = "starnix_lite"))]
    if let Some(supported_vendors) = &features.magma_supported_vendors {
        magma_device_init(locked, system_task, supported_vendors.clone());
    }
    #[cfg(not(feature = "starnix_lite"))]
    if features.gfxstream {
        gpu_device_init(locked, system_task);
    }
    #[cfg(not(feature = "starnix_lite"))]
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
    #[cfg(not(feature = "starnix_lite"))]
    if features.ashmem {
        ashmem_device_init(locked, system_task);
    }
    if !enabled_profiling {
        fuchsia_inspect_contrib::stop_self_profiling();
    }
    #[cfg(not(feature = "starnix_lite"))]
    if features.android_fdr {
        android_bootloader_message_store_init(locked, system_task);
        remote_block_device_init(locked, system_task);
    }
    if features.network_manager {
        if let Err(e) = kernel.network_manager.init(kernel) {
            log_error!("Network manager initialization failed: ({e:?})");
        }
    }
    if features.nanohub {
        nanohub_device_init(locked, system_task);
    }

    Ok(())
}

/// Runs features requested by individual components inside the container.
pub fn run_component_features(
    #[cfg(not(feature = "starnix_lite"))] kernel: &Arc<Kernel>,
    #[cfg(feature = "starnix_lite")] _kernel: &Arc<Kernel>,
    entries: &Vec<String>,
    #[cfg(not(feature = "starnix_lite"))] mut incoming_dir: Option<fidl_fuchsia_io::DirectoryProxy>,
    #[cfg(feature = "starnix_lite")] _incoming_dir: Option<fidl_fuchsia_io::DirectoryProxy>,
) -> Result<(), Errno> {
    for entry in entries {
        match entry.as_str() {
            "framebuffer" => {
                #[cfg(not(feature = "starnix_lite"))]
                kernel
                    .framebuffer
                    .start_server(kernel, incoming_dir.take())
                    .expect("Failed to start framebuffer server");
            }
            feature => {
                return error!(ENOSYS, format!("Unsupported feature: {}", feature));
            }
        }
    }
    Ok(())
}

#[cfg(not(feature = "starnix_lite"))]
pub async fn get_serial_number() -> anyhow::Result<BString> {
    let sysinfo = fuchsia_component::client::connect_to_protocol::<fsysinfo::SysInfoMarker>()?;
    let serial = sysinfo.get_serial_number().await?.map_err(zx::Status::from_raw)?;
    Ok(BString::from(serial))
}
