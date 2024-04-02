// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "256"]
#![allow(clippy::too_many_arguments)]
// TODO(https://fxbug.dev/42073005): Remove this allow once the lint is fixed.
#![allow(unknown_lints, clippy::extra_unused_type_parameters)]

// Avoid unused crate warnings on non-test/non-debug builds because this needs to be an
// unconditional dependency for rustdoc generation.
use extended_pstate as _;
use tracing_mutex as _;

use anyhow::Error;
use fidl::endpoints::ControlHandle;
use fidl_fuchsia_component_runner as frunner;
use fidl_fuchsia_process_lifecycle as flifecycle;
use fidl_fuchsia_starnix_container as fstarcontainer;
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::health::Reporter;
use fuchsia_runtime as fruntime;
use futures::{StreamExt, TryStreamExt};
use starnix_core::mm::{init_usercopy, zxio_maybe_faultable_copy_impl};
use starnix_kernel_runner::{
    create_component_from_stream, serve_component_runner, serve_container_controller, Container,
    ContainerServiceConfig,
};
use starnix_logging::{log_debug, trace_instant, CATEGORY_STARNIX, NAME_START_KERNEL};

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

fn maybe_serve_lifecycle() {
    if let Some(lifecycle) =
        fruntime::take_startup_handle(fruntime::HandleInfo::new(fruntime::HandleType::Lifecycle, 0))
    {
        fasync::Task::local(async move {
            if let Ok(mut stream) =
                fidl::endpoints::ServerEnd::<flifecycle::LifecycleMarker>::new(lifecycle.into())
                    .into_stream()
            {
                if let Ok(Some(request)) = stream.try_next().await {
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
    /// This service lets clients start a single container using this kernel.
    ///
    /// The `starnix_kernel` is capable of running a single container, which can be started using
    /// this protocol. Attempts to use this protocol a second time will fail.
    ///
    /// This service uses the `ComponentRunner` protocol but the service is exposed using the name
    /// `fuchsia.starnix.container.Runner` to reduce confusion with the instance of the
    /// `ComponentRunner` protocol that runs components inside the container.
    ContainerRunner(frunner::ComponentRunnerRequestStream),

    /// This service lets clients run components inside the container being run by this kernel.
    ///
    /// This service will wait to process any requests until the kernel starts a container.
    ///
    /// This service is also exposed via the container itself.
    ComponentRunner(frunner::ComponentRunnerRequestStream),

    /// This service lets clients control the container being run by this kernel.
    ///
    /// This service will wait to process any requests until the kernel starts a container.
    ///
    /// This service is also exposed via the container itself.
    ContainerController(fstarcontainer::ControllerRequestStream),
}

async fn build_container(
    stream: frunner::ComponentRunnerRequestStream,
    returned_config: &mut Option<ContainerServiceConfig>,
) -> Result<Container, Error> {
    let (container, config) = create_component_from_stream(stream).await?;
    *returned_config = Some(config);
    Ok(container)
}

#[fuchsia::main(logging_tags = ["starnix"], logging_blocking)]
async fn main() -> Result<(), Error> {
    // Because the starnix kernel state is shared among all of the processes in the same job,
    // we need to kill those in addition to the process which panicked.
    kill_job_on_panic::install_hook("\n\n\n\nSTARNIX KERNEL PANIC\n\n\n\n");

    let _inspect_server_task = inspect_runtime::publish(
        fuchsia_inspect::component::init_inspector_with_size(1_000_000),
        inspect_runtime::PublishOptions::default(),
    );
    let mut health = fuchsia_inspect::component::health();
    health.set_starting_up();

    fuchsia_trace_provider::trace_provider_create_with_fdio();
    fuchsia_trace_provider::trace_provider_wait_for_init();
    trace_instant!(CATEGORY_STARNIX, NAME_START_KERNEL, fuchsia_trace::Scope::Thread);

    let container = async_lock::OnceCell::<Container>::new();

    maybe_serve_lifecycle();

    let mut fs = ServiceFs::new_local();
    fs.dir("svc")
        .add_fidl_service_at("fuchsia.starnix.container.Runner", KernelServices::ContainerRunner)
        .add_fidl_service(KernelServices::ComponentRunner)
        .add_fidl_service(KernelServices::ContainerController);

    let inspector = fuchsia_inspect::component::inspector();
    #[cfg(target_arch = "x86_64")]
    {
        inspector.root().record_string(
            "x86_64_extended_pstate_strategy",
            format!("{:?}", *extended_pstate::x86_64::PREFERRED_STRATEGY),
        );
    }
    inspector.root().record_lazy_child("not_found", starnix_logging::not_found_lazy_node_callback);
    inspector.root().record_lazy_child("stubs", starnix_logging::track_stub_lazy_node_callback);

    log_debug!("Serving kernel services on outgoing directory handle.");
    fs.take_and_serve_directory_handle()?;
    health.set_ok();

    // We call this early during Starnix boot to make sure the usercopy utilities
    // are ready for use before any restricted-mode/Linux processes are created.
    init_usercopy();

    fs.for_each_concurrent(None, |request: KernelServices| async {
        match request {
            KernelServices::ContainerRunner(stream) => {
                let mut config: Option<ContainerServiceConfig> = None;
                let container = container
                    .get_or_try_init(|| build_container(stream, &mut config))
                    .await
                    .expect("failed to start container");
                if let Some(config) = config {
                    container
                        .serve(config)
                        .await
                        .expect("failed to serve the expected services from the container");
                }
            }
            KernelServices::ComponentRunner(stream) => {
                serve_component_runner(stream, &container.wait().await.system_task())
                    .await
                    .expect("failed to start component runner");
            }
            KernelServices::ContainerController(stream) => {
                serve_container_controller(stream, &container.wait().await.system_task())
                    .await
                    .expect("failed to start container controller");
            }
        }
    })
    .await;

    Ok(())
}
