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

use anyhow::Error;
use starnix_core::mm::{init_usercopy, zxio_maybe_faultable_copy_impl};
use starnix_kernel_config::Config;
use starnix_kernel_runner::{create_container_from_config, Container, ContainerServiceConfig};
use starnix_logging::log_info;
use {fuchsia_runtime as fruntime, fuchsia_zircon as zx};

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

#[fuchsia::main(
    logging_tags = ["starnix_lite"],
    logging_blocking,
    logging_panic_prefix="\n\n\n\nSTARNIX KERNEL PANIC\n\n\n\n",
)]
async fn main() -> Result<(), Error> {
    log_info!("Starnix Lite is starting up...");

    // Make sure that if this process panics in normal mode that the whole kernel's job is killed.
    if let Err(err) = fruntime::job_default()
        .set_critical(zx::JobCriticalOptions::RETCODE_NONZERO, &*fruntime::process_self())
    {
        panic!("Starnix Lite failed to set itself as critical: {}", err);
    }

    // We call this early during Starnix boot to make sure the usercopy utilities
    // are ready for use before any restricted-mode/Linux processes are created.
    init_usercopy();

    let config = Config::take_from_startup_handle();

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

/*
async fn async_main(config: Config) -> Result<(), Error> {
    let system_resource_handle =
        fruntime::take_startup_handle(fruntime::HandleType::SystemResource.into())
            .map(zx::Resource::from);

    let svc_stash_channel =
        fruntime::take_startup_handle(fruntime::HandleInfo::new(fruntime::HandleType::User0, 0))
            .map(zx::Channel::from)
            .expect("Failed to get svc server channel");

    let bootfs_svc = BootfsSvc::new().expect("Failed to create bootfs");
    let bootfs_svc = Some(bootfs_svc);
    let clock = create_utc_clock(&bootfs_svc).await.expect("failed to create UTC clock");

    // We are affecting the process-wide clock here, but since Rust test cases are run in their
    // own process, this won't interact with other running tests.
    let _ = swap_utc_clock_handle(clock).expect("failed to swap clocks");

    let bootfs_svc = bootfs_svc
        .unwrap()
        .ingest_bootfs_vmo_with_system_resource(&system_resource_handle)
        .expect("Failed to ingest bootfs");

    let _ = bootfs_svc.create_and_bind_vfs().expect("failed to bind");

    let mut service_fs = ServiceFs::new();

    // Set up the VmexResource service.
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

    let svc_stash = SvcController::new(svc_stash_channel);
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
*/
