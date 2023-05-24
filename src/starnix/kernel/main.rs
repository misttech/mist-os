// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(clippy::all)]
#![allow(clippy::too_many_arguments)]
// TODO(fxbug.dev/122028): Remove this allow once the lint is fixed.
#![allow(unknown_lints, clippy::extra_unused_type_parameters)]

#[macro_use]
extern crate macro_rules_attribute;

use crate::execution::create_container;
use anyhow::Error;
use fidl::endpoints::ControlHandle;
use fidl_fuchsia_component_runner as fcrunner;
use fidl_fuchsia_process_lifecycle as flifecycle;
use fidl_fuchsia_starnix_container as fstarcontainer;
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use fuchsia_runtime as fruntime;
use futures::{StreamExt, TryStreamExt};

#[macro_use]
mod trace;

mod arch;
mod auth;
mod bpf;
mod collections;
mod device;
mod drop_notifier;
mod dynamic_thread_pool;
mod execution;
mod fs;
mod loader;
mod lock;
mod logging;
mod mm;
mod mutable_state;
mod selinux;
mod signals;
mod syscalls;
mod task;
mod types;
mod vdso;
mod vmex_resource;

#[cfg(test)]
mod testing;

fn maybe_serve_lifecycle() {
    if let Some(lifecycle) =
        fruntime::take_startup_handle(fruntime::HandleInfo::new(fruntime::HandleType::Lifecycle, 0))
    {
        fasync::Task::local(async move {
            if let Ok(mut stream) =
                fidl::endpoints::ServerEnd::<flifecycle::LifecycleMarker>::new(lifecycle.into())
                    .into_stream()
            {
                while let Ok(Some(request)) = stream.try_next().await {
                    match request {
                        flifecycle::LifecycleRequest::Stop { control_handle } => {
                            control_handle.shutdown();
                            std::process::exit(0);
                        }
                    }
                }
            }
        })
        .detach();
    }
}

enum KernelServices {
    ComponentRunner(fcrunner::ComponentRunnerRequestStream),
    ContainerController(fstarcontainer::ControllerRequestStream),
}

#[fuchsia::main(logging_tags = ["starnix"])]
async fn main() -> Result<(), Error> {
    // Because the starnix kernel state is shared among all of the processes in the same job,
    // we need to kill those in addition to the process which panicked.
    kill_job_on_panic::install_hook("\n\n\n\nSTARNIX KERNEL PANIC\n\n\n\n");

    fuchsia_trace_provider::trace_provider_create_with_fdio();
    trace_instant!(
        trace_category_starnix!(),
        trace_name_start_kernel!(),
        fuchsia_trace::Scope::Thread
    );

    let container = create_container().await?;

    maybe_serve_lifecycle();

    let mut fs = ServiceFs::new_local();
    fs.dir("svc")
        .add_fidl_service(KernelServices::ComponentRunner)
        .add_fidl_service(KernelServices::ContainerController);
    fs.add_remote("linux_root", execution::expose_root(&container)?);

    inspect_runtime::serve(fuchsia_inspect::component::inspector(), &mut fs)?;

    fs.take_and_serve_directory_handle()?;

    fs.for_each_concurrent(None, |request: KernelServices| async {
        let container = container.clone();
        match request {
            KernelServices::ComponentRunner(stream) => {
                execution::serve_component_runner(stream, container)
                    .await
                    .expect("failed to start component runner");
            }
            KernelServices::ContainerController(stream) => {
                execution::serve_container_controller(stream, container)
                    .await
                    .expect("failed to start container controller");
            }
        }
    })
    .await;

    Ok(())
}
