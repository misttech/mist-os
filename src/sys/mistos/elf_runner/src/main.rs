// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::builtin::svc_controller::SvcController;
use elf_runner::process_launcher::ProcessLauncher;
use fidl::HandleBased;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_ldsvc as fldsvc;
use fuchsia_async as fasync;
use fuchsia_component::server::*;
use fuchsia_runtime::{take_startup_handle, HandleInfo, HandleType};
use fuchsia_zircon as zx;
use futures::channel::oneshot;
use futures::prelude::*;
use mistos_bootfs::bootfs::BootfsSvc;
use mistos_logger::klog;
use std::env;
use std::io::{self, Write};
use std::process::{self, Command};
use std::sync::Arc;
use std::thread;
use tracing::{info, warn};

mod builtin;

extern "C" {
    fn dl_set_loader_service(
        handle: fuchsia_zircon::sys::zx_handle_t,
    ) -> fuchsia_zircon::sys::zx_handle_t;
}

fn main() {
    klog::KernelLogger::init();
    info!("started");

    let (sender, receiver) = oneshot::channel::<i32>();

    // Close any loader service passed. If userboot invoked component manager directly,
    // this service was the only reason userboot
    // continued to run and closing it will let userboot terminate.
    let ldsvc = unsafe {
        fuchsia_zircon::Handle::from_raw(dl_set_loader_service(
            fuchsia_zircon::sys::ZX_HANDLE_INVALID,
        ))
    };
    drop(ldsvc);

    let system_resource_handle =
        take_startup_handle(HandleType::SystemResource.into()).map(zx::Resource::from);

    let svc_stash_channel = take_startup_handle(HandleInfo::new(HandleType::User0, 0))
        .map(zx::Channel::from)
        .expect("Failed to get svc server channel");

    let bootfs_svc = BootfsSvc::new().expect("Failed to create Rust bootfs");
    let service = bootfs_svc
        .ingest_bootfs_vmo_with_system_resource(&system_resource_handle)
        .expect("Failed to ingest bootfs");

    let _ = service.create_and_bind_vfs();

    let mut executor = fasync::SendExecutor::new(1);

    let run_root_fut = async move {
        let boot_dir = fuchsia_fs::directory::open_in_namespace(
            "/boot",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )
        .unwrap();

        // Construct a loader from the package library dir
        let ldsvc = match fuchsia_fs::directory::open_directory(
            &boot_dir,
            "lib",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )
        .await
        {
            Ok(lib_dir) => {
                let (ldsvc, server_end) =
                    fidl::endpoints::create_endpoints::<fldsvc::LoaderMarker>();
                let server_end = server_end.into_channel();
                library_loader::start(Arc::new(lib_dir), server_end);
                Some(ldsvc)
            }
            Err(e) => {
                warn!("Could not open /lib dir {:?}", e);
                None
            }
        };

        let ldsvc_hnd = ldsvc.unwrap().into_channel().into_handle();
        unsafe {
            fuchsia_zircon::Handle::from_raw(dl_set_loader_service(ldsvc_hnd.into_raw()));
        };

        let svc_stash = SvcController::new(svc_stash_channel);
        svc_stash.wait_for_epitaph().await;

        let mut service_fs = ServiceFs::new();
        service_fs.add_fidl_service(move |stream| {
            fasync::Task::spawn(async move {
                let result = ProcessLauncher::serve(stream).await;
                if let Err(error) = result {
                    warn!(%error, "ProcessLauncher.serve failed");
                }
            })
            .detach();
        });

        // Bind to the channel
        service_fs
            .serve_connection(svc_stash.svc_endpoint().lock().take().expect("No svc channel found"))
            .expect("Failed to serve connection");

        // Dispatch the execution thread.
        sender.send(42).unwrap();

        // Start up ServiceFs
        service_fs.collect::<()>().await;
    };

    thread::spawn(|| {
        let args: Vec<String> = env::args().collect();

        // Wait until services are up
        futures::executor::block_on(async {
            assert_eq!(receiver.await, Ok(42));
        });

        // TODO (Herrera) Check if the file exists in BootFS
        let output = Command::new("/boot/".to_owned() + &args[1])
            .args(&args[2..])
            .output()
            .expect("failed to execute process");

        io::stdout().write_all(&output.stdout).unwrap();
        info!("output status {}", output.status);
        process::exit(output.status.code().unwrap());
    });

    executor.run(run_root_fut);
}
