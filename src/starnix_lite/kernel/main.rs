// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "256"]
#![allow(clippy::too_many_arguments)]
// TODO(https://fxbug.dev/42073005): Remove this allow once the lint is fixed.
#![allow(unknown_lints, clippy::extra_unused_type_parameters)]

// Avoid unused crate warnings on non-test/non-debug builds because this needs to be an
// unconditional dependency for rustdoc generation.
use tracing_mutex as _;

use crate::bootfs::BootfsSvc;
use crate::builtin::svc_controller::SvcController;
use crate::builtin::time::create_utc_clock;
use anyhow::{Context, Error};
use builtins::vmex_resource::VmexResource;
use fidl::endpoints::RequestStream;
use fuchsia_boot::UserbootRequest;
use fuchsia_component::server::*;
use fuchsia_runtime::{swap_utc_clock_handle, take_startup_handle, HandleInfo, HandleType};
use futures::prelude::*;
use mistos_logger::klog;
use starnix_core::mm::{init_usercopy, zxio_maybe_faultable_copy_impl};
use starnix_lite_kernel_config::Config;
use starnix_lite_kernel_runner::{create_container_from_config, Container, ContainerServiceConfig};
use starnix_logging::{log_error, log_info, log_warn};
use {
    fidl_fuchsia_boot as fuchsia_boot, fuchsia_async as fasync, fuchsia_runtime as fruntime,
    fuchsia_zircon as zx,
};

mod bootfs;
mod builtin;

extern "C" {
    fn dl_set_loader_service(
        handle: fuchsia_zircon::sys::zx_handle_t,
    ) -> fuchsia_zircon::sys::zx_handle_t;
}

/// Overrides the `zxio_maybe_faultable_copy` weak symbol found in zxio.
#[no_mangle]
extern "C" fn zxio_maybe_faultable_copy(
    dest: *mut u8,
    src: *const u8,
    count: usize,
    ret_dest: bool,
) -> bool {
    // SAFETY: we know that we are either copying from or to a buffer that
    // zxio (and thus Starnix) owns per `zxio_maybe_faultable_copy`'s
    // documentation.
    unsafe { zxio_maybe_faultable_copy_impl(dest, src, count, ret_dest) }
}

/// Overrides the `zxio_fault_catching_disabled` weak symbol found in zxio.
#[no_mangle]
extern "C" fn zxio_fault_catching_disabled() -> bool {
    false
}

async fn build_container(
    config: Config,
    returned_config: &mut Option<ContainerServiceConfig>,
) -> Result<Container, Error> {
    let (container, config) = create_container_from_config(config).await?;
    *returned_config = Some(config);
    Ok(container)
}

fn main() -> Result<(), Error> {
    klog::KernelLogger::init();

    log_info!("Starnix Lite is starting up...");

    // Close any loader service passed to so that the service session can be
    // freed, as we won't make use of a loader service such as by calling dlopen.
    // If userboot invoked this directly, this service was the only reason userboot
    // continued to run and closing it will let userboot terminate.
    let ldsvc = unsafe {
        fuchsia_zircon::Handle::from_raw(dl_set_loader_service(
            fuchsia_zircon::sys::ZX_HANDLE_INVALID,
        ))
    };
    drop(ldsvc);

    // Make sure that if this process panics in normal mode that the whole kernel's job is killed.
    if let Err(err) = fruntime::job_default()
        .set_critical(zx::JobCriticalOptions::RETCODE_NONZERO, &*fruntime::process_self())
    {
        panic!("Starnix Lite failed to set itself as critical: {}", err);
    }

    let config = Config {
        features: vec!["container".to_owned()],
        init: vec!["/container/coremark".to_owned()],
        //init: vec!["/container/sqlite-bench-uk".to_owned()],
        kernel_cmdline: Default::default(),
        mounts: vec![
            "/:remote_bundle:data/system:nosuid,nodev,relatime".to_owned(),
            "/dev:devtmpfs::nosuid,relatime".to_owned(),
            "/dev/pts:devpts::nosuid,noexec,relatime".to_owned(),
            "/dev/shm:tmpfs::nosuid,nodev".to_owned(),
            "/proc:proc::nosuid,nodev,noexec,relatime".to_owned(),
            "/sys:sysfs::nosuid,nodev,noexec,relatime".to_owned(),
            "/tmp:tmpfs".to_owned(),
        ],

        rlimits: Default::default(),
        name: "starnix_lite".to_owned(),
        startup_file_path: Default::default(),
        remote_block_devices: Default::default(),
    };

    let mut executor = fasync::LocalExecutor::new();
    executor.run_singlethreaded(async_main(config)).context("async main")?;

    Ok(())
}

async fn async_main(config: Config) -> Result<(), Error> {
    let system_resource_handle =
        take_startup_handle(HandleType::SystemResource.into()).map(zx::Resource::from);

    // Drain messages from `fuchsia.boot.Userboot`, and expose appropriate capabilities.
    let userboot = take_startup_handle(HandleInfo::new(HandleType::User0, 0))
        .map(zx::Channel::from)
        .map(fasync::Channel::from_channel)
        .map(fuchsia_boot::UserbootRequestStream::from_channel);

    let mut svc_stash_provider = None;
    if let Some(userboot) = userboot {
        let messages = userboot.try_collect::<Vec<UserbootRequest>>().await;

        if let Ok(mut messages) = messages {
            while let Some(request) = messages.pop() {
                match request {
                    UserbootRequest::PostStashSvc { stash_svc_endpoint, control_handle: _ } => {
                        if svc_stash_provider.is_some() {
                            log_warn!("Expected at most a single SvcStash, but more were found. Last entry will be preserved.");
                        }
                        svc_stash_provider = Some(stash_svc_endpoint.into_channel());
                    }
                }
            }
        } else if let Err(err) = messages {
            log_error!("Error extracting 'fuchsia.boot.Userboot' messages:  {err}");
        }
    }

    let bootfs_svc = BootfsSvc::new().expect("Failed to create bootfs");
    let bootfs_svc = Some(bootfs_svc);
    let clock = create_utc_clock(&bootfs_svc).await.expect("failed to create UTC clock");

    // We are affecting the process-wide clock here, but since Rust test cases are run in their
    // own process, this won't interact with other running tests.
    let _ = swap_utc_clock_handle(clock).expect("failed to swap clocks");

    let vmex_resource = system_resource_handle
        .as_ref()
        .and_then(|handle| {
            handle
                .create_child(
                    zx::ResourceKind::SYSTEM,
                    None,
                    zx::sys::ZX_RSRC_SYSTEM_VMEX_BASE,
                    1,
                    b"vmex",
                )
                .ok()
        })
        .map(VmexResource::new)
        .and_then(Result::ok)
        .unwrap();

    let bootfs_svc = bootfs_svc
        .unwrap()
        .ingest_bootfs_vmo_with_system_resource(&system_resource_handle)
        .expect("Failed to ingest bootfs");

    let _ = bootfs_svc.create_and_bind_vfs().expect("failed to bind");

    let mut service_fs = ServiceFs::new();

    // Set up the VmexResource service.
    service_fs.add_fidl_service(move |stream| {
        let vmex = vmex_resource.clone();
        fasync::Task::spawn(async move {
            let result = vmex.serve(stream).await;
            if let Err(error) = result {
                log_warn!(%error, "vmex failed");
            }
        })
        .detach();
    });

    let svc_stash = SvcController::new(svc_stash_provider.unwrap());
    svc_stash.wait_for_epitaph().await;

    // Bind to the channel
    service_fs
        .serve_connection(svc_stash.svc_endpoint().lock().take().expect("No svc channel found"))?;

    // Run the service with its own executor to avoid reentrancy issues.
    std::thread::spawn(move || {
        fasync::LocalExecutor::new().run_singlethreaded(fasync::Task::spawn(async move {
            service_fs.collect::<()>().await;
        }));
    });

    // We call this early during Starnix boot to make sure the usercopy utilities
    // are ready for use before any restricted-mode/Linux processes are created.
    init_usercopy();

    let container = async_lock::OnceCell::<Container>::new();

    let mut config_service: Option<ContainerServiceConfig> = None;

    let container = container
        .get_or_try_init(|| build_container(config, &mut config_service))
        .await
        .expect("failed to start container");

    if let Some(config) = config_service {
        container
            .run(config)
            .await
            .expect("failed to run the expected services from the container");
    }
    Ok(())
}
