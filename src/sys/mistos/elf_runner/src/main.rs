// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO Follow 2018 idioms
#![allow(elided_lifetimes_in_paths)]
// This is needed for the pseudo_directory nesting in crate::model::tests
#![recursion_limit = "256"]
// Printing to stdout and stderr directly is discouraged for component_manager.
// Instead, the tracing library, e.g. through macros like `info!`, and `error!`,
// should be used.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr,))]

use crate::bootfs::BootfsSvc;
use crate::builtin::log::{ReadOnlyLog, WriteOnlyLog};
use crate::builtin::log_sink::LogSink;
use crate::builtin::svc_controller::SvcController;
use crate::builtin::time::create_utc_clock;
use ::cm_logger::klog;
use builtins::vmex_resource::VmexResource;
use elf_runner::process_launcher::ProcessLauncher;
use elf_runner::vdso_vmo::{get_next_vdso_vmo, get_stable_vdso_vmo};
use fidl::endpoints::{ClientEnd, Proxy, RequestStream};
use fidl::HandleBased;
use fuchsia_boot::UserbootRequest;
use fuchsia_component::server::*;
use fuchsia_runtime::{job_default, process_self, take_startup_handle, HandleInfo, HandleType};
use fuchsia_zircon::JobCriticalOptions;
use futures::channel::oneshot;
use futures::prelude::*;
use process_builder::{NamespaceEntry, ProcessBuilder, StartupHandle};
use std::ffi::CString;
use std::io::{self, Write};
use std::process::{self, Command};
use std::sync::Arc;
use std::{env, thread};
use tracing::{error, info, warn};
use {
    fidl_fuchsia_boot as fuchsia_boot, fidl_fuchsia_io as fio, fidl_fuchsia_ldsvc as fldsvc,
    fuchsia_async as fasync, fuchsia_zircon as zx,
};

mod bootfs;
mod builtin;

extern "C" {
    fn dl_set_loader_service(
        handle: fuchsia_zircon::sys::zx_handle_t,
    ) -> fuchsia_zircon::sys::zx_handle_t;
}

extern "C" {
    fn dl_clone_loader_service(handle: *mut zx::sys::zx_handle_t) -> zx::sys::zx_status_t;
}

// Clone the current loader service to provide to the new test processes.
fn clone_loader_service() -> Result<ClientEnd<fldsvc::LoaderMarker>, zx::Status> {
    let mut raw = 0;
    let status = unsafe { dl_clone_loader_service(&mut raw) };
    zx::Status::ok(status)?;

    let handle = unsafe { zx::Handle::from_raw(raw) };
    Ok(ClientEnd::new(zx::Channel::from(handle)))
}

fn namespace_entry(path: &str, flags: fio::OpenFlags) -> NamespaceEntry {
    let ns_path = path.to_string();
    let ns_dir = fuchsia_fs::directory::open_in_namespace(path, flags).unwrap();
    // TODO(https://fxbug.dev/42060182): Use Proxy::into_client_end when available.
    let client_end = ClientEnd::new(
        ns_dir.into_channel().expect("could not convert proxy to channel").into_zx_channel(),
    );
    NamespaceEntry { path: CString::new(ns_path).expect(""), directory: client_end }
}

fn boot_dir_namespace_entry() -> NamespaceEntry {
    namespace_entry("/boot", fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE)
}

fn pkg_dir_namespace_entry() -> NamespaceEntry {
    // Create a kind of link with /boot to programs that use /pkg
    let mut pkg = boot_dir_namespace_entry();
    pkg.path = CString::new("/pkg").expect("Failed to allocate CString");
    pkg
}

fn svc_dir_namespace_entry() -> NamespaceEntry {
    namespace_entry("/svc", fio::OpenFlags::RIGHT_READABLE)
}

fn main() {
    // Set ourselves as critical to our job. If we do not fail gracefully, our
    // job will be killed.
    if let Err(err) =
        job_default().set_critical(JobCriticalOptions::RETCODE_NONZERO, &process_self())
    {
        panic!("Component manager failed to set itself as critical: {}", err);
    }

    // Close any loader service passed. If userboot invoked component manager directly,
    // this service was the only reason userboot
    // continued to run and closing it will let userboot terminate.
    let ldsvc = unsafe {
        fuchsia_zircon::Handle::from_raw(dl_set_loader_service(
            fuchsia_zircon::sys::ZX_HANDLE_INVALID,
        ))
    };
    drop(ldsvc);

    klog::KernelLogger::init();
    info!("started");

    let (sender, receiver) = oneshot::channel::<zx::Handle>();

    let system_resource_handle =
        take_startup_handle(HandleType::SystemResource.into()).map(zx::Resource::from);

    let mut executor = fasync::SendExecutor::new(1);

    let run_root_fut = async move {
        let bootfs_svc = BootfsSvc::new().expect("Failed to create Rust bootfs");

        let bootfs_svc = Some(bootfs_svc);
        let utc = create_utc_clock(&bootfs_svc).await.expect("failed to create UTC clock");

        let service = bootfs_svc
            .unwrap()
            .ingest_bootfs_vmo_with_system_resource(&system_resource_handle)
            .expect("Failed to ingest bootfs")
            .publish_kernel_vmo(get_stable_vdso_vmo().unwrap())
            .expect("Failed to publish vdso/stable")
            .publish_kernel_vmo(get_next_vdso_vmo().unwrap())
            .expect("Failed to publish vdso/next")
            .publish_kernel_vmos(HandleType::KernelFileVmo, 0)
            .expect("Failed to publish KernelFileVmo");

        let _ = service.create_and_bind_vfs();

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
                                warn!("Expected at most a single SvcStash, but more were found. Last entry will be preserved.");
                            }
                            svc_stash_provider = Some(stash_svc_endpoint.into_channel());
                        }
                    }
                }
            } else if let Err(err) = messages {
                error!("Error extracting 'fuchsia.boot.Userboot' messages:  {err}");
            }
        }

        let svc_stash = SvcController::new(svc_stash_provider.unwrap());
        svc_stash.wait_for_epitaph().await;

        let mut service_fs = ServiceFs::new();

        // Set up the ReadOnlyLog service.
        let debuglog_resource = system_resource_handle
            .as_ref()
            .map(|handle| {
                match handle.create_child(
                    zx::ResourceKind::SYSTEM,
                    None,
                    zx::sys::ZX_RSRC_SYSTEM_DEBUGLOG_BASE,
                    1,
                    b"debuglog",
                ) {
                    Ok(resource) => Some(resource),
                    Err(_) => None,
                }
            })
            .flatten();

        if let Some(debuglog_resource) = debuglog_resource {
            let read_only_log = ReadOnlyLog::new(debuglog_resource);

            service_fs.add_fidl_service(move |stream| {
                let read_only_log = read_only_log.clone();
                fasync::Task::spawn(async move {
                    read_only_log.serve(stream).await.expect("Failed to serve read only log.");
                })
                .detach();
            });
        }

        // Set up WriteOnlyLog service.
        let debuglog_resource = system_resource_handle
            .as_ref()
            .map(|handle| {
                match handle.create_child(
                    zx::ResourceKind::SYSTEM,
                    None,
                    zx::sys::ZX_RSRC_SYSTEM_DEBUGLOG_BASE,
                    1,
                    b"debuglog",
                ) {
                    Ok(resource) => Some(resource),
                    Err(_) => None,
                }
            })
            .flatten();

        if let Some(debuglog_resource) = debuglog_resource {
            let write_only_log = WriteOnlyLog::new(
                zx::DebugLog::create(&debuglog_resource, zx::DebugLogOpts::empty()).unwrap(),
            );

            service_fs.add_fidl_service(move |stream| {
                let write_only_log = write_only_log.clone();
                fasync::Task::spawn(async move {
                    write_only_log.serve(stream).await.expect("Failed to serve write only log.");
                })
                .detach();
            });
        }

        service_fs.add_fidl_service(move |stream| {
            fasync::Task::spawn(async move {
                let result = ProcessLauncher::serve(stream).await;
                if let Err(error) = result {
                    warn!(%error, "ProcessLauncher.serve failed");
                }
            })
            .detach();
        });

        service_fs.add_fidl_service(move |stream| {
            fasync::Task::spawn(async move {
                let result = LogSink::serve(stream).await;
                if let Err(error) = result {
                    warn!(%error, "ProcessLauncher.serve failed");
                }
            })
            .detach();
        });

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
        sender.send(utc.into_handle()).unwrap();

        // Start up ServiceFs
        service_fs.collect::<()>().await;
    };

    thread::spawn(|| {
        let use_process_builder = true;

        let mut utc_handle = zx::Handle::invalid();

        // Wait until services are up
        futures::executor::block_on(async {
            utc_handle = receiver.await.expect("Failed to get UTC handle.");
            //assert_eq!(receiver.await, Ok(42));
        });

        let args: Vec<String> = env::args().collect();
        if use_process_builder {
            let mut executor = fasync::SendExecutor::new(1);
            let run_root_fut = async move {
                let bin_path = "/boot/".to_owned() + &args[1];
                let file = fdio::open_fd(
                    bin_path.as_str(),
                    fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
                )
                .expect("Failed to open");
                let vmo = fdio::get_vmo_exec_from_file(&file).expect("Failed to get vmo");

                // Create a new child job of this process's (this process that this code is running in) own 'default job'.
                let job = job_default().create_child_job().expect("Failed to create job");

                let procname: CString =
                    CString::new(bin_path.as_bytes()).expect("Failed to generate procname");
                let mut builder = ProcessBuilder::new(
                    &procname,
                    &job,
                    zx::ProcessOptions::empty(),
                    vmo,
                    get_next_vdso_vmo().unwrap(),
                )
                .expect("");

                // Build the command line args for the new process and send them to the launcher.
                let mut all_args: Vec<String> = vec![bin_path];
                all_args.extend(args[2..].to_vec());

                let arg_cstr =
                    all_args.into_iter().map(|a| CString::new(a)).collect::<Result<_, _>>();

                builder.add_arguments(arg_cstr.expect(""));

                // Send environment variables for the new process
                /*let vec_of_env: Result<Vec<CString>, _> =
                    vec![{ "LD_DEBUG=1" }].into_iter().map(CString::new).collect();
                builder.add_environment_variables(vec_of_env.expect(""));*/

                let stdout_res = zx::Resource::from(zx::Handle::invalid());
                let stdout = zx::DebugLog::create(&stdout_res, zx::DebugLogOpts::empty()).unwrap();

                // Add handles for the new process's default job (by convention, this is the same job that the
                // new process is launched in) and the fuchsia.ldsvc.Loader service created above, then send to
                // the launcher.
                let job_dup = job.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("");
                builder
                    .add_handles(vec![
                        StartupHandle {
                            handle: job_dup.into_handle(),
                            info: HandleInfo::new(HandleType::DefaultJob, 0),
                        },
                        StartupHandle {
                            handle: clone_loader_service().expect("").into_handle(),
                            info: HandleInfo::new(HandleType::LdsvcLoader, 0),
                        },
                        StartupHandle {
                            handle: stdout.into(),
                            info: HandleInfo::new(
                                HandleType::FileDescriptor,
                                0 | 32768, /*USE_FOR_STDIO */
                            ),
                        },
                        StartupHandle {
                            handle: utc_handle.into(),
                            info: HandleInfo::new(HandleType::ClockUtc, 0),
                        },
                    ])
                    .expect("");

                /*let next_vdso = true;
                if next_vdso {
                    builder
                        .add_handles(vec![::StartupHandle {
                            handle: get_next_vdso_vmo().unwrap().into_handle(),
                            info: HandleInfo::new(HandleType::VdsoVmo, 0),
                        }])
                        .expect("");
                }*/

                let entries = vec![
                    svc_dir_namespace_entry(),
                    boot_dir_namespace_entry(),
                    pkg_dir_namespace_entry(),
                ];
                builder.add_namespace_entries(entries).expect("");

                let process = builder.build().await.expect("").start().expect("");

                // Wait for process to return
                fasync::OnSignals::new(&process, zx::Signals::PROCESS_TERMINATED).await.unwrap();
                let process_info = process.info().expect("");

                info!("Return code : {:?}", process_info.return_code);
                process::exit(process_info.return_code as i32);
            };
            executor.run(run_root_fut);
        } else {
            // TODO (Herrera) Check if the file exists in BootFS
            let mut command = Command::new("/boot/".to_owned() + &args[1]);

            //command.env("LD_DEBUG", "1");

            // Add args
            command.args(&args[2..]);

            /*let output = match command.output() {
                Ok(out) => out,
                Err(e) => {
                    let err = format!("Failed to spawn {} as child: {:?}", args[0], e);
                    return ("failed".to_owned(), err.into_bytes(), ExitCode::FAILURE);
                }
            };*/
            let std::process::Output { stdout, stderr, status } = command.output().expect("");

            io::stdout().write_all(&stdout).unwrap();
            io::stdout().write_all(&stderr).unwrap();

            process::exit(status.code().unwrap());
        }
    });

    executor.run(run_root_fut);
}
