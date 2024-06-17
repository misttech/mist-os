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

use mistos_bootfs::bootfs::BootfsSvc;
use mistos_logger::klog;
use starnix_logging::log_info;
use {fuchsia_runtime as fruntime, fuchsia_zircon as zx};

extern "C" {
    fn dl_set_loader_service(
        handle: fuchsia_zircon::sys::zx_handle_t,
    ) -> fuchsia_zircon::sys::zx_handle_t;
}

fn main() {
    // Make sure that if this process panics in normal mode that the whole kernel's job is killed.
    if let Err(err) = fruntime::job_default()
        .set_critical(zx::JobCriticalOptions::RETCODE_NONZERO, &*fruntime::process_self())
    {
        panic!("Starnix Lite failed to set itself as critical: {}", err);
    }

    // Close any loader service passed to component manager so that the service session can be
    // freed, as component manager won't make use of a loader service such as by calling dlopen.
    // If userboot invoked component manager directly, this service was the only reason userboot
    // continued to run and closing it will let userboot terminate.
    let ldsvc = unsafe {
        fuchsia_zircon::Handle::from_raw(dl_set_loader_service(
            fuchsia_zircon::sys::ZX_HANDLE_INVALID,
        ))
    };
    drop(ldsvc);

    klog::KernelLogger::init();

    log_info!("Starnix Lite is starting up...");

    let system_resource_handle =
        fruntime::take_startup_handle(fruntime::HandleType::SystemResource.into())
            .map(zx::Resource::from);
    let bootfs_svc = BootfsSvc::new().expect("Failed to create Rust bootfs");

    let service = bootfs_svc
        .ingest_bootfs_vmo_with_system_resource(&system_resource_handle)
        .expect("Failed to ingest bootfs");

    let _ = service.create_and_bind_vfs();
}
