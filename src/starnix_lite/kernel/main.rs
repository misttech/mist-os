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
use starnix_core::mm::{init_usercopy, zxio_maybe_faultable_copy_impl};
use starnix_logging::log_info;
use {fuchsia_runtime as fruntime, fuchsia_zircon as zx};

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

fn main() {
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

    let system_resource_handle =
        fruntime::take_startup_handle(fruntime::HandleType::SystemResource.into())
            .map(zx::Resource::from);

    let bootfs_svc = BootfsSvc::new().expect("Failed to create bootfs");
    let bootfs_svc = bootfs_svc
        .ingest_bootfs_vmo_with_system_resource(&system_resource_handle)
        .expect("Failed to ingest bootfs");
    let _ = bootfs_svc.create_and_bind_vfs();

    //let mut executor = fasync::SendExecutor::new(1);

    // We call this early during Starnix boot to make sure the usercopy utilities
    // are ready for use before any restricted-mode/Linux processes are created.
    init_usercopy();
}
