// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        boot_args::BootArgs,
        config::apply_boot_args_to_config,
        environment::{Environment, FshostEnvironment},
        inspect::register_stats,
    },
    anyhow::{format_err, Error},
    fidl::prelude::*,
    fidl_fuchsia_fshost as fshost, fidl_fuchsia_io as fio,
    fuchsia_runtime::{take_startup_handle, HandleType},
    fuchsia_zircon::sys::zx_debug_write,
    futures::{channel::mpsc, lock::Mutex, StreamExt},
    inspect_runtime,
    std::{collections::HashSet, sync::Arc},
    vfs::{
        directory::{entry::DirectoryEntry, helper::DirectlyMutable},
        execution_scope::ExecutionScope,
        path::Path,
        remote::remote_dir,
    },
};

mod boot_args;
mod config;
mod copier;
mod crypt;
mod device;
mod environment;
mod fxblob;
mod inspect;
mod manager;
mod matcher;
mod ramdisk;
mod service;
mod volume;
mod watcher;

// Logs directly to the serial port.  To be used when it's expected that fshost will terminate
// shortly afterwards since messages via the log subsystem often don't make it.
fn debug_log(message: &str) {
    let message = format!("[fshost] {}\n", message);
    let message = message.as_bytes();
    unsafe {
        zx_debug_write(message.as_ptr(), message.len());
    }
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let boot_args = BootArgs::new().await;
    let mut config = fshost_config::Config::take_from_startup_handle();
    apply_boot_args_to_config(&mut config, &boot_args);
    let config = Arc::new(config);
    // NB There are tests that look for "fshost started".
    tracing::info!(?config, "fshost started");

    let directory_request =
        take_startup_handle(HandleType::DirectoryRequest.into()).ok_or_else(|| {
            format_err!("missing DirectoryRequest startup handle - not launched as a component?")
        })?;

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<service::FshostShutdownResponder>(1);
    let (watcher, device_stream) = watcher::Watcher::new().await?;

    // Potentially launch the boot items ramdisk. It's not fatal, so if it fails we print an error
    // and continue.
    let ramdisk_path = if config.ramdisk_image {
        ramdisk::set_up_ramdisk().await.unwrap_or_else(|error| {
            tracing::error!(?error, "failed to set up ramdisk filesystems");
            None
        })
    } else {
        None
    };

    let inspector = fuchsia_inspect::component::inspector();
    // matcher_lock is used to block matching temporarily and inject
    // paths to be ignored.
    let matcher_lock = Arc::new(Mutex::new(HashSet::new()));
    let mut env = FshostEnvironment::new(
        config.clone(),
        boot_args,
        ramdisk_path.clone(),
        matcher_lock.clone(),
        inspector.clone(),
    );

    let launcher = env.launcher();
    // Records inspect metrics
    register_stats(inspector.root(), env.data_root()?).await;
    let blob_exposed_dir = env.blobfs_exposed_dir()?;
    let data_exposed_dir = env.data_exposed_dir()?;
    let env: Arc<Mutex<dyn Environment>> = Arc::new(Mutex::new(env));
    let export = vfs::pseudo_directory! {
        "fs" => vfs::pseudo_directory! {
            "blob" => remote_dir(blob_exposed_dir),
            "data" => remote_dir(data_exposed_dir),
        },
        "mnt" => vfs::pseudo_directory! {},
        inspect_runtime::DIAGNOSTICS_DIR => inspect_runtime::create_diagnostics_dir(
            inspector.clone(),
        ),
    };
    let svc_dir = vfs::pseudo_directory! {
        fshost::AdminMarker::PROTOCOL_NAME =>
            service::fshost_admin(
                env.clone(),
                config.clone(),
                ramdisk_path.clone(),
                launcher,
                matcher_lock.clone()
            ),
        fshost::BlockWatcherMarker::PROTOCOL_NAME =>
            service::fshost_block_watcher(watcher),
    };
    if config.fxfs_blob {
        svc_dir
            .add_entry(
                fidl_fuchsia_update_verify::BlobfsVerifierMarker::PROTOCOL_NAME,
                fxblob::blobfs_verifier_service(),
            )
            .unwrap();
    }
    export.add_entry("svc", svc_dir).unwrap();

    // The inspector is global and will maintain strong references to callbacks used to gather
    // inspect data which will include env.data_root() which is a proxy with an async channel that
    // is registered with the executor.  The executor will assert if anything is regsistered with it
    // when its destructor runs, so we make sure to clean up the inspector here.
    scopeguard::defer! { inspector.root().clear_recorded(); }

    let _ = service::handle_lifecycle_requests(shutdown_tx)?;

    let scope = ExecutionScope::new();
    export.open(
        scope.clone(),
        fio::OpenFlags::RIGHT_READABLE
            | fio::OpenFlags::RIGHT_WRITABLE
            | fio::OpenFlags::DIRECTORY
            | fio::OpenFlags::RIGHT_EXECUTABLE,
        Path::dot(),
        directory_request.into(),
    );

    // TODO(fxbug.dev/118209): //src/tests/oom looks for "fshost: lifecycle handler ready" to
    // indicate the watcher is about to start.
    tracing::info!("fshost: lifecycle handler ready");

    // Run the main loop of fshost, handling devices as they appear according to our filesystem
    // policy.
    let mut fs_manager = manager::Manager::new(&config, ramdisk_path, env, matcher_lock);
    let shutdown_responder = if config.disable_block_watcher {
        // If the block watcher is disabled, fshost just waits on the shutdown receiver instead of
        // processing devices.
        shutdown_rx
            .next()
            .await
            .ok_or_else(|| format_err!("shutdown signal stream ended unexpectedly"))?
    } else {
        fs_manager.device_handler(device_stream, shutdown_rx).await?
    };

    tracing::info!("shutdown signal received");
    // TODO(fxbug.dev/118209): //src/tests/oom looks for "received shutdown command over lifecycle
    // interface" to indicate fshost shutdown is starting. Shutdown logs have to go straight to
    // serial because of timing issues (fxbug.dev/97630).
    debug_log("received shutdown command over lifecycle interface");

    // Shutting down fshost involves sending asynchronous shutdown signals to several different
    // systems in order. If at any point we hit an error, we log loudly, but continue with the
    // shutdown procedure.

    // 0. Before fshost is told to shut down, almost everything that is running out of the
    //    filesystems is shut down by component manager.

    // 1. Shut down the scope for the export directory. This hosts the fshost services. This
    //    prevents additional connections to fshost services from being created.
    scope.shutdown();

    // 2. Shut down all the filesystems we started.
    fs_manager.shutdown().await?;

    // NB There are tests that look for this specific log message.  We write directly to serial
    // because writing via syslog has been found to not reliably make it to serial before shutdown
    // occurs.
    debug_log("fshost shutdown complete");

    // 3. Notify whoever asked for a shutdown that it's complete. After this point, it's possible
    //    the fshost process will be terminated externally.
    shutdown_responder.close()?;

    Ok(())
}
