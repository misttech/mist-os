// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::boot_args::BootArgs;
use crate::config::apply_boot_args_to_config;
use crate::environment::{Environment, FshostEnvironment};
use crate::inspect::register_stats;
use crate::watcher::{DirSource, PathSource, PathSourceType, WatchSource, Watcher};
use anyhow::{format_err, Error};
use fidl::prelude::*;
use fuchsia_runtime::{take_startup_handle, HandleType};
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::{stream, StreamExt};
use std::collections::HashSet;
use std::sync::Arc;
use vfs::directory::entry_container::Directory;
use vfs::directory::helper::DirectlyMutable;
use vfs::execution_scope::ExecutionScope;
use vfs::path::Path;
use vfs::remote::remote_dir;
use zx::sys::zx_debug_write;
use {fidl_fuchsia_fshost as fshost, fidl_fuchsia_io as fio};

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

const DEV_CLASS_BLOCK: &str = "/dev/class/block";
const DEV_CLASS_NAND: &str = "/dev/class/nand";
const VOLUME_SERVICE_PATH: &str = "/svc/fuchsia.hardware.block.volume.Service";

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
    let (watcher, device_stream) = Watcher::new(if config.storage_host {
        let mut sources = vec![
            Box::new(PathSource::new(DEV_CLASS_BLOCK, PathSourceType::Block))
                as Box<dyn WatchSource>,
            Box::new(PathSource::new(DEV_CLASS_NAND, PathSourceType::Nand)) as Box<dyn WatchSource>,
        ];
        sources.extend(
            fuchsia_fs::directory::open_in_namespace(VOLUME_SERVICE_PATH, fio::Flags::empty())
                .map(|d| Box::new(DirSource::new(d)) as Box<dyn WatchSource>),
        );
        sources
    } else {
        vec![
            Box::new(PathSource::new(DEV_CLASS_BLOCK, PathSourceType::Block)),
            Box::new(PathSource::new(DEV_CLASS_NAND, PathSourceType::Nand)),
        ]
    })
    .await?;
    // Potentially launch the boot items ramdisk. It's not fatal, so if it fails we print an error
    // and continue.
    let ramdisk_device = if config.ramdisk_image {
        tracing::info!("setting up ramdisk image from boot items");
        ramdisk::set_up_ramdisk(config.storage_host).await.unwrap_or_else(|error| {
            tracing::error!(?error, "failed to set up ramdisk filesystems");
            None
        })
    } else {
        None
    };

    let inspector = fuchsia_inspect::component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(&inspector, inspect_runtime::PublishOptions::default());
    // matcher_lock is used to block matching temporarily and inject
    // paths to be ignored.
    let matcher_lock = Arc::new(Mutex::new(HashSet::new()));
    let mut env =
        FshostEnvironment::new(config.clone(), matcher_lock.clone(), inspector.clone(), watcher);

    let launcher = env.launcher();
    // Records inspect metrics. Too expensive to build the tree data in newer fxfs environments.
    register_stats(inspector.root(), env.data_root()?, config.data_filesystem_format != "fxfs")
        .await;
    let blob_exposed_dir = env.blobfs_exposed_dir()?;
    let data_exposed_dir = env.data_exposed_dir()?;
    let crypt_service_exposed_dir = env.crypt_service_exposed_dir()?;
    let export = vfs::pseudo_directory! {
        "fs" => vfs::pseudo_directory! {
            "blob" => remote_dir(blob_exposed_dir),
            "data" => remote_dir(data_exposed_dir),
        },
        "mnt" => vfs::pseudo_directory! {},
    };
    if config.storage_host {
        export.add_entry("gpt", remote_dir(env.gpt_exposed_dir()?)).unwrap();
    }
    let env: Arc<Mutex<dyn Environment>> = Arc::new(Mutex::new(env));
    let svc_dir = vfs::pseudo_directory! {
        fshost::AdminMarker::PROTOCOL_NAME =>
            service::fshost_admin(
                env.clone(),
                config.clone(),
                ramdisk_device.as_ref().map(|d| d.topological_path().to_string()),
                launcher,
                matcher_lock.clone()
            ),
    };
    if config.fxfs_blob {
        svc_dir
            .add_entry(
                fidl_fuchsia_update_verify::BlobfsVerifierMarker::PROTOCOL_NAME,
                fxblob::blobfs_verifier_service(),
            )
            .unwrap();
        svc_dir
            .add_entry(
                fidl_fuchsia_update_verify::ComponentOtaHealthCheckMarker::PROTOCOL_NAME,
                fxblob::ota_health_check_service(),
            )
            .unwrap();
    }
    if config.data_filesystem_format == "fxfs" {
        if let Some(dir) = crypt_service_exposed_dir {
            svc_dir
                .add_entry(
                    fidl_fuchsia_fxfs::CryptManagementMarker::PROTOCOL_NAME,
                    vfs::service::endpoint(move |_scope, server_end| {
                        dir.open3(
                            fidl_fuchsia_fxfs::CryptManagementMarker::PROTOCOL_NAME,
                            fio::Flags::PROTOCOL_SERVICE,
                            &fio::Options::default(),
                            server_end.into(),
                        )
                        .unwrap();
                    }),
                )
                .unwrap();
        }
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

    // TODO(https://fxbug.dev/42069366): //src/tests/oom looks for "fshost: lifecycle handler ready" to
    // indicate the watcher is about to start.
    tracing::info!("fshost: lifecycle handler ready");

    // Run the main loop of fshost, handling devices as they appear according to our filesystem
    // policy.
    let mut fs_manager = manager::Manager::new(&config, env, matcher_lock);
    let shutdown_responder = if config.disable_block_watcher {
        // If the block watcher is disabled, fshost just waits on the shutdown receiver instead of
        // processing devices.
        shutdown_rx
            .next()
            .await
            .ok_or_else(|| format_err!("shutdown signal stream ended unexpectedly"))?
    } else {
        fs_manager
            .device_handler(stream::iter(ramdisk_device).chain(device_stream), shutdown_rx)
            .await?
    };

    tracing::info!("shutdown signal received");
    // TODO(https://fxbug.dev/42069366): //src/tests/oom looks for "received shutdown command over lifecycle
    // interface" to indicate fshost shutdown is starting. Shutdown logs have to go straight to
    // serial because of timing issues (https://fxbug.dev/42179880).
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
